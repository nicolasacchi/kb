//! Session ids and the closed harness set.
//!
//! Ids are 8..=12 chars from `[a-z0-9]`. [`valid_id`] is applied at every
//! RPC entry and before an id reaches a path, socket name, systemd unit or
//! argv. Generation reads `/dev/urandom` (`byte % 36`) and regenerates when
//! `s/<id>.sock` or `s/<id>/` already exists.
//!
//! [`HARNESSES`] mirrors `kb_core::sessions::HARNESSES`
//! (`crates/kb-core/src/sessions/constants.rs`) without a dependency edge;
//! the drift test against that constant is T10's.

use std::path::Path;

use crate::wire;

use super::RunnerError;

/// The closed harness set, in `kb_core::sessions::HARNESSES` order.
pub const HARNESSES: [&str; 6] = ["claude", "codex", "opencode", "grok", "kimi", "omp"];

/// Whether `id` may be used in a path, socket name, unit name or argv.
pub fn valid_id(id: &str) -> bool {
    wire::is_valid_term_id(id)
}

/// Whether `harness` names a known harness.
pub fn valid_harness(harness: &str) -> bool {
    HARNESSES.contains(&harness)
}

/// Generate a fresh 10-char id that does not collide with an existing
/// `s/<id>.sock` or `s/<id>/` in `sessions_dir`.
pub fn generate_id(sessions_dir: &Path) -> Result<String, RunnerError> {
    for _ in 0..16 {
        let id = random_id()?;
        if !sessions_dir.join(format!("{id}.sock")).exists() && !sessions_dir.join(&id).exists() {
            return Ok(id);
        }
    }
    Err(RunnerError::Internal(
        "could not generate a fresh session id".to_string(),
    ))
}

fn random_id() -> Result<String, RunnerError> {
    const CHARS: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut buf = [0u8; 10];
    // No pid-derived fallback: /dev/urandom is always present on the
    // Linux hosts this runs on, and a low-entropy id would be worse
    // than a clean `internal` error.
    let mut f = std::fs::File::open("/dev/urandom")
        .map_err(|e| RunnerError::Internal(format!("/dev/urandom unavailable: {e}")))?;
    use std::io::Read;
    f.read_exact(&mut buf)
        .map_err(|e| RunnerError::Internal(format!("read /dev/urandom: {e}")))?;
    Ok(buf
        .iter()
        .map(|b| CHARS[(*b % 36) as usize] as char)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_accept_only_the_charset() {
        assert!(valid_id("abcd1234"));
        assert!(valid_id("abcd1234abcd"));
        for bad in [
            "../x",
            "abcd1234/../x",
            "A",
            "abcd1234 ",
            " abcd1234",
            "",
            "abcd123",       // 7 chars
            "abcd1234abcd1", // 13 chars
            "abcd-123",
            "abcd.123",
            "abcd/123",
            "ABCD1234",
        ] {
            assert!(!valid_id(bad), "id {bad:?} must be rejected");
        }
    }

    #[test]
    fn harness_closed_set() {
        assert_eq!(
            HARNESSES,
            ["claude", "codex", "opencode", "grok", "kimi", "omp"]
        );
        for good in HARNESSES {
            assert!(valid_harness(good), "harness {good} must be accepted");
        }
        assert!(!valid_harness("bash"), "bash must be rejected");
        assert!(!valid_harness("omp "), "trailing space must be rejected");
        assert!(!valid_harness(""), "empty must be rejected");
    }
}
