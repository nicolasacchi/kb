//! v0.44 X3 — the `kb remember` outbox: a memory write is never lost to a slow
//! or down daemon.
//!
//! `kb remember` mints a `client_ref` per invocation and sends it with the
//! write. When the write fails for a TRANSIENT reason (daemon unreachable,
//! slow, a 5xx) the whole intent is spooled here as one JSON file named after
//! the `client_ref`; the next `kb remember` that reaches the daemon (or an
//! explicit `kb outbox flush`) replays it with the SAME `client_ref`. The
//! daemon derives the artifact's filename from the key, so a replay of a
//! write that actually landed (the response was what got lost) finds the file
//! and answers 200 with the original id instead of writing a duplicate.
//!
//! The spool holds the UNRESOLVED intent (target kb, scope, link as typed) so
//! the replay re-runs the same ladder against whatever daemon answers; only
//! the session id is frozen at spool time. A permanently refused entry (a
//! 4xx) is renamed to `<ref>.rejected` and never retried, so one bad entry
//! cannot wedge the queue.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A failure worth spooling: nothing was durably accepted and a later attempt
/// may succeed. `Display` is the inner message verbatim, so wrapping an error
/// in it never changes what the caller prints.
#[derive(Debug)]
pub struct Transient(pub String);

impl std::fmt::Display for Transient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Transient {}

/// Is this error one the outbox should absorb?
pub fn is_transient(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Transient>().is_some()
}

/// One spooled `kb remember` invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Spooled {
    pub client_ref: String,
    pub queued_at: i64,
    pub text: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub kb: Option<String>,
    pub scope: Option<String>,
    pub category: String,
    pub tags: Option<String>,
    pub salience: Option<f32>,
    pub decay: Option<String>,
    pub supersedes: Option<String>,
    pub memory_type: Option<String>,
    pub source: Option<String>,
    pub failed: bool,
    /// Frozen at spool time. `None` together with `no_session`.
    pub session_id: Option<String>,
    pub no_session: bool,
    pub global: bool,
    pub link: Option<String>,
    pub daemon: Option<String>,
}

/// `${XDG_CACHE_HOME:-$HOME/.cache}/kb/outbox`.
pub fn outbox_dir() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache.join("kb").join("outbox"))
}

/// A fresh 16-hex idempotency key. The daemon requires 8-64 chars of
/// `[A-Za-z0-9_-]`.
pub fn new_client_ref() -> Result<String> {
    let mut buf = [0u8; 8];
    getrandom::fill(&mut buf).context("getrandom")?;
    Ok(hex::encode(buf))
}

fn entry_path(dir: &Path, client_ref: &str) -> PathBuf {
    dir.join(format!("{client_ref}.json"))
}

/// Write `s` atomically (tmp + rename), mode 0600 on unix: the spool carries
/// the memory text.
pub fn spool(dir: &Path, s: &Spooled) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = entry_path(dir, &s.client_ref);
    let tmp = dir.join(format!("{}.tmp", s.client_ref));
    let bytes = serde_json::to_vec_pretty(s)?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("open {}", tmp.display()))?;
        f.write_all(&bytes)?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(path)
}

/// Every spooled entry, oldest first (`queued_at`, then `client_ref`). A file
/// that does not parse is skipped here and left on disk for inspection.
pub fn pending(dir: &Path) -> Vec<(PathBuf, Spooled)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, Spooled)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let raw = std::fs::read(&p).ok()?;
            let s: Spooled = serde_json::from_slice(&raw).ok()?;
            Some((p, s))
        })
        .collect();
    out.sort_by(|a, b| (a.1.queued_at, &a.1.client_ref).cmp(&(b.1.queued_at, &b.1.client_ref)));
    out
}

/// The entry was applied: drop it.
pub fn complete(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The daemon refused the entry for good: park it as `<ref>.rejected`.
pub fn reject(path: &Path) {
    let _ = std::fs::rename(path, path.with_extension("rejected"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(client_ref: &str, at: i64) -> Spooled {
        Spooled {
            client_ref: client_ref.to_string(),
            queued_at: at,
            text: "the heron stands on one leg".into(),
            title: Some("Heron".into()),
            summary: None,
            kb: None,
            scope: Some("project".into()),
            category: "memory-user".into(),
            tags: Some("a,b".into()),
            salience: Some(0.7),
            decay: None,
            supersedes: None,
            memory_type: None,
            source: None,
            failed: false,
            session_id: Some("sess-1".into()),
            no_session: false,
            global: false,
            link: None,
            daemon: None,
        }
    }

    #[test]
    fn client_refs_satisfy_the_daemons_grammar() {
        let r = new_client_ref().unwrap();
        assert_eq!(r.len(), 16);
        assert!(r.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(r, new_client_ref().unwrap());
    }

    #[test]
    fn spool_round_trips_and_lists_oldest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("outbox");
        let newer = sample("bbbbbbbbbbbbbbbb", 200);
        let older = sample("aaaaaaaaaaaaaaaa", 100);
        spool(&dir, &newer).unwrap();
        let p = spool(&dir, &older).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the spool carries memory text");
        }
        let got = pending(&dir);
        assert_eq!(
            got.iter()
                .map(|(_, s)| s.client_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb"]
        );
        assert_eq!(got[0].1, older);
    }

    #[test]
    fn complete_removes_and_reject_parks_the_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let a = spool(&dir, &sample("aaaaaaaaaaaaaaaa", 1)).unwrap();
        let b = spool(&dir, &sample("bbbbbbbbbbbbbbbb", 2)).unwrap();
        complete(&a);
        reject(&b);
        assert!(pending(&dir).is_empty(), "neither is pending any more");
        assert!(!a.exists());
        assert!(dir.join("bbbbbbbbbbbbbbbb.rejected").exists());
    }

    #[test]
    fn transient_is_recognised_through_anyhow_and_prints_verbatim() {
        let e = anyhow::Error::new(Transient("daemon slow".into()));
        assert!(is_transient(&e));
        assert_eq!(e.to_string(), "daemon slow");
        assert!(!is_transient(&anyhow::anyhow!("daemon returned 400")));
    }
}
