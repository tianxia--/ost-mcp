//! Extracting `IPM.Note` message records from a decompressed `HxStore.hxd`
//! block.
//!
//! Credited in full to
//! [`securized/hxstore-reverse-engineering`](https://github.com/securized/hxstore-reverse-engineering)
//! (MIT) — see `docs/mac-outlook-format.md` for what was independently
//! re-verified in this project against a real store rather than taken on
//! trust. The record layout itself is not a fixed struct: metadata is a
//! sequence of NUL-terminated UTF-16LE strings around the literal
//! `"IPM.Note"` anchor, at byte offsets that drift with every field's length,
//! so a slot is identified by its *position in the sequence*, confirmed by a
//! type check, never by a fixed displacement.
//!
//! This is an independent implementation of that approach, not a port —
//! the heuristics below (email/GUID/opaque-blob detection, the
//! stored-twice-back-to-back rule that tells a subject from a body preview,
//! the distance bounds that keep one record's fields out of its neighbour's)
//! are re-derived from the credited project's write-up and cross-checked
//! against this project's own file, not copied from its source.

/// One message recovered from the store. Every field is `Option`/empty
/// because the store is a lossy cache — see `docs/mac-outlook-format.md` for
/// the measured coverage on this project's own file.
#[derive(Debug, Clone, Default)]
pub struct HxMessage {
    pub sender_address: Option<String>,
    pub sender_name: Option<String>,
    pub internet_message_id: Option<String>,
    pub subject: Option<String>,
    /// Outlook's own cached summary, capped at roughly 255 characters — this
    /// is genuinely the entire content the store holds for most messages
    /// (§4.4 of the credited write-up), not a truncated read on this
    /// project's part.
    pub preview: Option<String>,
    /// The full HTML body, when this message is one of the minority the
    /// store keeps a complete copy of.
    pub html: Option<String>,
    pub sent_unix: Option<i64>,
    /// Which message this is, derived from its content by [`deduplicate`]
    /// (0 on a raw record from [`extract`]). It stays the same when the store
    /// is re-read with more mail in it, so an id built from it still names
    /// the same message after a reload.
    pub identity: u64,
}

const ANCHOR: &str = "IPM.Note";
/// A record's sender pair sits within this many bytes of the anchor; a match
/// further out belongs to a neighbouring record, not this one.
const SENDER_MAX_DIST: isize = 320;
/// A display name sits within this many bytes of the address it belongs to.
const NAME_MAX_DIST: isize = 200;
/// A subject candidate this far past the anchor is body text, not a header
/// field.
const SUBJECT_MAX_REL: isize = 4096;

