//! DCB W2.A — the `<meta name="kb-code-rev">` REVERSE line map.
//!
//! ## What this is
//!
//! W1.C's line predicate answers "is a token that looks like what the prose
//! quoted unique near the cited line?" — a *lookalike* argument. When a
//! document declares which commit its line numbers were counted against
//! (`<meta name="kb-code-rev" content="<label>@<sha>">`), a strictly stronger
//! argument is available: ask **git** where that exact line went. That is
//! this module.
//!
//! ## Why `git diff --unified=0`, and NOT `git blame --reverse` (E1)
//!
//! `--reverse`'s porcelain header swaps the meaning of the
//! `<sourceline> <resultline>` pair, so it would need a second, subtly
//! different parser standing next to `blame::incremental`'s existing one —
//! and this crate has no `--reverse` machinery to build on at all
//! (`grep -rn -- "--reverse" crates/` was 0 hits when W2.A was specced). A
//! `-U0` hunk header is a two-integer-pair grammar with a trivially goldenable
//! parser and no new protocol surface.
//!
//! It is also strictly MORE capable here: `git diff <commit> -- <path>`
//! compares the commit to the **working tree**, which is exactly what the lens
//! links into (`routes::read_repo_file` with `rev = None`), so ONE invocation
//! per path covers committed drift *and* the operator's current uncommitted
//! edits. `<sha>..HEAD` would silently ignore the latter.
//!
//! ## What is refused, and why (the skip vocabulary)
//!
//! | condition | outcome | why |
//! |---|---|---|
//! | no `kb-code-rev` | `Skipped` / `no_doc_rev` | nothing to anchor to |
//! | `+dirty` | `Skipped` / `doc_rev_dirty` (E2) | the cited numbers were counted against an uncommitted tree **that no sha reproduces**; `<sha>` is therefore NOT their basis, and diffing from it would mint a confidently wrong map — the exact failure class the compound vocabulary exists to prevent |
//! | label names another repo | `Skipped` / `rev_label_mismatch` (E3) | remapping against a checkout the author never named is the silently-wrong-checkout failure Decision 1 exists to prevent |
//! | sha absent here | `Unavailable` / `rev_unknown` | this checkout cannot answer; not an error |
//! | line inside a changed hunk | per-ref `inside_change` (E4) | the line's own content was edited — no honest mapping exists, so fall through to the token pass, which may still find the moved content |
//! | half-mapped range | the whole ref falls through (E6) | a half-mapped span is a lie about the span |
//!
//! Every refusal falls back to W1.C's token predicate, which claims no basis
//! at all — never to a guess.
//!
//! ## Rename tracking is OUT of v1 scope (recorded refusal, §3.6)
//!
//! If the file moved between `<sha>` and now, `cat-file -e` fails on the
//! current path and the ref falls through with `path_absent_at_rev`. Adding
//! `-M` with a two-path diff would need a rename oracle over the whole tree
//! per ref; the token predicate already handles "content moved" honestly, and
//! the scorecard exists for "wrong checkout entirely".
//!
//! ## `--literal-pathspecs` on the diff, and NOT on `cat-file` (DCB-W2.A.R
//! fix 1)
//!
//! `path_exists_at_rev`'s `cat-file -e <sha>:<path>` addresses a blob
//! directly — it is never pathspec-matched, so a path containing
//! `[ ] * ? \` (a bracketed Next.js-style route segment like
//! `app/[slug]/page.tsx` is a real example, reachable here via basename
//! resolution) is looked up literally regardless. `run_diff`'s `git diff
//! <sha> -- <path>` is a DIFFERENT beast: the trailing `-- <path>` is a
//! pathspec, and git's default pathspec grammar interprets those same
//! characters as a glob — so the SAME literal path that `cat-file` gated on
//! can diff a glob-matching SIBLING instead (wrong hunks quietly stitched
//! onto the wrong file) or match nothing at all (empty stdout reads as "no
//! changes", minting a false identity map). `--literal-pathspecs` is a git
//! top-level option (same family as `-C`), so it is passed ahead of the
//! `diff` subcommand and turns every pathspec on this invocation, including
//! `--`'s, back into a literal string comparison — closing the gap between
//! this function and the literal `cat-file` gate right above it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::join::kb_client::CodeRev;

/// Cap on `git diff` stdout for ONE (repo, path) remap. A pathological
/// whole-file rewrite is not worth parsing; over-cap ⇒ the path is `Failed`
/// and every ref on it reports `diff_failed`.
pub const MAX_DIFF_BYTES: usize = 1024 * 1024;

/// Cap on distinct paths remapped per request. Each costs one `cat-file -e` +
/// one `diff` subprocess; beyond this the remaining refs fall through to the
/// token pass with `remap = "budget_exhausted"`.
pub const MAX_REMAP_PATHS: usize = 120;

/// Its OWN enum, deliberately NOT unified with `DiffError`/`BlameError`/
/// `CheckoutError`/`TimelineError` — this crate's standing convention,
/// rationale in `diff.rs`'s module doc ("Why `DiffError` isn't shared with
/// its siblings").
#[derive(Debug, thiserror::Error)]
pub enum RemapError {
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git failed (exit {status}): {stderr}")]
    GitFailed { status: i32, stderr: String },
    #[error("git diff produced non-UTF8 output")]
    InvalidUtf8,
}

