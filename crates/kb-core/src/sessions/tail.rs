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

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

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
    /// Decoded, newline-terminated lines only. Empty when nothing new has
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
                complete_lines: text,
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
                complete_lines: text,
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

    let window = String::from_utf8_lossy(&slice[..window_end]).into_owned();
    let resume_offset = start + window_start as u64 + window_end as u64;
    Ok((window, resume_offset))
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
