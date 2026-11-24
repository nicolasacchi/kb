//! The runner's upload store for one session's `up/` directory.
//!
//! Two doors share it. The T18 `upload` op ([`store_upload`]) stores one
//! picture named `<sha256>.<ext>`. The chunked ops (FXRUN) store a file of
//! any type: `begin` reserves quota and mints a token, `append` adds
//! chunks of at most [`CHUNK_MAX_BYTES`], `commit` hashes, classifies,
//! names and atomically places the file, and `abort` drops it.
//!
//! The files land under `<run-dir>/s/<id>/up/` (mode 0700, tmpfs, session
//! lifetime) so the harness running in that session can open them by
//! path. The runner never interprets a file's content beyond a kind sniff
//! ([`sniff_kind`]), which classifies and never gates, except that a
//! picture keeps the "declared must agree" rule. A copy for kb's own
//! corpus, if any, is made by kb-server before the relay; this module only
//! stores what it is handed.
//!
//! Hard rules (`tests/runner_upload.rs` exercises them through the public API
//! and through a real `runner.sock`):
//!
//! * the session id is validated ([`valid_id`]) and the session must exist
//!   (`<id>.sock` present) BEFORE any path is built or any byte is read;
//! * `len` above [`UPLOAD_MAX_BYTES`] (one-shot) or [`CHUNK_MAX_BYTES`]
//!   (chunk) is refused without reading the body;
//! * a picture's type comes from the BYTES and a declared `image/*` must
//!   agree; any other kind is accepted whatever the declared mime says;
//! * the client never names a path: the stored name is built here from a
//!   slugged ASCII stem, a short sha prefix and a validated extension;
//! * files are created `O_EXCL` with mode 0600 and never get an execute
//!   bit; a partial is a `.tmp-up-<token>` file renamed into place;
//! * identical bytes already stored are a success (dedupe) and are checked
//!   BEFORE the quota, so re-sending a file at the quota still works;
//! * at most [`UPLOAD_MAX_FILES`] files and [`UPLOAD_MAX_TOTAL_BYTES`]
//!   bytes per session; an open partial reserves its declared size against
//!   the byte quota (refused at begin), the file-count limit is enforced at
//!   commit after the dedupe check, and at most [`MAX_OPEN_PARTIALS`] are
//!   open at once;
//! * a partial idle for [`PARTIAL_TTL`] is swept; stored files never are.

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

/// Largest chunk one `upload_append` may carry.
pub const CHUNK_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Largest file a chunked upload may declare.
pub const FILE_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Most chunked uploads one session may have open at once.
pub const MAX_OPEN_PARTIALS: usize = 4;

/// How long an idle partial lives before it is swept.
pub const PARTIAL_TTL: Duration = Duration::from_secs(15 * 60);

/// The directory name under a session dir that holds its uploads.
const UP_DIR: &str = "up";

/// Prefix of an in-flight temporary file (never counted as a stored file
/// once renamed; counted against the quota while it exists).
const TMP_PREFIX: &str = ".tmp-";
/// Prefix of a chunked upload's partial data file (`.tmp-up-<token>`); its
/// metadata sits beside it as `.tmp-up-<token>.meta`. Both start with
/// [`TMP_PREFIX`], so [`usage`] never counts them as stored files.
const PARTIAL_PREFIX: &str = ".tmp-up-";
const META_SUFFIX: &str = ".meta";
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
        if !valid_id(id) {
            continue;
        }
        let up = up_dir(sessions_dir, id);
        if sessions_dir.join(format!("{id}.sock")).exists() {
            // A live session keeps its up/ but loses idle partials.
            if std::fs::symlink_metadata(&up).is_ok_and(|m| m.is_dir()) {
                sweep_partials(&up, PARTIAL_TTL);
            }
            continue;
        }
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
    usage_with(up, |p| std::fs::symlink_metadata(p))
}

