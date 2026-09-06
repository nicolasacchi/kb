//! Streamed `git blame --incremental` subprocess wrapper (ADR-4: shells out
//! rather than reimplementing git's own blame algorithm — the exact same
//! precedent `mirror::reconcile::run_git_diff` already established for
//! diff-shaped work) plus a from-scratch parser for the incremental wire
//! protocol.
//!
//! # Wire protocol
//!
//! Empirically verified against a real `git` (see this module's own
//! fixture-repo tests for captured transcripts). Each blamed region streams
//! as:
//!
//! ```text
//! <40-hex-sha> <orig-line> <final-line> <count>
//! [author <name>]
//! [author-mail <email>]
//! [author-time <unix>]
//! [author-tz <tz>]                (ignored — author-time is authoritative)
//! [committer ...]                 (ignored — never surfaced)
//! [summary <text>]
//! [previous <sha> <filename>]
//! [boundary]
//! filename <name>
//! ```
//!
//! The bracketed metadata lines appear ONLY the first time a commit sha is
//! seen in a given run — git never repeats them for a later region
//! attributed to the same commit. This parser caches that metadata
//! (author/author-mail/author-time/summary/previous/boundary — ALL of it,
//! since every one of those is a property of the COMMIT, not of the
//! specific line-group) the first time it sees a sha and reuses it for
//! every later region citing that sha, so every [`BlameRegion`] this module
//! emits carries full metadata regardless of whether git itself repeated it
//! on the wire. Every region block — first-sight or cache-hit — ends with a
//! `filename <name>` line; that is the ONE line this parser relies on to
//! decide "the block is complete."

use serde::Serialize;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

/// One attributed line-group from `git blame --incremental`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BlameRegion {
    pub sha: String,
    /// 1-based line number in the ORIGINAL (blamed-commit) version of the
    /// file.
    pub orig_start: u32,
    /// 1-based line number in the FINAL (requested) version of the file.
    pub final_start: u32,
    /// Number of consecutive lines this group covers, starting at
    /// `final_start`/`orig_start`.
    pub count: u32,
    pub author: String,
    pub author_mail: String,
    /// Unix seconds.
    pub author_time: i64,
    pub subject: String,
    /// The commit's own previous version of this file, if any — `None`
    /// exactly when `boundary` is `true` (a root commit, or a commit at a
    /// shallow/`--since` boundary, has no previous version for git to cite).
    pub previous_sha: Option<String>,
    pub previous_filename: Option<String>,
    /// The path this commit knew the file BY — differs from the queried
    /// path exactly when git followed a rename back through history.
    pub filename: String,
    /// `true` for a root commit (or a shallow/`--since` history boundary).
    pub boundary: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum IncrementalError {
    #[error("failed to spawn git blame: {0}")]
    Spawn(std::io::Error),
    #[error("git blame failed (exit {status}): {stderr}")]
    GitFailed { status: i32, stderr: String },
    #[error("failed to read git blame output: {0}")]
    Io(std::io::Error),
    #[error("git blame produced a malformed incremental header line: {line:?}")]
    MalformedHeader { line: String },
    #[error("git blame output ended mid-block (incomplete region at EOF)")]
    TruncatedOutput,
}

pub type Result<T> = std::result::Result<T, IncrementalError>;

/// Inputs to one `git blame --incremental` invocation.
pub struct BlameOptions<'a> {
    /// The repo root — invoked as `git -C <repo_root> ...`.
    pub repo_root: &'a Path,
    /// Repo-relative path (git pathspec, forward-slash).
    pub path: &'a str,
    /// Revision to blame at/from. `None` lets git default to the checked-out
    /// branch's HEAD (only meaningful when `contents` is also `None` — a
    /// caller wanting the dirty-working-tree-content case with a pinned
    /// history start should pass `Some("HEAD")` explicitly alongside
    /// `contents`).
    pub rev: Option<&'a str>,
    /// When `Some`, blame these EXACT bytes as the final image
    /// (`--contents -`, piped via stdin) instead of whatever git would
    /// itself read off the working tree or the ODB — the uncommitted-edit
    /// case (see `crate::blame`'s module doc).
    pub contents: Option<&'a [u8]>,
    /// Absolute path to a `blame.ignoreRevsFile`-style revision list
    /// (`--ignore-revs-file`).
    pub ignore_revs_file: Option<&'a Path>,
    /// Optional `-L start,end` (1-based, inclusive) — narrows the subprocess
    /// itself to that range rather than computing the whole file.
    pub line_range: Option<(u32, u32)>,
}

