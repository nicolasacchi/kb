//! Track V — read-only git bridge + prose/raw diff engine.
//!
//! Two concerns live here, both pure + side-effect-light so the daemon, the
//! CLI, and tests share one implementation:
//!
//! 1. **Git access** — `find_git_root` (walk up for `.git`), `git_log`
//!    (`--follow` history for one file), and `git_show` (a file's bytes at a
//!    revision). All shell out to the `git` binary via [`tokio::process`] —
//!    read-only, args passed as a vec (never a shell string), revisions
//!    validated as hex. Matches the `kb-server/build.rs` precedent and keeps
//!    the arrow-pinned dep graph free of `git2`/`gix`. Git being absent (no
//!    binary, not a repo, untracked file) degrades to an empty history, never
//!    an error — so the deployed container (no `.git`) falls back cleanly to
//!    index snapshots instead of surfacing a 500.
//!
//! 2. **Diffing** — [`diff_lines`] turns two texts into unified-diff hunks via
//!    the `similar` crate. The "focus on text, not HTML" requirement is met by
//!    feeding it [`crate::parser::text_blocks`] output (block-structured prose)
//!    for the text view; the raw view feeds the file bytes verbatim.

use serde::Serialize;
use similar::{ChangeTag, TextDiff};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Per-kb version-timeline source, parsed from `[kb.*] versions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VersionsMode {
    /// Git history when the file is tracked, else index snapshots — the
    /// default. Effectively per-file hybrid.
    #[default]
    Auto,
    /// Git commits + the working tree only.
    Git,
    /// kb index snapshots only (works without git).
    Index,
    /// Union of git commits + index snapshots, one timeline.
    Both,
    /// Feature disabled for this kb.
    Off,
}

impl VersionsMode {
    /// Parse a config string. Case-insensitive; `none`/`disabled` alias `off`.
    /// Returns `None` for an unrecognised value (the caller warns + defaults).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "git" => Some(Self::Git),
            "index" => Some(Self::Index),
            "both" => Some(Self::Both),
            "off" | "none" | "disabled" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Git => "git",
            Self::Index => "index",
            Self::Both => "both",
            Self::Off => "off",
        }
    }

    /// Does this mode ever read git history? (`auto`/`git`/`both`.)
    pub fn uses_git(&self) -> bool {
        matches!(self, Self::Auto | Self::Git | Self::Both)
    }

    /// Does this mode ever read index snapshots? (`auto`/`index`/`both`.)
    /// The snapshot-capture hook gates on this so a `git`-only kb stores
    /// nothing.
    pub fn uses_index(&self) -> bool {
        matches!(self, Self::Auto | Self::Index | Self::Both)
    }
}

/// One commit that touched the artifact's file, as `git log` returns them
/// (newest first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCommit {
    /// Full 40/64-hex commit hash — the revision passed back to [`git_show`].
    pub sha: String,
    /// Abbreviated hash for display.
    pub short_sha: String,
    pub author: String,
    /// Author timestamp, unix seconds.
    pub ts_unix: i64,
    /// First line of the commit message.
    pub subject: String,
}

/// Walk up from `start` looking for a `.git` entry (a dir for a normal repo,
/// a file for a worktree/submodule). Returns the repo root, or `None` when
/// the path isn't under git. Canonicalises `start` first so `..` segments and
/// symlinks resolve before the walk. Cheap + synchronous — the daemon calls
/// it once per kb at bring-up and memoises the result on `KbContext`.
///
/// The returned root may be an *ancestor* of the kb's source dir (a corpus is
/// often a subdirectory of a larger repo), so callers compute the file's path
/// relative to THIS root for `git log`/`git show`, not the corpus-relative
/// path.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// `git log --follow` for one file (path relative to `git_root`), parsed into
/// [`GitCommit`]s newest-first. Returns an empty vec — never an error — when
/// git is unavailable, the dir isn't a repo, or the file is untracked, so
/// callers degrade to index snapshots / an empty timeline instead of a 500.
pub async fn git_log(git_root: &Path, rel: &str) -> Vec<GitCommit> {
    // Unit Separator (0x1f) between fields, newline between records. Neither
    // appears in a commit subject's first line, so the split is unambiguous.
    const FMT: &str = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s";
    let output = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(["log", "--follow", FMT, "--"])
        .arg(rel)
        .output()
        .await;
    let output = match output {
        Ok(o) if o.status.success() => o,
        // Non-zero: not a repo, untracked path, or a repo with no commits.
        Ok(_) => return Vec::new(),
        Err(e) => {
            tracing::debug!(error = %e, "git log spawn failed; treating as no history");
            return Vec::new();
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();
    for line in stdout.lines() {
        let mut f = line.split('\u{1f}');
        if let (Some(sha), Some(short), Some(author), Some(at), Some(subject)) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        {
            commits.push(GitCommit {
                sha: sha.to_string(),
                short_sha: short.to_string(),
                author: author.to_string(),
                ts_unix: at.parse().unwrap_or(0),
                subject: subject.to_string(),
            });
        }
    }
    commits
}

/// Read the file's bytes at a specific commit via `git show <sha>:<rel>`.
/// `sha` must be hex (4–64 chars) — anything else is rejected so a caller
/// can't smuggle a revision expression or a `git` option. Returns `Err` when
/// the blob doesn't exist at that revision or git is unavailable (the caller
/// maps it to a problem+json on the specific diff request).
pub async fn git_show(git_root: &Path, sha: &str, rel: &str) -> Result<Vec<u8>, String> {
    if !is_hex_sha(sha) {
        return Err(format!("not a commit hash: {sha:?}"));
    }
    let spec = format!("{sha}:{rel}");
    let output = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .arg("show")
        .arg(&spec)
        .output()
        .await
        .map_err(|e| format!("git show: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git show {spec}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn is_hex_sha(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The URL of a git remote (default `origin`) at `git_root`, via
/// `git remote get-url <remote>`. `None` when git is unavailable, the dir isn't
/// a repo, or the remote doesn't exist — provenance is best-effort, never fatal
/// (session export records `null` rather than failing). Used to stamp a session
/// bundle's origin + to warn on a mismatch at rehydrate.
pub async fn git_remote_url(git_root: &Path, remote: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(["remote", "get-url", remote])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// The current `(branch, HEAD sha)` at `git_root` — `rev-parse --abbrev-ref
/// HEAD` + `rev-parse HEAD`. `branch` is `"HEAD"` on a detached checkout.
/// `None` when git is unavailable, the dir isn't a repo, or HEAD is unborn (a
/// fresh repo with no commit). Best-effort like [`git_remote_url`].
pub async fn git_head(git_root: &Path) -> Option<(String, String)> {
    let branch = git_rev(git_root, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let sha = git_rev(git_root, &["rev-parse", "HEAD"]).await?;
    Some((branch, sha))
}

/// Capture-time resolution of one commit sha a session's transcript detected
/// (`kb sessions capture`, W0.4). Best-effort — a missing/deleted repo, a sha
/// that's been rebased away, or a non-repo `cwd` all degrade to
/// `resolved: false` with every other field `None`/empty; this function
/// never returns an `Err`, so a capture with unresolvable git activity still
/// writes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedCommit {
    pub resolved: bool,
    /// Full 40/64-hex hash.
    pub sha_full: Option<String>,
    /// The `find_git_root` result the resolution ran against, when a repo was
    /// found at all (set even on an unresolvable sha, so the caller can tell
    /// "wrong sha" apart from "no repo").
    pub repo_root: Option<String>,
    /// The commit's TRUE subject (`%s`) — overwrites the transcript-guessed
    /// one when resolution succeeds.
    pub subject: Option<String>,
    /// `"Name <email>"` (`%an <%ae>`).
    pub author: Option<String>,
    pub parents: Option<u32>,
    /// Trailer lines verbatim (`"Key: value"`), in commit-message order —
    /// includes `Kb-Session` when W0.3's `prepare-commit-msg` hook stamped
    /// one (the exact session<->commit join source, invariant #11).
    pub trailers: Vec<String>,
}

