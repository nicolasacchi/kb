//! The `media` op: read one stored picture by its sha256.
//!
//! The tap writes each decoded kitty picture once, content-addressed,
//! under `<run-dir>/s/<id>/img/<sha256>` (hex64 lowercase). A viewer asks
//! for the bytes by hash alone, so the lookup walks the per-session
//! directories of the runner's own sessions dir; nothing in the request
//! reaches a path except the validated hash.
//!
//! Hard rules: the hash must be 64 lowercase hex characters before any
//! file system access; only regular files are read (a symlink at the
//! candidate path, at the `img` directory or at a session directory is
//! skipped, never followed); the read is capped at [`MAX_MEDIA_BYTES`]
//! (the kitty transmit cap), so an oversized file is `too-large`, never
//! truncated. The reply is the usual envelope header with `{"len":N}`
//! followed by exactly N raw bytes (see `server::respond_media`).

use std::io::Read;
use std::path::Path;

use crate::runner::ids::valid_id;
use crate::runner::RunnerError;

/// Largest picture the runner will hand out (the tap's kitty cap).
pub const MAX_MEDIA_BYTES: u64 = 32 * 1024 * 1024;

/// True iff `s` is exactly 64 lowercase hex characters.
pub fn valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Read the stored picture `sha` from any session under `sessions_dir`.
///
/// Blocking file I/O: callers run it on a blocking thread.
pub fn read_media(sessions_dir: &Path, sha: &str) -> Result<Vec<u8>, RunnerError> {
    if !valid_sha256(sha) {
        return Err(RunnerError::BadRequest(
            "media key must be 64 lowercase hex characters".to_string(),
        ));
    }
    let entries = std::fs::read_dir(sessions_dir)
        .map_err(|_| RunnerError::NotFound("no such picture".to_string()))?;
    for entry in entries.flatten() {
        // Session directories only: `<id>.sock` files and anything whose
        // name is not a valid id are skipped, and a symlinked directory
        // is never followed (`DirEntry::file_type` does not follow).
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !valid_id(&name) {
            continue;
        }
        match entry.file_type() {
            Ok(t) if t.is_dir() => {}
            _ => continue,
        }
        let img_dir = entry.path().join("img");
        match std::fs::symlink_metadata(&img_dir) {
            Ok(m) if m.is_dir() => {}
            _ => continue,
        }
        let path = img_dir.join(sha);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        if meta.len() > MAX_MEDIA_BYTES {
            return Err(RunnerError::TooLarge(
                "stored picture exceeds the media cap".to_string(),
            ));
        }
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut buf = Vec::with_capacity(meta.len() as usize);
        if file
            .take(MAX_MEDIA_BYTES + 1)
            .read_to_end(&mut buf)
            .is_err()
        {
            continue;
        }
        if buf.len() as u64 > MAX_MEDIA_BYTES {
            return Err(RunnerError::TooLarge(
                "stored picture exceeds the media cap".to_string(),
            ));
        }
        return Ok(buf);
    }
    Err(RunnerError::NotFound("no such picture".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn store(dir: &Path, id: &str, sha: &str, bytes: &[u8]) {
        let img = dir.join(id).join("img");
        std::fs::create_dir_all(&img).unwrap();
        std::fs::write(img.join(sha), bytes).unwrap();
    }

    #[test]
    fn sha_grammar_is_lowercase_hex64_only() {
        assert!(valid_sha256(SHA));
        assert!(!valid_sha256(&SHA.to_uppercase()));
        assert!(!valid_sha256(&SHA[..63]));
        assert!(!valid_sha256(&format!("{SHA}a")));
        assert!(!valid_sha256("../../etc/passwd"));
        assert!(!valid_sha256(&format!("{}g", &SHA[..63])));
    }

    #[test]
    fn finds_a_picture_in_any_session_dir() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), "abcd1234", SHA, b"one");
        store(dir.path(), "efgh5678", &"b".repeat(64), b"two");
        assert_eq!(read_media(dir.path(), SHA).unwrap(), b"one");
        assert_eq!(read_media(dir.path(), &"b".repeat(64)).unwrap(), b"two");
    }

    #[test]
    fn unknown_hash_is_not_found_and_bad_hash_is_bad_request() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), "abcd1234", SHA, b"one");
        let miss = read_media(dir.path(), &"c".repeat(64)).unwrap_err();
        assert_eq!(miss.code(), "not-found");
        let bad = read_media(dir.path(), "../x").unwrap_err();
        assert_eq!(bad.code(), "bad-request");
        let upper = read_media(dir.path(), &SHA.to_uppercase()).unwrap_err();
        assert_eq!(upper.code(), "bad-request");
    }

    #[test]
    fn a_symlinked_picture_is_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();
        let img = dir.path().join("abcd1234").join("img");
        std::fs::create_dir_all(&img).unwrap();
        std::os::unix::fs::symlink(&secret, img.join(SHA)).unwrap();
        let err = read_media(dir.path(), SHA).unwrap_err();
        assert_eq!(err.code(), "not-found");
    }

    #[test]
    fn a_symlinked_img_dir_is_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join(SHA), b"outside").unwrap();
        let sess = dir.path().join("abcd1234");
        std::fs::create_dir_all(&sess).unwrap();
        std::os::unix::fs::symlink(&elsewhere, sess.join("img")).unwrap();
        let err = read_media(dir.path(), SHA).unwrap_err();
        assert_eq!(err.code(), "not-found");
    }

    #[test]
    fn a_picture_over_the_cap_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("abcd1234").join("img");
        std::fs::create_dir_all(&img).unwrap();
        let f = std::fs::File::create(img.join(SHA)).unwrap();
        f.set_len(MAX_MEDIA_BYTES + 1).unwrap();
        let err = read_media(dir.path(), SHA).unwrap_err();
        assert_eq!(err.code(), "too-large");
    }

    #[test]
    fn non_id_entries_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        // `Bad` is not a valid session id, so its pictures are invisible.
        store(dir.path(), "Bad", SHA, b"nope");
        assert_eq!(read_media(dir.path(), SHA).unwrap_err().code(), "not-found");
    }
}