fn build_args(opts: &BlameOptions<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["blame".into(), "--incremental".into()];
    if let Some(f) = opts.ignore_revs_file {
        args.push("--ignore-revs-file".into());
        args.push(f.as_os_str().to_os_string());
    }
    if let Some((start, end)) = opts.line_range {
        args.push("-L".into());
        args.push(format!("{start},{end}").into());
    }
    if opts.contents.is_some() {
        args.push("--contents".into());
        args.push("-".into());
    }
    if let Some(rev) = opts.rev {
        args.push(rev.into());
    }
    args.push("--".into());
    args.push(opts.path.into());
    args
}

/// Run `git blame --incremental` per `opts`, invoking `on_region` as each
/// region completes — the streaming form. Blocking: spawns a real
/// subprocess and reads its stdout synchronously; an async caller MUST wrap
/// this in `spawn_blocking` (mirrors every other git-subprocess call in
/// this crate — see `mirror::reconcile`'s module doc, and note `GitRepo`
/// itself is `!Send`, so a caller inside `spawn_blocking` cannot share one
/// across the boundary — reopen fresh inside the blocking closure instead).
pub fn run_streaming(
    opts: &BlameOptions<'_>,
    mut on_region: impl FnMut(BlameRegion),
) -> Result<()> {
    let args = build_args(opts);
    let mut child = Command::new("git")
        .arg("-C")
        .arg(opts.repo_root)
        .args(&args)
        .stdin(if opts.contents.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(IncrementalError::Spawn)?;

    // Feed `--contents` bytes on a DEDICATED thread rather than inline here.
    // Writing directly before/while reading stdout risks a classic pipe
    // deadlock once the payload exceeds the OS pipe buffer (~64 KiB on
    // Linux): git can block trying to write blame metadata to stdout while
    // waiting on us to finish writing stdin, and we'd be blocked reading
    // stdout — neither side ever makes progress. A separate thread sidesteps
    // that regardless of payload size.
    let stdin_thread = opts.contents.map(|bytes| {
        let mut stdin = child
            .stdin
            .take()
            .expect("stdin piped when contents is Some");
        let bytes = bytes.to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
            // `stdin` drops here, closing the pipe — signals EOF to git.
        })
    });

    let stdout = child.stdout.take().expect("stdout always piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr always piped");
    let stderr_thread = thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        buf
    });

    let reader = BufReader::new(stdout);
    let mut parser = Parser::new();
    for line in reader.lines() {
        let line = line.map_err(IncrementalError::Io)?;
        if let Some(region) = parser.feed_line(&line)? {
            on_region(region);
        }
    }

    if let Some(t) = stdin_thread {
        let _ = t.join();
    }
    let stderr = stderr_thread.join().unwrap_or_default();
    let status = child.wait().map_err(IncrementalError::Io)?;
    if !status.success() {
        return Err(IncrementalError::GitFailed {
            status: status.code().unwrap_or(-1),
            stderr,
        });
    }
    if !parser.is_complete() {
        return Err(IncrementalError::TruncatedOutput);
    }
    Ok(())
}

/// Collect-all convenience — `crate::blame`'s v1 service (and the v1
/// `GET /api/blame` route) uses this rather than the streaming form; a
/// streaming SSE/chunked upgrade is the documented path if a very large
/// file's time-to-first-region ever matters (see `crate::blame`'s module
/// doc note on the v1 scope decision).
pub fn run_collect(opts: &BlameOptions<'_>) -> Result<Vec<BlameRegion>> {
    let mut out = Vec::new();
    run_streaming(opts, |r| out.push(r))?;
    Ok(out)
}

// --- the incremental-protocol parser (pure, no subprocess) -----------------

/// Per-commit metadata cached across a single parse run — see the module
/// doc for why `previous`/`boundary` are cached alongside author/summary
/// rather than treated as per-block fields.
#[derive(Debug, Clone, Default, PartialEq)]
struct CommitMeta {
    author: String,
    author_mail: String,
    author_time: i64,
    subject: String,
    previous_sha: Option<String>,
    previous_filename: Option<String>,
    boundary: bool,
}