/// [`usage`] with the stat injected (a test seam for the listing/stat race).
///
/// Other uploads run concurrently in the same directory: a temporary file
/// is renamed or unlinked between the listing and the stat. So temporary
/// entries are decided from the NAME and never statted, and a `NotFound`
/// on any other entry means it is gone (swept or removed by another call)
/// and is skipped. Every other stat error still fails.
fn usage_with(
    up: &Path,
    stat: impl Fn(&Path) -> std::io::Result<std::fs::Metadata>,
) -> Result<(usize, u64), RunnerError> {
    let mut files = 0usize;
    let mut total = 0u64;
    let entries = std::fs::read_dir(up)
        .map_err(|e| RunnerError::Io(format!("read upload directory: {e}")))?;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(RunnerError::Io(format!("read upload directory: {e}"))),
        };
        if entry.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            continue;
        }
        let meta = match stat(&entry.path()) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(RunnerError::Io(format!("stat upload: {e}"))),
        };
        if meta.is_file() {
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

// ------------------------------------------------------------ kind + names

/// What a file's bytes look like. A classification, never an admission gate
/// (except that a picture is checked against its declared type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// PNG, JPEG, GIF or WebP, with the sniffed mime.
    Image(&'static str),
    /// A `%PDF-` prefix.
    Pdf,
    /// The first 64 KiB is valid UTF-8 with no NUL.
    Text,
    /// Anything else (including an empty file).
    Opaque,
}

impl Kind {
    /// The wire name: `image`, `pdf`, `text` or `opaque`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Image(_) => "image",
            Kind::Pdf => "pdf",
            Kind::Text => "text",
            Kind::Opaque => "opaque",
        }
    }
}

/// How much of a file the text sniff looks at.
const TEXT_SNIFF_BYTES: usize = 64 * 1024;

/// Classify `bytes`. Total: never an error.
pub fn sniff_kind(bytes: &[u8]) -> Kind {
    if let Some(mime) = sniff_image(bytes) {
        return Kind::Image(mime);
    }
    if bytes.starts_with(b"%PDF-") {
        return Kind::Pdf;
    }
    if bytes.is_empty() {
        return Kind::Opaque;
    }
    let head = &bytes[..bytes.len().min(TEXT_SNIFF_BYTES)];
    if head.contains(&0) {
        return Kind::Opaque;
    }
    match std::str::from_utf8(head) {
        Ok(_) => Kind::Text,
        // A multi-byte character cut by the 64 KiB window is not an error.
        Err(e) if e.error_len().is_none() && head.len() < bytes.len() => Kind::Text,
        Err(_) => Kind::Opaque,
    }
}

/// Longest stem kept from a client name.
const STEM_MAX: usize = 48;

/// Slug `name` into `(stem, ext)`: the stem is lowercase ASCII
/// `[a-z0-9_-]` (runs of anything else become one `-`, trimmed, at most
/// 48 chars, empty becomes `file`); the extension is the slugged text after
/// the last dot when it matches `[a-z0-9]{1,8}`, else `None`. A leading dot
/// is never an extension separator. Path separators, `..`, NUL and
/// whitespace cannot survive.
pub fn slug_name(name: &str) -> (String, Option<String>) {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let (stem_raw, ext_raw) = match base.rfind('.') {
        Some(i) if i > 0 => (&base[..i], Some(&base[i + 1..])),
        _ => (base, None),
    };
    let mut stem = String::new();
    let mut last_dash = false;
    for c in stem_raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '_' {
            stem.push(c);
            last_dash = false;
        } else if !last_dash {
            stem.push('-');
            last_dash = true;
        }
    }
    let trim = |s: &str| s.trim_matches(|c| c == '-' || c == '_').to_string();
    let mut stem = trim(&stem);
    if stem.len() > STEM_MAX {
        stem.truncate(STEM_MAX);
        stem = trim(&stem);
    }
    if stem.is_empty() {
        stem = "file".to_string();
    }
    let ext = ext_raw
        .map(str::to_ascii_lowercase)
        .filter(|e| (1..=8).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()));
    (stem, ext)
}