/// Unit Separator between the fixed `%H%x1f%s%x1f%an <%ae>%x1f%P` fields,
/// Record Separator before the free-form (possibly multi-line) trailers
/// block — neither appears in a subject/author/trailer line, so the split is
/// unambiguous even when a trailer value is empty.
const RESOLVE_FMT: &str = "--format=%H%x1f%s%x1f%an <%ae>%x1f%P%x1e%(trailers:only,unfold)";

/// Resolve one detected commit sha via `git -C <cwd's repo root> show -s
/// <sha>` — ONE process per sha. `sha` need not be full (`git show` resolves
/// any unambiguous prefix); rejected outright when it isn't a plausible hex
/// token (`is_hex_sha`), so a caller can't smuggle a revision expression or a
/// `git` option through the transcript-detected string.
pub async fn resolve_commit(cwd: &Path, sha: &str) -> ResolvedCommit {
    if !is_hex_sha(sha) {
        return ResolvedCommit::default();
    }
    let Some(root) = find_git_root(cwd) else {
        return ResolvedCommit::default();
    };
    let root_str = root.to_string_lossy().to_string();
    let not_found = ResolvedCommit {
        repo_root: Some(root_str.clone()),
        ..Default::default()
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["show", "-s", RESOLVE_FMT])
        .arg(sha)
        .output()
        .await;
    let output = match output {
        Ok(o) if o.status.success() => o,
        // Non-zero: sha unresolvable (rebased away, ambiguous, never existed).
        Ok(_) => return not_found,
        Err(e) => {
            tracing::debug!(error = %e, sha, "git show spawn failed; treating as unresolved");
            return not_found;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut halves = stdout.splitn(2, '\u{1e}');
    let head = halves.next().unwrap_or_default();
    let trailers_block = halves.next().unwrap_or_default();
    let mut f = head.split('\u{1f}');
    let (Some(sha_full), Some(subject), Some(author), Some(parents_raw)) =
        (f.next(), f.next(), f.next(), f.next())
    else {
        return not_found;
    };
    let trailers: Vec<String> = trailers_block
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    ResolvedCommit {
        resolved: true,
        sha_full: Some(sha_full.to_string()),
        repo_root: Some(root_str),
        subject: (!subject.is_empty()).then(|| subject.to_string()),
        author: (!author.is_empty()).then(|| author.to_string()),
        parents: Some(parents_raw.split_whitespace().count() as u32),
        trailers,
    }
}

/// MI-W4.6 — one file a commit touched, plus a best-effort "has anything
/// touched it again since" signal for the provenance thread's staleness
/// badge ("source file last touched Nd ago").
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TouchedFile {
    /// Path relative to the repo root, exactly as `git show --name-only`
    /// printed it.
    pub path: String,
    /// Unix seconds of the most recent commit that touched `path`, at or
    /// after `sha` — either a LATER commit (when `changed_since` is
    /// `true`), or `sha`'s own commit time (when it's `false`). `None`
    /// when git couldn't answer (see [`commit_touched_files`]).
    pub last_touched_unix: Option<i64>,
    /// `true` when some commit strictly after `sha` also touched `path`
    /// (`git log <sha>..HEAD -- <path>` is non-empty). `None` — not
    /// `false` — when the answer is unknown (repo missing, sha
    /// unresolvable), so a caller never mistakes "couldn't check" for
    /// "confirmed unchanged".
    pub changed_since: Option<bool>,
}

/// Default cap on how many of a commit's touched files [`commit_touched_files`]
/// resolves staleness for — an on-demand provenance panel, not a list read on
/// every row, but still bounded against a commit that rewrote thousands of
/// files.
pub const TOUCHED_FILES_CAP: usize = 25;

/// The files `sha` touched (via `git show --name-only`), each annotated with
/// [`TouchedFile::changed_since`]/`last_touched_unix` (one extra `git log`
/// call per file, capped at `cap`). Returns `(files, truncated)` —
/// `truncated` is `true` when the commit touched more than `cap` files (only
/// the first `cap`, in `git show`'s own order, are resolved). Best-effort
/// throughout: an unresolvable `sha`, a missing/deleted repo, or git being
/// absent all degrade to `(Vec::new(), false)` rather than an error — this
/// backs a UI affordance, never a hard dependency.
pub async fn commit_touched_files(
    git_root: &Path,
    sha: &str,
    cap: usize,
) -> (Vec<TouchedFile>, bool) {
    if !is_hex_sha(sha) {
        return (Vec::new(), false);
    }
    // The commit's OWN time — used as `last_touched_unix` for a file nothing
    // has touched again since. `None` (repo/sha unresolvable) propagates to
    // every file's `changed_since`/`last_touched_unix` staying `None` below,
    // never a wrong-but-present timestamp.
    let commit_ts: Option<i64> = git_rev(git_root, &["show", "-s", "--format=%ct", sha])
        .await
        .and_then(|s| s.parse().ok());

    let names_output = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(["show", "--name-only", "--pretty=format:"])
        .arg(sha)
        .output()
        .await;
    let names_output = match names_output {
        Ok(o) if o.status.success() => o,
        _ => return (Vec::new(), false),
    };
    let paths: Vec<String> = String::from_utf8_lossy(&names_output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let truncated = paths.len() > cap;

    let mut files = Vec::with_capacity(paths.len().min(cap));
    for path in paths.into_iter().take(cap) {
        let range = format!("{sha}..HEAD");
        let since_output = Command::new("git")
            .arg("-C")
            .arg(git_root)
            .args(["log", "--format=%ct", &range, "--"])
            .arg(&path)
            .output()
            .await;
        let (last_touched_unix, changed_since) = match since_output {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                match stdout.lines().next() {
                    Some(first) => (first.parse::<i64>().ok(), Some(true)),
                    None => (commit_ts, Some(false)),
                }
            }
            _ => (None, None),
        };
        files.push(TouchedFile {
            path,
            last_touched_unix,
            changed_since,
        });
    }
    (files, truncated)
}

/// Run a read-only `git -C <root> <args…>` and return its trimmed stdout, or
/// `None` on any failure / empty output. Args are a vec, never a shell string.
async fn git_rev(git_root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(args)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Ensure a non-empty text ends with `\n` so `TextDiff::from_lines` treats its
/// final line the same whether or not more lines follow.
fn with_trailing_newline(s: &str) -> std::borrow::Cow<'_, str> {
    if s.is_empty() || s.ends_with('\n') {
        std::borrow::Cow::Borrowed(s)
    } else {
        std::borrow::Cow::Owned(format!("{s}\n"))
    }
}

// --- Diffing -----------------------------------------------------------------

/// Change kind for a single diff line.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffTag {
    /// Unchanged context line.
    Equal,
    /// Present only in the new text.
    Insert,
    /// Present only in the old text.
    Delete,
}

/// One line inside a [`DiffHunk`].
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    pub tag: DiffTag,
    /// 1-based line number in the OLD text (`None` for inserted lines).
    pub old_lineno: Option<usize>,
    /// 1-based line number in the NEW text (`None` for deleted lines).
    pub new_lineno: Option<usize>,
    pub text: String,
}