pub type Result<T> = std::result::Result<T, RemapError>;

/// One `-U0` hunk header: `@@ -old_start[,old_len] +new_start[,new_len] @@`.
/// Omitted counts default to 1 (git's own grammar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineMap {
    /// The line survives, at this (1-based) line in the current working tree.
    Mapped(u32),
    /// The line itself was modified or deleted between the two states — no
    /// honest mapping exists.
    InsideChange,
}

/// Per-doc remap availability, reported once on the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemapState {
    Applied,
    Skipped,
    Unavailable,
}

impl RemapState {
    pub fn as_str(self) -> &'static str {
        match self {
            RemapState::Applied => "applied",
            RemapState::Skipped => "skipped",
            RemapState::Unavailable => "unavailable",
        }
    }
}

/// The per-SPAN outcome of [`RevRemap::map`].
///
/// **Deviation from `15-w2a` §3.5, recorded:** the spec sketched
/// `map() -> Option<LineMap>` plus a separate `last_outcome(&self)`. That
/// pair is temporally coupled — after a `map()` that returned `None`,
/// `last_outcome()` reports a STALE string — and §9b.1 then calls `map` once
/// per span in a loop, which is exactly where such coupling turns into a
/// wrong wire value. One return value carrying both is the smaller, total
/// shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanRemap {
    Mapped(u32),
    InsideChange,
    PathAbsentAtRev,
    BudgetExhausted,
    DiffFailed,
}

impl SpanRemap {
    /// The frozen per-ref wire vocabulary (§7): `"applied" | "inside_change" |
    /// "path_absent_at_rev" | "budget_exhausted" | "diff_failed"`.
    pub fn outcome(self) -> &'static str {
        match self {
            SpanRemap::Mapped(_) => "applied",
            SpanRemap::InsideChange => "inside_change",
            SpanRemap::PathAbsentAtRev => "path_absent_at_rev",
            SpanRemap::BudgetExhausted => "budget_exhausted",
            SpanRemap::DiffFailed => "diff_failed",
        }
    }

    /// The mapping this outcome carries, if any. `None` for every arm that
    /// could not consult a map at all — those fall through to the token pass
    /// exactly like `InsideChange` does, but say something different on the
    /// wire.
    pub fn line_map(self) -> Option<LineMap> {
        match self {
            SpanRemap::Mapped(n) => Some(LineMap::Mapped(n)),
            SpanRemap::InsideChange => Some(LineMap::InsideChange),
            SpanRemap::PathAbsentAtRev | SpanRemap::BudgetExhausted | SpanRemap::DiffFailed => None,
        }
    }
}

/// One path's memoised map. **Deviation from §3.5's
/// `HashMap<String, Option<Vec<Hunk>>>`, recorded:** that shape has two
/// states, but the frozen per-ref vocabulary has THREE outcomes a path can
/// produce (`hunks` / `path_absent_at_rev` / `diff_failed`), so a two-state
/// memo would have to report a failed diff as "absent at rev" — a claim about
/// the commit that the daemon never verified.
#[derive(Debug, Clone)]
enum PathMap {
    Hunks(Vec<Hunk>),
    AbsentAtRev,
    /// Carries WHY for the log + the unit tests; the wire only ever sees
    /// `diff_failed` (the vocabulary is frozen, and one binary file among a
    /// doc's citations must not refuse the whole doc's remap).
    Failed(&'static str),
}

// --- the pure surface (every golden drives these directly) -----------------

/// Every `@@` header in `diff`, in file order. Ignores every other line — the
/// same "prefix-scan and skip the rest" shape `blame::timeline::parse_timeline`
/// uses. Tolerates the count-elided `@@ -a +c @@` form.
///
/// A `-U0` diff BODY line always starts with `+`, `-` or `\`, never `@@ `, so
/// a source line that happens to read `@@ …` can never be mistaken for a
/// header.
pub fn parse_hunks(diff: &str) -> Vec<Hunk> {
    diff.lines().filter_map(parse_hunk_header).collect()
}

fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ ")?;
    let (spec, _) = rest.split_once(" @@")?;
    let (old, new) = spec.split_once(' ')?;
    let (old_start, old_len) = parse_pair(old.strip_prefix('-')?)?;
    let (new_start, new_len) = parse_pair(new.strip_prefix('+')?)?;
    Some(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
    })
}

