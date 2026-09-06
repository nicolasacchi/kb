//! LSC-5 — the opencode adapter (design §4 "opencode — richest signals,
//! hardest plumbing"), a sibling of [`super::super::live`]'s Claude Code
//! adapter, but a genuinely WEAKER one — see the honesty note below before
//! reading further.
//!
//! opencode has no per-session transcript FILE at all: everything lives in
//! a sqlite database at `~/.local/share/opencode/opencode.db` (verified on
//! this box: `session`, `message`, `part`, `permission`, `session_input`,
//! `event`, … tables; 1101 sessions, a 2.3 GB file). `session` carries
//! `id`/`directory`/`title`/`time_created`/`time_updated`/`time_archived`
//! as plain columns (epoch millis; no JSON parse needed); `message` carries
//! a per-row `data` JSON blob with `role`/`time`/… keyed on an indexed
//! `(session_id, time_created, id)` — verified via `EXPLAIN QUERY PLAN`
//! that a per-session newest-message lookup is an index `SEARCH`, never a
//! table `SCAN`, so this stays bounded even against the 2.3 GB file.
//!
//! **VERIFIED LIMITATION (design §4, confirmed against the real schema):**
//! there is no durable busy/idle/permission-pending signal anywhere on
//! disk. The `permission` table doesn't even carry a `session_id` column
//! (its unique index is `(project_id, action, resource)` — an allow-list
//! of past decisions, not a live per-session pending-request queue), so
//! this adapter cannot honestly attempt Grok/Kimi's blocked detection at
//! all; there is no schema-level thread from a permission row back to a
//! session. Live busy/idle truth exists only in memory inside an
//! ephemeral, randomly-ported `opencode serve` process this adapter never
//! talks to (pull-only, direct-disk, same as every other adapter in this
//! module).
//!
//! So the ONLY honest signal available is "who spoke last, and when":
//! `role == "assistant"` on the newest message → the agent handed the turn
//! back (`Holder::Human`, mirroring the Claude adapter's `end_turn` case);
//! `role == "user"` → the human just spoke and the agent owes a reply
//! (`Holder::Agent`, mirroring the Claude adapter's rule 3). This is
//! reported at [`Confidence::Presumed`] via [`StateSource::Capture`] —
//! NEVER [`StateSource::Transcript`]'s `Inferred`, even though the read
//! mechanics resemble tailing a transcript — because there genuinely is no
//! busy/idle signal underneath the guess, unlike every other harness here.
//! `why` says exactly this every time, not just decoratively.
//!
//! No new dependency was needed for this: `rusqlite` (bundled sqlite) is
//! already a `kb-core` dependency (`storage/sqlite.rs`, `storage/backup.rs`)
//! — this module is a new READ-ONLY consumer of it, opened with
//! `SQLITE_OPEN_READ_ONLY`, never `storage/sqlite.rs`'s migrated
//! read-write handle.

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::sessions::live::{
    derive_state, Confidence, Holder, LivePolicy, LiveSession, StateSource, LIVE_SCAN_CAP,
};

/// This adapter's entry in the closed [`crate::sessions::HARNESSES`] set —
/// see `codex.rs`'s identical convention/rationale.
const HARNESS: &str = "opencode";

fn open_ro(db_path: &Path) -> Option<Connection> {
    Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
}

struct SessionRow {
    directory: Option<String>,
    title: Option<String>,
    time_updated: i64,
    archived: bool,
}