/// The stored file name: `<sha256[..12]>-<stem>.<ext>`.
fn stored_name(sha256: &str, client_name: &str, kind: Kind) -> String {
    let (stem, ext) = slug_name(client_name);
    let ext = match kind {
        Kind::Image(mime) => ext_for(mime).to_string(),
        Kind::Pdf => ext.unwrap_or_else(|| "pdf".to_string()),
        Kind::Text => ext.unwrap_or_else(|| "txt".to_string()),
        Kind::Opaque => ext.unwrap_or_else(|| "bin".to_string()),
    };
    format!("{}-{stem}.{ext}", &sha256[..12])
}

// ------------------------------------------------------- chunked uploads

/// The result of a successful commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// Absolute path of the stored file.
    pub path: PathBuf,
    /// Bytes stored.
    pub bytes: u64,
    /// The sniffed mime for a picture, else the declared one.
    pub mime: String,
    /// `image`, `pdf`, `text` or `opaque`.
    pub kind: &'static str,
    /// Lowercase hex sha256.
    pub sha256: String,
    /// The stored file name.
    pub name: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PartialMeta {
    name: String,
    mime: String,
    size: u64,
}

/// One open partial, as found on disk.
struct Partial {
    token: String,
    size: u64,
}

/// A runner-minted token: exactly 32 lowercase hex chars.
fn valid_token(t: &str) -> bool {
    t.len() == 32 && t.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn mint_token() -> Result<String, RunnerError> {
    use std::io::Read;
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(|e| RunnerError::Internal(format!("/dev/urandom unavailable: {e}")))?;
    Ok(hex::encode(buf))
}

fn partial_path(up: &Path, token: &str) -> PathBuf {
    up.join(format!("{PARTIAL_PREFIX}{token}"))
}

fn meta_path(up: &Path, token: &str) -> PathBuf {
    up.join(format!("{PARTIAL_PREFIX}{token}{META_SUFFIX}"))
}

fn remove_partial(up: &Path, token: &str) {
    let _ = std::fs::remove_file(partial_path(up, token));
    let _ = std::fs::remove_file(meta_path(up, token));
}

/// The token in a partial entry name (data or meta file), if it is one.
fn token_of(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(PARTIAL_PREFIX)?;
    let t = rest.strip_suffix(META_SUFFIX).unwrap_or(rest);
    valid_token(t).then_some(t)
}

/// Every well-formed open partial in `up` (data file plus readable meta).
fn list_partials(up: &Path) -> Vec<Partial> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(up) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(META_SUFFIX) {
            continue;
        }
        let Some(token) = token_of(&name) else {
            continue;
        };
        let Some(m) = read_meta(up, token) else {
            continue;
        };
        out.push(Partial {
            token: token.to_string(),
            size: m.size,
        });
    }
    out
}