/// `"2,0"` → `(2, 0)`; `"18"` → `(18, 1)` (git elides a count of 1).
fn parse_pair(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// Map a 1-based line in the OLD state to the NEW state.
///
/// `hunks` must be in ascending `old_start` order (git's own output order).
/// Git's `-U0` insertion convention is `@@ -a,0 +c,d @@` = "d lines inserted
/// AFTER old line a", hence the strict `<` on a zero-length hunk.
///
/// TOTAL by construction: out-of-order hunks stop the walk at the `break`
/// rather than panicking, and the `mapped >= 1` guard makes the `i64 → u32`
/// conversion total. Neither is reachable from real `git diff` output —
/// deletions above a surviving line can never remove more lines than exist
/// above it — but a parser fed a hostile or truncated diff must not panic.
///
/// **Deliberately has no EOF awareness** (DCB-W2.A.R fix 2): `hunks` only
/// covers the lines the diff actually touched, so an `old_line` past every
/// hunk still walks the loop to completion and returns a `Mapped` result —
/// extrapolated from the LAST applicable offset, not validated against
/// either file's real length. A hint that was already out of range in the
/// OLD state (a stale citation, or a hostile one) can therefore map to a
/// line past the NEW state's EOF too. This function has no file to check
/// that against and is not the place to add one (it stays pure and
/// hunks-only on purpose); the caller (`resolve::line_half`) owns the one
/// memoised read that bound-checks the result before shipping it.
pub fn map_line(hunks: &[Hunk], old_line: u32) -> LineMap {
    let mut offset: i64 = 0;
    for h in hunks {
        if h.old_len == 0 {
            // Pure insertion AFTER old line `old_start`.
            if i64::from(h.old_start) < i64::from(old_line) {
                offset += i64::from(h.new_len);
            }
            continue;
        }
        if old_line < h.old_start {
            // Hunks are ordered; nothing further applies.
            break;
        }
        if old_line < h.old_start.saturating_add(h.old_len) {
            // Modified or deleted.
            return LineMap::InsideChange;
        }
        offset += i64::from(h.new_len) - i64::from(h.old_len);
    }
    let mapped = i64::from(old_line) + offset;
    if mapped >= 1 {
        LineMap::Mapped(u32::try_from(mapped).unwrap_or(u32::MAX))
    } else {
        LineMap::InsideChange
    }
}

// --- the git invocations ---------------------------------------------------

/// `git -C <root> rev-parse --verify --quiet <sha>^{commit}` — `Ok(None)` when
/// the rev simply does not resolve here (the `rev_unknown` arm; NOT an error).
fn rev_parse_commit(root: &Path, sha: &str) -> Result<Option<String>> {
    let spec = format!("{sha}^{{commit}}");
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--quiet", &spec])
        .output()
        .map_err(RemapError::Spawn)?;
    if !out.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8(out.stdout).map_err(|_| RemapError::InvalidUtf8)?;
    let full = text.trim().to_string();
    Ok(if full.is_empty() { None } else { Some(full) })
}

/// `git -C <root> cat-file -e <sha>:<path>` — did the path exist at that
/// commit? Rename tracking is out of scope (§3.6), so a `false` here means
/// "this ref falls through", never "the file is gone".
///
/// `pub(crate)` (not private) — CT-F2's "when written" check (`resolve`'s
/// `DeclaredEraCache`) reuses this SAME direct blob-address lookup rather
/// than forking a second existence check.
pub(crate) fn path_exists_at_rev(root: &Path, sha: &str, path: &str) -> Result<bool> {
    let spec = format!("{sha}:{path}");
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "-e", &spec])
        .output()
        .map_err(RemapError::Spawn)?;
    Ok(out.status.success())
}