#[derive(Debug)]
struct Partial {
    sha: String,
    orig_start: u32,
    final_start: u32,
    count: u32,
    /// `Some` once at least one metadata line has been seen THIS block
    /// (i.e. this is a first-sight block for `sha`); `None` for a
    /// cache-hit block (metadata lookup falls back to `Parser::metadata`).
    fresh: Option<CommitMeta>,
}

/// Streaming state machine over `git blame --incremental`'s line protocol.
/// Exposed (not just an implementation detail of [`run_streaming`]) so the
/// protocol logic itself is unit-testable against captured transcripts
/// without spawning a real `git` process.
#[derive(Debug, Default)]
pub struct Parser {
    metadata: HashMap<String, CommitMeta>,
    partial: Option<Partial>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when no block is mid-parse — i.e. the last line fed (if any)
    /// was a `filename` line, or nothing has been fed yet. `false` after
    /// EOF means the stream ended mid-block, a protocol violation.
    pub fn is_complete(&self) -> bool {
        self.partial.is_none()
    }

    /// Feed one line of `git blame --incremental` output (no trailing
    /// newline). Returns `Some(region)` exactly when this line completed a
    /// block (a `filename` line always terminates one — see the module
    /// doc), `None` otherwise.
    pub fn feed_line(&mut self, line: &str) -> Result<Option<BlameRegion>> {
        match self.partial.take() {
            None => {
                self.partial = Some(parse_header(line)?);
                Ok(None)
            }
            Some(mut p) => {
                if let Some(name) = line.strip_prefix("filename ") {
                    Ok(Some(self.finalize(p, name.to_string())))
                } else {
                    apply_kv_line(&mut p, line);
                    self.partial = Some(p);
                    Ok(None)
                }
            }
        }
    }

    fn finalize(&mut self, p: Partial, filename: String) -> BlameRegion {
        let meta = match p.fresh {
            Some(m) => {
                self.metadata.insert(p.sha.clone(), m.clone());
                m
            }
            None => self.metadata.get(&p.sha).cloned().unwrap_or_default(),
        };
        BlameRegion {
            sha: p.sha,
            orig_start: p.orig_start,
            final_start: p.final_start,
            count: p.count,
            author: meta.author,
            author_mail: meta.author_mail,
            author_time: meta.author_time,
            subject: meta.subject,
            previous_sha: meta.previous_sha,
            previous_filename: meta.previous_filename,
            filename,
            boundary: meta.boundary,
        }
    }
}

fn malformed(line: &str) -> IncrementalError {
    IncrementalError::MalformedHeader {
        line: line.to_string(),
    }
}

fn parse_header(line: &str) -> Result<Partial> {
    let mut parts = line.split_whitespace();
    let sha = parts.next().ok_or_else(|| malformed(line))?;
    if sha.is_empty() || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(malformed(line));
    }
    let orig_start: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| malformed(line))?;
    let final_start: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| malformed(line))?;
    let count: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| malformed(line))?;
    if parts.next().is_some() {
        return Err(malformed(line));
    }
    Ok(Partial {
        sha: sha.to_string(),
        orig_start,
        final_start,
        count,
        fresh: None,
    })
}