/// A contiguous run of changed lines plus a few lines of surrounding context,
/// like a unified-diff hunk.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffHunk {
    /// 1-based first OLD-text line number covered by this hunk.
    pub old_start: usize,
    /// 1-based first NEW-text line number covered by this hunk.
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

/// Unchanged context lines kept around each change.
const DIFF_CONTEXT: usize = 3;

/// Line-level diff of two texts, grouped into unified-diff hunks with
/// [`DIFF_CONTEXT`] lines of context. An **empty result means the texts are
/// identical** — which is exactly how a markup-only HTML change reads once
/// both sides are reduced to prose via [`crate::parser::text_blocks`].
pub fn diff_lines(old: &str, new: &str) -> Vec<DiffHunk> {
    // Normalise a trailing newline on both sides first: `from_lines` keeps
    // line endings, so a shorter text's last line ("x") would otherwise
    // mismatch the same line once it's mid-document ("x\n") — a spurious
    // delete+insert when a line is appended after it (caught dogfooding).
    let old_n = with_trailing_newline(old);
    let new_n = with_trailing_newline(new);
    let diff = TextDiff::from_lines(old_n.as_ref(), new_n.as_ref());
    let mut hunks = Vec::new();
    for group in diff.grouped_ops(DIFF_CONTEXT) {
        if group.is_empty() {
            continue;
        }
        let mut lines = Vec::new();
        let mut old_start = None;
        let mut new_start = None;
        for op in &group {
            for change in diff.iter_changes(op) {
                let old_i = change.old_index();
                let new_i = change.new_index();
                if old_start.is_none() {
                    old_start = old_i;
                }
                if new_start.is_none() {
                    new_start = new_i;
                }
                let tag = match change.tag() {
                    ChangeTag::Equal => DiffTag::Equal,
                    ChangeTag::Insert => DiffTag::Insert,
                    ChangeTag::Delete => DiffTag::Delete,
                };
                lines.push(DiffLine {
                    tag,
                    old_lineno: old_i.map(|i| i + 1),
                    new_lineno: new_i.map(|i| i + 1),
                    text: change.value().trim_end_matches(['\r', '\n']).to_string(),
                });
            }
        }
        hunks.push(DiffHunk {
            old_start: old_start.map(|i| i + 1).unwrap_or(1),
            new_start: new_start.map(|i| i + 1).unwrap_or(1),
            lines,
        });
    }
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::text_blocks;

    #[test]
    fn versions_mode_parse_and_roundtrip() {
        assert_eq!(VersionsMode::parse("auto"), Some(VersionsMode::Auto));
        assert_eq!(VersionsMode::parse("GIT"), Some(VersionsMode::Git));
        assert_eq!(VersionsMode::parse(" both "), Some(VersionsMode::Both));
        assert_eq!(VersionsMode::parse("index"), Some(VersionsMode::Index));
        assert_eq!(VersionsMode::parse("off"), Some(VersionsMode::Off));
        assert_eq!(VersionsMode::parse("none"), Some(VersionsMode::Off));
        assert_eq!(VersionsMode::parse("bogus"), None);
        assert_eq!(VersionsMode::default(), VersionsMode::Auto);
        assert_eq!(VersionsMode::Both.as_str(), "both");
        assert!(VersionsMode::Auto.uses_git() && VersionsMode::Auto.uses_index());
        assert!(VersionsMode::Git.uses_git() && !VersionsMode::Git.uses_index());
        assert!(!VersionsMode::Index.uses_git() && VersionsMode::Index.uses_index());
        assert!(!VersionsMode::Off.uses_git() && !VersionsMode::Off.uses_index());
    }

    #[test]
    fn is_hex_sha_accepts_only_hex() {
        assert!(is_hex_sha("9d3db99"));
        assert!(is_hex_sha(&"a".repeat(40)));
        assert!(!is_hex_sha("HEAD"));
        assert!(!is_hex_sha("9d3db99; rm -rf /"));
        assert!(!is_hex_sha("abc")); // too short (<4)
        assert!(!is_hex_sha(""));
    }

    #[test]
    fn find_git_root_walks_up_to_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        let sub = root.join("corpus/canon");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(find_git_root(&sub), Some(root.clone()));
        assert_eq!(find_git_root(&root), Some(root));
    }

    #[test]
    fn find_git_root_none_outside_repo() {
        let tmp = tempfile::tempdir().unwrap();
        // A tempdir under /tmp is not itself a repo; /tmp has no .git either.
        assert_eq!(find_git_root(tmp.path()), None);
    }

    #[test]
    fn markup_only_change_yields_empty_prose_diff() {
        // Same words, different tags/attributes/whitespace → identical prose
        // → no diff. This is the "focus on text, not HTML code" guarantee.
        let a = text_blocks("<body><p>Hello world</p></body>");
        let b = text_blocks(r#"<body><div class="wrap"><p>Hello   world</p></div></body>"#);
        assert_eq!(a, b);
        assert!(diff_lines(&a, &b).is_empty());
    }

    #[test]
    fn prose_change_yields_diff_lines() {
        let a = text_blocks("<body><p>Hello world</p><p>second line</p></body>");
        let b = text_blocks("<body><p>Hello there</p><p>second line</p></body>");
        let hunks = diff_lines(&a, &b);
        assert!(!hunks.is_empty());
        let lines: Vec<&DiffLine> = hunks.iter().flat_map(|h| &h.lines).collect();
        assert!(lines
            .iter()
            .any(|l| l.tag == DiffTag::Delete && l.text.contains("Hello world")));
        assert!(lines
            .iter()
            .any(|l| l.tag == DiffTag::Insert && l.text.contains("Hello there")));
        // The unchanged paragraph is carried as context.
        assert!(lines
            .iter()
            .any(|l| l.tag == DiffTag::Equal && l.text.contains("second line")));
    }

    #[test]
    fn diff_lines_pure_insert_from_empty() {
        let hunks = diff_lines("", "new line one\nnew line two");
        let inserts: usize = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.tag == DiffTag::Insert)
            .count();
        assert_eq!(inserts, 2);
    }

    #[test]
    fn appended_line_keeps_prior_line_as_context() {
        // Appending a line must not re-emit the previous last line as a
        // delete+insert — the trailing-newline normalisation. (Regression
        // for the dogfood finding.)
        let hunks = diff_lines("Title\nFirst para.", "Title\nFirst para.\nSecond para.");
        let lines: Vec<&DiffLine> = hunks.iter().flat_map(|h| &h.lines).collect();
        assert_eq!(
            lines.iter().filter(|l| l.tag == DiffTag::Delete).count(),
            0,
            "no deletes when only appending: {lines:?}"
        );
        let inserts: Vec<&&DiffLine> = lines.iter().filter(|l| l.tag == DiffTag::Insert).collect();
        assert_eq!(inserts.len(), 1);
        assert!(inserts[0].text.contains("Second para."));
        assert!(lines
            .iter()
            .any(|l| l.tag == DiffTag::Equal && l.text == "First para."));
    }

    // --- Git integration (uses the real `git` binary, present in dev/CI) ---

    fn run_git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=test@kb",
                "-c",
                "user.name=kb-test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    #[tokio::test]
    async fn git_log_and_show_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        run_git(root, &["init", "-q"]);

        let file = root.join("doc.html");
        std::fs::write(&file, "<body><p>Version one</p></body>").unwrap();
        run_git(root, &["add", "doc.html"]);
        run_git(root, &["commit", "-q", "-m", "first"]);

        std::fs::write(&file, "<body><p>Version two</p></body>").unwrap();
        run_git(root, &["add", "doc.html"]);
        run_git(root, &["commit", "-q", "-m", "second"]);

        let log = git_log(root, "doc.html").await;
        assert_eq!(log.len(), 2, "two commits touched the file");
        assert_eq!(log[0].subject, "second", "newest first");
        assert_eq!(log[1].subject, "first");
        assert_eq!(log[0].author, "kb-test");
        assert!(log[0].ts_unix > 0);

        let old_bytes = git_show(root, &log[1].sha, "doc.html").await.unwrap();
        assert_eq!(
            String::from_utf8_lossy(&old_bytes),
            "<body><p>Version one</p></body>"
        );

        // A prose diff between the two committed revisions shows the change.
        let old_prose = text_blocks(&String::from_utf8_lossy(&old_bytes));
        let new_prose = text_blocks("<body><p>Version two</p></body>");
        assert!(!diff_lines(&old_prose, &new_prose).is_empty());
    }

    #[tokio::test]
    async fn git_log_empty_for_non_repo() {
        let tmp = tempfile::tempdir().unwrap();
        // Not a git repo → empty history, not an error.
        assert!(git_log(tmp.path(), "whatever.html").await.is_empty());
    }

    #[tokio::test]
    async fn git_show_rejects_non_hex_revision() {
        let tmp = tempfile::tempdir().unwrap();
        let err = git_show(tmp.path(), "HEAD", "doc.html").await.unwrap_err();
        assert!(err.contains("not a commit hash"));
    }

    #[tokio::test]
    async fn git_head_and_remote_url_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        run_git(root, &["init", "-q"]);
        run_git(
            root,
            &["remote", "add", "origin", "https://example.com/x.git"],
        );
        // Unborn HEAD (no commit yet) → `rev-parse HEAD` fails → None.
        assert!(git_head(root).await.is_none());

        std::fs::write(root.join("f.txt"), "x").unwrap();
        run_git(root, &["add", "f.txt"]);
        run_git(root, &["commit", "-q", "-m", "first"]);

        let (branch, sha) = git_head(root).await.expect("head after a commit");
        assert!(!branch.is_empty(), "branch name resolved");
        assert!(is_hex_sha(&sha), "HEAD is a hex sha: {sha}");

        assert_eq!(
            git_remote_url(root, "origin").await.as_deref(),
            Some("https://example.com/x.git")
        );
        // A missing remote → None, not an error.
        assert!(git_remote_url(root, "upstream").await.is_none());
        // A non-repo dir → None for both.
        let empty = tempfile::tempdir().unwrap();
        assert!(git_head(empty.path()).await.is_none());
        assert!(git_remote_url(empty.path(), "origin").await.is_none());
    }

    // --- resolve_commit (W0.4) ---------------------------------------------

    #[tokio::test]
    async fn resolve_commit_recovers_full_sha_subject_author_parents_trailers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        run_git(&root, &["init", "-q"]);

        std::fs::write(root.join("a.txt"), "one").unwrap();
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "-q", "-m", "first"]);

        std::fs::write(root.join("a.txt"), "two").unwrap();
        run_git(&root, &["add", "a.txt"]);
        // A real Kb-Session trailer, exactly as W0.3's prepare-commit-msg
        // hook stamps it.
        run_git(
            &root,
            &[
                "commit",
                "-q",
                "-m",
                "feat: second\n\nKb-Session: abc-123-def",
            ],
        );

        let head = git_head(&root).await.unwrap().1;
        let short = &head[..7];

        let resolved = resolve_commit(&root, short).await;
        assert!(resolved.resolved);
        assert_eq!(resolved.sha_full.as_deref(), Some(head.as_str()));
        assert_eq!(resolved.subject.as_deref(), Some("feat: second"));
        assert_eq!(resolved.author.as_deref(), Some("kb-test <test@kb>"));
        assert_eq!(resolved.parents, Some(1));
        assert_eq!(
            resolved.trailers,
            vec!["Kb-Session: abc-123-def".to_string()]
        );
        assert_eq!(resolved.repo_root.as_deref(), Some(root.to_str().unwrap()));
    }

    #[tokio::test]
    async fn resolve_commit_root_has_no_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        run_git(&root, &["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one").unwrap();
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "-q", "-m", "root commit"]);
        let head = git_head(&root).await.unwrap().1;

        let resolved = resolve_commit(&root, &head).await;
        assert!(resolved.resolved);
        assert_eq!(resolved.parents, Some(0));
        assert!(
            resolved.trailers.is_empty(),
            "no trailers on a plain message"
        );
    }

    #[tokio::test]
    async fn resolve_commit_unresolvable_sha_in_a_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        run_git(&root, &["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one").unwrap();
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "-q", "-m", "only commit"]);

        // A well-formed but never-existed sha (as if the branch was rebased
        // and this commit no longer exists in the repo).
        let resolved = resolve_commit(&root, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef").await;
        assert!(!resolved.resolved);
        assert!(resolved.sha_full.is_none());
        assert!(resolved.subject.is_none());
        // The repo itself WAS found — distinguishable from "no repo at all".
        assert_eq!(resolved.repo_root.as_deref(), Some(root.to_str().unwrap()));
    }

    #[tokio::test]
    async fn resolve_commit_deleted_repo_dir_is_unresolved() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        run_git(&root, &["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one").unwrap();
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "-q", "-m", "only commit"]);
        let head = git_head(&root).await.unwrap().1;

        // The repo directory is gone by the time capture-time resolution
        // runs (e.g. a scratch worktree cleaned up between the session and
        // the capture).
        std::fs::remove_dir_all(&root).unwrap();

        let resolved = resolve_commit(&root, &head).await;
        assert!(!resolved.resolved);
        assert!(
            resolved.repo_root.is_none(),
            "no repo could be found at all"
        );
    }

    #[tokio::test]
    async fn resolve_commit_non_repo_cwd_is_unresolved() {
        let tmp = tempfile::tempdir().unwrap();
        let resolved = resolve_commit(tmp.path(), "abc1234").await;
        assert!(!resolved.resolved);
        assert!(resolved.repo_root.is_none());
    }

    #[tokio::test]
    async fn resolve_commit_rejects_non_hex_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        run_git(&root, &["init", "-q"]);
        // A shell-injection-shaped "sha" must never reach the git subprocess
        // as anything but a rejected argument.
        let resolved = resolve_commit(&root, "HEAD; rm -rf /").await;
        assert_eq!(resolved, ResolvedCommit::default());
    }

    // --- commit_touched_files (MI-W4.6) -------------------------------------

    #[tokio::test]
    async fn commit_touched_files_reports_changed_since_and_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        run_git(&root, &["init", "-q"]);

        std::fs::write(root.join("a.txt"), "one").unwrap();
        std::fs::write(root.join("b.txt"), "one").unwrap();
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "first"]);
        let first = git_head(&root).await.unwrap().1;

        // A later commit touches ONLY a.txt — b.txt is untouched since.
        std::fs::write(root.join("a.txt"), "two").unwrap();
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "-q", "-m", "second"]);

        let (files, truncated) = commit_touched_files(&root, &first, TOUCHED_FILES_CAP).await;
        assert!(!truncated);
        assert_eq!(files.len(), 2);
        let a = files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!(a.changed_since, Some(true));
        assert!(a.last_touched_unix.unwrap() > 0);
        let b = files.iter().find(|f| f.path == "b.txt").unwrap();
        assert_eq!(b.changed_since, Some(false));
        assert!(
            b.last_touched_unix.unwrap() > 0,
            "falls back to the commit's own time"
        );
    }

    #[tokio::test]
    async fn commit_touched_files_caps_and_reports_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        run_git(&root, &["init", "-q"]);
        for i in 0..5 {
            std::fs::write(root.join(format!("f{i}.txt")), "x").unwrap();
        }
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "many files"]);
        let sha = git_head(&root).await.unwrap().1;

        let (files, truncated) = commit_touched_files(&root, &sha, 2).await;
        assert_eq!(files.len(), 2, "capped to 2 even though 5 files changed");
        assert!(truncated);
    }

    #[tokio::test]
    async fn commit_touched_files_unresolvable_sha_degrades_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        run_git(&root, &["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one").unwrap();
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "-q", "-m", "only commit"]);

        let (files, truncated) = commit_touched_files(
            &root,
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            TOUCHED_FILES_CAP,
        )
        .await;
        assert!(files.is_empty());
        assert!(!truncated);
    }

    #[tokio::test]
    async fn commit_touched_files_rejects_non_hex_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        run_git(&root, &["init", "-q"]);
        let (files, truncated) =
            commit_touched_files(&root, "HEAD; rm -rf /", TOUCHED_FILES_CAP).await;
        assert!(files.is_empty());
        assert!(!truncated);
    }

    #[tokio::test]
    async fn commit_touched_files_non_repo_degrades_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let (files, truncated) =
            commit_touched_files(tmp.path(), &"a".repeat(40), TOUCHED_FILES_CAP).await;
        assert!(files.is_empty());
        assert!(!truncated);
    }
}