/// The reverse line map for ONE path.
///
/// Flags the builder must not "clean up":
/// * **`--literal-pathspecs`** (DCB-W2.A.R fix 1, module doc) — without it,
///   the trailing `-- <path>` is glob-interpreted, so a path containing
///   `[ ] * ? \` can diff a glob-matching SIBLING instead of itself, or match
///   nothing at all. Must be a top-level option, ahead of the `diff`
///   subcommand.
/// * **`<sha>` with no `..HEAD`** — `git diff <commit> -- <path>` compares the
///   commit to the WORKING TREE, which is what the lens links into.
/// * **`--unified=0`** is what makes the hunk headers a complete line map; any
///   context line would fold unchanged lines into hunks and turn correct
///   mappings into `InsideChange`.
/// * **`--no-renames`** keeps the output single-file, so the parser never has
///   to decide which `diff --git` block it is inside.
/// * **`--no-ext-diff --no-textconv --no-color`** neutralise a user's global
///   git config; an `external diff` or a textconv filter would emit something
///   that is not a unified diff at all.
fn run_diff(root: &Path, sha: &str, path: &str) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "--literal-pathspecs",
            "diff",
            "--unified=0",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--ignore-submodules=all",
            sha,
            "--",
            path,
        ])
        .output()
        .map_err(RemapError::Spawn)?;
    if !out.status.success() {
        return Err(RemapError::GitFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    String::from_utf8(out.stdout).map_err(|_| RemapError::InvalidUtf8)
}

/// `git -C <root> cat-file -s <sha>:<path>` — the blob's byte size at that
/// commit, checked BEFORE [`show_blob_at_rev`] pulls it into memory (CT-F2 —
/// the "when written" check must cap the same way the working-tree read
/// does, via `doclens::LENS_FILE_READ_CAP`).
fn blob_size_at_rev(root: &Path, sha: &str, path: &str) -> Result<u64> {
    let spec = format!("{sha}:{path}");
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "-s", &spec])
        .output()
        .map_err(RemapError::Spawn)?;
    if !out.status.success() {
        return Err(RemapError::GitFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let text = String::from_utf8(out.stdout).map_err(|_| RemapError::InvalidUtf8)?;
    text.trim()
        .parse::<u64>()
        .map_err(|_| RemapError::InvalidUtf8)
}

/// `git -C <root> show <sha>:<path>` — the blob's raw bytes at that commit.
/// Object-address syntax (`<rev>:<path>`), same as [`path_exists_at_rev`]'s
/// `cat-file -e` — never pathspec-matched, so a bracketed path (the module
/// doc's fix 1) needs no `--literal-pathspecs` here either.
fn show_blob_at_rev(root: &Path, sha: &str, path: &str) -> Result<Vec<u8>> {
    let spec = format!("{sha}:{path}");
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", &spec])
        .output()
        .map_err(RemapError::Spawn)?;
    if !out.status.success() {
        return Err(RemapError::GitFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(out.stdout)
}

/// CT-F2 — read one path's FULL TEXT at a commit, capped like the
/// working-tree read. `Err` reasons mirror `resolve::file_text`'s own
/// vocabulary (`"file_too_large"` / `"unreadable"` / `"not_text"`) so a
/// consumer renders the SAME words regardless of which tree the text came
/// from. `pub(crate)`: `resolve::DeclaredEraCache` is the one caller.
pub(crate) fn read_text_at_rev(
    root: &Path,
    sha: &str,
    path: &str,
    cap: u64,
) -> std::result::Result<String, &'static str> {
    match blob_size_at_rev(root, sha, path) {
        Ok(size) if size > cap => return Err("file_too_large"),
        Ok(_) => {}
        Err(_) => return Err("unreadable"),
    }
    match show_blob_at_rev(root, sha, path) {
        Ok(bytes) if bytes.len() as u64 > cap => Err("file_too_large"),
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| "not_text"),
        Err(_) => Err("unreadable"),
    }
}

fn compute_path_map(root: &Path, sha: &str, path: &str) -> PathMap {
    match path_exists_at_rev(root, sha, path) {
        Ok(true) => {}
        Ok(false) => return PathMap::AbsentAtRev,
        Err(e) => {
            tracing::warn!(path, error = %e, "doc-lens remap: cat-file failed");
            return PathMap::Failed("cat_file_failed");
        }
    }
    let text = match run_diff(root, sha, path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(path, error = %e, "doc-lens remap: git diff failed");
            return PathMap::Failed("diff_failed");
        }
    };
    if text.len() > MAX_DIFF_BYTES {
        tracing::warn!(
            path,
            bytes = text.len(),
            "doc-lens remap: diff over MAX_DIFF_BYTES — not parsed"
        );
        return PathMap::Failed("diff_too_large");
    }
    // An empty hunk list from a BINARY file would otherwise read as "no
    // changes" and mint an identity map over bytes that have no lines at all.
    if text.lines().any(|l| l.starts_with("Binary files ")) {
        return PathMap::Failed("binary");
    }
    PathMap::Hunks(parse_hunks(&text))
}

// --- orchestration ---------------------------------------------------------

/// Everything the per-request blocking closure needs to remap one repo.
///
/// Built ONCE per (repo, request); `Applied` only when the doc carried a
/// `kb-code-rev`, it named THIS repo, it was not `+dirty`, and the sha
/// resolves here. Blocking: [`RevRemap::prepare`] and [`RevRemap::map`] both
/// spawn real subprocesses, so BOTH must run inside the caller's
/// `spawn_blocking` (the discipline `routes::blame`/`diff_route`/
/// `checkout_route` follow). Takes a bare `&Path` root rather than a
/// `GitRepo` for `blame::timeline`'s own recorded reason: `GitRepo` is
/// `!Send`, so there is nothing non-`Send` to smuggle across the boundary.
pub struct RevRemap {
    pub state: RemapState,
    pub reason: Option<&'static str>,
    pub repo_label: Option<String>,
    /// As WRITTEN in the doc (possibly abbreviated).
    pub sha: Option<String>,
    /// Full 40-hex, when it resolved here.
    pub resolved_sha: Option<String>,
    /// The doc's `+dirty` marker — never the selected repo's own dirtiness.
    pub dirty: bool,
    root: PathBuf,
    memo: HashMap<String, PathMap>,
    budget: usize,
    budget_exhausted: bool,
}

impl RevRemap {
    /// The decision table, in order (§3.5).
    pub fn prepare(root: &Path, repo_name: &str, code_rev: Option<&CodeRev>) -> Self {
        let base = Self {
            state: RemapState::Skipped,
            reason: None,
            repo_label: None,
            sha: None,
            resolved_sha: None,
            dirty: false,
            root: root.to_path_buf(),
            memo: HashMap::new(),
            budget: MAX_REMAP_PATHS,
            budget_exhausted: false,
        };
        let Some(rev) = code_rev else {
            return Self {
                reason: Some("no_doc_rev"),
                ..base
            };
        };
        let base = Self {
            repo_label: Some(rev.label.clone()),
            sha: Some(rev.sha.clone()),
            dirty: rev.dirty,
            ..base
        };
        // E2 — a `+dirty` rev states that the cited numbers were counted
        // against a tree no sha reproduces. Refused BEFORE the label check so
        // the reason a reader sees is the load-bearing one.
        if rev.dirty {
            return Self {
                reason: Some("doc_rev_dirty"),
                ..base
            };
        }
        // E3 — remapping against a checkout the author never named is the
        // silently-wrong-checkout failure the scorecard exists to prevent.
        if !rev.label.eq_ignore_ascii_case(repo_name) {
            return Self {
                reason: Some("rev_label_mismatch"),
                ..base
            };
        }
        if rev.sha.trim().is_empty() {
            return Self {
                state: RemapState::Unavailable,
                reason: Some("rev_unknown"),
                ..base
            };
        }
        match rev_parse_commit(root, rev.sha.trim()) {
            Ok(Some(full)) => Self {
                state: RemapState::Applied,
                resolved_sha: Some(full),
                ..base
            },
            Ok(None) => Self {
                state: RemapState::Unavailable,
                reason: Some("rev_unknown"),
                ..base
            },
            Err(e) => {
                tracing::warn!(sha = %rev.sha, error = %e, "doc-lens remap: rev-parse failed");
                Self {
                    state: RemapState::Unavailable,
                    reason: Some("rev_unknown"),
                    ..base
                }
            }
        }
    }