fn read_meta(up: &Path, token: &str) -> Option<PartialMeta> {
    let bytes = std::fs::read(meta_path(up, token)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Remove every partial idle for longer than `ttl` (judged by the newest
/// mtime of its data and meta files). Never touches a stored file.
pub fn sweep_partials(up: &Path, ttl: Duration) {
    let Ok(entries) = std::fs::read_dir(up) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut newest: std::collections::BTreeMap<String, std::time::SystemTime> =
        std::collections::BTreeMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(token) = token_of(&name) else {
            continue;
        };
        let Ok(mtime) = std::fs::symlink_metadata(entry.path()).and_then(|m| m.modified()) else {
            continue;
        };
        let slot = newest.entry(token.to_string()).or_insert(mtime);
        if mtime > *slot {
            *slot = mtime;
        }
    }
    for (token, mtime) in newest {
        if now.duration_since(mtime).is_ok_and(|age| age > ttl) {
            remove_partial(up, &token);
        }
    }
}

fn quota_refusal() -> RunnerError {
    RunnerError::TooLarge(format!(
        "upload quota reached: at most {UPLOAD_MAX_FILES} files and {} MiB per session",
        UPLOAD_MAX_TOTAL_BYTES / (1024 * 1024)
    ))
}

/// Open a chunked upload: validate, sweep idle partials, check the quota
/// (stored files plus open partials' reservations), create the partial.
/// Returns the token. Reads no body.
pub fn begin_upload(
    sessions_dir: &Path,
    id: &str,
    name: &str,
    mime: &str,
    size: u64,
) -> Result<String, RunnerError> {
    check_target(sessions_dir, id)?;
    if size > FILE_MAX_BYTES {
        return Err(RunnerError::TooLarge(format!(
            "file exceeds {} MiB",
            FILE_MAX_BYTES / (1024 * 1024)
        )));
    }
    if name.len() > 255 || mime.len() > 255 || mime.chars().any(char::is_control) {
        return Err(RunnerError::BadRequest(
            "name or mime is too long or has control characters".to_string(),
        ));
    }
    let up = up_dir(sessions_dir, id);
    ensure_up_dir(&up)?;
    sweep_partials(&up, PARTIAL_TTL);
    let partials = list_partials(&up);
    if partials.len() >= MAX_OPEN_PARTIALS {
        return Err(RunnerError::TooLarge(format!(
            "at most {MAX_OPEN_PARTIALS} uploads may be open at once"
        )));
    }
    let (files, total) = usage(&up)?;
    let reserved: u64 = partials.iter().map(|p| p.size).sum();
    // The file-count limit is enforced at commit, AFTER the dedupe check
    // (the final name is unknown here, and re-sending a stored file at the
    // limit must still succeed); only the byte reservation is refused now.
    if files + partials.len() > UPLOAD_MAX_FILES || total + reserved + size > UPLOAD_MAX_TOTAL_BYTES
    {
        return Err(quota_refusal());
    }
    let token = mint_token()?;
    let meta = serde_json::to_vec(&PartialMeta {
        name: name.to_string(),
        mime: mime.to_string(),
        size,
    })
    .map_err(|e| RunnerError::Internal(e.to_string()))?;
    let create = |p: &Path| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(p)
    };
    let mut data = create(&partial_path(&up, &token))
        .map_err(|e| RunnerError::Io(format!("create upload: {e}")))?;
    let _ = data.flush();
    let wrote = create(&meta_path(&up, &token)).and_then(|mut f| f.write_all(&meta));
    if let Err(e) = wrote {
        remove_partial(&up, &token);
        return Err(RunnerError::Io(format!("create upload: {e}")));
    }
    Ok(token)
}

/// The cheap pre-body checks of an append: id, session, token, chunk cap,
/// offset against the bytes received so far, and the declared total. Run
/// before the body is read. Returns the partial's current length.
pub fn check_append(
    sessions_dir: &Path,
    id: &str,
    token: &str,
    offset: u64,
    len: u64,
) -> Result<u64, RunnerError> {
    check_target(sessions_dir, id)?;
    if !valid_token(token) {
        return Err(RunnerError::BadRequest("bad upload token".to_string()));
    }
    if len > CHUNK_MAX_BYTES {
        return Err(RunnerError::TooLarge(format!(
            "chunk exceeds {} MiB",
            CHUNK_MAX_BYTES / (1024 * 1024)
        )));
    }
    let up = up_dir(sessions_dir, id);
    let meta =
        read_meta(&up, token).ok_or_else(|| RunnerError::NotFound("no such upload".into()))?;
    let received = std::fs::symlink_metadata(partial_path(&up, token))
        .map_err(|_| RunnerError::NotFound("no such upload".to_string()))?
        .len();
    if offset != received {
        return Err(RunnerError::BadRequest(format!(
            "offset {offset} does not match the {received} bytes received"
        )));
    }
    if received + len > meta.size {
        return Err(RunnerError::BadRequest(
            "chunk runs past the declared size".to_string(),
        ));
    }
    Ok(received)
}

/// Append one complete chunk (blocking). Re-checks everything
/// [`check_append`] checks, then writes; a failed write is rolled back to
/// the previous length. Returns the bytes received so far.
pub fn append_chunk(
    sessions_dir: &Path,
    id: &str,
    token: &str,
    offset: u64,
    body: &[u8],
) -> Result<u64, RunnerError> {
    check_append(sessions_dir, id, token, offset, body.len() as u64)?;
    let path = partial_path(&up_dir(sessions_dir, id), token);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| RunnerError::Io(format!("append upload: {e}")))?;
    if let Err(e) = f.write_all(body).and_then(|_| f.flush()) {
        let _ = f.set_len(offset);
        return Err(RunnerError::Io(format!("append upload: {e}")));
    }
    Ok(offset + body.len() as u64)
}

