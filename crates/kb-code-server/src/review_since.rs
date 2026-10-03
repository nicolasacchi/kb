//! v0.44 F9 — `GET /api/reviews/{id}/since`: what the AUTHOR changed between
//! two patchsets of a review, with base movement taken out.
//!
//! # Why not the interdiff
//!
//! `review_interdiff` and (before this unit) `review_finding_touches` diff
//! one patchset TIP against another. With base tracking, a patchset that is
//! only a rebase onto a newer main is routine, and a tip-to-tip diff of two
//! such patchsets lists every upstream file and line: the noise reads as
//! author edits. This module answers the question the reviewer actually
//! has — "what did the author change since I last looked?" — from each
//! patchset's change set against its OWN base:
//!
//!   1. `git diff -U0 --no-renames <base> <tip>` per patchset (upstream
//!      movement is on the base side of both ends, so it is not in either
//!      diff);
//!   2. every hunk is addressed with `kbc-hunkid/1`
//!      ([`crate::review_hunks::hunk_id`]) — the id leaves out line numbers
//!      and context "so the id survives a rebase", so a change carried
//!      through a rebase keeps its id;
//!   3. ids are compared as per-path MULTISETS: two identical changes in
//!      one file share an id (the id's one accepted collision), so a set
//!      would call a second copy "carried". A count per path does not.
//!
//! No merge-tree and no scratch object store: invariant 5 of
//! `crates/kb-code-server/CLAUDE.md` is not touched.
//!
//! # What this is not
//!
//!   * Not a verdict. It says which hunks are carried / new / gone; it
//!     never says anything is "fixed" or "addressed", and `rebase_only` is
//!     the statement "no hunk is new or gone", nothing more. The daemon
//!     never carries a verdict forward (D20) — a caller reads this and the
//!     HUMAN decides.
//!   * Not stored. Computed on each read; the only memory is a bounded,
//!     process-wide memo keyed on the immutable `(base_sha, tip_sha)` pair.
//!   * Not the SPA's viewed-hunk ids. The SPA mints ids from `-U3` hunks
//!     (adjacent changes merge), these come from `-U0` hunks, so the two
//!     sets of ids must never be mixed.
//!   * Conflict resolution during a rebase rightly shows up as NEW hunks:
//!     the author changed those lines, whatever the reason.
//!   * A file whose only change is a mode flip or an empty-file add has no
//!     hunk and is invisible here (rename detection is off, so a rename is
//!     one delete plus one add and reads as gone + new).

use crate::git::roots::GitCtx;
use crate::review_hunks::{self, DiffHunk};
use crate::reviews::{self, is_full_sha};
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{ReviewPatchsetRow, StoreBlocking};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, Mutex};

pub const SINCE_SCHEMA: &str = "kbc-review-since/1";

/// One hunk of a patchset's `-U0` change set: its `kbc-hunkid/1` address
/// plus where it sits on the TIP side (used to attribute a tip-to-tip hunk
/// to the author — see [`author_new_ranges`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkRef {
    pub id: String,
    pub new_start: u32,
    pub new_lines: u32,
    /// A binary file's single pseudo-hunk: it has no line range.
    pub binary: bool,
}

/// `path -> hunks in file order`.
pub type PathHunks = BTreeMap<String, Vec<HunkRef>>;

// --- parsing a whole-patchset diff (pure) -------------------------------------

