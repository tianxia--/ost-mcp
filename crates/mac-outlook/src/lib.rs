//! Read-only reader for Mac Outlook's local store: `Outlook.sqlite` plus
//! `.olk15*` companion files, and `HxStore.hxd`.
//!
//! ```text
//! model  <-->  ost-mcp (MCP stdio + DuckDB)  <-->  Outlook.sqlite + .olk15* + HxStore.hxd
//! ```
//!
//! Every claim about the on-disk format is measurement-backed in
//! `docs/mac-outlook-format.md`; read that first. The short version: an
//! account's classic engine (`Outlook.sqlite` + `.olk15*`) holds real
//! structural data (folders, categories, signatures) but, for New
//! Outlook/Exchange, no message content — and `HxStore.hxd` (see the
//! `hxstore`/`hxrecord` module docs for credit) holds that content instead,
//! for whatever window the account's own sync setting keeps locally. Both
//! degrade to empty/`NULL` rather than a guess where they have nothing.
//!
//! **Known limitations**, both from what is not yet resolved in
//! `docs/mac-outlook-format.md`, not from something guessed around:
//! - `message()`'s classic-engine path only resolves a body from
//!   `.olk15Message` (the 100%-of-viewed-messages cache); `.olk15MsgSource`
//!   (full RFC822 MIME, higher fidelity, present for a minority of messages)
//!   is located but not parsed.
//! - Messages recovered from `HxStore.hxd` carry no folder identity and no
//!   attachment linkage (§2.8) — they are exposed as one synthetic
//!   [`Folder`](mailbox::Folder), not sorted into Inbox/Sent/etc., and
//!   `attachments()`/`attachment_bytes()` return nothing for them.

pub mod discover;
mod hxlz4;
pub mod hxrecord;
pub mod hxstore;
pub mod olk15;
mod schema;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use mailbox::{Error, Folder, Mailbox, Message, MessageRow, Result};
use rusqlite::Connection;

/// Synthetic folder id for messages recovered from `HxStore.hxd`, which
/// carries no folder identity of its own (`docs/mac-outlook-format.md`
/// §2.8). Negative, so it can never collide with a real
/// `Folders.Record_RecordID`, always a positive SQLite rowid.
const HX_FOLDER_ID: i64 = -1;
/// Base for synthetic message ids drawn from the Hx cache — same reasoning,
/// a different negative range so the two synthetic id spaces stay visually
/// distinguishable in logs and error messages.
const HX_ID_BASE: i64 = -1_000_000;

type HxMessages = Arc<Vec<(i64, hxrecord::HxMessage)>>;

/// The parsed Hx cache and the `HxStore.hxd` modification time and size it
/// was parsed from.
struct HxSnapshot {
    stamp: Option<(Option<SystemTime>, u64)>,
    messages: HxMessages,
}

pub struct Profile {
    data_dir: PathBuf,
    conn: Mutex<Connection>,
    hx_cache: Mutex<Option<HxSnapshot>>,
}

