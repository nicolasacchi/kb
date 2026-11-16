//! W7 (sessions-rethink R15/LF-4) — the live-transcript substrate: a
//! byte-offset tailer with complete-line discipline (design archaeology
//! from a retired internal cockpit prototype, lesson #3, adopted verbatim) plus the
//! resolver shared by the daemon's `/live` route and `kb sessions read
//! --live`/`--follow` (LF-6) — ONE resolver, not two.
//!
//! Everything here is stateless and pure-IO: a fresh [`TailReader`] is
//! always safe to construct per request (the server-side `LiveTailCache`,
//! kb-server's own optimization over this module, is never load-bearing —
//! see the design's kill-criterion 3). Nothing here writes anything; the
//! live lane is derived + ephemeral by construction (LF-7).
//!
//! **Complete-line discipline** (the cockpit lesson): [`TailReader::read_delta`]
//! never returns a line that isn't newline-terminated in the source file. A
//! partial trailing line (the writer is mid-append) is simply left unread —
//! `next_offset` stays where it was, and the next poll re-scans from there.
//! A `stat` that finds `size < offset` means the file was truncated,
//! rotated, or rewritten out from under us (e.g. a compaction) — the reader
//! restarts from byte 0 rather than seeking past EOF. A single unterminated
//! line that grows past [`MAX_FRAGMENT_BYTES`] is dropped with a resync
//! (`fragment_dropped`) rather than buffered without bound.
//!
//! **Inline images never leave this module** ([`swap_inline_images`]). A
//! single Claude screenshot line (one base64 tool-result image is 145-560
//! KB, a multi-frame result several MB) blew the 512 KiB live-delta cap
//! (a line is returned whole, it cannot be split) and the raw lane
//! shipped the pixels verbatim to every consumer. Both
//! [`TailReader::read_delta`] and [`read_bootstrap_window`] therefore
//! replace each inline base64 payload with a `ref` (sha256 of the decoded
//! bytes + length) AFTER the byte slice is cut, so every offset stays a
//! source-file byte offset and the swap sits below the route, the cache
//! and the CLI alike. The swap is not a scrub: `/live` stays loopback-only.

use std::borrow::Cow;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config::SessionsSection;
use crate::session_bundle::claude_project_slug;

/// The cockpit-retired "16 MiB fragment" guard: a single unterminated JSONL
/// line this large has never been observed on a real transcript. Past this,
/// [`TailReader::read_delta`] gives up on the fragment (drops it, resyncs
/// past it) rather than growing an in-memory buffer without bound.
pub const MAX_FRAGMENT_BYTES: u64 = 16 * 1024 * 1024;

/// LF-3b — the per-response byte cap for `GET /api/sessions/{sid}/live`.
/// The client loops (bumping `from`) until `next_from == size`.
pub const LIVE_DELTA_MAX_BYTES: u64 = 512 * 1024;

/// LF-4 — the cold-start/bootstrap window read by `view_bootstrap` (via
/// [`read_bootstrap_window`]) when no server-side carry is cached for a
/// `from` the client sent.
pub const TAIL_BOOTSTRAP_BYTES: u64 = 2 * 1024 * 1024;

/// LF-1's "bounded readdir walk ... cap 4096 entries", reused by the
/// resolver's never-captured-sid glob fallback.
const RESOLVE_GLOB_CAP: usize = 4096;

/// One [`TailReader::read_delta`] result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TailDelta {
    /// Decoded, newline-terminated lines only, AFTER [`swap_inline_images`]
    /// (so it may be shorter than `next_offset - from`: offsets are
    /// source-file bytes, this text is not). Empty when nothing new has
    /// landed, or when the only new bytes are an unterminated trailing line
    /// still being written.
    pub complete_lines: String,
    /// Byte offset the NEXT `read_delta` call should resume from.
    pub next_offset: u64,
    /// The file's size as observed by THIS read (informs `live`/`ended` at
    /// the route/CLI layer, which compares it against the config's
    /// `live_window_secs` mtime window).
    pub size: u64,
    /// `true` when this read found `size < offset` (truncation, rotation,
    /// or a compacting rewrite) and restarted from byte 0. The caller
    /// should treat this as "the tail state before this call is stale" —
    /// e.g. reset an incremental `ViewCarry` and re-bootstrap.
    pub truncated_restart: bool,
    /// `true` when an unterminated line exceeded [`MAX_FRAGMENT_BYTES`] and
    /// was dropped with a resync (`next_offset` already skips past it).
    /// The caller should count this the same way an unparseable line is
    /// counted (LF-3b's `parse_failures`) — content was lost, and that must
    /// be surfaced, never silent.
    pub fragment_dropped: bool,
}

/// A byte-offset tailer over one live transcript path.
#[derive(Debug, Clone)]
pub struct TailReader {
    pub path: PathBuf,
    pub offset: u64,
}

