//! The blob-hash guard (design-lip.md, "THE BLOB GUARD (load-bearing, get
//! it exactly right)") — a git-compatible blob content hash, computed
//! before AND after every LSP round trip. This is the mechanism that lets
//! a caller (kb-code) trust a lip/1 answer as `exact`: the adapter proves,
//! byte-for-byte, that it queried (and the server answered against) the
//! SAME content the caller already hashed on its side. Either hash not
//! matching the caller's `blob_sha` is a REFUSAL, never a downgrade.
//!
//! `git_blob_sha1` is intentionally a small, self-contained duplicate of
//! kb-code-server's `ingest::git_blob_hash` (same formula: hex-encoded
//! `sha1("blob " len "\0" content)` — bit-for-bit what `git hash-object`
//! prints). kb-lip must not depend on kb-code-server (it is a standalone,
//! zero-heavy-deps crate — see the Cargo.toml doc comment), so the ~8
//! lines are copied rather than shared; both are pinned against the real
//! `git hash-object` CLI so they can never silently drift apart.

use sha1::{Digest, Sha1};
use std::path::Path;

/// Git-compatible blob content hash of `bytes`, as 40 lowercase hex chars.
pub fn git_blob_sha1(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(b"blob ");
    hasher.update(bytes.len().to_string());
    hasher.update(b"\0");
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// On-disk bytes plus their freshly-computed blob hash — the unit the blob
/// guard reasons about. Read fresh every time (no caching): a cached blob
/// is exactly the staleness this guard exists to refuse.
#[derive(Debug, Clone)]
pub struct Blob {
    pub sha: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Read `path` fresh off disk and hash it. Called twice per request (before
/// and after the LSP round trip) by design — see the module doc.
pub fn read_and_hash(path: &Path) -> Result<Blob, BlobError> {
    let bytes = std::fs::read(path).map_err(|source| BlobError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let sha = git_blob_sha1(&bytes);
    Ok(Blob { sha, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn real_git_hash_object(bytes: &[u8]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, bytes).unwrap();
        let out = Command::new("git")
            .arg("hash-object")
            .arg(&path)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git hash-object failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn git_blob_sha1_matches_git_hash_object() {
        for bytes in [
            &b""[..],
            b"hello world\n",
            b"no trailing newline",
            b"line one\nline two\nline three\n",
            &[0xffu8, 0x00, 0xfe, 0x01, 0x02][..], // embedded NUL + non-UTF8 bytes
            "héllo 😀 wörld".as_bytes(),
        ] {
            let want = real_git_hash_object(bytes);
            let got = git_blob_sha1(bytes);
            assert_eq!(got, want, "mismatch for {bytes:?}");
            assert_eq!(got.len(), 40);
            assert!(got.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn git_blob_sha1_known_vector() {
        // A hand-verifiable vector independent of a `git` binary being on
        // PATH: `git hash-object` for the single byte "a" is a well-known
        // published SHA1 value (sha1("blob 1\0a")).
        assert_eq!(
            git_blob_sha1(b"a"),
            "2e65efe2a145dda7ee51d1741299f848e5bf752e"
        );
    }

    #[test]
    fn read_and_hash_reads_fresh_off_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"v1").unwrap();
        let b1 = read_and_hash(&path).unwrap();
        assert_eq!(b1.sha, git_blob_sha1(b"v1"));

        std::fs::write(&path, b"v2 (changed)").unwrap();
        let b2 = read_and_hash(&path).unwrap();
        assert_eq!(b2.sha, git_blob_sha1(b"v2 (changed)"));
        assert_ne!(b1.sha, b2.sha);
    }

    #[test]
    fn read_and_hash_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_and_hash(&dir.path().join("nope")).unwrap_err();
        assert!(matches!(err, BlobError::Read { .. }));
    }
}