impl Profile {
    pub fn open(data_dir: PathBuf) -> Result<Profile> {
        let conn = schema::open_readonly(&data_dir.join("Outlook.sqlite"))?;
        Ok(Profile {
            data_dir,
            conn: Mutex::new(conn),
            hx_cache: Mutex::new(None),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `HxStore.hxd` sits as a sibling of `Data/`, at the identity root.
    fn hx_store_path(&self) -> Option<PathBuf> {
        self.data_dir.parent().map(|p| p.join("HxStore.hxd"))
    }

    /// The Hx cache, re-parsed whenever `HxStore.hxd` has changed since the
    /// last call, so mail that arrives while a long-lived MCP server is
    /// running shows up without a restart. Checking costs one `stat`;
    /// re-parsing a real store takes under a second and only happens after
    /// Outlook has written to it.
    ///
    /// The stamp is taken before the file is read, so a write that lands
    /// mid-read leaves a stale stamp and the next call parses again.
    ///
    /// A plain read-only `fs::read` of the live file is safe even while
    /// Outlook is actively rewriting it: every block is independently
    /// checksummed (§2.3), so a torn read just fails validation and is
    /// skipped rather than returning wrong data. This never writes to the
    /// file either way. Absent, unreadable or unparseable is not an error —
    /// it is the normal state for a classic-engine-backed account with no
    /// Hx cache at all, and the empty result is what makes that honest.
    fn hx_cache(&self) -> HxMessages {
        let path = self.hx_store_path();
        let stamp = path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| (m.modified().ok(), m.len()));
        let mut slot = self.hx_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(snap) = slot.as_ref().filter(|s| s.stamp == stamp) {
            return snap.messages.clone();
        }
        let messages: HxMessages = Arc::new(path.map(|p| Self::parse_hx(&p)).unwrap_or_default());
        *slot = Some(HxSnapshot { stamp, messages: messages.clone() });
        messages
    }

    /// Parse `HxStore.hxd` into messages, each with an id derived from the
    /// message's content rather than its position. A positional id would
    /// shift by one for every mail that arrives, so an id a caller got from
    /// `search` before a reload would name a different message after it.
    fn parse_hx(path: &std::path::Path) -> Vec<(i64, hxrecord::HxMessage)> {
        let Ok(data) = std::fs::read(path) else {
            return Vec::new();
        };
        if hxstore::check_header(&data).is_err() {
            return Vec::new();
        }
        let records: Vec<_> = hxstore::scan_blocks(&data)
            .iter()
            .flat_map(|b| hxrecord::extract(&b.data))
            .collect();
        // Newest first, so a caller reading the folder without its own
        // ordering still sees recent mail before old.
        let mut taken = std::collections::HashSet::new();
        hxrecord::deduplicate(records)
            .into_iter()
            .map(|m| {
                // 48 bits keeps every id exact as a JSON number (2^53), and
                // below HX_ID_BASE so it reads as an Hx id. On the rare
                // collision, step to the next free id; the output order is
                // deterministic, so the step is the same in every process.
                let mut id = HX_ID_BASE - 1 - (m.identity & 0xFFFF_FFFF_FFFF) as i64;
                while !taken.insert(id) {
                    id -= 1;
                }
                (id, m)
            })
            .collect()
    }

    fn hx_message_row(id: i64, m: &hxrecord::HxMessage) -> MessageRow {
        MessageRow {
            id,
            folder_id: HX_FOLDER_ID,
            subject: m.subject.clone(),
            sender_name: m.sender_name.clone(),
            sender_email: m.sender_address.clone(),
            delivered_us: m.sent_unix.map(|s| s * 1_000_000),
            submitted_us: m.sent_unix.map(|s| s * 1_000_000),
            modified_us: None,
            size: None,
            unread: None,
            has_attachments: None,
            // Not a guess: every record recovered here is anchored by this
            // literal string (`docs/mac-outlook-format.md` §2.4).
            message_class: Some("IPM.Note".to_string()),
        }
    }

    /// Read one `.olk15*` file relative to `Data/`, decoding the
    /// URL-encoded path segments a `Blocks.PathToDataFile` value carries
    /// (`docs/mac-outlook-format.md` §3.2).
    ///
    /// `relative` comes straight out of `Outlook.sqlite` — a `PathToDataFile`
    /// column, not something this reader controls. Only plain name components
    /// are joined: `PathBuf::join` sanitizes nothing, and an absolute or
    /// root-relative argument replaces the base outright, so without this check
    /// a crafted or corrupted database row would let any tool that surfaces a
    /// message body or attachment read an arbitrary file the process has access
    /// to, not just the mailbox.
    ///
    /// The test is "every component is a normal name", not `is_absolute()`.
    /// `is_absolute()` is false on Windows for a path like `/etc/passwd`, which
    /// has a root but no drive, and `C:\dir` joined with it yields `C:/etc/passwd`
    /// — outside the profile. Rejecting anything that is not a name covers a
    /// root, a drive prefix and a `..` on both platforms.
    fn read_data_file(&self, relative: &str) -> Result<Vec<u8>> {
        use std::path::Component;

        let decoded = percent_decode(relative);
        let rel_path = std::path::Path::new(&decoded);
        let escapes = rel_path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
        if escapes {
            return Err(Error::Format(format!(
                "refusing to read outside the profile data directory: {relative:?}"
            )));
        }
        let path = self.data_dir.join(rel_path);
        std::fs::read(&path).map_err(Error::Io)
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Mailbox for Profile {
    fn kind(&self) -> &'static str {
        "mac-olk15"
    }

    fn display_name(&self) -> Option<String> {
        schema::display_name(&self.lock())
    }

    fn folders(&self) -> Result<Vec<mailbox::Folder>> {
        let mut folders = schema::folders(&self.lock())?;
        let hx = self.hx_cache();
        // Only appear when there is something to show: an account with no
        // Hx cache at all should not gain an empty folder that implies one
        // exists.
        if !hx.is_empty() {
            folders.push(Folder {
                id: HX_FOLDER_ID,
                parent_id: None,
                name: Some("Recovered Mail (Hx cache)".to_string()),
                path: "/Recovered Mail (Hx cache)".to_string(),
                item_count: Some(hx.len() as i64),
                // Read state is not recovered from this source (§2.8).
                unread_count: None,
                has_subfolders: false,
                is_search_folder: false,
            });
        }
        Ok(folders)
    }

    fn messages(&self, folder_id: i64) -> Result<Vec<mailbox::MessageRow>> {
        if folder_id == HX_FOLDER_ID {
            return Ok(self.hx_cache().iter().map(|(id, m)| Self::hx_message_row(*id, m)).collect());
        }
        schema::messages(&self.lock(), folder_id)
    }

    fn message(&self, id: i64) -> Result<mailbox::Message> {
        if id <= HX_ID_BASE {
            let hx = self.hx_cache();
            let (_, m) = hx
                .iter()
                .find(|(i, _)| *i == id)
                .ok_or_else(|| Error::NotFound(format!("no Hx-cached message with id {id}")))?;
            return Ok(Message {
                id,
                subject: m.subject.clone(),
                sender_name: m.sender_name.clone(),
                sender_email: m.sender_address.clone(),
                // Not recovered from this source (§2.8): no structured
                // recipient table has been located in HxStore.hxd.
                display_to: None,
                display_cc: None,
                display_bcc: None,
                delivered_us: m.sent_unix.map(|s| s * 1_000_000),
                submitted_us: m.sent_unix.map(|s| s * 1_000_000),
                modified_us: None,
                size: None,
                unread: None,
                message_class: Some("IPM.Note".to_string()),
                internet_message_id: m.internet_message_id.clone(),
                conversation_topic: None,
                // A message's `body_plain` gets Outlook's cached preview
                // only when there is no full HTML copy — the preview is a
                // truncated summary of the same content, not a second,
                // distinct body worth surfacing alongside it.
                body_plain: if m.html.is_none() { m.preview.clone() } else { None },
                body_html: m.html.clone(),
                body_rtf: None,
                recipients: Vec::new(),
                // Not recovered from this source (§2.8): no linkage from a
                // record to the plain-file attachment cache has been found.
                attachments: Vec::new(),
            });
        }

        let (row, path_to_data_file, conversation_topic) = schema::mail_row(&self.lock(), id)?;

        let mut body_plain = None;
        let mut body_html = None;
        let mut body_rtf = None;
        if let Some(rel) = path_to_data_file {
            if let Ok(data) = self.read_data_file(&rel) {
                if let Some(body) = olk15::parse_message(&data) {
                    match body.kind {
                        olk15::BodyKind::Html => body_html = Some(body.text),
                        olk15::BodyKind::Rtf => body_rtf = Some(body.text),
                        // A calendar body and a plain-text fallback both
                        // read fine as plain text; there is no richer slot
                        // for either in `mailbox::Message`.
                        olk15::BodyKind::Calendar | olk15::BodyKind::Plain => {
                            body_plain = Some(body.text)
                        }
                    }
                }
            }
        }

        Ok(mailbox::Message {
            id: row.id,
            subject: row.subject,
            sender_name: row.sender_name,
            sender_email: row.sender_email,
            // Not populated: `Mail` carries flat recipient-address-list
            // strings, not a structured recipient table, and the list
            // delimiter/encoding has not been measured against a real row
            // (docs/mac-outlook-format.md §3.3). Guessing one would be
            // exactly the fabricated-but-plausible value CONTRIBUTING.md
            // rules out.
            display_to: None,
            display_cc: None,
            display_bcc: None,
            delivered_us: row.delivered_us,
            submitted_us: row.submitted_us,
            modified_us: row.modified_us,
            size: row.size,
            unread: row.unread,
            message_class: row.message_class,
            internet_message_id: None,
            conversation_topic,
            body_plain,
            body_html,
            body_rtf,
            recipients: Vec::new(),
            attachments: self.attachments(id)?,
        })
    }

    fn attachments(&self, message_id: i64) -> Result<Vec<mailbox::Attachment>> {
        if message_id <= HX_ID_BASE {
            // Not recovered from this source (§2.8): honestly empty, not a
            // lookup error — an Hx-cached message may well have attachments,
            // this reader simply has no way to find them yet.
            return Ok(Vec::new());
        }
        let blocks = schema::linked_blocks(&self.lock(), message_id, "Message Attachments")?;
        Ok(blocks
            .into_iter()
            .map(|b| {
                let (filename, mime) = self
                    .read_data_file(&b.path_to_data_file)
                    .ok()
                    .and_then(|data| olk15::attachment_metadata(&data).ok())
                    .unwrap_or((None, None));
                mailbox::Attachment {
                    id: b.id,
                    filename,
                    mime,
                    content_id: None,
                    declared_size: None,
                    data_len: None,
                }
            })
            .collect())
    }

    fn attachment_bytes(&self, message_id: i64, attachment_id: i64) -> Result<Vec<u8>> {
        if message_id <= HX_ID_BASE {
            return Err(Error::NotFound(format!(
                "message {message_id} was recovered from HxStore.hxd, which carries no attachment linkage (docs/mac-outlook-format.md §2.8)"
            )));
        }
        let blocks = schema::linked_blocks(&self.lock(), message_id, "Message Attachments")?;
        let block = blocks
            .into_iter()
            .find(|b| b.id == attachment_id)
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "attachment {attachment_id} is not linked to message {message_id}"
                ))
            })?;
        let data = self.read_data_file(&block.path_to_data_file)?;
        Ok(olk15::parse_attachment(&data)?.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_spaces_and_plain_text() {
        assert_eq!(
            percent_decode("Message%20Attachments/35/x.olk15MsgAttachment"),
            "Message Attachments/35/x.olk15MsgAttachment"
        );
        assert_eq!(percent_decode("Categories/1/a.olk15Category"), "Categories/1/a.olk15Category");
    }

    /// An empty but valid SQLite file, so `Profile::open`'s read-only open
    /// succeeds without needing the real schema — these tests exercise
    /// `read_data_file` directly, not anything SQL-shaped.
    fn empty_profile(dir: &std::path::Path) -> Profile {
        rusqlite::Connection::open(dir.join("Outlook.sqlite")).unwrap();
        Profile::open(dir.to_path_buf()).unwrap()
    }

    #[test]
    fn read_data_file_rejects_parent_dir_traversal() {
        let dir = std::env::temp_dir().join(format!("mac-outlook-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let secret = dir.parent().unwrap().join("mac-outlook-test-secret.txt");
        std::fs::write(&secret, b"do not read me").unwrap();

        let profile = empty_profile(&dir);
        let err = profile
            .read_data_file("../mac-outlook-test-secret.txt")
            .unwrap_err();
        assert!(matches!(err, Error::Format(_)));

        std::fs::remove_file(&secret).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_data_file_rejects_absolute_path() {
        let dir = std::env::temp_dir().join(format!("mac-outlook-test-abs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let profile = empty_profile(&dir);
        let err = profile.read_data_file("/etc/passwd").unwrap_err();
        assert!(matches!(err, Error::Format(_)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hx_cache_reloads_when_the_store_changes_and_ids_survive() {
        use hxrecord::tests::synthetic_record;
        let root = std::env::temp_dir().join(format!("mac-outlook-test-hx-{}", std::process::id()));
        let data = root.join("Data");
        std::fs::create_dir_all(&data).unwrap();
        let store = root.join("HxStore.hxd");
        let first = synthetic_record("a@example.com", "Alice", "<1@x>", "The first message, long enough to key on", "First", 1_700_000_000);
        std::fs::write(&store, hxstore::test_store(std::slice::from_ref(&first))).unwrap();

        let profile = empty_profile(&data);
        let before = profile.messages(HX_FOLDER_ID).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(profile.messages(HX_FOLDER_ID).unwrap().len(), 1, "unchanged file: same snapshot");

        // New mail arrives: the file grows (and its mtime moves), so the next
        // call re-reads it without reopening the profile.
        let second = synthetic_record("b@example.com", "Bob", "<2@x>", "A second message that just arrived here", "Second", 1_700_000_600);
        std::fs::write(&store, hxstore::test_store(&[first, second])).unwrap();
        let after = profile.messages(HX_FOLDER_ID).unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].subject.as_deref(), Some("Second"), "newest first");
        let kept = after.iter().find(|m| m.subject.as_deref() == Some("First")).unwrap();
        assert_eq!(kept.id, before[0].id, "the first message keeps its id");
        assert_eq!(profile.message(before[0].id).unwrap().subject.as_deref(), Some("First"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_data_file_allows_a_normal_relative_path() {
        let dir = std::env::temp_dir().join(format!("mac-outlook-test-ok-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Categories/1")).unwrap();
        std::fs::write(dir.join("Categories/1/a.olk15Category"), b"hi").unwrap();

        let profile = empty_profile(&dir);
        let data = profile.read_data_file("Categories/1/a.olk15Category").unwrap();
        assert_eq!(data, b"hi");

        std::fs::remove_dir_all(&dir).ok();
    }
}
