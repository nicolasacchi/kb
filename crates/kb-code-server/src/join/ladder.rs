//! join/1 — the deterministic commit→session resolution ladder (W3.2).
//!
//! [`resolve_commit`] runs a FIXED, first-hit-wins sequence of arms —
//! resolved LOCALLY where possible, escalating to kb's federated session
//! data only when a local signal doesn't settle it:
//!
//! 1. **trailer** — the commit's own `Kb-Session:` trailer, from git's
//!    OFFICIAL last-paragraph trailer-block parse (`kb_core::vcs::
//!    resolve_commit`, the SAME shell-out kb's own `kb sessions capture`
//!    uses — see `crate::join::local`). Pure local; works with the kb
//!    daemon fully unreachable.
//! 2. **exact** — kb's `GET /api/sessions/by-commit` returns a
//!    `session_commits` row whose `sha`/`sha_full` equals our LOCALLY
//!    disambiguated full sha (see "prefix disambiguation" below).
//! 3. **fuzzy (subject)** — the commit's subject equals a commit-map row's
//!    `subject`, in a session whose captured commit's `repo_root` aligns
//!    with this repo.
//! 4. **squash** — folded into the ladder here, between fuzzy-subject and
//!    fuzzy-time-window (see the module-level doc below for why): a
//!    `Kb-Session:` trailer preserved ANYWHERE in the commit's body (a
//!    squashed/merged commit often concatenates original commits' full
//!    messages, trailers included, outside git's own last-paragraph
//!    heuristic) resolves at **confidence = exact** (`via =
//!    "squash-trailer"`); failing that, a subject-containment match against
//!    a repo-aligned commit-map row resolves at **confidence = fuzzy**
//!    (`via = "squash-subject"`).
//! 5. **fuzzy (time-window)** — a repo-aligned session whose captured
//!    commit's `started_at` falls within a fixed window of this commit's
//!    author-time. The broadest, riskiest arm — tried last.
//! 6. **none** — honest absence. `via` still names WHY: `"no-match"` (every
//!    arm ran, nothing hit), `"kb-unreachable"` (the daemon round-trip
//!    itself failed), or `"kb-disabled"` (`[kb_daemon] enabled = false`).
//!
//! # The confidence/via split
//!
//! `Confidence` is the STRICT 4-valued, golden-pinned enum
//! (`trailer|exact|fuzzy|none`) the design calls for. Squash resolutions
//! are NOT a 5th confidence value — they map onto the existing four
//! (`squash-trailer` → `exact`, `squash-subject` → `fuzzy`), and `via` is a
//! separate, free-form taxonomy label carrying which ARM actually fired.
//! This keeps `Confidence` stable (a client can switch on 4 cases forever)
//! while `via` stays extensible (a future arm needs no `Confidence` bump).
//!
//! # Repo scoping — "never join repo A's commit to repo B's session"
//!
//! Arms 3/4b/5 only ever consider a commit-map row whose `repo_root`
//! (V0025, captured server-side at `kb sessions capture` time) EQUALS this
//! repo's own canonical path ([`repo_aligned`]). A row with no `repo_root`
//! (an unresolved capture) or a DIFFERENT one is rejected outright — fail
//! closed. The design also names `cwd` as an alternative alignment signal;
//! this ladder does NOT consult it — see "Deviations" below.
//!
//! # Deviations from the literal design brief
//!
//! - **`cwd` alignment is not implemented.** The design says repo alignment
//!   may go through "`repo_root` (V0025) **or** cwd." This module is scoped
//!   to exactly the two kb HTTP endpoints [`crate::join::kb_client`] calls
//!   (`by-commit`/`commit-map`); NEITHER response shape carries a session's
//!   `cwd` (only each individual commit's own resolved `repo_root` —
//!   `CommitMatchOut`/`CommitMapRowOut` in `kb-server`). Adding a THIRD kb
//!   endpoint (or widening these two) to expose `cwd` was judged out of
//!   scope for this ladder; `repo_root` is also the more precise of the two
//!   signals (git-resolved at the moment of the commit, not a coarse
//!   session-wide working directory), so the safety property the design
//!   cares about ("never cross repos") is fully preserved either way.
//! - **The repo-scoped time window has no session END time.** kb's
//!   `commit-map` feed carries each row's session `started_at` only (no
//!   `ended_at` — the daemon's own `SessionRow.ended_at` isn't exposed by
//!   either endpoint this ladder is scoped to). [`WINDOW_BEFORE_SECS`] /
//!   [`WINDOW_AFTER_SECS`] are therefore a documented APPROXIMATION of a
//!   plausible session span around its start, not a measured window — this
//!   is exactly what makes this arm "fuzzy."