/// Finish an upload (blocking): require every byte, hash, classify, name
/// and rename into place. A picture that disagrees with its declared type
/// or a quota refusal removes the partial; an incomplete upload keeps it.
pub fn commit_upload(sessions_dir: &Path, id: &str, token: &str) -> Result<Committed, RunnerError> {
    check_target(sessions_dir, id)?;
    if !valid_token(token) {
        return Err(RunnerError::BadRequest("bad upload token".to_string()));
    }
    let up = up_dir(sessions_dir, id);
    let meta =
        read_meta(&up, token).ok_or_else(|| RunnerError::NotFound("no such upload".into()))?;
    let part = partial_path(&up, token);
    let received = std::fs::symlink_metadata(&part)
        .map_err(|_| RunnerError::NotFound("no such upload".to_string()))?
        .len();
    if received != meta.size {
        return Err(RunnerError::BadRequest(format!(
            "upload incomplete: {received} of {} bytes received",
            meta.size
        )));
    }
    let finish = || -> Result<Committed, RunnerError> {
        let bytes =
            std::fs::read(&part).map_err(|e| RunnerError::Io(format!("read upload: {e}")))?;
        let kind = sniff_kind(&bytes);
        if meta.mime.starts_with("image/") {
            match kind {
                Kind::Image(m) if m == meta.mime => {}
                Kind::Image(m) => {
                    return Err(RunnerError::BadImage(format!(
                        "declared type {} does not match the picture ({m})",
                        meta.mime
                    )))
                }
                _ => {
                    return Err(RunnerError::BadImage(
                        "declared a picture but the bytes are not a PNG, JPEG, GIF or WebP"
                            .to_string(),
                    ))
                }
            }
        }
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let name = stored_name(&sha256, &meta.name, kind);
        let final_path = up.join(&name);
        let path_text = final_path.to_string_lossy().into_owned();
        if !final_path.is_absolute() || path_text.chars().any(char::is_whitespace) {
            return Err(RunnerError::Io(
                "upload path is not an absolute path without spaces".to_string(),
            ));
        }
        let mime = match kind {
            Kind::Image(m) => m.to_string(),
            _ if meta.mime.is_empty() => "application/octet-stream".to_string(),
            _ => meta.mime.clone(),
        };
        let done = Committed {
            path: final_path.clone(),
            bytes: bytes.len() as u64,
            mime,
            kind: kind.as_str(),
            sha256,
            name,
        };
        // Dedupe FIRST, before the quota.
        if std::fs::symlink_metadata(&final_path).is_ok_and(|m| m.is_file()) {
            return Ok(done);
        }
        // This partial's own reservation turns into the stored file; any
        // other open partial still holds its slot.
        let (files, total) = usage(&up)?;
        let others: Vec<Partial> = list_partials(&up)
            .into_iter()
            .filter(|p| p.token != token)
            .collect();
        let reserved: u64 = others.iter().map(|p| p.size).sum();
        if files >= UPLOAD_MAX_FILES || total + reserved + done.bytes > UPLOAD_MAX_TOTAL_BYTES {
            return Err(quota_refusal());
        }
        std::fs::rename(&part, &final_path)
            .map_err(|e| RunnerError::Io(format!("place upload: {e}")))?;
        Ok(done)
    };
    match finish() {
        Ok(c) => {
            remove_partial(&up, token);
            Ok(c)
        }
        Err(e @ (RunnerError::BadImage(_) | RunnerError::TooLarge(_))) => {
            remove_partial(&up, token);
            Err(e)
        }
        Err(e) => Err(e),
    }
}