/// Split a multi-file unified diff into `(path, that file's diff text)`.
/// The path is read from the `diff --git a/X b/X` header; with rename
/// detection off both sides name the same path, so `X` is the middle of
/// the remainder (robust to spaces). A header that does not fit that shape
/// (a quoted name) keys on the whole remainder — still identical for the
/// same file in both patchsets, which is all a key needs.
pub fn split_file_diffs(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    for line in lines {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let n = rest.len();
            let path = match rest.strip_prefix("a/") {
                Some(_) if n >= 5 && (n - 5) % 2 == 0 => {
                    let l = (n - 5) / 2;
                    rest.get(2..2 + l).unwrap_or(rest).to_string()
                }
                _ => rest.to_string(),
            };
            out.push((path, String::new()));
        }
        if let Some((_, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

/// A binary file has no hunk; its identity is its path plus the blob it
/// ends up as (the `index <old>..<new>` line), so an unchanged binary
/// carried through a rebase is carried and a changed one is new.
fn binary_id(path: &str, preamble: &[String]) -> String {
    let new_blob = preamble
        .iter()
        .find_map(|l| l.strip_prefix("index "))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|r| r.split("..").nth(1))
        .unwrap_or("");
    review_hunks::fnv1a64_utf16(&format!("{path}\nbinary\n{new_blob}"))
}

/// Every hunk of a whole-patchset diff, addressed. Pure.
pub fn hunks_by_path(diff_text: &str) -> PathHunks {
    let mut out: PathHunks = BTreeMap::new();
    for (path, body) in split_file_diffs(diff_text) {
        let parsed = review_hunks::parse_unified_diff(&body);
        let entry = out.entry(path.clone()).or_default();
        if parsed.binary {
            entry.push(HunkRef {
                id: binary_id(&path, &parsed.preamble),
                new_start: 0,
                new_lines: 0,
                binary: true,
            });
            continue;
        }
        for h in &parsed.hunks {
            entry.push(hunk_ref(&path, h));
        }
    }
    out.retain(|_, v| !v.is_empty());
    out
}

fn hunk_ref(path: &str, h: &DiffHunk) -> HunkRef {
    HunkRef {
        id: review_hunks::hunk_id(path, h),
        new_start: h.new_start,
        new_lines: h.new_lines,
        binary: false,
    }
}

// --- comparing two patchsets (pure) -------------------------------------------

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SincePath {
    pub path: String,
    // Hunks present in both patchsets (matched id for id, by count).
    pub carried: usize,
    // Hunks only the later patchset has.
    pub new: usize,
    // Hunks only the earlier patchset has.
    pub gone: usize,
}

/// One path's comparison: `(carried, the later patchset's NEW hunks,
/// gone count)`. Multiset semantics: each earlier hunk can be carried at
/// most once, in file order.
fn classify<'a>(from: &[HunkRef], to: &'a [HunkRef]) -> (usize, Vec<&'a HunkRef>, usize) {
    let mut remaining: HashMap<&str, usize> = HashMap::new();
    for h in from {
        *remaining.entry(h.id.as_str()).or_insert(0) += 1;
    }
    let mut carried = 0usize;
    let mut new: Vec<&HunkRef> = Vec::new();
    for h in to {
        match remaining.get_mut(h.id.as_str()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                carried += 1;
            }
            _ => new.push(h),
        }
    }
    let gone = from.len() - carried;
    (carried, new, gone)
}

/// Per-path carried / new / gone, over the union of both patchsets' paths,
/// path-sorted. Pure.
pub fn compare(from: &PathHunks, to: &PathHunks) -> Vec<SincePath> {
    let mut paths: Vec<&String> = from.keys().chain(to.keys()).collect();
    paths.sort();
    paths.dedup();
    let empty: Vec<HunkRef> = Vec::new();
    paths
        .into_iter()
        .map(|p| {
            let f = from.get(p).unwrap_or(&empty);
            let t = to.get(p).unwrap_or(&empty);
            let (carried, new, gone) = classify(f, t);
            SincePath {
                path: p.clone(),
                carried,
                new: new.len(),
                gone,
            }
        })
        .collect()
}

/// Per path, the TIP-side `(first, last)` line range of every hunk the
/// later patchset has and the earlier one does not (zero-width hunks are
/// the single point `new_start`). This is what lets `touched_in` ask "did
/// the AUTHOR change those lines?" in the later tip's own coordinates.
/// Binary entries (no lines) are left out. Pure.
pub fn author_new_ranges(from: &PathHunks, to: &PathHunks) -> HashMap<String, Vec<(u32, u32)>> {
    let empty: Vec<HunkRef> = Vec::new();
    let mut out: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    for (path, t) in to {
        let f = from.get(path).unwrap_or(&empty);
        let (_, new, _) = classify(f, t);
        let ranges: Vec<(u32, u32)> = new
            .iter()
            .filter(|h| !h.binary)
            .map(|h| {
                let hi = h.new_start + h.new_lines.saturating_sub(1);
                (h.new_start, hi.max(h.new_start))
            })
            .collect();
        if !ranges.is_empty() {
            out.insert(path.clone(), ranges);
        }
    }
    out
}

// --- the git read, memoised ---------------------------------------------------