/// Apply one metadata (key-value, or bare `boundary`) line onto `p`'s
/// in-progress `fresh` metadata. Unknown keys (`author-tz`, `committer*`,
/// and anything a future git version adds) are silently ignored —
/// forward-compatible by design, not an oversight.
fn apply_kv_line(p: &mut Partial, line: &str) {
    let m = p.fresh.get_or_insert_with(CommitMeta::default);
    if let Some(v) = line.strip_prefix("author-mail ") {
        m.author_mail = v.trim_matches(|c| c == '<' || c == '>').to_string();
    } else if let Some(v) = line.strip_prefix("author-time ") {
        m.author_time = v.trim().parse().unwrap_or(0);
    } else if let Some(v) = line.strip_prefix("author ") {
        m.author = v.to_string();
    } else if let Some(v) = line.strip_prefix("summary ") {
        m.subject = v.to_string();
    } else if line == "summary" {
        m.subject = String::new();
    } else if let Some(rest) = line.strip_prefix("previous ") {
        if let Some((sha, name)) = rest.split_once(' ') {
            m.previous_sha = Some(sha.to_string());
            m.previous_filename = Some(name.to_string());
        }
    } else if line == "boundary" {
        m.boundary = true;
    }
    // else: author-tz / committer / committer-mail / committer-time /
    // committer-tz / any future unknown key — deliberately ignored.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path as StdPath;

    // --- pure parser tests (no subprocess) ---------------------------------

    fn feed_all(parser: &mut Parser, transcript: &str) -> Vec<BlameRegion> {
        let mut out = Vec::new();
        for line in transcript.lines() {
            if let Some(r) = parser.feed_line(line).expect("valid transcript") {
                out.push(r);
            }
        }
        out
    }

    /// A hand-captured transcript shape (mirrors a real 3-commit,
    /// 2-author run): sha A first-sight+boundary, sha B first-sight, then a
    /// SECOND region citing sha A again with ONLY a `filename` line (the
    /// cache-hit shape git itself produces).
    const TRANSCRIPT: &str = "\
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 2 2 2
author Bob
author-mail <bob@example.com>
author-time 1700000200
author-tz +0000
committer Bob
committer-mail <bob@example.com>
committer-time 1700000200
committer-tz +0000
summary bob's change
previous aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa f.txt
filename f.txt
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1 1
author Alice
author-mail <alice@example.com>
author-time 1700000100
author-tz +0000
committer Alice
committer-mail <alice@example.com>
committer-time 1700000100
committer-tz +0000
summary alice's initial commit
boundary
filename f.txt
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 3 4 1
filename f.txt
";

    #[test]
    fn parses_first_sight_metadata_and_reuses_it_on_a_cache_hit_block() {
        let mut parser = Parser::new();
        let regions = feed_all(&mut parser, TRANSCRIPT);
        assert_eq!(regions.len(), 3);

        assert_eq!(regions[0].sha, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(regions[0].author, "Bob");
        assert_eq!(regions[0].author_mail, "bob@example.com");
        assert_eq!(regions[0].author_time, 1700000200);
        assert_eq!(regions[0].subject, "bob's change");
        assert_eq!(
            regions[0].previous_sha.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(regions[0].previous_filename.as_deref(), Some("f.txt"));
        assert!(!regions[0].boundary);

        assert_eq!(regions[1].sha, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(regions[1].author, "Alice");
        assert!(regions[1].boundary);
        assert_eq!(regions[1].previous_sha, None);

        // The THIRD region cites sha A again with ONLY `filename` on the
        // wire — the parser must still report Alice's full metadata,
        // including `boundary`, propagated from the cache.
        assert_eq!(regions[2].sha, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(regions[2].author, "Alice");
        assert_eq!(regions[2].author_mail, "alice@example.com");
        assert!(
            regions[2].boundary,
            "boundary is a commit-level property — must be cached and reused, \
             not dropped on a cache-hit block"
        );
        assert_eq!(regions[2].final_start, 4);
        assert_eq!(regions[2].orig_start, 3);
        assert_eq!(regions[2].count, 1);
    }

    #[test]
    fn is_complete_tracks_mid_block_state() {
        let mut parser = Parser::new();
        assert!(parser.is_complete());
        parser
            .feed_line("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1 1")
            .unwrap();
        assert!(!parser.is_complete(), "mid-block after only a header line");
        parser.feed_line("author Alice").unwrap();
        assert!(!parser.is_complete());
        parser.feed_line("filename f.txt").unwrap();
        assert!(parser.is_complete(), "filename always terminates a block");
    }

    #[test]
    fn malformed_header_line_is_an_error_not_a_panic() {
        let mut parser = Parser::new();
        let err = parser.feed_line("not a valid header line").unwrap_err();
        assert!(matches!(err, IncrementalError::MalformedHeader { .. }));

        let mut parser2 = Parser::new();
        let err2 = parser2
            .feed_line("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1") // missing 4th token
            .unwrap_err();
        assert!(matches!(err2, IncrementalError::MalformedHeader { .. }));
    }

    #[test]
    fn unknown_metadata_keys_are_ignored_forward_compatibly() {
        let mut parser = Parser::new();
        let region = feed_all(
            &mut parser,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1 1\n\
             author Alice\n\
             author-tz +0000\n\
             committer Alice\n\
             committer-mail <alice@example.com>\n\
             committer-time 1700000100\n\
             committer-tz +0000\n\
             a-brand-new-future-key some value\n\
             filename f.txt\n",
        );
        assert_eq!(region.len(), 1);
        assert_eq!(region[0].author, "Alice");
    }

    #[test]
    fn empty_input_yields_no_regions() {
        let mut parser = Parser::new();
        assert_eq!(feed_all(&mut parser, ""), Vec::new());
        assert!(parser.is_complete());
    }

    // --- real fixture-repo tests (spawns real `git`) ------------------------

    fn git(dir: &StdPath, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?} failed: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_out(dir: &StdPath, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?} failed: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn commit_as(dir: &StdPath, name: &str, email: &str, msg: &str) {
        git(dir, &["config", "user.name", name]);
        git(dir, &["config", "user.email", email]);
        git(dir, &["commit", "-q", "-m", msg]);
    }

    /// Three commits, three authors, matching this module's own manual
    /// verification transcript: c1 (Alice, 3 lines), c2 (Bob, edits line 2 +
    /// inserts a line), c3 (Carol, appends two lines).
    fn multi_author_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);

        std::fs::write(dir.join("f.txt"), "line1\nline2\nline3\n").unwrap();
        git(dir, &["add", "-A"]);
        commit_as(dir, "Alice", "alice@example.com", "c1: alice adds 3 lines");

        std::fs::write(
            dir.join("f.txt"),
            "line1\nline2-changed\nline2b-new\nline3\n",
        )
        .unwrap();
        git(dir, &["add", "-A"]);
        commit_as(dir, "Bob", "bob@example.com", "c2: bob edits + inserts");

        std::fs::write(
            dir.join("f.txt"),
            "line1\nline2-changed\nline2b-new\nline3\nline4\nline5\n",
        )
        .unwrap();
        git(dir, &["add", "-A"]);
        commit_as(dir, "Carol", "carol@example.com", "c3: carol appends");

        tmp
    }

    #[test]
    fn multi_region_file_with_three_authors_and_commits() {
        let repo = multi_author_repo();
        let opts = BlameOptions {
            repo_root: repo.path(),
            path: "f.txt",
            rev: Some("HEAD"),
            contents: None,
            ignore_revs_file: None,
            line_range: None,
        };
        let regions = run_collect(&opts).unwrap();

        let authors: Vec<&str> = regions.iter().map(|r| r.author.as_str()).collect();
        assert!(authors.contains(&"Alice"));
        assert!(authors.contains(&"Bob"));
        assert!(authors.contains(&"Carol"));

        // Every line of the 6-line final file is covered exactly once.
        let mut covered: Vec<u32> = regions
            .iter()
            .flat_map(|r| r.final_start..r.final_start + r.count)
            .collect();
        covered.sort_unstable();
        assert_eq!(covered, vec![1, 2, 3, 4, 5, 6]);

        // The root commit (Alice's) is the boundary.
        let alice_region = regions.iter().find(|r| r.author == "Alice").unwrap();
        assert!(alice_region.boundary);
        let carol_region = regions.iter().find(|r| r.author == "Carol").unwrap();
        assert!(!carol_region.boundary);
        assert!(carol_region.previous_sha.is_some());
    }

    #[test]
    fn line_range_narrows_to_exactly_the_requested_group() {
        let repo = multi_author_repo();
        let opts = BlameOptions {
            repo_root: repo.path(),
            path: "f.txt",
            rev: Some("HEAD"),
            contents: None,
            ignore_revs_file: None,
            line_range: Some((2, 3)),
        };
        let regions = run_collect(&opts).unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].author, "Bob");
        assert_eq!(regions[0].final_start, 2);
        assert_eq!(regions[0].count, 2);
    }

    #[test]
    fn renamed_file_previous_line_and_filename_reflect_the_rename() {
        let repo = multi_author_repo();
        let dir = repo.path();
        git(dir, &["mv", "f.txt", "g.txt"]);
        commit_as(dir, "Dave", "dave@example.com", "c4: rename f.txt to g.txt");

        let opts = BlameOptions {
            repo_root: dir,
            path: "g.txt",
            rev: Some("HEAD"),
            contents: None,
            ignore_revs_file: None,
            line_range: None,
        };
        let regions = run_collect(&opts).unwrap();
        assert!(!regions.is_empty());
        // git blame follows the rename by default (no --follow needed) — the
        // pre-rename regions' own `filename` still cites the OLD name, since
        // that's what the file was called AT that commit.
        assert!(
            regions.iter().any(|r| r.filename == "f.txt"),
            "expected at least one pre-rename region citing the old filename: {regions:?}"
        );
        // No region is attributed to Dave — the rename-only commit touched
        // no line content.
        assert!(regions.iter().all(|r| r.author != "Dave"));
    }

    #[test]
    fn ignore_revs_file_makes_a_listed_reformat_commit_vanish_from_blame() {
        let repo = multi_author_repo();
        let dir = repo.path();

        // A "reformat" commit: every line gets a trailing space, same author
        // as nobody in particular — Eve.
        std::fs::write(
            dir.join("f.txt"),
            "line1 \nline2-changed \nline2b-new \nline3 \nline4 \nline5 \n",
        )
        .unwrap();
        git(dir, &["add", "-A"]);
        commit_as(
            dir,
            "Eve",
            "eve@example.com",
            "c4: eve reformat (trailing ws)",
        );
        let reformat_sha = git_out(dir, &["rev-parse", "HEAD"]);

        let ignore_file = dir.join(".git-blame-ignore-revs");
        std::fs::write(&ignore_file, format!("{reformat_sha}\n")).unwrap();

        // WITHOUT the ignore-revs-file: Eve owns every line.
        let without = run_collect(&BlameOptions {
            repo_root: dir,
            path: "f.txt",
            rev: Some("HEAD"),
            contents: None,
            ignore_revs_file: None,
            line_range: None,
        })
        .unwrap();
        assert!(without.iter().all(|r| r.author == "Eve"));

        // WITH it: Eve's reformat commit is skipped entirely — every line
        // falls back to whoever last touched its actual content.
        let with = run_collect(&BlameOptions {
            repo_root: dir,
            path: "f.txt",
            rev: Some("HEAD"),
            contents: None,
            ignore_revs_file: Some(&ignore_file),
            line_range: None,
        })
        .unwrap();
        assert!(
            with.iter().all(|r| r.author != "Eve"),
            "the reformat commit must vanish from blame when listed: {with:?}"
        );
        let authors: Vec<&str> = with.iter().map(|r| r.author.as_str()).collect();
        assert!(authors.contains(&"Alice"));
        assert!(authors.contains(&"Bob"));
        assert!(authors.contains(&"Carol"));
    }

    #[test]
    fn contents_option_blames_fed_bytes_instead_of_the_committed_blob() {
        let repo = multi_author_repo();
        let dir = repo.path();
        let head = git_out(dir, &["rev-parse", "HEAD"]);

        // Bytes that were never committed — an extra uncommitted line.
        let live = b"line1\nline2-changed\nline2b-new\nline3\nline4\nline5\nline6-uncommitted\n";
        let opts = BlameOptions {
            repo_root: dir,
            path: "f.txt",
            rev: Some(head.as_str()),
            contents: Some(live),
            ignore_revs_file: None,
            line_range: None,
        };
        let regions = run_collect(&opts).unwrap();

        // The extra final line is attributed to the sentinel "not committed"
        // sha (all zeroes) — git's own convention for --contents/dirty
        // blame, proven directly rather than assumed.
        let last = regions
            .iter()
            .find(|r| r.final_start <= 7 && r.final_start + r.count > 7)
            .expect("line 7 must be covered");
        assert_eq!(last.sha, "0".repeat(40));

        // Every earlier line is still attributed to a real historical
        // author, unaffected by the uncommitted tail.
        assert!(regions.iter().any(|r| r.author == "Alice"));
    }

    #[test]
    fn nonexistent_path_reports_a_git_failed_error() {
        let repo = multi_author_repo();
        let opts = BlameOptions {
            repo_root: repo.path(),
            path: "does-not-exist.txt",
            rev: Some("HEAD"),
            contents: None,
            ignore_revs_file: None,
            line_range: None,
        };
        let err = run_collect(&opts).unwrap_err();
        match err {
            IncrementalError::GitFailed { status, stderr } => {
                assert_ne!(status, 0);
                assert!(
                    stderr.contains("no such path") || stderr.contains("does not exist"),
                    "got stderr: {stderr}"
                );
            }
            other => panic!("expected GitFailed, got: {other:?}"),
        }
    }

    #[test]
    fn streaming_form_invokes_the_callback_once_per_region() {
        let repo = multi_author_repo();
        let opts = BlameOptions {
            repo_root: repo.path(),
            path: "f.txt",
            rev: Some("HEAD"),
            contents: None,
            ignore_revs_file: None,
            line_range: None,
        };
        let mut count = 0usize;
        run_streaming(&opts, |_region| count += 1).unwrap();
        assert!(count >= 3, "expected at least 3 regions, got {count}");

        let collected = run_collect(&opts).unwrap();
        assert_eq!(count, collected.len());
    }
}