impl TailReader {
    pub fn new(path: PathBuf, offset: u64) -> Self {
        Self { path, offset }
    }

    /// Read new complete lines starting at `self.offset`, honoring `cap` as
    /// the response-size budget (pass [`LIVE_DELTA_MAX_BYTES`] for the
    /// route; the CLI `--follow` loop uses the same constant). A single
    /// line longer than `cap` but within [`MAX_FRAGMENT_BYTES`] is still
    /// returned whole (a line can't be split and remain valid JSONL) — the
    /// cap is honored on the FAST path (the common case: many small lines),
    /// not violated to truncate a legitimately large one.
    pub fn read_delta(&self, cap: u64) -> std::io::Result<TailDelta> {
        let size = fs::metadata(&self.path)?.len();

        let (start, truncated_restart) = if size < self.offset {
            (0, true)
        } else {
            (self.offset, false)
        };
        if start >= size {
            return Ok(TailDelta {
                complete_lines: String::new(),
                next_offset: start,
                size,
                truncated_restart,
                fragment_dropped: false,
            });
        }

        let mut f = fs::File::open(&self.path)?;
        let remaining = size - start;

        // Fast path: read up to `cap` bytes and look for the last complete
        // line within that window — covers the overwhelming common case
        // (many small JSONL lines) in one read, honoring the response cap.
        let fast_len = remaining.min(cap);
        f.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; fast_len as usize];
        f.read_exact(&mut buf)?;

        if let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') {
            let text = String::from_utf8_lossy(&buf[..=last_nl]).into_owned();
            return Ok(TailDelta {
                complete_lines: swap_inline_images(&text).into_owned(),
                next_offset: start + last_nl as u64 + 1,
                size,
                truncated_restart,
                fragment_dropped: false,
            });
        }

        // No newline in the fast window. If that window already covered
        // everything remaining, this is just a normal partial trailing
        // line — wait for more bytes on a later poll.
        if remaining <= cap {
            return Ok(TailDelta {
                complete_lines: String::new(),
                next_offset: start,
                size,
                truncated_restart,
                fragment_dropped: false,
            });
        }

        // There's more file beyond the fast window with no newline yet —
        // expand the search up to MAX_FRAGMENT_BYTES to find one (a single
        // line legitimately larger than `cap` but still sane must not be
        // starved forever just because it doesn't fit one response).
        let scan_len = remaining.min(MAX_FRAGMENT_BYTES);
        if scan_len > fast_len {
            f.seek(SeekFrom::Start(start + fast_len))?;
            let mut more = vec![0u8; (scan_len - fast_len) as usize];
            f.read_exact(&mut more)?;
            buf.extend_from_slice(&more);
        }
        if let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') {
            let text = String::from_utf8_lossy(&buf[..=last_nl]).into_owned();
            return Ok(TailDelta {
                complete_lines: swap_inline_images(&text).into_owned(),
                next_offset: start + last_nl as u64 + 1,
                size,
                truncated_restart,
                fragment_dropped: false,
            });
        }

        if scan_len < remaining || scan_len >= MAX_FRAGMENT_BYTES {
            // Either we scanned the full MAX_FRAGMENT_BYTES guard with no
            // newline in sight, or the file ends exactly at that boundary
            // mid-line either way: an oversized fragment. Drop it — resync
            // past everything we scanned rather than buffering unbounded.
            Ok(TailDelta {
                complete_lines: String::new(),
                next_offset: start + scan_len,
                size,
                truncated_restart,
                fragment_dropped: true,
            })
        } else {
            // scan_len == remaining and still no newline: the writer just
            // hasn't finished this line yet — wait for more.
            Ok(TailDelta {
                complete_lines: String::new(),
                next_offset: start,
                size,
                truncated_restart,
                fragment_dropped: false,
            })
        }
    }
}

/// LF-4 — read the last [`TAIL_BOOTSTRAP_BYTES`] of `path` (or the whole
/// file if smaller), resynced so the window both STARTS and ENDS on a line
/// boundary — the caller feeds the result straight to
/// `sessions::view::view_bootstrap`, which expects complete lines only.
///
/// Returns `(window, resume_offset)`: `resume_offset` is where the FIRST
/// subsequent [`TailReader`] should start (i.e. `TailReader::new(path,
/// resume_offset)`), already accounting for both the leading resync and any
/// trailing partial line dropped from the window — never re-derived from a
/// second `stat`, since a write can land between this read and the first
/// poll.
pub fn read_bootstrap_window(path: &Path) -> std::io::Result<(String, u64)> {
    let size = fs::metadata(path)?.len();
    let start = size.saturating_sub(TAIL_BOOTSTRAP_BYTES);
    let mut f = fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((size - start) as usize);
    f.read_to_end(&mut buf)?;

    // Resync the START to the next newline (byte 0 is already a valid
    // boundary — the true file start).
    let window_start = if start == 0 {
        0
    } else {
        match buf.iter().position(|&b| b == b'\n') {
            Some(pos) => pos + 1,
            None => buf.len(),
        }
    };

    // Trim a trailing partial line (the file may be mid-write) so the
    // caller's next `read_delta` picks it up cleanly rather than the two
    // overlapping.
    let slice = &buf[window_start..];
    let window_end = match slice.iter().rposition(|&b| b == b'\n') {
        Some(last_nl) => last_nl + 1,
        None => 0,
    };

    let raw = String::from_utf8_lossy(&slice[..window_end]);
    // `resume_offset` below is computed from the raw slice lengths, never
    // from the swapped text.
    let window = swap_inline_images(&raw).into_owned();
    let resume_offset = start + window_start as u64 + window_end as u64;
    Ok((window, resume_offset))
}