fn anchor_needle() -> Vec<u8> {
    ANCHOR.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// One NUL-terminated UTF-16LE run, with its byte offset relative to an
/// anchor.
#[derive(Debug, Clone)]
struct Field {
    rel: isize,
    text: String,
}

/// Walk UTF-16LE runs in `blob[lo..hi]`, tagging each with its offset
/// relative to `anchor`.
///
/// A run must decode entirely as printable ASCII, Latin-1/Extended, general
/// punctuation, currency symbols or emoji — this store's real text is Latin
/// script, so a run with a meaningful share of anything else (CJK, Hangul,
/// unassigned code points) is binary that happened to decode without error,
/// not a value.
fn walk_fields(blob: &[u8], anchor: usize, lo: usize, hi: usize) -> Vec<Field> {
    let hi = hi.min(blob.len());
    let mut out = Vec::new();
    let mut i = lo;

    while i + 1 < hi {
        if blob[i + 1] != 0 || !(0x20..0x7f).contains(&blob[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut s = String::new();
        loop {
            if i + 1 >= hi {
                break;
            }
            if blob[i + 1] == 0 && (0x20..0x7f).contains(&blob[i]) {
                s.push(blob[i] as char);
                i += 2;
                continue;
            }
            let u = u16::from_le_bytes([blob[i], blob[i + 1]]);
            if (0xA0..0xD800).contains(&u) || (0xE000..0xFFFD).contains(&u) {
                if let Some(c) = char::from_u32(u as u32) {
                    s.push(c);
                    i += 2;
                    continue;
                }
            }
            if (0xD800..0xDC00).contains(&u) && i + 3 < hi {
                let low = u16::from_le_bytes([blob[i + 2], blob[i + 3]]);
                if (0xDC00..0xE000).contains(&low) {
                    let cp = 0x1_0000 + ((u as u32 - 0xD800) << 10) + (low as u32 - 0xDC00);
                    if let Some(c) = char::from_u32(cp) {
                        s.push(c);
                        i += 4;
                        continue;
                    }
                }
            }
            break;
        }
        if s.chars().count() >= 3 && !is_binary_run(&s) {
            out.push(Field { rel: start as isize - anchor as isize, text: s });
        }
    }
    out
}

fn is_binary_run(s: &str) -> bool {
    let mut total = 0usize;
    let mut off_script = 0usize;
    for c in s.chars() {
        total += 1;
        let u = c as u32;
        let latin_or_symbol = u < 0x250
            || (0x2000..0x2070).contains(&u)
            || (0x20A0..0x20D0).contains(&u)
            || matches!(u, 0x2122 | 0x2190..=0x21FF)
            || (0x2600..0x27C0).contains(&u)
            || matches!(u, 0x203C | 0x2049 | 0x23E9..=0x23FA | 0x25AA..=0x25FE)
            || (0x1F000..0x1FAFF).contains(&u)
            || matches!(u, 0xFE0F | 0x200D);
        if !latin_or_symbol {
            off_script += 1;
        }
    }
    total >= 3 && off_script * 5 >= total
}

fn undouble(t: &str) -> String {
    let n = t.chars().count();
    if n.is_multiple_of(2) {
        let half: String = t.chars().take(n / 2).collect();
        if t.chars().skip(n / 2).collect::<String>() == half {
            return half;
        }
    }
    t.to_string()
}

fn is_email(s: &str) -> bool {
    let Some(at) = s.find('@') else { return false };
    if at == 0 || at + 1 >= s.len() || s.contains(' ') || s.len() > 254 {
        return false;
    }
    if !s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'@' | b'.' | b'_' | b'-' | b'+' | b'%')) {
        return false;
    }
    let domain = &s[at + 1..];
    let Some(dot) = domain.rfind('.') else { return false };
    let tld = &domain[dot + 1..];
    !domain.starts_with('.') && dot > 0 && (2..=12).contains(&tld.len()) && tld.bytes().all(|c| c.is_ascii_alphabetic())
}

fn is_guid(t: &str) -> bool {
    let t = t.trim_matches(|c| c == '{' || c == '}');
    let groups: Vec<&str> = t.split('-').collect();
    groups.len() == 5
        && [8usize, 4, 4, 4, 12] == *groups.iter().map(|g| g.len()).collect::<Vec<_>>()
        && t.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// A long unbroken hex run, or SafeLinks' base64-encoded JSON (`{"..."` ->
/// `eyJ...`) — machine payloads that sit among the text fields but are never
/// display text.
fn is_opaque(t: &str) -> bool {
    t.starts_with("eyJ") || (t.len() >= 24 && t.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_enum_value(t: &str) -> bool {
    const FLAGS: [&str; 10] = [
        "IPM.Note", "Anonymous", "Internal", "External", "Normal", "None", "Unknown", "Focused",
        "Mailbox", "SMTP",
    ];
    FLAGS.iter().any(|f| f.eq_ignore_ascii_case(t))
        || t.starts_with("image/")
        || t.starts_with("application/")
        || t.starts_with("text/")
        || t.starts_with("multipart/")
        || is_guid(t)
        || is_opaque(t)
}

fn is_message_id(t: &str) -> bool {
    t.starts_with('<') && t.ends_with('>') && t.contains('@') && !t.contains(' ')
}

/// Does this run read as a person's or organisation's display name?
fn looks_like_person(t: &str) -> bool {
    let n = t.chars().count();
    if !(2..=64).contains(&n) || t.contains('@') || is_enum_value(t) {
        return false;
    }
    if t.contains(". ") || n > 48 || t.contains('<') || t.contains('>') || t.ends_with(',') {
        return false;
    }
    t.chars().filter(|c| c.is_alphabetic()).count() * 2 >= n
}

/// Does this run read as a subject line rather than a body preview, an
/// identifier or a machine payload?
fn looks_like_subject(t: &str) -> bool {
    let n = t.chars().count();
    if !(3..=400).contains(&n) || is_email(t) || is_enum_value(t) {
        return false;
    }
    if t.contains("http") || t.contains("mailto:") {
        return false;
    }
    if t.matches(". ").count() >= 2 {
        return false;
    }
    t.chars().filter(|c| c.is_alphabetic()).count() * 2 >= n
}

fn strip_reply_prefix(s: &str) -> &str {
    let mut t = s.trim();
    loop {
        let lower = t.to_ascii_lowercase();
        let cut = ["re:", "fw:", "fwd:", "aw:", "tr:"].iter().find(|p| lower.starts_with(**p)).map(|p| p.len());
        match cut {
            Some(n) => t = t[n..].trim_start(),
            None => return t,
        }
    }
}

/// `NormalizedSubject` and `Topic` are written back to back and near
/// identical (`Topic` strips reply/forward prefixes; `NormalizedSubject`
/// keeps them). That duplication — not a heading, not a fixed offset — is
/// what makes a subject identifiable at all: a value appearing twice after
/// the anchor is the subject, one appearing once is the body preview.
fn find_subject(after: &[Field]) -> Option<&Field> {
    let key = |s: &str| -> String { strip_reply_prefix(s).chars().take(40).collect() };
    after.iter().find(|f| {
        if f.rel > SUBJECT_MAX_REL {
            return false;
        }
        let t = undouble(f.text.trim());
        if !looks_like_subject(&t) {
            return false;
        }
        let k = key(&t);
        after.iter().filter(|g| key(&undouble(g.text.trim())) == k).count() > 1
    })
}

/// The sender address/name pair: the nearest address to the anchor (within
/// [`SENDER_MAX_DIST`]), and the display name immediately beside it.
///
/// **Measured limitation, not a guess:** a before-anchor match (the common
/// case, "Layout A") is trusted as soon as it is found — there is exactly one
/// sender-shaped candidate in that narrow, bounded window in every case this
/// project has checked. An after-anchor match ("Layout B", §4.2) is
/// different: real records were found, by diagnosing this exact function
/// against a live store with `examples/hx_probe.rs --layout-near`, where the
/// after-anchor region holds *two* address+display-name pairs — a
/// participant's address, then the true sender's, thousands of bytes further
/// out — and this function was picking the nearer, wrong one. There is no
/// signal in what this project has measured so far (§2.8 of
/// `docs/mac-outlook-format.md`) that reliably says which of several
/// after-anchor candidates is the sender, so when more than one exists here,
/// this returns `None` rather than the nearer guess: a wrong sender that
/// looks plausible is worse than no sender, per `CONTRIBUTING.md`.
fn find_sender(before: &[Field], after: &[Field]) -> (Option<String>, Option<String>) {
    if let Some((addr, addr_rel)) = before
        .iter()
        .rev()
        .find(|f| f.rel.abs() <= SENDER_MAX_DIST && is_email(f.text.trim()))
        .map(|f| (undouble(f.text.trim()), f.rel))
    {
        let name = nearby_person_name(before.iter().rev(), addr_rel).or_else(|| nearby_person_name(after.iter(), addr_rel));
        return (Some(addr.to_lowercase()), name);
    }

    let after_emails: Vec<&Field> = after.iter().filter(|f| is_email(f.text.trim())).collect();
    match after_emails.as_slice() {
        [] => (None, None),
        [only] => {
            let addr = undouble(only.text.trim());
            let name = nearby_person_name(after.iter(), only.rel);
            (Some(addr.to_lowercase()), name)
        }
        _ => (None, None),
    }
}

/// A person-name-shaped field within [`NAME_MAX_DIST`] of `addr_rel`.
fn nearby_person_name<'a>(fields: impl Iterator<Item = &'a Field>, addr_rel: isize) -> Option<String> {
    fields
        .filter(|f| f.rel != addr_rel && (f.rel - addr_rel).abs() <= NAME_MAX_DIST)
        .map(|f| undouble(f.text.trim()))
        .find(|t| looks_like_person(t))
}

/// The earliest 100-nanosecond .NET tick value in `span`, as Unix seconds.
///
/// .NET ticks count from `0001-01-01T00:00:00Z`; a record holds several
/// (send, delivery, last-modified, sync), and a message is always sent
/// before any of the others, so the earliest tick in its span is the send
/// time. The range bound (2015-01-01 .. 2027-01-01, in ticks) is what makes
/// scanning raw 8-byte windows for a plausible tick safe — arbitrary binary
/// only very rarely lands inside a 12-year window.
fn find_send_time(span: &[u8]) -> Option<i64> {
    const TICK_LO: u64 = 0x08d1_f36d_0530_8000;
    const TICK_HI: u64 = 0x08df_679a_2dbe_c000;
    const UNIX_EPOCH_TICKS: u64 = 621_355_968_000_000_000;

    if span.len() < 8 {
        return None;
    }
    (0..=span.len() - 8)
        .filter_map(|i| {
            let v = u64::from_le_bytes(span[i..i + 8].try_into().ok()?);
            (TICK_LO..TICK_HI).contains(&v).then_some(v)
        })
        .min()
        .map(|v| ((v - UNIX_EPOCH_TICKS) / 10_000_000) as i64)
}

/// The HTML body, if this record has one: from the first recognised HTML
/// marker to the nearest closing tag, bounded so it never runs into the next
/// record.
fn find_html(region: &[u8]) -> Option<String> {
    let start = ["<html", "<!DOCTYPE", "<body", "<div", "<table"]
        .iter()
        .filter_map(|m| find_bytes(region, m.as_bytes()))
        .min()?;
    let tail = &region[start..];
    let end = ["</html>", "</body>"]
        .iter()
        .filter_map(|m| find_bytes(tail, m.as_bytes()).map(|p| p + m.len()))
        .min()
        .unwrap_or_else(|| text_run_end(tail));
    // HTML is stored as single-byte UTF-8 read one byte per `char` elsewhere
    // in this store's text, but the body itself is genuine UTF-8 bytes, so
    // it is decoded directly rather than through the byte-widening path
    // `walk_fields` uses for the UTF-16LE metadata runs.
    Some(String::from_utf8_lossy(&tail[..end]).into_owned())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len().max(1)).position(|w| w == needle)
}

/// Where a byte run stops looking like printable text — used to bound an
/// HTML body that has no closing tag, so the scan does not run into the
/// following record's binary framing.
fn text_run_end(b: &[u8]) -> usize {
    const WIN: usize = 32;
    let printable = |c: u8| (0x20..0x7f).contains(&c) || matches!(c, b'\n' | b'\r' | b'\t');
    let mut i = 0usize;
    while i + WIN <= b.len() {
        if b[i..i + WIN].iter().filter(|&&c| printable(c)).count() * 4 < WIN * 3 {
            return i;
        }
        i += WIN;
    }
    b.len()
}

/// A field's shape, with its text redacted — for diagnosing extraction
/// behaviour against a real store without exposing real content. Used by
/// `examples/hx_probe.rs`'s `--layout` mode.
#[derive(Debug, Clone, Copy)]
pub struct FieldShape {
    pub rel: isize,
    pub is_email: bool,
    pub is_person_name: bool,
    pub char_len: usize,
}

impl From<&Field> for FieldShape {
    fn from(f: &Field) -> Self {
        let t = f.text.trim();
        FieldShape {
            rel: f.rel,
            is_email: is_email(t),
            is_person_name: looks_like_person(t),
            char_len: t.chars().count(),
        }
    }
}

/// Extract every `IPM.Note` record from one decompressed block.
pub fn extract(blob: &[u8]) -> Vec<HxMessage> {
    extract_with_shapes(blob).into_iter().map(|(m, _)| m).collect()
}

/// [`extract`], alongside the redacted field shapes ([`FieldShape`]) that
/// produced each message — for diagnosing extraction behaviour against a
/// real store without exposing real content.
pub fn extract_with_shapes(blob: &[u8]) -> Vec<(HxMessage, Vec<FieldShape>)> {
    let needle = anchor_needle();
    let mut out = Vec::new();
    let mut search_from = 0usize;
    // The true start of the current record's span, updated to each anchor in
    // turn. Bounding the timestamp scan by this (rather than a fixed
    // lookback) matters: a tick sitting further back than a short fixed
    // window, but still within this record's own span, would otherwise be
    // missed, and a message's send time would then differ between two
    // revisions of the same message that happened to have different amounts
    // of preceding field data — which skews the median time a merged
    // message reports (`deduplicate`).
    let mut prev_anchor = 0usize;

    while let Some(pos) = find_bytes(&blob[search_from..], &needle) {
        let anchor = search_from + pos;
        let next = find_bytes(&blob[anchor + needle.len()..], &needle)
            .map(|p| anchor + needle.len() + p)
            .unwrap_or(blob.len());
        let sender_search_lo = anchor - anchor.min(SENDER_MAX_DIST as usize + 32);

        let before = walk_fields(blob, anchor, sender_search_lo, anchor);
        let after = walk_fields(blob, anchor, anchor + needle.len(), next);
        let shapes: Vec<FieldShape> =
            before.iter().chain(after.iter()).map(FieldShape::from).collect();

        let (sender_address, sender_name) = find_sender(&before, &after);
        let internet_message_id = after.iter().find(|f| is_message_id(f.text.trim())).map(|f| {
            f.text.trim().trim_matches(['<', '>']).to_string()
        });
        let subject = find_subject(&after).map(|f| undouble(f.text.trim()));
        let claimed_subject_rel = find_subject(&after).map(|f| f.rel);
        let preview = after
            .iter()
            .filter(|f| Some(f.rel) != claimed_subject_rel && !f.text.trim_start().starts_with('<'))
            .max_by_key(|f| f.text.len())
            .map(|f| f.text.trim().to_string());
        let html = find_html(&blob[anchor..next]);
        let sent_unix = find_send_time(&blob[prev_anchor..next]);
        prev_anchor = anchor;

        out.push((
            HxMessage {
                sender_address,
                sender_name,
                internet_message_id,
                subject,
                preview,
                html,
                sent_unix,
                identity: 0,
            },
            shapes,
        ));

        search_from = anchor + needle.len();
    }
    out
}

/// Revisions of one message are recovered with times minutes apart (the send
/// time is the earliest tick in a span that can reach into the previous
/// record), while a repeated automated mail with identical text arrives hours
/// or days apart. Records further apart than this stay separate.
const SAME_MESSAGE_WINDOW_SECS: i64 = 12 * 3600;
/// A preview shorter than this, reduced to letters and digits, is too generic
/// ("Thanks!") to identify one message on its own.
const MIN_TEXT_KEY: usize = 24;
/// How much of a preview's end, reduced to letters and digits, must reappear
/// in an HTML body to tie the two records together. The end, not the start:
/// a preview can open with a mail-gateway banner the HTML body does not.
const PREVIEW_TAIL: usize = 40;

/// Letters and digits only, lowercased, so whitespace, punctuation and
/// spacing differences between a preview and its HTML body do not matter.
fn text_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// [`text_key`] of an HTML body's text: tags and `&…;` entities are dropped.
fn html_text_key(html: &str) -> String {
    let mut out = String::new();
    let (mut in_tag, mut in_entity) = (false, false);
    for c in html.chars() {
        if in_tag {
            in_tag = c != '>';
        } else if c == '<' {
            in_tag = true;
        } else if in_entity && (c.is_ascii_alphanumeric() || c == '#') {
            // still inside `&nbsp;` / `&#160;`
        } else if c == '&' {
            in_entity = true;
        } else {
            in_entity = false;
            if c.is_alphanumeric() {
                out.extend(c.to_lowercase());
            }
        }
    }
    out
}

/// The most common value; ties go to the smallest, so the result does not
/// depend on record order.
fn majority<'a>(vals: impl Iterator<Item = &'a String>) -> Option<String> {
    let mut counts: std::collections::BTreeMap<&String, usize> = Default::default();
    for v in vals {
        *counts.entry(v).or_default() += 1;
    }
    let best = counts.values().copied().max()?;
    counts.into_iter().find(|(_, n)| *n == best).map(|(v, _)| v.clone())
}

/// Split `idxs` into runs whose times are within [`SAME_MESSAGE_WINDOW_SECS`]
/// of their neighbour. A record with no time joins whichever run it sorts into.
fn split_by_time(records: &[HxMessage], mut idxs: Vec<usize>) -> Vec<Vec<usize>> {
    idxs.sort_by_key(|&i| (records[i].sent_unix, i));
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut last: Option<i64> = None;
    for i in idxs {
        let t = records[i].sent_unix;
        let gap = matches!((last, t), (Some(a), Some(b)) if b - a > SAME_MESSAGE_WINDOW_SECS);
        match runs.last_mut() {
            Some(run) if !gap => run.push(i),
            _ => runs.push(vec![i]),
        }
        last = t.or(last);
    }
    runs
}

/// Collapse one group of revisions into a message. Sender and subject are
/// voted on, because a revision whose sender came from its neighbour record
/// is outvoted by the revisions that read it correctly.
fn merge(records: &[HxMessage], idxs: &[usize]) -> HxMessage {
    let group: Vec<&HxMessage> = idxs.iter().map(|&i| &records[i]).collect();
    let sender_address = majority(group.iter().filter_map(|r| r.sender_address.as_ref()));
    let sender_name = majority(
        group
            .iter()
            .filter(|r| sender_address.is_none() || r.sender_address == sender_address)
            .filter_map(|r| r.sender_name.as_ref()),
    );
    let mut times: Vec<i64> = group.iter().filter_map(|r| r.sent_unix).collect();
    times.sort_unstable();
    HxMessage {
        sender_address,
        sender_name,
        internet_message_id: group.iter().find_map(|r| r.internet_message_id.clone()),
        subject: majority(group.iter().filter_map(|r| r.subject.as_ref())),
        // From a revision without an HTML body when there is one: an HTML
        // revision's "preview" is often unrelated text (see `deduplicate`).
        preview: group
            .iter()
            .filter(|r| r.html.is_none())
            .chain(group.iter())
            .find_map(|r| r.preview.clone()),
        html: group.iter().filter_map(|r| r.html.as_ref()).max_by_key(|h| h.len()).cloned(),
        // The upper median. A revision's time is the earliest tick in a span
        // that can reach into the record before it, so an error only ever
        // pulls a time earlier: with two revisions the later one is right,
        // and a lone stray value on either side is ignored.
        sent_unix: times.get(times.len() / 2).copied(),
        identity: 0,
    }
}

/// FNV-1a over `parts`, each length-prefixed so `("ab", "c")` and `("a", "bc")`
/// differ. Not cryptographic, and does not need to be: it only has to be the
/// same in every process, which `std`'s hasher does not promise.
fn identity_of(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in (p.len() as u64).to_le_bytes().iter().chain(p.iter()) {
            h = (h ^ *b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// Merge the records that are revisions of one message, newest first.
///
/// The store rewrites a message on every sync, and revisions are not
/// uniformly complete: one carries the preview and subject, another only the
/// full HTML body, and their recovered times differ by minutes. So the
/// grouping is by content, not by `(sender, time)`:
///
/// 1. Records with the same preview text, close in time, are one message.
/// 2. A record with an HTML body joins the preview group nearest in time whose
///    preview ending appears in its text.
/// 3. A record with neither joins a group holding the same `(sender, time)`,
///    or groups with others on that pair, as before.
///
/// `InternetMessageId` is deliberately not a key: the store reuses one across
/// an entire conversation, so keying on it merges genuinely distinct messages.
///
/// The output order is fully determined by the records, so message ids
/// assigned from it are the same in every process reading the same file.
pub fn deduplicate(records: Vec<HxMessage>) -> Vec<HxMessage> {
    use std::collections::{BTreeMap, HashMap};

    // BTreeMaps, not HashMaps, wherever iteration order decides which group a
    // record joins: the result has to be the same in every process.
    let mut by_preview: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut with_html = Vec::new();
    let mut textless = Vec::new();
    for (i, r) in records.iter().enumerate() {
        // A record with an HTML body is matched by that body even when it also
        // has a "preview": on a real store that field is often whatever text
        // run sat after the anchor, not the summary its sibling revision holds.
        if r.html.is_some() {
            with_html.push(i);
            continue;
        }
        match r.preview.as_deref().map(text_key).filter(|k| k.len() >= MIN_TEXT_KEY) {
            Some(k) => by_preview.entry(k).or_default().push(i),
            None => textless.push(i),
        }
    }

    // 1. Preview groups, each with the tail its HTML body must contain.
    // Each group's identity is its text key plus its place among the groups
    // sharing that text, oldest first, so newer mail never renumbers it.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut identities: Vec<u64> = Vec::new();
    let mut tails: Vec<String> = Vec::new();
    for (key, idxs) in by_preview {
        let skip = key.chars().count().saturating_sub(PREVIEW_TAIL);
        let tail: String = key.chars().skip(skip).collect();
        for (n, run) in split_by_time(&records, idxs).into_iter().enumerate() {
            groups.push(run);
            identities.push(identity_of(&[b"preview", key.as_bytes(), &n.to_le_bytes()]));
            tails.push(tail.clone());
        }
    }

    // 2. Records with an HTML body. Candidates are found by time, so each record is
    // compared with the few groups near it rather than with every group.
    let group_time = |g: &[usize]| {
        let mut t: Vec<i64> = g.iter().filter_map(|&i| records[i].sent_unix).collect();
        t.sort_unstable();
        t.get(t.len().saturating_sub(1) / 2).copied()
    };
    let mut by_time: Vec<(i64, usize)> = groups
        .iter()
        .enumerate()
        .filter_map(|(g, idxs)| group_time(idxs).map(|t| (t, g)))
        .collect();
    by_time.sort_unstable();
    let mut unattached: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for i in with_html {
        let key = html_text_key(records[i].html.as_deref().unwrap_or_default());
        let target = records[i].sent_unix.and_then(|t| {
            let lo = by_time.partition_point(|&(gt, _)| gt < t - SAME_MESSAGE_WINDOW_SECS);
            let hi = by_time.partition_point(|&(gt, _)| gt <= t + SAME_MESSAGE_WINDOW_SECS);
            by_time[lo..hi]
                .iter()
                .filter(|&&(_, g)| key.contains(tails[g].as_str()))
                .min_by_key(|&&(gt, g)| ((gt - t).abs(), g))
                .map(|&(_, g)| g)
        });
        match target {
            Some(g) => groups[g].push(i),
            None => unattached.entry(key).or_default().push(i),
        }
    }
    for (key, idxs) in unattached {
        for (n, run) in split_by_time(&records, idxs).into_iter().enumerate() {
            groups.push(run);
            identities.push(identity_of(&[b"html", key.as_bytes(), &n.to_le_bytes()]));
        }
    }

    // 3. Records with no text, matched on the old `(sender, time)` key.
    let mut by_pair: HashMap<(Option<String>, Option<i64>), usize> = HashMap::new();
    for (g, idxs) in groups.iter().enumerate() {
        for &i in idxs {
            let r = &records[i];
            by_pair.entry((r.sender_address.clone(), r.sent_unix)).or_insert(g);
        }
    }
    for i in textless {
        let r = &records[i];
        let pair = (r.sender_address.clone(), r.sent_unix);
        match by_pair.get(&pair) {
            Some(&g) => groups[g].push(i),
            None => {
                let sender = pair.0.as_deref().unwrap_or_default().as_bytes();
                let time = pair.1.map(i64::to_le_bytes).unwrap_or_default();
                identities.push(identity_of(&[b"pair", sender, &time, &[pair.1.is_some() as u8]]));
                by_pair.insert(pair, groups.len());
                groups.push(vec![i]);
            }
        }
    }

    let mut out: Vec<HxMessage> = groups
        .into_iter()
        .zip(identities)
        .map(|(mut idxs, identity)| {
            idxs.sort_unstable();
            HxMessage { identity, ..merge(&records, &idxs) }
        })
        .collect();
    out.sort_by(|a, b| {
        b.sent_unix
            .cmp(&a.sent_unix)
            .then_with(|| a.sender_address.cmp(&b.sender_address))
            .then_with(|| a.subject.cmp(&b.subject))
            .then_with(|| a.preview.cmp(&b.preview))
            .then_with(|| a.html.cmp(&b.html))
    });
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        let mut v: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
        v.extend_from_slice(&[0, 0]); // NUL terminator
        v
    }

    /// Build a synthetic decompressed block matching the common record
    /// layout: sender pair before the anchor, then message-id/preview/subject
    /// pair after it, per §4.2 of the credited write-up.
    pub(crate) fn synthetic_record(
        sender: &str,
        sender_name: &str,
        message_id: &str,
        preview: &str,
        subject: &str,
        sent_unix: i64,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(utf16le(sender));
        b.extend(utf16le(sender_name));
        b.extend(utf16le(ANCHOR));
        b.extend(utf16le(message_id));
        // .NET tick timestamp, embedded as raw bytes among the fields.
        const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
        let ticks = (sent_unix * 10_000_000) + UNIX_EPOCH_TICKS;
        b.extend_from_slice(&(ticks as u64).to_le_bytes());
        b.extend(utf16le(preview));
        b.extend(utf16le(subject));
        b.extend(utf16le(subject)); // Topic/NormalizedSubject pair
        b
    }

    #[test]
    fn extracts_sender_subject_preview_and_time() {
        let blob = synthetic_record(
            "sender@example.com",
            "Example Sender",
            "<abc123@example.com>",
            "This is the cached preview text of the message body",
            "Quarterly update",
            1_700_000_000,
        );
        let records = extract(&blob);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.sender_address.as_deref(), Some("sender@example.com"));
        assert_eq!(r.sender_name.as_deref(), Some("Example Sender"));
        assert_eq!(r.internet_message_id.as_deref(), Some("abc123@example.com"));
        assert_eq!(r.subject.as_deref(), Some("Quarterly update"));
        assert_eq!(r.sent_unix, Some(1_700_000_000));
    }

    #[test]
    fn ambiguous_after_anchor_sender_returns_none_rather_than_a_guess() {
        // Reproduces the real layout found via `examples/hx_probe.rs
        // --layout-near` against a live store: no sender-shaped field before
        // the anchor, and two address+name pairs after it — a participant's,
        // then the true sender's, with no signal here to tell which is
        // which. Measured wrong once (a participant's address returned as
        // the sender); this must now return `None` instead.
        let mut b = Vec::new();
        b.extend(utf16le(ANCHOR));
        b.extend(utf16le("<abc123@example.com>"));
        const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
        let ticks = (1_700_000_000i64 * 10_000_000) + UNIX_EPOCH_TICKS;
        b.extend_from_slice(&(ticks as u64).to_le_bytes());
        b.extend(utf16le("participant@example.com"));
        b.extend(utf16le("A Participant"));
        b.extend(utf16le("gap filler text that is not a name or address"));
        b.extend(utf16le("true.sender@example.com"));
        b.extend(utf16le("The True Sender"));

        let records = extract(&b);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].sender_address, None);
        assert_eq!(records[0].sender_name, None);
    }

    #[test]
    fn extracts_multiple_records_without_bleeding_into_neighbours() {
        let mut blob = synthetic_record(
            "alice@example.com",
            "Alice Example",
            "<one@example.com>",
            "First message preview text goes here",
            "First subject",
            1_700_000_000,
        );
        blob.extend(synthetic_record(
            "bob@example.com",
            "Bob Example",
            "<two@example.com>",
            "Second message preview text goes here",
            "Second subject",
            1_700_100_000,
        ));
        let records = extract(&blob);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sender_address.as_deref(), Some("alice@example.com"));
        assert_eq!(records[0].subject.as_deref(), Some("First subject"));
        assert_eq!(records[1].sender_address.as_deref(), Some("bob@example.com"));
        assert_eq!(records[1].subject.as_deref(), Some("Second subject"));
    }

    #[test]
    fn no_anchor_yields_no_records() {
        let blob = utf16le("nothing to see here");
        assert!(extract(&blob).is_empty());
    }

    #[test]
    fn extracts_html_body_bounded_by_closing_tag() {
        let mut blob = synthetic_record(
            "sender@example.com",
            "Example Sender",
            "<abc@example.com>",
            "preview text here for the record",
            "A subject line",
            1_700_000_000,
        );
        blob.extend_from_slice(b"<html><body>Hello world</body></html>");
        blob.extend(utf16le("NextFieldStub"));
        let records = extract(&blob);
        // Whichever closing tag is found first (by end position) bounds the
        // body — here `</body>` ends before `</html>` does, and nothing of
        // value is lost by stopping there.
        assert_eq!(records[0].html.as_deref(), Some("<html><body>Hello world</body>"));
    }

    #[test]
    fn deduplicate_merges_revisions_by_sender_and_time() {
        let mut a = HxMessage { sender_address: Some("x@example.com".into()), sent_unix: Some(1), ..Default::default() };
        a.subject = Some("Known subject".into());
        let mut b = HxMessage { sender_address: Some("x@example.com".into()), sent_unix: Some(1), ..Default::default() };
        b.html = Some("<html>body</html>".into());

        let merged = deduplicate(vec![a, b]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].subject.as_deref(), Some("Known subject"));
        assert_eq!(merged[0].html.as_deref(), Some("<html>body</html>"));
    }

    #[test]
    fn deduplicate_keeps_distinct_senders_and_times_separate() {
        let a = HxMessage { sender_address: Some("x@example.com".into()), sent_unix: Some(1), ..Default::default() };
        let b = HxMessage { sender_address: Some("y@example.com".into()), sent_unix: Some(2), ..Default::default() };
        assert_eq!(deduplicate(vec![a, b]).len(), 2);
    }

    fn rec(sender: Option<&str>, subject: Option<&str>, preview: Option<&str>, html: Option<&str>, t: i64) -> HxMessage {
        HxMessage {
            sender_address: sender.map(Into::into),
            subject: subject.map(Into::into),
            preview: preview.map(Into::into),
            html: html.map(Into::into),
            sent_unix: Some(t),
            ..Default::default()
        }
    }

    const REPLY: &str = "Dear Bilal, Thank you for the clarification. We understand that all APIs should meet the average response time requirement.";

    #[test]
    fn revisions_with_drifting_times_merge_and_vote_on_sender_and_subject() {
        let me = Some("gang@example.com");
        let merged = deduplicate(vec![
            rec(me, Some("Re: MOM"), Some(REPLY), None, 1_000),
            rec(me, Some("Re: MOM"), Some(REPLY), None, 1_900),
            // A revision whose sender and subject came from the record before it.
            rec(Some("no-reply@codes.example"), Some("MOM"), Some(REPLY), None, 2_000),
            rec(me, Some("Re: MOM"), Some(REPLY), None, 3_300),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].sender_address.as_deref(), me);
        assert_eq!(merged[0].subject.as_deref(), Some("Re: MOM"));
        assert_eq!(merged[0].sent_unix, Some(2_000), "upper median of the four times");
    }

    #[test]
    fn html_only_revision_joins_its_preview_despite_a_banner_and_entities() {
        let preview = format!("Caution: ***[External:] This email originated outside.*** {REPLY}");
        let html = "<html><body><p>Dear Bilal,&nbsp;Thank you for the <b>clarification</b>. \
                    We understand that all APIs should meet the average response time \
                    requirement.</p><p>More text the preview never had.</p></body>";
        let merged = deduplicate(vec![
            rec(Some("gang@example.com"), Some("Re: MOM"), Some(&preview), None, 1_000),
            rec(None, None, None, Some(html), 1_300),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].html.as_deref(), Some(html));
        assert_eq!(merged[0].sender_address.as_deref(), Some("gang@example.com"));
    }

    #[test]
    fn same_text_far_apart_or_different_codes_stay_separate() {
        let code = |c: &str| format!("Email Verification Code. Please enter the code below to verify your identity. {c}");
        let day = 24 * 3600;
        let merged = deduplicate(vec![
            rec(Some("no-reply@codes.example"), None, Some(&code("497184")), None, 1_000),
            rec(Some("no-reply@codes.example"), None, Some(&code("077129")), None, 1_000),
            // The same automated text two days later is a second mail.
            rec(Some("no-reply@codes.example"), None, Some(&code("497184")), None, 1_000 + 2 * day),
        ]);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn identity_survives_newer_mail_and_tells_messages_apart() {
        let old = vec![
            rec(Some("a@example.com"), Some("one"), Some("first message body text, long enough"), None, 1_000),
            rec(Some("a@example.com"), Some("one"), Some("first message body text, long enough"), None, 1_200),
            rec(Some("b@example.com"), None, None, None, 2_000),
        ];
        let before: Vec<(Option<String>, u64)> =
            deduplicate(old.clone()).into_iter().map(|m| (m.subject, m.identity)).collect();
        let mut grown = old;
        grown.push(rec(Some("c@example.com"), Some("new"), Some("a newer message that just arrived"), None, 9_000));
        let after = deduplicate(grown);

        assert_eq!(after.len(), 3);
        for (subject, identity) in &before {
            assert!(after.iter().any(|m| &m.subject == subject && m.identity == *identity));
        }
        let mut ids: Vec<u64> = after.iter().map(|m| m.identity).collect();
        ids.dedup();
        assert_eq!(ids.len(), 3, "every message gets its own identity");
    }

    #[test]
    fn output_order_does_not_depend_on_input_order() {
        let records = vec![
            rec(Some("a@example.com"), Some("one"), Some("first message body text, long enough"), None, 5),
            rec(Some("b@example.com"), Some("two"), Some("second message body text, long enough"), None, 5),
            rec(Some("c@example.com"), Some("three"), None, None, 5),
            rec(None, None, None, Some("<p>an unrelated html body nobody previews</p>"), 5),
        ];
        let forward: Vec<_> = deduplicate(records.clone()).into_iter().map(|m| m.subject).collect();
        let reversed: Vec<_> = deduplicate(records.into_iter().rev().collect()).into_iter().map(|m| m.subject).collect();
        assert_eq!(forward, reversed);
        assert_eq!(forward.len(), 4);
    }

    #[test]
    fn rejects_a_run_that_is_mostly_off_script() {
        assert!(is_binary_run("=꼄딂aĀ"));
        assert!(!is_binary_run("Zoé - Réseau"));
    }

    #[test]
    fn email_and_guid_and_opaque_detection() {
        assert!(is_email("someone@example.com"));
        assert!(!is_email("not an email"));
        assert!(is_guid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_opaque("eyJhbGciOiJIUzI1NiJ9"));
        assert!(is_opaque(&"a".repeat(24)));
    }
}
