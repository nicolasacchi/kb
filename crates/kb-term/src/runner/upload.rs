//! The `upload` op: store one picture the operator picked on a phone and
//! hand back its path (T18).
//!
//! kb persists no pixels and reads no pixels: the picture lands under
//! `<run-dir>/s/<id>/up/` (mode 0700, tmpfs, session lifetime) so the
//! harness running in that session can open it by path. The route that
//! feeds this op lives strictly inside the terminal-tier gate; raw bytes
//! cannot be scrubbed, so they never take any other path.
//!
//! Hard rules (`tests/runner_upload.rs` exercises them through the public API
//! and through a real `runner.sock`):
//!
//! * the session id is validated ([`valid_id`]) and the session must exist
//!   (`<id>.sock` present) BEFORE any path is built or any byte is read;
//! * `len` above [`UPLOAD_MAX_BYTES`] is refused without reading the body;
//! * the type comes from the BYTES (PNG, JPEG, GIF, WebP magic only; no
//!   SVG, HTML, PDF or text) and the declared mime must agree;
//! * the client never names a file: the name is `<sha256 hex>.<ext>`;
//! * the file is written under a temporary name (`O_EXCL`, 0600) in the
//!   same directory and `rename`d into place; a failure removes the
//!   temporary file;
//! * identical bytes already stored are a success (dedupe) and are checked
//!   BEFORE the quota, so re-sending a picture at the quota still works;
//! * at most [`UPLOAD_MAX_FILES`] files and [`UPLOAD_MAX_TOTAL_BYTES`]
//!   bytes per session.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::runner::ids::valid_id;
use crate::runner::media::sniff_image;
use crate::runner::RunnerError;

/// Largest single upload (also the kb-server body cap's anchor).
pub const UPLOAD_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Most files one session's `up/` directory may hold.
pub const UPLOAD_MAX_FILES: usize = 64;

/// Most bytes one session's `up/` directory may hold.
pub const UPLOAD_MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

/// How long the runner waits for the body after the request line.
pub const UPLOAD_BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// The directory name under a session dir that holds its uploads.
const UP_DIR: &str = "up";

/// Prefix of an in-flight temporary file (never counted as a stored file
/// once renamed; counted against the quota while it exists).
const TMP_PREFIX: &str = ".tmp-";
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One stored picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// Absolute path of the file.
    pub path: PathBuf,
    /// Bytes stored.
    pub bytes: u64,
    /// The sniffed mime type.
    pub mime: &'static str,
    /// Lowercase hex sha256 (the file stem).
    pub sha256: String,
}

/// The upload directory of session `id` (the id must already be valid).
pub fn up_dir(sessions_dir: &Path, id: &str) -> PathBuf {
    sessions_dir.join(id).join(UP_DIR)
}

/// The file extension for a sniffed mime type.
fn ext_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        _ => "webp",
    }
}

/// Cheap pre-body checks: the id grammar, then session presence. Run
/// before the body is read so a refused request costs no bytes.
pub fn check_target(sessions_dir: &Path, id: &str) -> Result<(), RunnerError> {
    if !valid_id(id) {
        return Err(RunnerError::InvalidId);
    }
    if !sessions_dir.join(format!("{id}.sock")).exists() {
        return Err(RunnerError::NotFound(format!("no session {id}")));
    }
    Ok(())
}

/// Remove the `up/` directory of every session whose holder socket is
/// gone (a harness that exited on its own never goes through `kill`,
/// which removes the whole session directory). Best effort and bounded
/// by the number of session directories.
pub fn sweep_orphans(sessions_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str() else { continue };
        if !valid_id(id) || sessions_dir.join(format!("{id}.sock")).exists() {
            continue;
        }
        let up = up_dir(sessions_dir, id);
        // Never follow a symlink out of the tree.
        if std::fs::symlink_metadata(&up).is_ok_and(|m| m.is_dir()) {
            let _ = std::fs::remove_dir_all(&up);
        }
    }
}

/// Create (or verify) the 0700 upload directory.
fn ensure_up_dir(up: &Path) -> Result<(), RunnerError> {
    match std::fs::symlink_metadata(up) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Err(RunnerError::Io(
                "upload directory is not a directory".to_string(),
            ))
        }
        Err(_) => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(up)
                .map_err(|e| RunnerError::Io(format!("create upload directory: {e}")))?;
        }
    }
    std::fs::set_permissions(up, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| RunnerError::Io(format!("chmod upload directory: {e}")))
}