    /// Map one cited line on one repo-relative path.
    ///
    /// `None` when this remap is not `Applied` at all — the doc-level state
    /// already says why, and the per-ref field stays `null`. Memoised per
    /// path: at most one `cat-file -e` + one `diff` per distinct path per
    /// request, no matter how many refs or spans cite it.
    pub fn map(&mut self, path: &str, line: u32) -> Option<SpanRemap> {
        if self.state != RemapState::Applied {
            return None;
        }
        let sha = self.resolved_sha.clone()?;
        if !self.memo.contains_key(path) {
            if self.budget == 0 {
                self.budget_exhausted = true;
                return Some(SpanRemap::BudgetExhausted);
            }
            self.budget -= 1;
            let entry = compute_path_map(&self.root, &sha, path);
            self.memo.insert(path.to_string(), entry);
        }
        Some(match self.memo.get(path) {
            Some(PathMap::Hunks(h)) => match map_line(h, line) {
                LineMap::Mapped(n) => SpanRemap::Mapped(n),
                LineMap::InsideChange => SpanRemap::InsideChange,
            },
            Some(PathMap::AbsentAtRev) => SpanRemap::PathAbsentAtRev,
            Some(PathMap::Failed(why)) => {
                // The wire vocabulary is frozen at `diff_failed`; the specific
                // cause (binary, over-cap, git said no) lives here and in the
                // one-per-path `warn!` `compute_path_map` already emitted.
                tracing::debug!(path, why, "doc-lens remap: no usable map for this path");
                SpanRemap::DiffFailed
            }
            // The insert above is unconditional, so `None` is unreachable;
            // treating it as a failed diff keeps the fn total without an
            // `expect` that could panic a request thread.
            None => SpanRemap::DiffFailed,
        })
    }

    /// How many distinct paths this request actually built a map for.
    pub fn paths_mapped(&self) -> usize {
        self.memo
            .values()
            .filter(|m| matches!(m, PathMap::Hunks(_)))
            .count()
    }

    pub fn budget_exhausted(&self) -> bool {
        self.budget_exhausted
    }