const MEMO_CAP: usize = 256;
/// A single cached map above this many bytes (estimated by [`weigh`]) is
/// returned to its caller but never remembered: one giant patchset must
/// not be able to pin memory for the life of the process.
const MEMO_ENTRY_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Total estimated bytes held; past it the memo is cleared (a memo, not a
/// store) — the entry-count cap alone bounded nothing about SIZE.
const MEMO_TOTAL_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
struct Memo {
    map: HashMap<(String, String), Arc<PathHunks>>,
    bytes: usize,
}

static MEMO: LazyLock<Mutex<Memo>> = LazyLock::new(Default::default);

/// Estimated heap bytes of a hunk map: path strings, hunk ids and the fixed
/// per-hunk fields. An estimate, deliberately a little generous.
fn weigh(m: &PathHunks) -> usize {
    m.iter()
        .map(|(p, hs)| {
            p.len()
                + 48
                + hs.iter()
                    .map(|h| h.id.len() + std::mem::size_of::<HunkRef>())
                    .sum::<usize>()
        })
        .sum()
}

/// Remember `hunks` under `key` if it fits the per-entry budget, evicting
/// everything first when the count or total-bytes cap would be exceeded.
/// Returns whether it was cached.
fn memo_put(memo: &mut Memo, key: (String, String), hunks: Arc<PathHunks>) -> bool {
    let w = weigh(&hunks);
    if w > MEMO_ENTRY_MAX_BYTES {
        return false;
    }
    if memo.map.len() >= MEMO_CAP || memo.bytes + w > MEMO_TOTAL_MAX_BYTES {
        memo.map.clear();
        memo.bytes = 0;
    }
    if memo.map.insert(key, hunks).is_none() {
        memo.bytes += w;
    }
    true
}

/// One patchset's `-U0` hunks against its own base. BLOCKING (a git
/// subprocess); callers run it inside `spawn_blocking`. Only successful
/// reads are remembered, and the memo is keyed on two immutable shas, so a
/// stale answer is impossible; it is bounded in entries AND bytes (see
/// [`memo_put`]). No guard is held across the git call.
pub fn patchset_hunks(ctx: &GitCtx, base: &str, tip: &str) -> Result<Arc<PathHunks>, String> {
    if !is_full_sha(base) || !is_full_sha(tip) {
        return Err(format!("not a full sha: {base}..{tip}"));
    }
    let key = (base.to_string(), tip.to_string());
    if let Some(hit) = MEMO.lock().ok().and_then(|m| m.map.get(&key).cloned()) {
        return Ok(hit);
    }
    let text = ctx
        .read_with_fallback(|root| crate::diff::diff_range_u0(root.git_path(), base, tip))
        .map_err(|e| e.to_string())?;
    let hunks = Arc::new(hunks_by_path(&text));
    if let Ok(mut m) = MEMO.lock() {
        memo_put(&mut m, key, hunks.clone());
    }
    Ok(hunks)
}

/// What the author did between two patchsets, as line ranges in the two
/// tips' own coordinates: the hunks the later patchset ADDED (later-tip
/// coordinates) and the hunks it REMOVED/reverted (earlier-tip
/// coordinates). `touched_in` needs both: an author who reverts or drops a
/// change over a finding's lines acted on them just as surely as one who
/// edits them, and that act leaves no new hunk behind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorRanges {
    pub added: HashMap<String, Vec<(u32, u32)>>,
    pub removed: HashMap<String, Vec<(u32, u32)>>,
}

/// Per path, the EARLIER-tip `(first, last)` line range of every hunk the
/// earlier patchset's change set has and the later one does not (the
/// author reverted or dropped it). Same multiset matching as
/// [`author_new_ranges`]; the mirror image. Pure.
pub fn author_gone_ranges(from: &PathHunks, to: &PathHunks) -> HashMap<String, Vec<(u32, u32)>> {
    let empty: Vec<HunkRef> = Vec::new();
    let mut out: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    for (path, f) in from {
        let t = to.get(path).unwrap_or(&empty);
        // `classify(to, from)` yields the hunks of `from` that `to` lacks.
        let (_, gone, _) = classify(t, f);
        let ranges: Vec<(u32, u32)> = gone
            .iter()
            .filter(|h| !h.binary)
            .map(|h| {
                let hi = h.new_start + h.new_lines.saturating_sub(1);
                (h.new_start, hi.max(h.new_start))
            })
            .collect();
        if !ranges.is_empty() {
            out.insert(path.clone(), ranges);
        }
    }
    out
}