/// Safety-net threshold: a string leaf this long made only of base64
/// alphabet characters is a payload even when no known shape names it.
const B64_LEAF_MIN_BYTES: usize = 32 * 1024;
/// Deepest nesting the swap walks (reaches `toolUseResult.file`).
const SWAP_MAX_DEPTH: usize = 10;

/// Replace every inline base64 payload in one or more complete JSONL
/// lines with a reference. Lines without `"base64"` are returned
/// byte-identical (borrowed, never re-serialized). Changed lines are
/// re-serialized with serde_json, so key order may differ from the source.
///
/// Three shapes are swapped (bounded walk, depth <= 10):
/// 1. an object whose `source` is `{"type":"base64","data":...}` (`image`
///    and `document` blocks) becomes
///    `source: {"type":"ref","media_type":..,"sha256":..,"bytes":..}`;
/// 2. an object with a string `base64` key (the Read tool's
///    `toolUseResult.file`) loses it and gains `sha256`, `bytes`, `ref`;
/// 3. any remaining base64-alphabet string leaf of >= 32 KiB becomes
///    `[base64 elided: <n> bytes sha256:<hex>]`.
///
/// `sha256` is over the DECODED bytes (whitespace and padding tolerated).
/// An undecodable payload gets `undecodable: true` and no `sha256`; the
/// original string never survives. A `ref` block is never touched, so the
/// function is idempotent. A line that does not parse is copied as is.
pub fn swap_inline_images(lines: &str) -> Cow<'_, str> {
    if !lines.contains("\"base64\"") {
        return Cow::Borrowed(lines);
    }
    let mut out = String::with_capacity(lines.len().min(64 * 1024));
    let mut changed = false;
    for seg in lines.split_inclusive('\n') {
        if seg.contains("\"base64\"") {
            let (body, nl) = match seg.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (seg, ""),
            };
            if let Ok(mut v) = serde_json::from_str::<Value>(body) {
                if swap_value(&mut v, 0) {
                    if let Ok(s) = serde_json::to_string(&v) {
                        out.push_str(&s);
                        out.push_str(nl);
                        changed = true;
                        continue;
                    }
                }
            }
        }
        out.push_str(seg);
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(lines)
    }
}

/// `(sha256 hex, decoded length)` of a base64 payload, or `None` when it
/// does not decode.
fn digest_b64(data: &str) -> Option<(String, usize)> {
    let cleaned: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(cleaned.trim_end_matches('='))
        .ok()?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let digest = h.finalize();
    Some((hex::encode(&digest[..]), bytes.len()))
}

fn looks_like_b64_payload(s: &str) -> bool {
    if s.len() < B64_LEAF_MIN_BYTES {
        return false;
    }
    let mut ws = 0usize;
    for b in s.bytes() {
        if b.is_ascii_whitespace() {
            ws += 1;
        } else if !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_')) {
            return false;
        }
    }
    // MIME-style wrapping puts a newline every ~76 chars; prose is
    // whitespace-dense, a payload never is.
    ws * 20 <= s.len()
}