use crate::config::RepoEntry;
use crate::join::kb_client::{CommitMapRow, CommitMatch, KbClient, KbClientError};
use crate::join::local::{self, LocalCommit};
use crate::store::{CommitSessionRow, Store, StoreBlocking};
use serde::Serialize;
use std::sync::Arc;

/// join/1 — bump this (and the golden test below) only alongside a
/// deliberate, documented shape change.
pub const SCHEMA: &str = "join/1";

/// `fuzzy`/`none` cache rows go stale after this long, so a later capture
/// (the operator's in-flight session finishes and gets indexed) gets a
/// chance to upgrade a previously-approximate or absent resolution.
/// `trailer`/`exact` rows never expire — see [`Confidence::is_permanent`].
pub const TTL_SECS: i64 = 24 * 60 * 60;

/// The time-window arm's tolerance either side of a candidate session's
/// `started_at` — see the module doc's "Deviations" section.
const WINDOW_BEFORE_SECS: i64 = 5 * 60;
const WINDOW_AFTER_SECS: i64 = 12 * 60 * 60;

/// Minimum trimmed length a squash "containment" match's subject (on
/// EITHER side of the comparison) must clear — guards against a trivial
/// short subject (`"wip"`, `"fix"`) false-positiving across unrelated
/// commits that happen to share a common word.
const SQUASH_SUBJECT_MIN_LEN: usize = 12;

const VIA_COMMIT_TRAILER: &str = "commit-trailer";
const VIA_BY_COMMIT: &str = "by-commit";
const VIA_SUBJECT: &str = "subject";
const VIA_SQUASH_TRAILER: &str = "squash-trailer";
const VIA_SQUASH_SUBJECT: &str = "squash-subject";
const VIA_TIME_WINDOW: &str = "time-window";
// The three "honest absence" vias are `pub(crate)`: `provenance::story`'s
// attention-gap beat maps them onto its two gap REASONS (`no-match` ⇒ "no
// captured session recorded", `kb-unreachable`/`kb-disabled` ⇒ "the join
// itself was unavailable") and single-sourcing the strings here keeps that
// mapping from silently drifting when a via is renamed.
pub(crate) const VIA_NO_MATCH: &str = "no-match";
pub(crate) const VIA_KB_UNREACHABLE: &str = "kb-unreachable";
pub(crate) const VIA_KB_DISABLED: &str = "kb-disabled";

/// The strict 4-valued, golden-pinned confidence — see the module doc's
/// "confidence/via split."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Trailer,
    Exact,
    Fuzzy,
    None,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trailer => "trailer",
            Self::Exact => "exact",
            Self::Fuzzy => "fuzzy",
            Self::None => "none",
        }
    }

    /// Round-trip from the `commit_sessions.confidence` TEXT column.
    /// `None` (the Rust `Option`, not this enum's `None` variant) on any
    /// unrecognized value — a cache row this build doesn't understand is
    /// treated as a miss (`resolve_commit` recomputes) rather than a panic.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "trailer" => Some(Self::Trailer),
            "exact" => Some(Self::Exact),
            "fuzzy" => Some(Self::Fuzzy),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// `trailer`/`exact` cache rows never expire (a commit's own trailer,
    /// or kb's own indexed sha match, can't become MORE true later for a
    /// fixed sha); `fuzzy`/`none` are TTL'd ([`TTL_SECS`]) — see the
    /// migration's doc.
    fn is_permanent(self) -> bool {
        matches!(self, Self::Trailer | Self::Exact)
    }
}