/// The author's hunks between `own` and `later` (added and removed). `None`
/// when either patchset's change set cannot be read — the caller then keeps
/// its honest "cannot attribute" fallback.
pub fn author_ranges_between(
    ctx: &GitCtx,
    own: &ReviewPatchsetRow,
    later: &ReviewPatchsetRow,
) -> Option<AuthorRanges> {
    let from = patchset_hunks(ctx, &own.base_sha, &own.tip_sha).ok()?;
    let to = patchset_hunks(ctx, &later.base_sha, &later.tip_sha).ok()?;
    Some(AuthorRanges {
        added: author_new_ranges(&from, &to),
        removed: author_gone_ranges(&from, &to),
    })
}

// --- the report ---------------------------------------------------------------

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SincePatchset {
    pub ps: u32,
    pub base_sha: String,
    pub tip_sha: String,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SinceAuthorDelta {
    pub new_hunks: usize,
    pub gone_hunks: usize,
    pub paths_changed: usize,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SinceBases {
    pub from: String,
    pub to: String,
    // The two patchsets sit on different bases (the later one was
    // rebased or retargeted). `rebase_only` with `moved: false` is just
    // "nothing changed".
    pub moved: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SinceReport {
    pub schema: String,
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub review_id: i64,
    pub from: SincePatchset,
    pub to: SincePatchset,
    // `verdict` when `from=verdict` named the verdict's patchset, else `ps`.
    pub from_source: String,
    pub paths: Vec<SincePath>,
    pub author_delta: SinceAuthorDelta,
    // No hunk is new or gone. Surfaced, never a verdict.
    pub rebase_only: bool,
    pub bases: SinceBases,
    pub note: String,
}

const NOTE: &str = "Derived on each read from each patchset's -U0 diff against its own base; \
hunk ids are kbc-hunkid/1 over -U0 hunks (not the SPA's stored viewed-hunk ids). Shown, never a \
verdict; the daemon carries nothing forward.";

fn patchset_ref(p: &ReviewPatchsetRow) -> SincePatchset {
    SincePatchset {
        ps: u32::try_from(p.ps_number).unwrap_or(0),
        base_sha: p.base_sha.clone(),
        tip_sha: p.tip_sha.clone(),
    }
}

/// Assemble the report from the two patchsets' hunks. Pure.
pub fn build_report(
    review_id: i64,
    from: &ReviewPatchsetRow,
    to: &ReviewPatchsetRow,
    from_source: &str,
    from_hunks: &PathHunks,
    to_hunks: &PathHunks,
) -> SinceReport {
    let paths = compare(from_hunks, to_hunks);
    let new_hunks: usize = paths.iter().map(|p| p.new).sum();
    let gone_hunks: usize = paths.iter().map(|p| p.gone).sum();
    let paths_changed = paths.iter().filter(|p| p.new > 0 || p.gone > 0).count();
    SinceReport {
        schema: SINCE_SCHEMA.to_string(),
        review_id,
        from: patchset_ref(from),
        to: patchset_ref(to),
        from_source: from_source.to_string(),
        paths,
        author_delta: SinceAuthorDelta {
            new_hunks,
            gone_hunks,
            paths_changed,
        },
        rebase_only: new_hunks == 0 && gone_hunks == 0,
        bases: SinceBases {
            from: from.base_sha.clone(),
            to: to.base_sha.clone(),
            moved: from.base_sha != to.base_sha,
        },
        note: NOTE.to_string(),
    }
}

/// The impure shell. BLOCKING.
pub fn compute_since(
    ctx: &GitCtx,
    review_id: i64,
    from: &ReviewPatchsetRow,
    to: &ReviewPatchsetRow,
    from_source: &str,
) -> Result<SinceReport, String> {
    let f = patchset_hunks(ctx, &from.base_sha, &from.tip_sha)?;
    let t = patchset_hunks(ctx, &to.base_sha, &to.tip_sha)?;
    Ok(build_report(review_id, from, to, from_source, &f, &t))
}

// --- the route ----------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct SinceParams {
    /// `psN` | `N` | `verdict` (default): the patchset the verdict sits on.
    #[serde(default)]
    pub from: Option<String>,
    /// `psN` | `N` | `latest` (default).
    #[serde(default)]
    pub to: Option<String>,
}

/// `Ok(None)` = `verdict` / `latest` (resolved against the review),
/// `Ok(Some(n))` = a patchset number. Pure.
pub fn parse_ps_ref(s: &str, keyword: &str) -> Result<Option<i64>, String> {
    let t = s.trim();
    if t.is_empty() || t == keyword {
        return Ok(None);
    }
    let digits = t.strip_prefix("ps").unwrap_or(t);
    digits
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .map(Some)
        .ok_or_else(|| format!("invalid patchset reference {s:?} (want psN, N or {keyword})"))
}

/// `GET /api/reviews/{id}/since?from=psN|verdict&to=psN`
pub async fn review_since_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<SinceParams>,
) -> Result<Response, ApiError> {
    let (review, repo, _) = reviews::require_review(&state, id).await?;
    let from_ref = parse_ps_ref(params.from.as_deref().unwrap_or("verdict"), "verdict")
        .map_err(ApiError::bad_request)?;
    let to_ref = parse_ps_ref(params.to.as_deref().unwrap_or("latest"), "latest")
        .map_err(ApiError::bad_request)?;
    let verdict_ps = review.verdict_ps;
    let (from_ps, to_ps, from_source) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let (n, source) = match from_ref {
                Some(n) => (n, "ps"),
                None => match verdict_ps {
                    Some(n) => (n, "verdict"),
                    None => {
                        return Err(ApiError::bad_request(
                            "this review has no verdict, so from=verdict has no patchset to \
                             start from; pass from=psN",
                        ))
                    }
                },
            };
            let from_ps = reviews::resolve_ps(store, id, Some(&n.to_string()))?;
            let to_ps = match to_ref {
                Some(n) => reviews::resolve_ps(store, id, Some(&n.to_string()))?,
                None => reviews::resolve_ps(store, id, None)?,
            };
            Ok((from_ps, to_ps, source))
        })
        .await?;
    let ctx = GitCtx::resolve_entry(&state.store, repo).await;
    let report = crate::review_jobs::spawn_blocking_tracked(move || {
        compute_since(&ctx, id, &from_ps, &to_ps, from_source)
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(report)).into_response())
}

