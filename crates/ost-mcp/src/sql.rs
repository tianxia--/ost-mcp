//! Running SQL against the mounted store and shaping results as JSON.

use duckdb::types::{TimeUnit, Value};
use duckdb::Connection;
use ost::props::format_time_us;
use serde_json::{Map, Value as J};

/// Statements a caller is allowed to run.
///
/// This is a guardrail, not a sandbox — the real protection is
/// `enable_external_access=false` plus `lock_configuration=true`, set when the
/// connection is opened, which stop DuckDB reading or writing files and stop a
/// `PRAGMA x = y` (DuckDB's spelling of `SET`) from changing any setting. The
/// keyword check exists so that a model asking for data cannot accidentally
/// mutate the in-memory catalog the views live in.
const READ_ONLY_STARTS: [&str; 8] = [
    "select", "with", "describe", "summarize", "show", "explain", "pragma", "table",
];

/// The first word of `s`, lowercased, and the text after it. Leading
/// parentheses are skipped, so `(SELECT 1) UNION (SELECT 2)` starts with
/// `select`.
fn first_word(s: &str) -> (String, &str) {
    let s = s.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
    let end = s
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(s.len());
    (s[..end].to_ascii_lowercase(), &s[end..])
}

/// Reject anything that is not a single read-only statement.
pub fn check_read_only(sql: &str) -> Result<(), String> {
    let trimmed = sql.trim();
    let (first, rest) = first_word(trimmed);
    // `EXPLAIN ANALYZE` executes the statement it wraps, so that statement has
    // to pass the same check.
    if first == "explain" {
        let (next, after) = first_word(rest);
        let inner = if next == "analyze" || next == "analyse" { after } else { rest };
        return check_read_only(inner);
    }
    if !READ_ONLY_STARTS.contains(&first.as_str()) {
        return Err(format!(
            "only read-only statements are allowed; this one starts with `{first}`"
        ));
    }
    // One statement per call, so a leading SELECT cannot smuggle a second verb.
    if trimmed.trim_end_matches(';').contains(';') {
        return Err("only one statement per call".into());
    }
    Ok(())
}

pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<J>,
    /// True when the query produced more rows than `limit`.
    pub truncated: bool,
}

/// Run `sql` and collect at most `limit` rows as JSON objects.
pub fn query<P: duckdb::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
    limit: usize,
) -> duckdb::Result<Rows> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params)?;
    let columns = rows
        .as_ref()
        .map(|s| s.column_names())
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        if out.len() >= limit {
            truncated = true;
            break;
        }
        let mut obj = Map::with_capacity(columns.len());
        for (i, name) in columns.iter().enumerate() {
            obj.insert(name.clone(), to_json(row.get::<usize, Value>(i)?));
        }
        out.push(J::Object(obj));
    }
    Ok(Rows {
        columns,
        rows: out,
        truncated,
    })
}

/// [`query`], interrupted if it runs longer than `timeout`.
///
/// The MCP server holds one connection behind a lock, so a runaway statement
/// (a cross join of `messages` with itself, say) would otherwise block every
/// later tool call for the rest of the session.
pub fn query_with_timeout<P: duckdb::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
    limit: usize,
    timeout: std::time::Duration,
) -> Result<Rows, String> {
    use std::sync::mpsc::{channel, RecvTimeoutError};

    let handle = conn.interrupt_handle();
    let (done, wait) = channel::<()>();
    let watchdog = std::thread::spawn(move || {
        let fired = wait.recv_timeout(timeout) == Err(RecvTimeoutError::Timeout);
        if fired {
            handle.interrupt();
        }
        fired
    });
    let result = query(conn, sql, params, limit);
    drop(done);
    // Joined before returning, so an interrupt can only land on this query.
    let fired = watchdog.join().unwrap_or(false);
    match result {
        Err(_) if fired => Err(format!(
            "query cancelled after {}s; narrow it with a WHERE clause or LIMIT",
            timeout.as_secs()
        )),
        other => other.map_err(|e| e.to_string()),
    }
}