/// Drop an open upload. Idempotent: an unknown token is a success. A bad
/// id, a missing session or a malformed token is still refused, so a
/// hostile string never reaches a path join.
pub fn abort_upload(sessions_dir: &Path, id: &str, token: &str) -> Result<(), RunnerError> {
    check_target(sessions_dir, id)?;
    if !valid_token(token) {
        return Err(RunnerError::BadRequest("bad upload token".to_string()));
    }
    remove_partial(&up_dir(sessions_dir, id), token);
    Ok(())
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

    #[test]
    fn usage_skips_temp_entries_without_statting_and_entries_that_vanish() {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path();
        std::fs::write(up.join(format!("{TMP_PREFIX}inflight")), b"xxxx").unwrap();
        std::fs::write(up.join("kept.png"), b"12345").unwrap();
        std::fs::write(up.join("vanishes.png"), b"123").unwrap();
        std::fs::write(up.join("broken.png"), b"1").unwrap();
        let stat = |p: &Path| -> std::io::Result<std::fs::Metadata> {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                !name.starts_with(TMP_PREFIX),
                "a temp entry must never be statted"
            );
            match name.as_str() {
                "vanishes.png" => Err(std::io::ErrorKind::NotFound.into()),
                "broken.png" => Err(std::io::ErrorKind::PermissionDenied.into()),
                _ => std::fs::symlink_metadata(p),
            }
        };
        // Any other stat error still fails.
        assert!(matches!(usage_with(up, stat), Err(RunnerError::Io(_))));
        std::fs::remove_file(up.join("broken.png")).unwrap();
        assert_eq!(usage_with(up, stat).unwrap(), (1, 5));
    }

    #[test]
    fn sniff_kind_table() {
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0];
        jpeg.extend([0x11; 16]);
        let mut gif = b"GIF89a".to_vec();
        gif.extend([0x22; 16]);
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.extend([0x33; 16]);
        assert_eq!(sniff_kind(&png(1)), Kind::Image("image/png"));
        assert_eq!(sniff_kind(&jpeg), Kind::Image("image/jpeg"));
        assert_eq!(sniff_kind(&gif), Kind::Image("image/gif"));
        assert_eq!(sniff_kind(&webp), Kind::Image("image/webp"));
        assert_eq!(sniff_kind(b"%PDF-1.7\n..."), Kind::Pdf);
        assert_eq!(sniff_kind(r#"{"a":"café é"}"#.as_bytes()), Kind::Text);
        assert_eq!(sniff_kind(b"text\0with nul"), Kind::Opaque);
        assert_eq!(sniff_kind(&[0xff, 0xfe, 0x80, 0x81, 0x82]), Kind::Opaque);
        assert_eq!(sniff_kind(b""), Kind::Opaque);
        // A multi-byte char cut by the 64 KiB window is still text.
        let mut long = vec![b'a'; TEXT_SNIFF_BYTES - 1];
        long.extend("é".as_bytes());
        long.extend(b"tail");
        assert_eq!(sniff_kind(&long), Kind::Text);
    }

    #[test]
    fn slug_name_table() {
        let s = |n: &str| slug_name(n);
        assert_eq!(s("../../etc/passwd"), ("passwd".to_string(), None));
        assert_eq!(
            s("Rep ort.PDF"),
            ("rep-ort".to_string(), Some("pdf".to_string()))
        );
        assert_eq!(
            s("résumé final.pdf"),
            ("r-sum-final".to_string(), Some("pdf".to_string()))
        );
        assert_eq!(s(".bashrc"), ("bashrc".to_string(), None));
        let long = "a".repeat(300);
        assert_eq!(s(&long).0.len(), STEM_MAX);
        assert_eq!(s("a.b.c.sh"), ("a-b-c".to_string(), Some("sh".to_string())));
        assert_eq!(s("x.verylongext1"), ("x".to_string(), None));
        assert_eq!(s(""), ("file".to_string(), None));
        assert_eq!(s("..."), ("file".to_string(), None));
        assert_eq!(
            s("a\0b\\c\\d.txt"),
            ("d".to_string(), Some("txt".to_string()))
        );
        for n in ["../x", "a b\tc.sh", "\u{0}.\u{0}", "..\\..\\x.y"] {
            let (stem, ext) = s(n);
            let all = format!("{stem}.{}", ext.unwrap_or_default());
            assert!(
                all.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.".contains(c)),
                "{n:?} -> {all:?}"
            );
            assert!(!all.starts_with('.') && !all.contains(".."), "{all:?}");
        }
    }
}