/// join/1's response shape — see the module doc. Every enrichment field
/// (`session_id`/`kb`/`display_name`/`started_at`) is independently
/// optional: a `trailer` hit with the kb daemon unreachable carries
/// `session_id` alone; a `fuzzy`/`squash-*` hit built from the commit-map
/// feed never carries `display_name` (that feed omits it — see
/// `kb_client::CommitMapRow`'s doc); a `none` row carries none of them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attribution {
    pub schema: &'static str,
    pub confidence: Confidence,
    pub via: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// The canonical (locally disambiguated, full-hex when resolvable) sha
    /// this attribution is FOR — echoed back so a caller that passed a
    /// short prefix can see what it actually resolved to.
    pub sha: String,
}

impl Attribution {
    fn none(sha: String, via: &'static str) -> Self {
        Self {
            schema: SCHEMA,
            confidence: Confidence::None,
            via: via.to_string(),
            session_id: None,
            kb: None,
            display_name: None,
            started_at: None,
            sha,
        }
    }
}

/// `true` if `s` is a plausible (possibly abbreviated) git sha — the
/// route's own input gate, mirroring `kb_core::vcs`'s private
/// `is_hex_sha` bound (4–64 hex chars). Deliberately more permissive than
/// kb-server's `by-commit` route's own 7-char floor: THIS route's first arm
/// is local git resolution, which can safely disambiguate a short prefix
/// against a single known repo the way a cross-corpus server-side `LIKE`
/// prefix search cannot.
pub fn is_plausible_sha(s: &str) -> bool {
    let s = s.trim();
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `fuzzy`/`none` rows go stale after [`TTL_SECS`]; `trailer`/`exact` never
/// do. `now`/`resolved_at` are both unix seconds.
fn is_fresh(confidence: Confidence, resolved_at: i64, now: i64) -> bool {
    confidence.is_permanent() || now.saturating_sub(resolved_at) < TTL_SECS
}

fn via_for_kb_error(e: &KbClientError) -> &'static str {
    match e {
        KbClientError::Disabled => VIA_KB_DISABLED,
        _ => VIA_KB_UNREACHABLE,
    }
}

fn attribution_to_row(a: &Attribution, resolved_at: i64) -> CommitSessionRow {
    CommitSessionRow {
        confidence: a.confidence.as_str().to_string(),
        via: a.via.clone(),
        session_id: a.session_id.clone(),
        kb: a.kb.clone(),
        display_name: a.display_name.clone(),
        started_at: a.started_at,
        resolved_at,
    }
}

fn row_to_attribution(sha: &str, row: CommitSessionRow) -> Option<Attribution> {
    let confidence = Confidence::parse(&row.confidence)?;
    Some(Attribution {
        schema: SCHEMA,
        confidence,
        via: row.via,
        session_id: row.session_id,
        kb: row.kb,
        display_name: row.display_name,
        started_at: row.started_at,
        sha: sha.to_string(),
    })
}

/// `true` if `row`'s own resolved `repo_root` equals `repo_path` — see the
/// module doc's "Repo scoping" section. `repo_path` is expected already
/// canonical (`RepoEntry.path`, canonicalized by `kb-code.toml`'s
/// `resolve_repos` — invariant #27); a trailing-slash difference is
/// tolerated, nothing else.
fn repo_aligned(row: &CommitMapRow, repo_path: &str) -> bool {
    match row.repo_root.as_deref() {
        Some(root) => root.trim_end_matches('/') == repo_path.trim_end_matches('/'),
        None => false,
    }
}

/// Deterministic tie-break for "more than one candidate matched": newest
/// `started_at` first, then `session_id` ascending — shared by the
/// fuzzy-subject and squash-subject arms (the time-window arm uses its own
/// proximity-based order — see [`time_window_match`]).
fn pick_newest(mut candidates: Vec<&CommitMapRow>) -> Option<&CommitMapRow> {
    candidates.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    candidates.into_iter().next()
}