    /// Test-visible failure cause for one path (`"binary"`, `"diff_too_large"`,
    /// …). The wire only ever renders `diff_failed`.
    #[cfg(test)]
    pub(crate) fn path_failure(&self, path: &str) -> Option<&'static str> {
        match self.memo.get(path) {
            Some(PathMap::Failed(why)) => Some(why),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn memo_len(&self) -> usize {
        self.memo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    // --- pure: parse_hunks --------------------------------------------------

    #[test]
    fn parse_hunks_reads_unified_zero_headers_with_and_without_counts() {
        let diff = "@@ -2,0 +3,3 @@\n@@ -18 +21 @@\n";
        assert_eq!(
            parse_hunks(diff),
            vec![
                Hunk {
                    old_start: 2,
                    old_len: 0,
                    new_start: 3,
                    new_len: 3
                },
                // Elided counts default to 1 — git's own grammar.
                Hunk {
                    old_start: 18,
                    old_len: 1,
                    new_start: 21,
                    new_len: 1
                },
            ]
        );
    }

    #[test]
    fn parse_hunks_ignores_diff_body_and_file_headers() {
        let diff = "diff --git a/app/models/purchase.rb b/app/models/purchase.rb\n\
                    index 1111111..2222222 100644\n\
                    --- a/app/models/purchase.rb\n\
                    +++ b/app/models/purchase.rb\n\
                    @@ -2,0 +3,3 @@\n\
                    +  has_many :shipments\n\
                    +\n\
                    +  # V2\n\
                    @@ -18 +21 @@\n\
                    -  def stale_helper\n\
                    +  def fresh_helper\n\
                    \\ No newline at end of file\n";
        assert_eq!(parse_hunks(diff).len(), 2);
        // A `-U0` body line always starts with +/-/\, so a source line that
        // itself reads `@@ …` can never be mistaken for a header.
        assert!(parse_hunks("+@@ -1 +1 @@\n-@@ -9 +9 @@\n").is_empty());
    }

    #[test]
    fn parse_hunks_returns_empty_for_an_empty_diff() {
        assert!(parse_hunks("").is_empty());
        assert!(parse_hunks("\n\n").is_empty());
        // Malformed headers are skipped, never panicked on.
        assert!(parse_hunks("@@ nonsense @@\n@@ -x,y +z @@\n@@\n").is_empty());
    }

    // --- pure: map_line -----------------------------------------------------

    /// The §5.1 fixture's own diff: three lines inserted after old line 2, and
    /// old line 18 rewritten in place.
    fn gamma_hunks() -> Vec<Hunk> {
        parse_hunks("@@ -2,0 +3,3 @@\n@@ -18 +21 @@\n")
    }

    #[test]
    fn map_line_shifts_by_a_pure_insertion_above() {
        // The headline case: the doc cited 10, the method now lives at 13.
        assert_eq!(map_line(&gamma_hunks(), 10), LineMap::Mapped(13));
        assert_eq!(map_line(&gamma_hunks(), 3), LineMap::Mapped(6));
        assert_eq!(map_line(&gamma_hunks(), 21), LineMap::Mapped(24));
    }

    #[test]
    fn map_line_leaves_the_insertion_anchor_line_itself_unshifted() {
        // `@@ -2,0 +3,3 @@` inserts AFTER old line 2, so 1 and 2 do not move.
        assert_eq!(map_line(&gamma_hunks(), 1), LineMap::Mapped(1));
        assert_eq!(map_line(&gamma_hunks(), 2), LineMap::Mapped(2));
    }

    #[test]
    fn map_line_reports_inside_change_for_a_modified_line() {
        assert_eq!(map_line(&gamma_hunks(), 18), LineMap::InsideChange);
    }

    #[test]
    fn map_line_reports_inside_change_for_a_deleted_line() {
        let deletion = [Hunk {
            old_start: 5,
            old_len: 4,
            new_start: 5,
            new_len: 0,
        }];
        assert_eq!(map_line(&deletion, 6), LineMap::InsideChange);
        // A line BELOW the deleted range survives, shifted up by four.
        assert_eq!(map_line(&deletion, 9), LineMap::Mapped(5));
        let head = [Hunk {
            old_start: 1,
            old_len: 10,
            new_start: 1,
            new_len: 0,
        }];
        assert_eq!(map_line(&head, 3), LineMap::InsideChange);
    }

    #[test]
    fn map_line_is_identity_on_an_empty_hunk_list() {
        // Correct, not a fallback: the file is byte-identical between the
        // doc's rev and the working tree.
        assert_eq!(map_line(&[], 42), LineMap::Mapped(42));
        assert_eq!(map_line(&[], 1), LineMap::Mapped(1));
    }

    #[test]
    fn map_line_accumulates_multiple_hunks_in_order() {
        let hunks = [
            Hunk {
                old_start: 1,
                old_len: 3,
                new_start: 1,
                new_len: 0,
            },
            Hunk {
                old_start: 10,
                old_len: 0,
                new_start: 8,
                new_len: 5,
            },
        ];
        // Below the deletion, above the insertion: −3.
        assert_eq!(map_line(&hunks, 8), LineMap::Mapped(5));
        assert_eq!(map_line(&hunks, 4), LineMap::Mapped(1));
        assert_eq!(map_line(&hunks, 1), LineMap::InsideChange);
        // Below both: −3 +5.
        assert_eq!(map_line(&hunks, 11), LineMap::Mapped(13));
    }

    /// The two totality guards. Neither is reachable from real `git diff`
    /// output — git emits hunks in ascending `old_start`, and deletions above
    /// a surviving line can never remove more lines than exist above it — but
    /// a parser fed a hostile or truncated diff must not panic.
    #[test]
    fn map_line_is_total_on_a_malformed_hunk_list() {
        let out_of_order = [
            Hunk {
                old_start: 5,
                old_len: 1,
                new_start: 5,
                new_len: 1,
            },
            Hunk {
                old_start: 2,
                old_len: 1,
                new_start: 2,
                new_len: 1,
            },
        ];
        assert_eq!(map_line(&out_of_order, 6), LineMap::Mapped(6));
        // A single well-formed deletion can never push a surviving line below
        // 1 — the smallest line BELOW `@@ -1,3 +1,0 @@` is 4, and it lands
        // exactly on 1.
        let one_deletion = [Hunk {
            old_start: 1,
            old_len: 3,
            new_start: 1,
            new_len: 0,
        }];
        assert_eq!(map_line(&one_deletion, 4), LineMap::Mapped(1));
        assert_eq!(map_line(&one_deletion, 1), LineMap::InsideChange);
        // The `mapped >= 1` guard therefore needs MALFORMED input to reach:
        // two overlapping deletions delete more lines than exist above 4.
        let overlapping = [one_deletion[0], one_deletion[0]];
        assert_eq!(map_line(&overlapping, 4), LineMap::InsideChange);
        // `old_start + old_len` saturates instead of panicking in debug.
        let overflow = [Hunk {
            old_start: 1,
            old_len: u32::MAX,
            new_start: 1,
            new_len: 1,
        }];
        assert_eq!(map_line(&overflow, 5), LineMap::InsideChange);
    }

    // --- fixture repo (RevRemap) -------------------------------------------

    fn git(dir: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {args:?} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(dir: &Path, rel: &str, body: &[u8]) {
        let abs = dir.join(rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, body).unwrap();
    }

    fn head_sha(dir: &Path) -> String {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A two-commit repo: `a.txt` gains three lines after line 2 in commit B.
    fn fixture() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        write(dir, "a.txt", b"1\n2\n3\n4\n5\n");
        write(dir, "b.txt", b"x\n");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "A"]);
        let sha_a = head_sha(dir);
        write(dir, "a.txt", b"1\n2\nnew1\nnew2\nnew3\n3\n4\n5\n");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "B"]);
        (tmp, sha_a)
    }

    fn rev(label: &str, sha: &str, dirty: bool) -> CodeRev {
        CodeRev {
            label: label.to_string(),
            sha: sha.to_string(),
            dirty,
        }
    }

    #[test]
    fn remap_prepare_skips_when_the_doc_has_no_rev() {
        let (tmp, _) = fixture();
        let r = RevRemap::prepare(tmp.path(), "gamma", None);
        assert_eq!(r.state, RemapState::Skipped);
        assert_eq!(r.reason, Some("no_doc_rev"));
        assert!(r.repo_label.is_none() && r.sha.is_none());
    }

    #[test]
    fn remap_prepare_skips_a_dirty_doc_rev_with_the_recorded_reason() {
        let (tmp, sha_a) = fixture();
        let mut r = RevRemap::prepare(tmp.path(), "gamma", Some(&rev("gamma", &sha_a, true)));
        assert_eq!(r.state, RemapState::Skipped);
        assert_eq!(r.reason, Some("doc_rev_dirty"));
        assert!(r.dirty);
        // The sha is REPORTED (a consumer renders "written against …+dirty")
        // but never resolved, and no ref may consult a map.
        assert_eq!(r.sha.as_deref(), Some(sha_a.as_str()));
        assert!(r.resolved_sha.is_none());
        assert_eq!(r.map("a.txt", 3), None);
    }

    #[test]
    fn remap_prepare_skips_a_label_that_names_another_repo() {
        let (tmp, sha_a) = fixture();
        let mut r = RevRemap::prepare(tmp.path(), "gamma", Some(&rev("other", &sha_a, false)));
        assert_eq!(r.state, RemapState::Skipped);
        assert_eq!(r.reason, Some("rev_label_mismatch"));
        assert_eq!(r.map("a.txt", 3), None);
        // ASCII-case-insensitive (E3) — `Gamma` names the same checkout.
        let ok = RevRemap::prepare(tmp.path(), "gamma", Some(&rev("Gamma", &sha_a, false)));
        assert_eq!(ok.state, RemapState::Applied);
    }

    #[test]
    fn remap_prepare_is_unavailable_for_an_unresolvable_sha() {
        let (tmp, _) = fixture();
        for bad in ["deadbeefdeadbeef", "", "   ", "not-a-sha"] {
            let mut r = RevRemap::prepare(tmp.path(), "gamma", Some(&rev("gamma", bad, false)));
            assert_eq!(r.state, RemapState::Unavailable, "sha {bad:?}");
            assert_eq!(r.reason, Some("rev_unknown"), "sha {bad:?}");
            assert_eq!(r.map("a.txt", 3), None);
        }
    }

    #[test]
    fn remap_maps_and_memoises_one_diff_per_path_per_request() {
        let (tmp, sha_a) = fixture();
        let mut r = RevRemap::prepare(tmp.path(), "gamma", Some(&rev("gamma", &sha_a, false)));
        assert_eq!(r.state, RemapState::Applied);
        assert_eq!(r.resolved_sha.as_deref(), Some(sha_a.as_str()));
        // Three lines inserted after old line 2.
        assert_eq!(r.map("a.txt", 3), Some(SpanRemap::Mapped(6)));
        assert_eq!(r.map("a.txt", 2), Some(SpanRemap::Mapped(2)));
        assert_eq!(r.map("a.txt", 5), Some(SpanRemap::Mapped(8)));
        assert_eq!(r.memo_len(), 1, "three calls, ONE diff");
        assert_eq!(r.paths_mapped(), 1);
        // An unchanged file maps identically — and still costs one memo slot.
        assert_eq!(r.map("b.txt", 1), Some(SpanRemap::Mapped(1)));
        assert_eq!(r.memo_len(), 2);
        assert!(!r.budget_exhausted());
    }

    /// DCB-W2.A.R fix 1 — `app/[slug]/page.tsx` is a real Next.js-style path
    /// (reachable here via basename resolution); git's DEFAULT pathspec
    /// grammar treats `[slug]` as a character class matching any ONE of
    /// `s`/`l`/`u`/`g`, so `app/s/page.tsx` is a glob match for it. Before
    /// `--literal-pathspecs`, `run_diff`'s trailing `-- <path>` would diff
    /// that SIBLING instead — while `path_exists_at_rev`'s `cat-file -e` (a
    /// direct blob lookup, never pathspec-matched) had already gated on the
    /// bracketed path literally, so the two calls disagreed about which file
    /// was even being asked about.
    #[test]
    fn remap_of_a_bracketed_path_never_diffs_a_glob_matching_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        write(dir, "app/[slug]/page.tsx", b"a\nb\nc\n");
        write(dir, "app/s/page.tsx", b"x\ny\n");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "A"]);
        let sha_a = head_sha(dir);
        // The bracketed path is UNTOUCHED between A and B; the glob-matching
        // sibling gains a line at the TOP, so its own map is a real shift
        // (line 1 -> 2) — a hunk that must never leak onto the query below.
        write(dir, "app/s/page.tsx", b"w\nx\ny\n");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "B"]);

        let mut r = RevRemap::prepare(dir, "gamma", Some(&rev("gamma", &sha_a, false)));
        assert_eq!(r.state, RemapState::Applied);
        // Identity map: the bracketed path is unchanged, so every line stays
        // put. A glob-interpreted diff would instead answer with the
        // sibling's +1 shift (line 1 -> 2).
        assert_eq!(r.map("app/[slug]/page.tsx", 1), Some(SpanRemap::Mapped(1)));
        assert_eq!(r.map("app/[slug]/page.tsx", 3), Some(SpanRemap::Mapped(3)));
        // The sibling really did change — the fixture is real, not just
        // coincidentally empty either way.
        assert_eq!(r.map("app/s/page.tsx", 1), Some(SpanRemap::Mapped(2)));
    }

    #[test]
    fn remap_reports_a_path_absent_at_the_rev_without_a_diff() {
        let (tmp, sha_a) = fixture();
        let dir = tmp.path();
        write(dir, "later.txt", b"only in B\n");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "add later"]);
        let mut r = RevRemap::prepare(dir, "gamma", Some(&rev("gamma", &sha_a, false)));
        assert_eq!(r.map("later.txt", 1), Some(SpanRemap::PathAbsentAtRev));
        assert_eq!(r.paths_mapped(), 0, "no map was built for an absent path");
    }

    #[test]
    fn remap_stops_at_max_remap_paths_and_reports_budget_exhausted() {
        let (tmp, sha_a) = fixture();
        let mut r = RevRemap::prepare(tmp.path(), "gamma", Some(&rev("gamma", &sha_a, false)));
        // Drain the budget with synthetic (absent) paths — the budget counts
        // distinct paths, not successful maps.
        for i in 0..MAX_REMAP_PATHS {
            assert!(r.map(&format!("p{i}.txt"), 1).is_some());
        }
        assert!(!r.budget_exhausted());
        assert_eq!(r.map("a.txt", 3), Some(SpanRemap::BudgetExhausted));
        assert!(r.budget_exhausted());
        // A path ALREADY memoised still answers after the budget is gone —
        // the budget bounds subprocesses, not lookups.
        assert_eq!(r.map("p0.txt", 1), Some(SpanRemap::PathAbsentAtRev));
    }

    #[test]
    fn remap_refuses_a_binary_diff_rather_than_minting_an_identity_map() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        write(dir, "blob.bin", &[0u8, 159, 146, 150, 0, 1, 2]);
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "A"]);
        let sha_a = head_sha(dir);
        write(dir, "blob.bin", &[0u8, 7, 7, 7, 0, 9]);
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "B"]);

        let mut r = RevRemap::prepare(dir, "gamma", Some(&rev("gamma", &sha_a, false)));
        assert_eq!(r.map("blob.bin", 1), Some(SpanRemap::DiffFailed));
        assert_eq!(r.path_failure("blob.bin"), Some("binary"));
        assert_eq!(r.paths_mapped(), 0);
    }

    #[test]
    fn remap_refuses_a_diff_over_max_diff_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        write(dir, "big.txt", b"seed\n");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "A"]);
        let sha_a = head_sha(dir);
        // A whole-file rewrite whose diff comfortably clears the 1 MiB cap.
        // Sized by BYTES, not lines: `git diff` emits one `+`-prefixed copy of
        // every line, so ~20k × ~70 B lands near 1.4 MiB — well clear of
        // `MAX_DIFF_BYTES` without writing a pathologically large fixture.
        let pad = "x".repeat(60);
        let body: String = (0..20_000).map(|i| format!("{i} {pad}\n")).collect();
        assert!(body.len() > MAX_DIFF_BYTES, "fixture must clear the cap");
        write(dir, "big.txt", body.as_bytes());
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "B"]);

        let mut r = RevRemap::prepare(dir, "gamma", Some(&rev("gamma", &sha_a, false)));
        assert_eq!(r.map("big.txt", 1), Some(SpanRemap::DiffFailed));
        assert_eq!(r.path_failure("big.txt"), Some("diff_too_large"));
    }

    #[test]
    fn span_remap_outcome_strings_are_the_frozen_wire_vocabulary() {
        assert_eq!(SpanRemap::Mapped(3).outcome(), "applied");
        assert_eq!(SpanRemap::InsideChange.outcome(), "inside_change");
        assert_eq!(SpanRemap::PathAbsentAtRev.outcome(), "path_absent_at_rev");
        assert_eq!(SpanRemap::BudgetExhausted.outcome(), "budget_exhausted");
        assert_eq!(SpanRemap::DiffFailed.outcome(), "diff_failed");
        assert_eq!(SpanRemap::Mapped(3).line_map(), Some(LineMap::Mapped(3)));
        assert_eq!(
            SpanRemap::InsideChange.line_map(),
            Some(LineMap::InsideChange)
        );
        // Everything that could not consult a map at all carries none.
        assert!(SpanRemap::PathAbsentAtRev.line_map().is_none());
        assert!(SpanRemap::BudgetExhausted.line_map().is_none());
        assert!(SpanRemap::DiffFailed.line_map().is_none());
        assert_eq!(RemapState::Applied.as_str(), "applied");
        assert_eq!(RemapState::Skipped.as_str(), "skipped");
        assert_eq!(RemapState::Unavailable.as_str(), "unavailable");
    }
}