fn fetch_session_row(conn: &Connection, session_id: &str) -> Option<SessionRow> {
    conn.query_row(
        "SELECT directory, title, time_updated, time_archived FROM session WHERE id = ?1",
        [session_id],
        |row| {
            Ok(SessionRow {
                directory: row.get::<_, Option<String>>(0)?,
                title: row.get::<_, Option<String>>(1)?,
                time_updated: row.get::<_, i64>(2)?,
                archived: row.get::<_, Option<i64>>(3)?.is_some(),
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

/// The newest message row for a session: `(time_created, time_updated,
/// role)`. Uses the `(session_id, time_created, id)` index (verified via
/// `EXPLAIN QUERY PLAN` — an indexed `SEARCH`, never a `SCAN`). `role` is
/// pulled out of the `data` JSON blob; a malformed blob on this — the
/// newest — row is this adapter's "torn trailing line" analogue and is
/// treated exactly the same way: honest `None` from the caller, not a
/// guess.
fn fetch_newest_message(conn: &Connection, session_id: &str) -> Option<(i64, i64, Option<String>)> {
    let (time_created, time_updated, data): (i64, i64, String) = conn
        .query_row(
            "SELECT time_created, time_updated, data FROM message \
             WHERE session_id = ?1 ORDER BY time_created DESC LIMIT 1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .ok()
        .flatten()?;
    let role = serde_json::from_str::<Value>(&data)
        .ok()
        .and_then(|v| v.get("role").and_then(Value::as_str).map(str::to_string));
    Some((time_created, time_updated, role))
}

fn classify_with_conn(
    conn: &Connection,
    session_id: &str,
    now_unix: i64,
    policy: &LivePolicy,
) -> Option<LiveSession> {
    let session = fetch_session_row(conn, session_id)?;
    if session.archived {
        // An archived session has no live status to report — honest
        // omission, not a guess at a lane.
        return None;
    }
    let (msg_created_ms, msg_updated_ms, role) = fetch_newest_message(conn, session_id)?;
    let role = role?; // A message row with no recognisable `role` at all: None.

    let holder = match role.as_str() {
        // The agent just finished speaking → handed the turn back.
        "assistant" => Holder::Human,
        // The human just spoke → the agent owes a reply.
        "user" => Holder::Agent,
        // Any other/unrecognised role (e.g. a future `system` message
        // shape): genuinely ambiguous, honest None.
        _ => return None,
    };

    let newest_ms = msg_updated_ms.max(msg_created_ms).max(session.time_updated);
    let mtime_unix = newest_ms / 1000;
    let last_activity_unix = mtime_unix.min(now_unix);

    // NEVER StateSource::Transcript here — see the module docs. This is
    // the one honest lever that keeps a Presumed-grade guess from reading
    // as Inferred-grade on the wire.
    let (state, _confidence_from_transcript_mapping) = derive_state(
        holder,
        last_activity_unix,
        now_unix,
        StateSource::Capture,
        policy,
    );

    let project = session
        .directory
        .as_deref()
        .and_then(|d| Path::new(d).file_name())
        .map(|n| n.to_string_lossy().into_owned());

    Some(LiveSession {
        resume: format!("opencode --session {session_id}"),
        session_id: session_id.to_string(),
        harness: HARNESS.to_string(),
        holder,
        state,
        source: StateSource::Capture,
        confidence: Confidence::Presumed,
        since_unix: last_activity_unix,
        since_secs: (now_unix - last_activity_unix).max(0),
        project,
        cwd: session.directory,
        model: None,
        title: session.title,
        transcript_path: PathBuf::new(),
        why: Some(format!(
            "db: newest message role={role} (source=capture, presumed — opencode has no \
             durable busy/idle signal on disk; see design §4)"
        )),
        version: None,
    })
}

/// Classify one opencode session by id. Opens its OWN read-only connection
/// — for a bulk scan prefer [`scan_opencode`], which shares one connection
/// across every candidate session instead of opening 1000+.
///
/// Deviates from this module's usual `path_or_dir` single-argument shape
/// (documented, not accidental): an opencode "session" is a database row,
/// not a file, so classification genuinely needs both the db path and a
/// session id.
pub fn classify_opencode_session(
    db_path: &Path,
    session_id: &str,
    now_unix: i64,
    policy: &LivePolicy,
) -> Option<LiveSession> {
    let conn = open_ro(db_path)?;
    classify_with_conn(&conn, session_id, now_unix, policy)
}

/// Query the `session` table for non-archived sessions touched within
/// `max_age_secs` (the cheap gate pushed INTO the SQL — cheaper than even
/// opening a file, since indexed columns are read without touching the
/// `data` blob columns), capped at [`LIVE_SCAN_CAP`] rows, newest first.
/// Every candidate is then classified via ONE shared read-only connection
/// (never one connection per session).
pub fn scan_opencode(
    db_path: &Path,
    now_unix: i64,
    policy: &LivePolicy,
    max_age_secs: i64,
) -> Vec<LiveSession> {
    let mut out = Vec::new();
    let Some(conn) = open_ro(db_path) else {
        return out;
    };
    let cutoff_ms = (now_unix - max_age_secs).max(0) * 1000;
    let ids: Vec<String> = {
        let Ok(mut stmt) = conn.prepare(
            "SELECT id FROM session \
             WHERE time_archived IS NULL AND time_updated >= ?1 \
             ORDER BY time_updated DESC LIMIT ?2",
        ) else {
            return out;
        };
        let Ok(rows) = stmt.query_map(rusqlite::params![cutoff_ms, LIVE_SCAN_CAP as i64], |row| {
            row.get::<_, String>(0)
        }) else {
            return out;
        };
        rows.flatten().collect()
    };
    for id in ids {
        if let Some(session) = classify_with_conn(&conn, &id, now_unix, policy) {
            out.push(session);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal opencode-shaped schema (just the two tables this
    /// adapter reads) in a temp file-backed sqlite db — a real file so
    /// `open_with_flags(..., SQLITE_OPEN_READ_ONLY)` behaves exactly as it
    /// does against the real `opencode.db`.
    fn make_db() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY, directory TEXT, title TEXT,
                time_created INTEGER, time_updated INTEGER, time_archived INTEGER
             );
             CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER,
                time_updated INTEGER, data TEXT
             );
             CREATE INDEX message_session_time_created_id_idx
                ON message (session_id, time_created, id);",
        )
        .unwrap();
        (tmp, path)
    }

    fn insert_session(
        conn: &Connection,
        id: &str,
        directory: &str,
        title: &str,
        time_updated_ms: i64,
        archived: bool,
    ) {
        conn.execute(
            "INSERT INTO session (id, directory, title, time_created, time_updated, time_archived) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            rusqlite::params![
                id,
                directory,
                title,
                time_updated_ms,
                archived.then_some(time_updated_ms)
            ],
        )
        .unwrap();
    }

    fn insert_message(conn: &Connection, id: &str, session_id: &str, time_ms: i64, data: &str) {
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![id, session_id, time_ms, data],
        )
        .unwrap();
    }

    #[test]
    fn human_holds_when_newest_message_role_is_assistant() {
        let (_tmp, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        insert_session(
            &conn,
            "ses_a",
            "/home/user/project/kb",
            "Fix bug",
            1_700_000_000_000,
            false,
        );
        insert_message(
            &conn,
            "msg_1",
            "ses_a",
            1_699_999_000_000,
            r#"{"role":"user"}"#,
        );
        insert_message(
            &conn,
            "msg_2",
            "ses_a",
            1_700_000_000_000,
            r#"{"role":"assistant"}"#,
        );
        drop(conn);

        let now = 1_700_000_100; // seconds; message time is ms
        let got =
            classify_opencode_session(&db_path, "ses_a", now, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Human);
        assert_eq!(got.harness, "opencode");
        assert_eq!(got.confidence, Confidence::Presumed);
        assert_eq!(got.source, StateSource::Capture);
        assert_eq!(got.project.as_deref(), Some("kb"));
        assert!(got.why.as_deref().unwrap().contains("presumed"));
    }

    #[test]
    fn agent_holds_when_newest_message_role_is_user() {
        let (_tmp, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        insert_session(
            &conn,
            "ses_b",
            "/tmp",
            "New session",
            1_700_000_000_000,
            false,
        );
        insert_message(
            &conn,
            "msg_1",
            "ses_b",
            1_700_000_000_000,
            r#"{"role":"user"}"#,
        );
        drop(conn);

        let now = 1_700_000_100;
        let got =
            classify_opencode_session(&db_path, "ses_b", now, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
        assert_eq!(got.confidence, Confidence::Presumed);
    }

    #[test]
    fn robustness_no_session_row_returns_none() {
        let (_tmp, db_path) = make_db();
        assert!(classify_opencode_session(
            &db_path,
            "ses-missing",
            1_700_000_100,
            &LivePolicy::default()
        )
        .is_none());
    }

    #[test]
    fn robustness_session_with_no_messages_returns_none() {
        let (_tmp, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        insert_session(&conn, "ses_c", "/tmp", "Empty", 1_700_000_000_000, false);
        drop(conn);
        assert!(classify_opencode_session(
            &db_path,
            "ses_c",
            1_700_000_100,
            &LivePolicy::default()
        )
        .is_none());
    }

    #[test]
    fn robustness_archived_session_returns_none() {
        let (_tmp, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        insert_session(&conn, "ses_d", "/tmp", "Old", 1_700_000_000_000, true);
        insert_message(
            &conn,
            "msg_1",
            "ses_d",
            1_700_000_000_000,
            r#"{"role":"user"}"#,
        );
        drop(conn);
        assert!(classify_opencode_session(
            &db_path,
            "ses_d",
            1_700_000_100,
            &LivePolicy::default()
        )
        .is_none());
    }

    /// This adapter's "torn trailing line" analogue: the NEWEST message
    /// row's `data` blob is malformed JSON — honest None, not a panic or a
    /// guess.
    #[test]
    fn robustness_malformed_newest_message_data_returns_none() {
        let (_tmp, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        insert_session(&conn, "ses_e", "/tmp", "Bad row", 1_700_000_000_000, false);
        insert_message(
            &conn,
            "msg_1",
            "ses_e",
            1_700_000_000_000,
            "not json at all",
        );
        drop(conn);
        assert!(classify_opencode_session(
            &db_path,
            "ses_e",
            1_700_000_100,
            &LivePolicy::default()
        )
        .is_none());
    }

    #[test]
    fn robustness_unrecognised_role_returns_none() {
        let (_tmp, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        insert_session(
            &conn,
            "ses_f",
            "/tmp",
            "Weird role",
            1_700_000_000_000,
            false,
        );
        insert_message(
            &conn,
            "msg_1",
            "ses_f",
            1_700_000_000_000,
            r#"{"role":"system"}"#,
        );
        drop(conn);
        assert!(classify_opencode_session(
            &db_path,
            "ses_f",
            1_700_000_100,
            &LivePolicy::default()
        )
        .is_none());
    }

    #[test]
    fn robustness_nonexistent_db_returns_none_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.db");
        assert!(classify_opencode_session(
            &missing,
            "ses_x",
            1_700_000_100,
            &LivePolicy::default()
        )
        .is_none());
    }

    #[test]
    fn scan_applies_the_age_cutoff_in_sql_and_excludes_archived() {
        let (_tmp, db_path) = make_db();
        let conn = Connection::open(&db_path).unwrap();
        insert_session(
            &conn,
            "ses_fresh",
            "/tmp",
            "Fresh",
            1_700_000_000_000,
            false,
        );
        insert_message(
            &conn,
            "m1",
            "ses_fresh",
            1_700_000_000_000,
            r#"{"role":"user"}"#,
        );
        insert_session(&conn, "ses_stale", "/tmp", "Stale", 1_000_000_000, false);
        insert_message(
            &conn,
            "m2",
            "ses_stale",
            1_000_000_000,
            r#"{"role":"user"}"#,
        );
        insert_session(
            &conn,
            "ses_archived",
            "/tmp",
            "Archived",
            1_700_000_000_000,
            true,
        );
        insert_message(
            &conn,
            "m3",
            "ses_archived",
            1_700_000_000_000,
            r#"{"role":"user"}"#,
        );
        drop(conn);

        let now = 1_700_000_100;
        let got = scan_opencode(&db_path, now, &LivePolicy::default(), 3_600);
        let ids: Vec<&str> = got.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, vec!["ses_fresh"]);
    }

    #[test]
    fn scan_nonexistent_db_returns_empty_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.db");
        assert!(scan_opencode(&missing, 1_700_000_100, &LivePolicy::default(), 86_400).is_empty());
    }

    #[test]
    fn harness_label_is_a_member_of_the_closed_set() {
        assert!(crate::sessions::HARNESSES.contains(&HARNESS));
    }
}