/// Count the regular files and bytes already in `up`.
fn usage(up: &Path) -> Result<(usize, u64), RunnerError> {
    let mut files = 0usize;
    let mut total = 0u64;
    let entries = std::fs::read_dir(up)
        .map_err(|e| RunnerError::Io(format!("read upload directory: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| RunnerError::Io(format!("read upload directory: {e}")))?;
        let meta = std::fs::symlink_metadata(entry.path())
            .map_err(|e| RunnerError::Io(format!("stat upload: {e}")))?;
        let is_tmp = entry.file_name().to_string_lossy().starts_with(TMP_PREFIX);
        if meta.is_file() && !is_tmp {
            files += 1;
            total += meta.len();
        }
    }
    Ok((files, total))
}

/// Store `bytes` for session `id` (blocking file I/O; callers run it on a
/// blocking thread). `declared_mime` is what the client said; the sniffed
/// type is the truth and must equal it.
pub fn store_upload(
    sessions_dir: &Path,
    id: &str,
    declared_mime: &str,
    bytes: &[u8],
) -> Result<Stored, RunnerError> {
    check_target(sessions_dir, id)?;
    if bytes.len() as u64 > UPLOAD_MAX_BYTES {
        return Err(RunnerError::TooLarge(format!(
            "upload exceeds {} MiB",
            UPLOAD_MAX_BYTES / (1024 * 1024)
        )));
    }
    let mime = sniff_image(bytes)
        .ok_or_else(|| RunnerError::BadImage("not a PNG, JPEG, GIF or WebP picture".to_string()))?;
    if declared_mime != mime {
        return Err(RunnerError::BadImage(format!(
            "declared type {declared_mime} does not match the picture ({mime})"
        )));
    }
    let sha256 = hex::encode(Sha256::digest(bytes));
    let up = up_dir(sessions_dir, id);
    ensure_up_dir(&up)?;
    let final_path = up.join(format!("{sha256}.{}", ext_for(mime)));
    let path_text = final_path.to_string_lossy().into_owned();
    if !final_path.is_absolute() || path_text.chars().any(char::is_whitespace) {
        return Err(RunnerError::Io(
            "upload path is not an absolute path without spaces".to_string(),
        ));
    }
    let stored = Stored {
        path: final_path.clone(),
        bytes: bytes.len() as u64,
        mime,
        sha256: sha256.clone(),
    };

    // Dedupe FIRST: identical bytes already stored succeed without a
    // rewrite and without touching the quota.
    if std::fs::symlink_metadata(&final_path).is_ok_and(|m| m.is_file()) {
        return Ok(stored);
    }

    let (files, total) = usage(&up)?;
    if files >= UPLOAD_MAX_FILES || total + bytes.len() as u64 > UPLOAD_MAX_TOTAL_BYTES {
        return Err(RunnerError::TooLarge(format!(
            "upload quota reached: at most {UPLOAD_MAX_FILES} pictures and {} MiB per session",
            UPLOAD_MAX_TOTAL_BYTES / (1024 * 1024)
        )));
    }

    // A name unique per CALL (pid + process-wide counter): two concurrent
    // uploads of identical bytes must never share a temp file, or one's
    // error path would unlink the other's in-flight write.
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_path = up.join(format!(
        "{TMP_PREFIX}{}-{}-{seq}",
        &sha256[..16],
        std::process::id()
    ));
    // `create_new` is the ownership claim: only a temp this call created
    // is ever removed by this call.
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp_path)
        .map_err(|e| RunnerError::Io(format!("write upload: {e}")))?;
    let mut write = || -> std::io::Result<()> {
        f.write_all(bytes)?;
        f.flush()?;
        std::fs::rename(&tmp_path, &final_path)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(RunnerError::Io(format!("write upload: {e}")));
    }
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(tag: u8) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        v.extend(std::iter::repeat_n(tag, 32));
        v
    }

    fn session(dir: &Path, id: &str) {
        std::fs::write(dir.join(format!("{id}.sock")), b"").unwrap();
    }

    #[test]
    fn extension_follows_the_sniffed_type() {
        assert_eq!(ext_for("image/png"), "png");
        assert_eq!(ext_for("image/jpeg"), "jpg");
        assert_eq!(ext_for("image/gif"), "gif");
        assert_eq!(ext_for("image/webp"), "webp");
    }

    #[test]
    fn sweep_removes_only_the_up_dir_of_dead_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let live = "livesess01";
        let dead = "deadsess01";
        session(tmp.path(), live);
        for id in [live, dead] {
            std::fs::create_dir_all(up_dir(tmp.path(), id)).unwrap();
            std::fs::write(tmp.path().join(id).join("tap.sock"), b"").unwrap();
        }
        sweep_orphans(tmp.path());
        assert!(
            up_dir(tmp.path(), live).exists(),
            "a live session keeps up/"
        );
        assert!(
            !up_dir(tmp.path(), dead).exists(),
            "a dead session loses up/"
        );
        assert!(
            tmp.path().join(dead).join("tap.sock").exists(),
            "the sweep touches nothing but up/"
        );
    }

    #[test]
    fn store_returns_an_absolute_path_named_by_sha() {
        let tmp = tempfile::tempdir().unwrap();
        session(tmp.path(), "abcd1234");
        let bytes = png(1);
        let got = store_upload(tmp.path(), "abcd1234", "image/png", &bytes).unwrap();
        assert!(got.path.is_absolute());
        let want = hex::encode(Sha256::digest(&bytes));
        assert_eq!(got.sha256, want);
        assert_eq!(
            got.path.file_name().unwrap().to_str().unwrap(),
            format!("{want}.png")
        );
    }
}