// --- route contract (invariant 15) --------------------------------------------

use crate::entities::RouteContract;

fn since_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("from", "verdict"), ("to", "latest")] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<SinceParams>(serde_json::Value::Object(map)).is_ok()
}

pub const SINCE_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/since",
    handler: "review_since::review_since_route",
    required_params: &[],
    params_accept_without: since_accept_without,
};

/// v0.44 F9's read, walked from BOTH sides (router registration in
/// `entities`' test, a CLI request builder in kb-code-cli's).
pub const V044_F9_ROUTES: &[RouteContract] = &[SINCE_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command as StdCommand;

    fn git(dir: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn ps_row(ps_number: i64, base: &str, tip: &str) -> ReviewPatchsetRow {
        ReviewPatchsetRow {
            id: ps_number,
            review_id: 1,
            ps_number,
            tip_sha: tip.to_string(),
            base_sha: base.to_string(),
            captured_at: 0,
        }
    }

    fn ctx(dir: &Path) -> GitCtx {
        GitCtx::work_tree_only(crate::git::roots::WorkTreeRoot::user_clone(dir))
    }

    const A0: &str = "a1\na2\na3\na4\na5\na6\na7\na8\na9\na10\n";

    /// main: c0 (a.txt, b.txt). Feature (ps1) edits a3 on c0. Main then
    /// moves: it edits a8 and b.txt (upstream work). Returns the repo, c0,
    /// ps1 tip and the new main head.
    fn fixture() -> (tempfile::TempDir, String, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        git(d, &["init", "-q", "-b", "main"]);
        git(d, &["config", "user.email", "t@example.com"]);
        git(d, &["config", "user.name", "T"]);
        std::fs::write(d.join("a.txt"), A0).unwrap();
        std::fs::write(d.join("b.txt"), "b1\nb2\n").unwrap();
        git(d, &["add", "."]);
        git(d, &["commit", "-q", "-m", "c0"]);
        let c0 = git_out(d, &["rev-parse", "HEAD"]);

        git(d, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(d.join("a.txt"), A0.replace("a3\n", "a3-author\n")).unwrap();
        git(d, &["commit", "-aq", "-m", "feature"]);
        let ps1_tip = git_out(d, &["rev-parse", "HEAD"]);

        git(d, &["checkout", "-q", "main"]);
        std::fs::write(d.join("a.txt"), A0.replace("a8\n", "a8-upstream\n")).unwrap();
        std::fs::write(d.join("b.txt"), "b1-upstream\nb2\n").unwrap();
        git(d, &["commit", "-aq", "-m", "upstream"]);
        let main_head = git_out(d, &["rev-parse", "HEAD"]);
        (tmp, c0, ps1_tip, main_head)
    }

    /// Rebase the feature commit onto `main_head` (cherry-pick onto a new
    /// branch), optionally with a further author commit on top.
    fn rebase(d: &Path, ps1_tip: &str, main_head: &str, extra_edit: bool) -> String {
        git(d, &["checkout", "-q", "-b", "feature2", main_head]);
        git(d, &["cherry-pick", ps1_tip]);
        if extra_edit {
            let cur = std::fs::read_to_string(d.join("a.txt")).unwrap();
            std::fs::write(d.join("a.txt"), cur.replace("a5\n", "a5-author2\n")).unwrap();
            git(d, &["commit", "-aq", "-m", "author edit"]);
        }
        git_out(d, &["rev-parse", "HEAD"])
    }

    #[test]
    fn a_pure_rebase_is_rebase_only_and_upstream_edits_are_not_counted() {
        let (tmp, c0, ps1_tip, main_head) = fixture();
        let d = tmp.path();
        let ps2_tip = rebase(d, &ps1_tip, &main_head, false);
        let (p1, p2) = (ps_row(1, &c0, &ps1_tip), ps_row(2, &main_head, &ps2_tip));
        // The tip-to-tip view is NOT clean: it lists the upstream edits.
        let tip_to_tip = git_out(d, &["diff", "--name-only", &ps1_tip, &ps2_tip]);
        assert!(tip_to_tip.contains("b.txt"), "the noise this fixes");

        let r = compute_since(&ctx(d), 1, &p1, &p2, "ps").expect("since");
        assert!(r.rebase_only);
        assert!(r.bases.moved);
        assert_eq!(
            r.author_delta,
            SinceAuthorDelta {
                new_hunks: 0,
                gone_hunks: 0,
                paths_changed: 0
            }
        );
        // Golden: exactly the one carried hunk, no b.txt, no a8.
        assert_eq!(
            serde_json::to_value(&r.paths).unwrap(),
            serde_json::json!([{"path": "a.txt", "carried": 1, "new": 0, "gone": 0}])
        );
        assert_eq!(r.schema, "kbc-review-since/1");
    }

    #[test]
    fn a_rebase_plus_one_edit_reports_exactly_that_hunk_as_new() {
        let (tmp, c0, ps1_tip, main_head) = fixture();
        let d = tmp.path();
        let ps2_tip = rebase(d, &ps1_tip, &main_head, true);
        let (p1, p2) = (ps_row(1, &c0, &ps1_tip), ps_row(2, &main_head, &ps2_tip));
        let r = compute_since(&ctx(d), 1, &p1, &p2, "ps").expect("since");
        assert!(!r.rebase_only);
        assert_eq!(
            serde_json::to_value(&r.paths).unwrap(),
            serde_json::json!([{"path": "a.txt", "carried": 1, "new": 1, "gone": 0}])
        );
        assert_eq!(r.author_delta.new_hunks, 1);
        assert_eq!(r.author_delta.paths_changed, 1);
        // The new hunk's tip-side range is the edited line (a5 -> line 5).
        let f = patchset_hunks(&ctx(d), &p1.base_sha, &p1.tip_sha).unwrap();
        let t = patchset_hunks(&ctx(d), &p2.base_sha, &p2.tip_sha).unwrap();
        assert_eq!(author_new_ranges(&f, &t)["a.txt"], vec![(5, 5)]);
    }

    #[test]
    fn a_dropped_change_is_gone() {
        let (tmp, c0, ps1_tip, main_head) = fixture();
        let d = tmp.path();
        // ps2 = main with NO author change at all.
        let (p1, p2) = (ps_row(1, &c0, &ps1_tip), ps_row(2, &main_head, &main_head));
        let r = compute_since(&ctx(d), 1, &p1, &p2, "ps").expect("since");
        assert_eq!(r.author_delta.gone_hunks, 1);
        assert!(!r.rebase_only);
    }

    #[test]
    fn identical_changes_are_counted_not_collapsed() {
        let h = |id: &str| HunkRef {
            id: id.into(),
            new_start: 1,
            new_lines: 1,
            binary: false,
        };
        let mut from = PathHunks::new();
        from.insert("a".into(), vec![h("x")]);
        let mut to = PathHunks::new();
        to.insert("a".into(), vec![h("x"), h("x")]);
        let paths = compare(&from, &to);
        // A set would call both copies carried; counts say one is new.
        assert_eq!(
            paths,
            vec![SincePath {
                path: "a".into(),
                carried: 1,
                new: 1,
                gone: 0
            }]
        );
    }

    fn big_map(hunks: usize) -> PathHunks {
        let mut m = PathHunks::new();
        m.insert(
            "a".into(),
            (0..hunks)
                .map(|i| HunkRef {
                    id: format!("{i:016x}"),
                    new_start: 1,
                    new_lines: 1,
                    binary: false,
                })
                .collect(),
        );
        m
    }

    /// F9b — the memo was bounded by entry COUNT only, so a handful of
    /// enormous patchsets could pin unbounded memory. A map over the
    /// per-entry byte budget is not remembered; the total byte budget
    /// evicts. Fails without the byte bounds (the 256-entry count never
    /// trips here).
    #[test]
    fn the_memo_is_bounded_in_bytes_not_only_in_entries() {
        let mut memo = Memo::default();
        let huge = Arc::new(big_map(MEMO_ENTRY_MAX_BYTES / 32));
        assert!(weigh(&huge) > MEMO_ENTRY_MAX_BYTES);
        assert!(!memo_put(&mut memo, ("b".into(), "t".into()), huge));
        assert!(memo.map.is_empty() && memo.bytes == 0);

        // Many individually-acceptable maps never exceed the total budget.
        let mid = big_map(MEMO_ENTRY_MAX_BYTES / 80);
        let w = weigh(&mid);
        assert!(w <= MEMO_ENTRY_MAX_BYTES);
        for i in 0..64 {
            assert!(memo_put(
                &mut memo,
                (format!("b{i}"), "t".into()),
                Arc::new(mid.clone())
            ));
            assert!(memo.bytes <= MEMO_TOTAL_MAX_BYTES, "after {i}");
        }
        assert!(memo.map.len() < 64, "the total budget must have evicted");
    }

    /// F9b — gone ranges are the mirror of new ranges, in the EARLIER
    /// tip's coordinates.
    #[test]
    fn a_dropped_hunk_yields_a_gone_range_in_the_earlier_coordinates() {
        let h = |id: &str, start: u32| HunkRef {
            id: id.into(),
            new_start: start,
            new_lines: 2,
            binary: false,
        };
        let mut from = PathHunks::new();
        from.insert("a".into(), vec![h("keep", 1), h("drop", 8)]);
        let mut to = PathHunks::new();
        to.insert("a".into(), vec![h("keep", 40)]);
        assert_eq!(author_gone_ranges(&from, &to)["a"], vec![(8, 9)]);
        assert!(author_new_ranges(&from, &to).is_empty());
    }

    #[test]
    fn split_file_diffs_reads_paths_with_spaces() {
        let text = "diff --git a/my file.txt b/my file.txt\nindex 1..2 100644\n--- a/my file.txt\n+++ b/my file.txt\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/z.txt b/z.txt\n@@ -0,0 +1 @@\n+q\n";
        let files = split_file_diffs(text);
        let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names, vec!["my file.txt", "z.txt"]);
        let m = hunks_by_path(text);
        assert_eq!(m["my file.txt"].len(), 1);
        assert_eq!(m["z.txt"].len(), 1);
    }

    #[test]
    fn ps_refs_parse_and_keywords_resolve_late() {
        assert_eq!(parse_ps_ref("ps3", "verdict"), Ok(Some(3)));
        assert_eq!(parse_ps_ref("3", "verdict"), Ok(Some(3)));
        assert_eq!(parse_ps_ref("verdict", "verdict"), Ok(None));
        assert_eq!(parse_ps_ref("latest", "latest"), Ok(None));
        assert!(parse_ps_ref("psx", "verdict").is_err());
        assert!(parse_ps_ref("0", "verdict").is_err());
        // `verdict` is only a keyword on the `from` side.
        assert!(parse_ps_ref("verdict", "latest").is_err());
    }
}