/// Arm 3 — exact subject equality, repo-scoped.
fn fuzzy_subject_match<'a>(
    snapshot: &'a [CommitMapRow],
    repo_path: &str,
    local_subject: &str,
) -> Option<&'a CommitMapRow> {
    let local_subject = local_subject.trim();
    if local_subject.is_empty() {
        return None;
    }
    let candidates: Vec<&CommitMapRow> = snapshot
        .iter()
        .filter(|r| repo_aligned(r, repo_path))
        .filter(|r| r.subject.as_deref().map(str::trim) == Some(local_subject))
        .collect();
    pick_newest(candidates)
}

/// Arm 4b — subject CONTAINMENT (either direction), repo-scoped. The
/// squash-specific relaxation of arm 3's strict equality: a squash
/// commit's own subject often summarizes several originals (or vice
/// versa), so equality would never hit.
fn squash_subject_match<'a>(
    snapshot: &'a [CommitMapRow],
    repo_path: &str,
    local_subject: &str,
) -> Option<&'a CommitMapRow> {
    let local_subject = local_subject.trim();
    if local_subject.len() < SQUASH_SUBJECT_MIN_LEN {
        return None;
    }
    let candidates: Vec<&CommitMapRow> = snapshot
        .iter()
        .filter(|r| repo_aligned(r, repo_path))
        .filter(|r| {
            let Some(subject) = r.subject.as_deref().map(str::trim) else {
                return false;
            };
            subject.len() >= SQUASH_SUBJECT_MIN_LEN
                && (local_subject.contains(subject) || subject.contains(local_subject))
        })
        .collect();
    pick_newest(candidates)
}