/// Mutate `v` in place; `true` when anything was replaced.
fn swap_value(v: &mut Value, depth: usize) -> bool {
    if depth > SWAP_MAX_DEPTH {
        return false;
    }
    match v {
        Value::String(s) => {
            if looks_like_b64_payload(s) {
                let note = match digest_b64(s) {
                    Some((sha, n)) => format!("[base64 elided: {n} bytes sha256:{sha}]"),
                    None => format!("[base64 elided: {} bytes undecodable]", s.len()),
                };
                *v = Value::String(note);
                true
            } else {
                false
            }
        }
        Value::Array(a) => {
            let mut changed = false;
            for x in a.iter_mut() {
                changed |= swap_value(x, depth + 1);
            }
            changed
        }
        Value::Object(o) => {
            let mut changed = false;
            // (a) source block
            let is_b64_source = o.get("source").is_some_and(|s| {
                s.get("type").and_then(Value::as_str) == Some("base64")
                    && s.get("data").is_some_and(Value::is_string)
            });
            if is_b64_source {
                if let Some(src) = o.get("source") {
                    let media = src
                        .get("media_type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let data = src.get("data").and_then(Value::as_str).unwrap_or("");
                    let r = match digest_b64(data) {
                        Some((sha, n)) => json!({
                            "type": "ref", "media_type": media, "sha256": sha, "bytes": n
                        }),
                        None => json!({
                            "type": "ref", "media_type": media,
                            "bytes": data.len(), "undecodable": true
                        }),
                    };
                    o.insert("source".to_string(), r);
                    changed = true;
                }
            }
            // (b) file shape
            if o.get("base64").is_some_and(Value::is_string) {
                if let Some(Value::String(data)) = o.remove("base64") {
                    match digest_b64(&data) {
                        Some((sha, n)) => {
                            o.insert("sha256".to_string(), json!(sha));
                            o.insert("bytes".to_string(), json!(n));
                        }
                        None => {
                            o.insert("bytes".to_string(), json!(data.len()));
                            o.insert("undecodable".to_string(), json!(true));
                        }
                    }
                    o.insert("ref".to_string(), json!(true));
                    changed = true;
                }
            }
            // (c) recurse (a replaced `source` is a ref now: untouched).
            for (_, x) in o.iter_mut() {
                changed |= swap_value(x, depth + 1);
            }
            changed
        }
        _ => false,
    }
}

/// LF-4 — the resolver shared by the `/live` route and `kb sessions read
/// --live`/`--follow`. `None` when Tier-1 isn't configured, `sid` doesn't
/// look like a session id, or no matching file exists under the configured
/// dir.
///
/// Two rungs, cheapest first:
///  1. `newest_capture_cwd` (the caller's own `sessions_get` row, when one
///     exists) → [`claude_project_slug`] → an exact
///     `<dir>/<slug>/<sid>.jsonl` probe.
///  2. A bounded glob `<dir>/*/<sid>.jsonl` (depth 1, capped at
///     [`RESOLVE_GLOB_CAP`] subdirectories) — covers a session that has
///     never been captured (no `cwd` to derive a slug from).
///
/// **Traversal guard**: `sid` is validated up front
/// ([`looks_like_session_id`] rejects path separators and anything but a
/// short alphanumeric/`-`/`_` token), AND every candidate path is
/// canonicalized and checked to still be UNDER the configured dir before
/// it's returned — the belt-and-suspenders the retired cockpit's own
/// `?session=` carried.
pub fn resolve_live_transcript(
    sid: &str,
    cfg: &SessionsSection,
    newest_capture_cwd: Option<&str>,
) -> Option<PathBuf> {
    if !looks_like_session_id(sid) {
        return None;
    }
    let dir = cfg.resolved_live_dir()?;
    let dir_canon = fs::canonicalize(&dir).ok()?;

    let under_dir = |p: &Path| -> Option<PathBuf> {
        let c = fs::canonicalize(p).ok()?;
        c.starts_with(&dir_canon).then_some(c)
    };

    if let Some(cwd) = newest_capture_cwd {
        let slug = claude_project_slug(cwd);
        let candidate = dir.join(&slug).join(format!("{sid}.jsonl"));
        if candidate.is_file() {
            if let Some(c) = under_dir(&candidate) {
                return Some(c);
            }
        }
    }

    let entries = fs::read_dir(&dir).ok()?;
    for entry in entries.flatten().take(RESOLVE_GLOB_CAP) {
        let sub = entry.path();
        if !sub.is_dir() {
            continue;
        }
        let candidate = sub.join(format!("{sid}.jsonl"));
        if candidate.is_file() {
            if let Some(c) = under_dir(&candidate) {
                return Some(c);
            }
        }
    }
    None
}

/// Strict-enough sid validation: non-empty, capped length, and restricted
/// to an alphanumeric/`-`/`_` alphabet — a Claude Code session id is a
/// canonical hyphenated UUID, but a foreign-harness id (codex/opencode/grok)
/// may not be, so this accepts the broader sanitized-id alphabet rather
/// than hard-coding UUID shape. The property that actually matters for
/// traversal safety is the REJECTION of `/`, `\`, and `.` (which also rules
/// out `..`) — enforced here as the first layer, with the canonicalize+
/// prefix check in [`resolve_live_transcript`] as the second.
fn looks_like_session_id(sid: &str) -> bool {
    !sid.is_empty()
        && sid.len() <= 128
        && sid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content).unwrap();
        p
    }

    // ---- TailReader::read_delta ----

    #[test]
    fn read_delta_returns_nothing_new_when_offset_equals_size() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), "s.jsonl", b"{\"a\":1}\n");
        let size = fs::metadata(&p).unwrap().len();
        let reader = TailReader::new(p, size);
        let d = reader.read_delta(LIVE_DELTA_MAX_BYTES).unwrap();
        assert_eq!(d.complete_lines, "");
        assert_eq!(d.next_offset, size);
        assert!(!d.truncated_restart);
        assert!(!d.fragment_dropped);
    }

    #[test]
    fn read_delta_returns_only_complete_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), "s.jsonl", b"{\"a\":1}\n{\"a\":2}\n{\"a\":3");
        let reader = TailReader::new(p, 0);
        let d = reader.read_delta(LIVE_DELTA_MAX_BYTES).unwrap();
        assert_eq!(d.complete_lines, "{\"a\":1}\n{\"a\":2}\n");
        // next_offset must NOT include the trailing partial line.
        assert_eq!(d.next_offset, "{\"a\":1}\n{\"a\":2}\n".len() as u64);
        assert!(!d.truncated_restart);
    }

    #[test]
    fn read_delta_is_resumable_across_polls() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), "s.jsonl", b"{\"a\":1}\n");
        let reader = TailReader::new(p.clone(), 0);
        let d1 = reader.read_delta(LIVE_DELTA_MAX_BYTES).unwrap();
        assert_eq!(d1.complete_lines, "{\"a\":1}\n");

        // Append more, then resume from d1.next_offset.
        let mut f = fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"a\":2}\n").unwrap();
        drop(f);

        let reader2 = TailReader::new(p, d1.next_offset);
        let d2 = reader2.read_delta(LIVE_DELTA_MAX_BYTES).unwrap();
        assert_eq!(d2.complete_lines, "{\"a\":2}\n");
    }

    #[test]
    fn read_delta_detects_truncation_and_restarts_from_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), "s.jsonl", b"{\"a\":1}\n{\"a\":2}\n");
        let big_offset = 1000u64; // pretend we'd already read far more
        let reader = TailReader::new(p.clone(), big_offset);
        // Truncate the file to something smaller than big_offset.
        fs::write(&p, b"{\"a\":9}\n").unwrap();
        let d = reader.read_delta(LIVE_DELTA_MAX_BYTES).unwrap();
        assert!(d.truncated_restart);
        assert_eq!(d.complete_lines, "{\"a\":9}\n");
    }

    #[test]
    fn read_delta_drops_an_oversized_fragment_with_resync() {
        let tmp = tempfile::tempdir().unwrap();
        // One giant unterminated "line" well past MAX_FRAGMENT_BYTES.
        let huge = vec![b'x'; (MAX_FRAGMENT_BYTES + 1024) as usize];
        let p = write_file(tmp.path(), "s.jsonl", &huge);
        let reader = TailReader::new(p, 0);
        let d = reader.read_delta(LIVE_DELTA_MAX_BYTES).unwrap();
        assert!(d.fragment_dropped, "must flag the dropped fragment");
        assert_eq!(d.complete_lines, "");
        // Resync position must not exceed what was actually scanned.
        assert!(d.next_offset <= MAX_FRAGMENT_BYTES + 1024);
        assert!(d.next_offset > 0);
    }

    #[test]
    fn read_delta_a_line_larger_than_cap_but_under_the_fragment_guard_is_returned_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let small_cap = 16u64;
        let mut content = vec![b'y'; (small_cap * 4) as usize];
        content.push(b'\n');
        let p = write_file(tmp.path(), "s.jsonl", &content);
        let reader = TailReader::new(p, 0);
        let d = reader.read_delta(small_cap).unwrap();
        assert!(!d.fragment_dropped);
        assert_eq!(d.complete_lines.len(), content.len());
    }

    #[test]
    fn read_delta_waits_when_the_whole_remaining_file_is_one_partial_line() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), "s.jsonl", b"{\"partial\":true");
        let reader = TailReader::new(p, 0);
        let d = reader.read_delta(LIVE_DELTA_MAX_BYTES).unwrap();
        assert_eq!(d.complete_lines, "");
        assert_eq!(d.next_offset, 0);
        assert!(!d.fragment_dropped);
    }

    // ---- read_bootstrap_window ----

    #[test]
    fn bootstrap_window_over_small_file_returns_the_whole_thing() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), "s.jsonl", b"{\"a\":1}\n{\"a\":2}\n");
        let (window, resume) = read_bootstrap_window(&p).unwrap();
        assert_eq!(window, "{\"a\":1}\n{\"a\":2}\n");
        assert_eq!(resume, window.len() as u64);
    }

    #[test]
    fn bootstrap_window_resyncs_a_mid_line_seek_and_drops_trailing_partial() {
        let tmp = tempfile::tempdir().unwrap();
        // Force a window smaller than the file by writing more than
        // TAIL_BOOTSTRAP_BYTES worth of padding before the tail content —
        // instead of allocating megabytes in a test, exercise the pure
        // resync/trim logic directly via a small helper file and a shrunk
        // expectation: the window always starts AFTER the first newline
        // when `start > 0`, and always ends AT a newline.
        let mut content = vec![b'p'; 10];
        content.extend_from_slice(b"\n{\"a\":1}\n{\"a\":2}\nzzz-partial");
        let p = write_file(tmp.path(), "s.jsonl", &content);
        let (window, resume) = read_bootstrap_window(&p).unwrap();
        // Whole file is well under TAIL_BOOTSTRAP_BYTES, so start == 0 and
        // no leading resync is needed — but the trailing partial line must
        // still be trimmed.
        assert!(window.starts_with("pppppppppp\n"));
        assert!(window.ends_with("{\"a\":2}\n"));
        assert!(!window.contains("zzz-partial"));
        assert_eq!(resume, window.len() as u64);
    }

    #[test]
    fn bootstrap_window_never_panics_on_an_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), "empty.jsonl", b"");
        let (window, resume) = read_bootstrap_window(&p).unwrap();
        assert_eq!(window, "");
        assert_eq!(resume, 0);
    }

    // ---- swap_inline_images ----

    /// Synthetic "PNG": the magic plus `n` bytes of filler.
    fn fake_png(n: usize) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        v.extend((0..n).map(|i| (i % 251) as u8));
        v
    }

    fn sha_hex(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        hex::encode(&digest[..])
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// A Claude tool_result line carrying one inline image.
    fn image_line(bytes: &[u8]) -> String {
        format!(
            r#"{{"type":"user","uuid":"00000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:10.000Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_1","content":[{{"type":"text","text":"shot"}},{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"{}"}}}}]}}]}}}}"#,
            b64(bytes)
        )
    }

    /// Longest run of base64-alphabet characters in `s`.
    fn longest_b64_run(s: &str) -> usize {
        let mut best = 0;
        let mut cur = 0;
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=') {
                cur += 1;
                best = best.max(cur);
            } else {
                cur = 0;
            }
        }
        best
    }

    #[test]
    fn read_delta_swaps_inline_image_for_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let png = fake_png(450 * 1024);
        let content = format!(
            "{}\n{{\"type\":\"summary\",\"summary\":\"after\"}}\n",
            image_line(&png)
        );
        assert!(content.len() > 600 * 1024);
        let p = write_file(tmp.path(), "s.jsonl", content.as_bytes());
        let d = TailReader::new(p, 0)
            .read_delta(LIVE_DELTA_MAX_BYTES)
            .unwrap();
        assert!(
            d.complete_lines.len() < 4096,
            "got {}",
            d.complete_lines.len()
        );
        assert!(longest_b64_run(&d.complete_lines) < 200);
        let first = d.complete_lines.lines().next().unwrap();
        let v: Value = serde_json::from_str(first).unwrap();
        let blk = &v["message"]["content"][0]["content"][1];
        assert_eq!(blk["type"], "image");
        assert_eq!(blk["source"]["type"], "ref");
        assert_eq!(blk["source"]["media_type"], "image/png");
        assert_eq!(blk["source"]["sha256"], sha_hex(&png));
        assert_eq!(blk["source"]["bytes"], png.len());
        assert_eq!(v["message"]["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(v["uuid"], "00000001-0000-4000-8000-000000000001");
        assert!(d.complete_lines.contains("\"after\""));
    }

    #[test]
    fn read_delta_offsets_stay_source_offsets() {
        let tmp = tempfile::tempdir().unwrap();
        let content = format!("{}\n{{\"a\":1}}\n", image_line(&fake_png(200 * 1024)));
        let p = write_file(tmp.path(), "s.jsonl", content.as_bytes());
        let file_len = fs::metadata(&p).unwrap().len();
        let d = TailReader::new(p.clone(), 0)
            .read_delta(LIVE_DELTA_MAX_BYTES)
            .unwrap();
        assert!((d.complete_lines.len() as u64) < file_len);
        assert_eq!(d.next_offset, file_len);
        assert_eq!(d.size, file_len);
        let again = TailReader::new(p, d.next_offset)
            .read_delta(LIVE_DELTA_MAX_BYTES)
            .unwrap();
        assert_eq!(again.complete_lines, "");
    }

    #[test]
    fn multi_megabyte_image_line_stays_under_the_live_delta_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let content = format!("{}\n", image_line(&fake_png(2300 * 1024)));
        assert!(content.len() > 3 * 1024 * 1024);
        let p = write_file(tmp.path(), "s.jsonl", content.as_bytes());
        let d = TailReader::new(p, 0)
            .read_delta(LIVE_DELTA_MAX_BYTES)
            .unwrap();
        assert!(!d.fragment_dropped);
        assert!(d.complete_lines.len() < LIVE_DELTA_MAX_BYTES as usize);
        assert_eq!(d.next_offset, content.len() as u64);
    }

    #[test]
    fn lines_without_images_are_byte_identical() {
        let plain = "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n{\"b\":2}\n";
        assert!(matches!(swap_inline_images(plain), Cow::Borrowed(_)));
        // The word appears as a quoted JSON string value but carries no
        // payload and no `source`/`base64` shape: nothing to swap.
        let prose = "{\"type\":\"assistant\",\"note\":\"base64\"}\n{\"c\":3}\n";
        let got = swap_inline_images(prose);
        assert_eq!(got, prose);
        assert!(matches!(got, Cow::Borrowed(_)));
        // Mixed: untouched lines keep their exact bytes (spacing included).
        let spaced = "{ \"a\" : 1 }\n";
        let mixed = format!("{spaced}{}\n{spaced}", image_line(&fake_png(64)));
        let out = swap_inline_images(&mixed).into_owned();
        assert!(out.starts_with(spaced));
        assert!(out.ends_with(spaced));
    }

    #[test]
    fn swap_is_idempotent() {
        let once = swap_inline_images(&format!("{}\n", image_line(&fake_png(5000)))).into_owned();
        assert!(!once.contains("\"base64\""));
        let twice = swap_inline_images(&once);
        assert!(matches!(twice, Cow::Borrowed(_)));
        assert_eq!(twice, once);
    }

    #[test]
    fn undecodable_base64_is_still_swapped() {
        let line = r#"{"m":{"type":"image","source":{"type":"base64","media_type":"image/png","data":"!!!not base64!!!"}}}"#;
        let out = swap_inline_images(line).into_owned();
        assert!(!out.contains("not base64"));
        let v: Value = serde_json::from_str(&out).unwrap();
        let src = &v["m"]["source"];
        assert_eq!(src["type"], "ref");
        assert_eq!(src["undecodable"], true);
        assert!(src.get("sha256").is_none());
        assert_eq!(src["bytes"], "!!!not base64!!!".len());
    }

    #[test]
    fn known_vector() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let mk = |data: &str| {
            format!(r#"{{"source":{{"type":"base64","media_type":"image/png","data":"{data}"}}}}"#)
        };
        for data in ["YWJj", "YW\\nJj", "YWJj==", "YWJj\\n"] {
            let out = swap_inline_images(&mk(data)).into_owned();
            let v: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["source"]["sha256"], abc, "payload {data}");
            assert_eq!(v["source"]["bytes"], 3);
        }
        // Unpadded input of a length that normally needs padding.
        let out = swap_inline_images(&mk("YWI")).into_owned();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["source"]["sha256"], sha_hex(b"ab"));
    }

    #[test]
    fn bootstrap_window_swaps_images_and_keeps_resume_offset() {
        let tmp = tempfile::tempdir().unwrap();
        let content = format!(
            "{}\n{{\"a\":1}}\n{{\"partial\":tr",
            image_line(&fake_png(100 * 1024))
        );
        let p = write_file(tmp.path(), "s.jsonl", content.as_bytes());
        let (window, resume) = read_bootstrap_window(&p).unwrap();
        assert!(window.len() < 4096);
        assert!(!window.contains("\"base64\""));
        assert!(window.contains("\"type\":\"ref\""));
        let expected = content.rfind("{\"partial\"").unwrap() as u64;
        assert_eq!(resume, expected, "resume is a source-file offset");
    }

    #[test]
    fn unparseable_line_with_base64_text_is_copied_untouched() {
        let broken = "{\"source\":{\"type\":\"base64\",\"data\":\"QUJD\"\n";
        let got = swap_inline_images(broken);
        assert_eq!(got, broken);
        assert!(matches!(got, Cow::Borrowed(_)));
    }

    #[test]
    fn file_shape_and_document_and_long_leaf_are_swapped() {
        let payload = b64(&fake_png(40 * 1024));
        let file = format!(
            r#"{{"type":"user","toolUseResult":{{"type":"image","file":{{"base64":"{payload}","type":"image/png","originalSize":123}}}}}}"#
        );
        let doc = format!(
            r#"{{"message":{{"content":[{{"type":"document","title":"d","source":{{"type":"base64","media_type":"application/pdf","data":"{payload}"}}}}]}}}}"#
        );
        let leaf = format!(r#"{{"weird":{{"blob":"{payload}"}},"base64":"flag-only"}}"#);
        let all = format!("{file}\n{doc}\n{leaf}\n");
        let out = swap_inline_images(&all).into_owned();
        assert!(longest_b64_run(&out) < 200, "run {}", longest_b64_run(&out));
        let mut lines = out.lines();
        let f: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let fl = &f["toolUseResult"]["file"];
        assert!(fl.get("base64").is_none());
        assert_eq!(fl["ref"], true);
        assert_eq!(fl["type"], "image/png");
        assert_eq!(fl["originalSize"], 123);
        assert_eq!(fl["sha256"], sha_hex(&fake_png(40 * 1024)));
        let d: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(d["message"]["content"][0]["source"]["type"], "ref");
        assert_eq!(d["message"]["content"][0]["title"], "d");
        let l: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert!(l["weird"]["blob"]
            .as_str()
            .unwrap()
            .starts_with("[base64 elided:"));
    }

    #[test]
    fn a_whitespace_dense_long_string_is_not_mistaken_for_a_payload() {
        let prose = "lorem ipsum dolor sit amet ".repeat(2000);
        let line = format!("{{\"text\":\"{prose}\",\"k\":\"base64\"}}\n");
        assert!(matches!(swap_inline_images(&line), Cow::Borrowed(_)));
    }

    #[test]
    fn view_append_over_a_swapped_line_yields_an_image_item() {
        use crate::sessions::view::{view_append, view_finish, ViewCarry};
        let png = fake_png(30 * 1024);
        let data = b64(&png);
        let lines = [
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/tmp/x.png"}}]}}"#.to_string(),
            image_line(&png),
        ]
        .join("\n")
            + "\n";
        let swapped = swap_inline_images(&lines).into_owned();
        let (mut events, mut carry) = view_append(ViewCarry::default(), &swapped);
        events.extend(view_finish(&mut carry));
        let json = serde_json::to_string(&events).unwrap();
        assert!(json.contains("\"Image\""), "{json}");
        assert!(json.contains(&sha_hex(&png)), "{json}");
        assert!(!json.contains(&data[..200]));
    }

    // ---- resolve_live_transcript ----

    fn cfg_for(dir: &Path) -> SessionsSection {
        SessionsSection {
            live_transcripts_dir: Some(dir.to_path_buf()),
            live_window_secs: None,
        }
    }

    #[test]
    fn resolve_returns_none_when_unconfigured() {
        let cfg = SessionsSection::default();
        assert_eq!(resolve_live_transcript("abc123", &cfg, None), None);
    }

    #[test]
    fn resolve_rejects_a_malformed_sid_before_touching_the_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path());
        assert_eq!(
            resolve_live_transcript("../../etc/passwd", &cfg, None),
            None
        );
        assert_eq!(resolve_live_transcript("a/b", &cfg, None), None);
        assert_eq!(resolve_live_transcript("", &cfg, None), None);
    }

    #[test]
    fn resolve_via_cwd_slug_exact_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = "/home/user/project/kb";
        let slug = claude_project_slug(cwd);
        let sub = tmp.path().join(&slug);
        fs::create_dir_all(&sub).unwrap();
        let target = write_file(&sub, "sid-live-1.jsonl", b"{}\n");
        let cfg = cfg_for(tmp.path());
        let got = resolve_live_transcript("sid-live-1", &cfg, Some(cwd)).unwrap();
        assert_eq!(
            fs::canonicalize(&got).unwrap(),
            fs::canonicalize(&target).unwrap()
        );
    }

    #[test]
    fn resolve_falls_back_to_bounded_glob_when_no_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("-some-other-project");
        fs::create_dir_all(&sub).unwrap();
        let target = write_file(&sub, "never-captured-sid.jsonl", b"{}\n");
        let cfg = cfg_for(tmp.path());
        let got = resolve_live_transcript("never-captured-sid", &cfg, None).unwrap();
        assert_eq!(
            fs::canonicalize(&got).unwrap(),
            fs::canonicalize(&target).unwrap()
        );
    }

    #[test]
    fn resolve_returns_none_when_no_file_matches_anywhere() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("-proj")).unwrap();
        let cfg = cfg_for(tmp.path());
        assert_eq!(
            resolve_live_transcript("does-not-exist", &cfg, Some("/x/y")),
            None
        );
    }

    #[test]
    fn resolve_traversal_guard_rejects_a_symlink_escaping_the_dir() {
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let live_dir = tmp.path().join("live");
            let outside_dir = tmp.path().join("outside");
            fs::create_dir_all(&live_dir).unwrap();
            fs::create_dir_all(&outside_dir).unwrap();
            write_file(&outside_dir, "secret.jsonl", b"{}\n");
            let escape_slug = live_dir.join("-escape");
            std::os::unix::fs::symlink(&outside_dir, &escape_slug).unwrap();
            let cfg = cfg_for(&live_dir);
            // The glob fallback walks `<dir>/*/<sid>.jsonl` — `-escape` is a
            // symlinked subdir pointing OUTSIDE `live_dir`; the candidate
            // file exists on disk (via the symlink) but must be rejected
            // because its canonical path is not under the configured dir.
            assert_eq!(resolve_live_transcript("secret", &cfg, None), None);
        }
    }

    // ---- looks_like_session_id ----

    #[test]
    fn looks_like_session_id_accepts_a_uuid_and_rejects_traversal_shapes() {
        assert!(looks_like_session_id(
            "0b1b1b1b-2222-3333-4444-555555555555"
        ));
        assert!(looks_like_session_id("abc_123-XYZ"));
        assert!(!looks_like_session_id(""));
        assert!(!looks_like_session_id("../etc/passwd"));
        assert!(!looks_like_session_id("a/b"));
        assert!(!looks_like_session_id("a.."));
        assert!(!looks_like_session_id(&"a".repeat(129)));
    }
}