/// A timestamp becomes `YYYY-MM-DD HH:MM:SS` (this computer's local time, see
/// [`crate::vtab::local_wall_us`]) rather than a bare integer,
/// because that is what a model can reason about without being told the unit.
fn to_json(v: Value) -> J {
    match v {
        Value::Null => J::Null,
        Value::Boolean(b) => J::Bool(b),
        Value::TinyInt(n) => n.into(),
        Value::SmallInt(n) => n.into(),
        Value::Int(n) => n.into(),
        Value::BigInt(n) => n.into(),
        Value::UTinyInt(n) => n.into(),
        Value::USmallInt(n) => n.into(),
        Value::UInt(n) => n.into(),
        Value::UBigInt(n) => n.into(),
        // `sum()` over a BIGINT column returns HUGEINT, so this is a normal
        // aggregate result, not an exotic one.
        Value::HugeInt(n) => i128_json(n),
        Value::UHugeInt(n) => i128::try_from(n).map(i128_json).unwrap_or(J::Null),
        Value::Float(n) => J::from(n as f64),
        Value::Double(n) => J::from(n),
        Value::Text(s) => J::String(s),
        Value::Timestamp(unit, n) => J::String(format_time_us(match unit {
            TimeUnit::Second => n.saturating_mul(1_000_000),
            TimeUnit::Millisecond => n.saturating_mul(1_000),
            TimeUnit::Microsecond => n,
            TimeUnit::Nanosecond => n / 1_000,
        })),
        Value::Blob(b) => J::String(format!("<{} bytes>", b.len())),
        other => J::String(format!("{other:?}")),
    }
}

/// JSON has no 128-bit integer, so anything outside `i64` becomes a string
/// rather than a silently rounded float.
fn i128_json(n: i128) -> J {
    match i64::try_from(n) {
        Ok(v) => v.into(),
        Err(_) => J::String(n.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_check() {
        assert!(check_read_only("SELECT 1").is_ok());
        assert!(check_read_only("  with x as (select 1) select * from x").is_ok());
        assert!(check_read_only("(SELECT 1) UNION (SELECT 2)").is_ok());
        assert!(check_read_only("select 1;").is_ok());
        assert!(check_read_only("CREATE TABLE t (a INT)").is_err());
        assert!(check_read_only("COPY messages TO 'out.csv'").is_err());
        assert!(check_read_only("select 1; drop view messages").is_err());
        assert!(check_read_only("EXPLAIN SELECT 1").is_ok());
        assert!(check_read_only("explain analyze select 1").is_ok());
        assert!(check_read_only("EXPLAIN ANALYZE DROP VIEW messages").is_err());
        assert!(check_read_only("explain create view v as select 1").is_err());
        assert!(check_read_only("explain").is_err());
    }

    /// The connection `main::open_db` builds: no file access, no setting
    /// changes, whatever the statement passes the keyword check.
    fn locked_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::lock_down(&conn).unwrap();
        conn
    }

    #[test]
    fn pragma_cannot_change_settings() {
        let conn = locked_conn();
        for q in [
            "PRAGMA enable_profiling = 'json'",
            "PRAGMA profiling_output = 'out.json'",
            "PRAGMA memory_limit = '1TB'",
            "PRAGMA lock_configuration = false",
            "PRAGMA enable_external_access = true",
        ] {
            assert!(check_read_only(q).is_ok(), "{q} should reach DuckDB");
            assert!(conn.execute_batch(q).is_err(), "{q} should be refused");
        }
        // A read-only pragma still works.
        assert!(conn.execute_batch("PRAGMA database_list").is_ok());
    }

    #[test]
    fn runaway_query_is_cancelled() {
        let conn = locked_conn();
        let start = std::time::Instant::now();
        let r = query_with_timeout(
            &conn,
            "SELECT sum(a.range * b.range) FROM range(1000000) a, range(1000000) b",
            [],
            10,
            std::time::Duration::from_millis(300),
        );
        let Err(e) = r else { panic!("the cross join finished inside the timeout") };
        assert!(e.contains("cancelled"), "{e}");
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        // The connection is usable afterwards.
        let ok = query_with_timeout(&conn, "SELECT 1 AS x", [], 10, std::time::Duration::from_secs(5));
        assert_eq!(ok.unwrap().rows.len(), 1);
    }

    #[test]
    fn no_file_access() {
        let conn = locked_conn();
        assert!(conn.execute_batch("SELECT * FROM read_csv('/etc/hosts')").is_err());
    }
}