/// Arm 5 — repo-scoped time window. Candidates are ordered by PROXIMITY to
/// `author_time` (not recency) — a broad [`WINDOW_AFTER_SECS`] can let
/// several sessions qualify, and "closest start to when the commit was
/// authored" is the more meaningful tie-break for "which session was
/// active."
fn time_window_match<'a>(
    snapshot: &'a [CommitMapRow],
    repo_path: &str,
    author_time: i64,
) -> Option<&'a CommitMapRow> {
    let mut candidates: Vec<&CommitMapRow> = snapshot
        .iter()
        .filter(|r| repo_aligned(r, repo_path))
        .filter(|r| {
            let lo = r.started_at - WINDOW_BEFORE_SECS;
            let hi = r.started_at + WINDOW_AFTER_SECS;
            (lo..=hi).contains(&author_time)
        })
        .collect();
    candidates.sort_by(|a, b| {
        (a.started_at - author_time)
            .abs()
            .cmp(&(b.started_at - author_time).abs())
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    candidates.into_iter().next()
}

/// `true` if `m` (a `by-commit` match) is FOR `canonical_sha` — both of
/// kb's own two sha columns are checked since an unresolved capture only
/// ever populates `sha` (short, transcript-detected), never `sha_full`.
fn matches_canonical_sha(m: &CommitMatch, canonical_sha: &str) -> bool {
    m.sha_full.as_deref() == Some(canonical_sha) || m.sha.as_deref() == Some(canonical_sha)
}

/// `(kb, started_at)` for the FIRST commit-map row belonging to
/// `session_id` — the squash-trailer sub-arm's best-effort enrichment (that
/// arm's session id comes from a body scan, not from a `by-commit`/
/// commit-map MATCH, so there is no `CommitMatch`/`CommitMapRow` in hand
/// yet to read these off directly). No `display_name` — the commit-map feed
/// never carries one (see `CommitMapRow`'s doc).
fn enrich_from_snapshot(
    session_id: &str,
    snapshot: &[CommitMapRow],
) -> (Option<String>, Option<i64>) {
    snapshot
        .iter()
        .find(|r| r.session_id == session_id)
        .map(|r| (Some(r.kb.clone()), Some(r.started_at)))
        .unwrap_or((None, None))
}

/// `(kb, display_name, started_at)` for `session_id`, via a `by-commit`
/// lookup on `canonical_sha` — the trailer arm's best-effort enrichment.
/// Silently degrades to `(None, None, None)` on ANY failure (disabled,
/// unreachable, no matching row) — the trailer arm's CONFIDENCE never
/// depends on this succeeding (see the module doc, arm 1).
async fn enrich_via_by_commit(
    session_id: &str,
    canonical_sha: &str,
    kb_client: &KbClient,
) -> (Option<String>, Option<String>, Option<i64>) {
    let Ok(matches) = kb_client.by_commit(canonical_sha).await else {
        return (None, None, None);
    };
    matches
        .iter()
        .find(|m| m.session_id == session_id)
        .map(|m| {
            (
                Some(m.kb.clone()),
                Some(m.display_name.clone()),
                Some(m.started_at),
            )
        })
        .unwrap_or((None, None, None))
}

/// Runs arms 1–6 in order over an already-locally-resolved commit — split
/// out from [`resolve_commit`] so the cache read-through wrapping it stays
/// separate from the ladder logic itself.
async fn compute_ladder(
    repo_path: &str,
    canonical_sha: &str,
    local: &LocalCommit,
    kb_client: &KbClient,
) -> Attribution {
    // Arm 1: trailer — pure local, works with the daemon fully unreachable.
    if let Some(session_id) = local::kb_session_trailer(&local.trailers) {
        let (kb, display_name, started_at) =
            enrich_via_by_commit(&session_id, canonical_sha, kb_client).await;
        return Attribution {
            schema: SCHEMA,
            confidence: Confidence::Trailer,
            via: VIA_COMMIT_TRAILER.to_string(),
            session_id: Some(session_id),
            kb,
            display_name,
            started_at,
            sha: canonical_sha.to_string(),
        };
    }

    // Arm 2: exact — kb's own indexed sha match. A failure here (disabled
    // or unreachable) is total: per the design, everything past arm 1
    // degrades to `none` rather than attempting the local-only squash-
    // trailer scan anyway (see the module doc / invariant point 5).
    let matches = match kb_client.by_commit(canonical_sha).await {
        Ok(m) => m,
        Err(e) => return Attribution::none(canonical_sha.to_string(), via_for_kb_error(&e)),
    };
    if let Some(m) = matches
        .iter()
        .find(|m| matches_canonical_sha(m, canonical_sha))
    {
        return Attribution {
            schema: SCHEMA,
            confidence: Confidence::Exact,
            via: VIA_BY_COMMIT.to_string(),
            session_id: Some(m.session_id.clone()),
            kb: Some(m.kb.clone()),
            display_name: Some(m.display_name.clone()),
            started_at: Some(m.started_at),
            sha: canonical_sha.to_string(),
        };
    }

    // Arms 3-5 all read the same commit-map snapshot.
    let snapshot = match kb_client.commit_map_snapshot(false).await {
        Ok(s) => s,
        Err(e) => return Attribution::none(canonical_sha.to_string(), via_for_kb_error(&e)),
    };

    // Arm 3: fuzzy — resolved-subject equality, repo-scoped.
    if let Some(subject) = local.subject.as_deref() {
        if let Some(m) = fuzzy_subject_match(&snapshot, repo_path, subject) {
            return Attribution {
                schema: SCHEMA,
                confidence: Confidence::Fuzzy,
                via: VIA_SUBJECT.to_string(),
                session_id: Some(m.session_id.clone()),
                kb: Some(m.kb.clone()),
                display_name: None,
                started_at: Some(m.started_at),
                sha: canonical_sha.to_string(),
            };
        }
    }

    // Arm 4: squash — preserved body trailer (exact), else subject
    // containment (fuzzy), repo-agnostic for the trailer half (a preserved
    // `Kb-Session:` is its own proof regardless of repo alignment).
    if let Some(session_id) = local::kb_session_body_scan(&local.raw_message) {
        let (kb, started_at) = enrich_from_snapshot(&session_id, &snapshot);
        return Attribution {
            schema: SCHEMA,
            confidence: Confidence::Exact,
            via: VIA_SQUASH_TRAILER.to_string(),
            session_id: Some(session_id),
            kb,
            display_name: None,
            started_at,
            sha: canonical_sha.to_string(),
        };
    }
    if let Some(subject) = local.subject.as_deref() {
        if let Some(m) = squash_subject_match(&snapshot, repo_path, subject) {
            return Attribution {
                schema: SCHEMA,
                confidence: Confidence::Fuzzy,
                via: VIA_SQUASH_SUBJECT.to_string(),
                session_id: Some(m.session_id.clone()),
                kb: Some(m.kb.clone()),
                display_name: None,
                started_at: Some(m.started_at),
                sha: canonical_sha.to_string(),
            };
        }
    }

    // Arm 5: fuzzy — repo-scoped time window. Needs an author-time; a
    // commit gix couldn't read (see `join::local`) simply can't run this.
    if let Some(author_time) = local.author_time {
        if let Some(m) = time_window_match(&snapshot, repo_path, author_time) {
            return Attribution {
                schema: SCHEMA,
                confidence: Confidence::Fuzzy,
                via: VIA_TIME_WINDOW.to_string(),
                session_id: Some(m.session_id.clone()),
                kb: Some(m.kb.clone()),
                display_name: None,
                started_at: Some(m.started_at),
                sha: canonical_sha.to_string(),
            };
        }
    }

    Attribution::none(canonical_sha.to_string(), VIA_NO_MATCH)
}

/// The join ladder's public entrypoint — `crate::routes::join_commit`
/// (`GET /api/join/commit?repo=&sha=`) and `kb-code join` both call this
/// directly. Reads through `store`'s `commit_sessions` cache
/// ([`is_fresh`]); on a miss/stale row, runs [`compute_ladder`] and writes
/// the result back (best-effort — a cache-write failure is logged and
/// otherwise ignored, never surfaced to the caller: the resolution itself
/// already succeeded).
pub async fn resolve_commit(
    repo: &RepoEntry,
    repo_id: i64,
    sha: &str,
    store: &Arc<Store>,
    kb_client: &KbClient,
) -> Attribution {
    let local = local::resolve_local(&repo.path, sha).await;
    let canonical_sha = local
        .sha_full
        .clone()
        .unwrap_or_else(|| sha.trim().to_ascii_lowercase());
    let now = chrono::Utc::now().timestamp();

    // 2026-08-31 incident (store.rs module doc): the cache read and the
    // (later) cache write are separate blocking-pool round trips — real
    // async work (`compute_ladder`'s kb HTTP calls) runs between them, so
    // they cannot share one closure.
    let cached = {
        let canonical_sha_c = canonical_sha.clone();
        store
            .run_blocking(move |store| store.get_commit_session(repo_id, &canonical_sha_c))
            .await
    };
    if let Ok(Some(cached)) = cached {
        if let Some(confidence) = Confidence::parse(&cached.confidence) {
            if is_fresh(confidence, cached.resolved_at, now) {
                if let Some(attribution) = row_to_attribution(&canonical_sha, cached) {
                    return attribution;
                }
            }
        }
    }

    let repo_path = repo.path.to_string_lossy().to_string();
    let attribution = compute_ladder(&repo_path, &canonical_sha, &local, kb_client).await;
    let row = attribution_to_row(&attribution, now);
    let canonical_sha_c = canonical_sha.clone();
    let write_result = store
        .run_blocking(move |store| store.upsert_commit_session(repo_id, &canonical_sha_c, &row))
        .await;
    if let Err(e) = write_result {
        tracing::warn!(
            repo = %repo.name, sha = %canonical_sha, error = %e,
            "join ladder: failed to write the commit_sessions cache row (resolution itself still succeeded)"
        );
    }
    attribution
}

#[cfg(test)]
#[path = "ladder_tests.rs"]
mod tests;
