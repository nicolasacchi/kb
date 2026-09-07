//! V73-K3 — the hunk↔turn join (design D18 + D25's Track O row, "hunk↔turn
//! reading captured transcripts — loopback tier").
//!
//! "Which agent turn wrote this hunk?" is a provenance question the
//! existing session↔commit join cannot answer: that join is per COMMIT, and
//! a commit is many hunks by many turns. This module answers it per HUNK,
//! and — because a wrong answer here would attribute code to the wrong
//! author — it answers in TWO TIERS and refuses outright rather than
//! guessing a third time.
//!
//! # The two tiers, and the third answer
//!
//! * **`exact`** — a captured `Edit`/`MultiEdit`/`Write` tool_use in a
//!   session whose COMMIT JOIN includes a commit whose OWN diff contains
//!   this exact hunk id, whose `file_path` resolves to this hunk's path,
//!   and whose `old_string`/`new_string` appear BYTE-FOR-BYTE in the
//!   hunk's removed/added text. Three independent witnesses: the bytes,
//!   the path, and the commit.
//! * **`likely`** — the bytes match, but one of the other two witnesses is
//!   missing: the path moved, or no commit join reaches this session, or
//!   the commit that carries the hunk could not be named exactly (see
//!   [`CommitBasis`]). Evidence, not proof.
//! * **not claimed** — anything else. An EMPTY list with a `reason`,
//!   never a fuzzy third tier. This crate's release blocker is a wrong
//!   `exact`, and its stated law is that an uncertain match is an honest
//!   orphan; a "possible" tier would be a place for both to hide.
//!
//! The byte match is guarded by [`MIN_MATCH_BYTES`]. A one-line
//! `old_string` of `end` is contained in half the hunks in a Ruby
//! repository, so a match below the floor is NOT claimed at any tier — the
//! same reason `mentions::MIN_MENTION_LEN` exists on the kb side.
//!
//! # What is never stored
//!
//! Nothing. The diff is re-derived, the hunk ids are re-minted, the tool
//! inputs are re-read from the JSONL, the tiers are re-computed. There is
//! no `hunk_turns` table and there must not be one: a stored attribution
//! would be indistinguishable from a fresh one the moment the branch is
//! rebased (root invariant #2's "kb-code mints classes, nothing is
//! cached"; `rails/1`'s invariant 20(a) for the same reason).
//!
//! # Sensitivity
//!
//! `old_string`/`new_string` are file content out of a raw transcript, so
//! this route is LOOPBACK-ONLY (D19's `raw-transcript` class, root
//! invariant #4's ethos, the same gate `/search/transcripts` and
//! `/session-diff` already ride). The bearer-visible half D9 describes
//! (session id, turn id, tool name, file/range) is what the TIMELINE
//! surfaces; the surrounding assistant text never leaves loopback and is
//! not returned here either.

use crate::join::ladder;
use crate::review_hunks::{self, DiffHunk};
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{self, StoreBlocking};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const SCHEMA: &str = "kbc-hunk-turns/1";

/// The tools whose inputs describe a file edit. A CLOSED set: a tool this
/// list does not name is not inspected at all, so a future tool cannot
/// silently start producing attributions under a shape nobody checked.
pub const EDIT_TOOLS: &[&str] = &["Edit", "MultiEdit", "Write", "NotebookEdit"];

/// The byte floor below which a textual match proves nothing. Applies to
/// the LONGER of the two sides, so a genuine one-line rename with a long
/// enough line still qualifies while `end`→`end` never does.
pub const MIN_MATCH_BYTES: usize = 24;

/// How many commits in the patchset range this join will probe for the
/// hunk before it gives up naming one exactly. Past the budget the basis
/// degrades to [`CommitBasis::PathInRange`] — which caps every tier at
/// `likely` — and the response says so.
pub const MAX_COMMIT_PROBES: usize = 60;

/// How many candidate sessions are inspected. Past it the response is
/// `partial` and names the budget.
pub const MAX_SESSIONS: usize = 40;

/// How many turns per session are inspected.
pub const MAX_TURNS_PER_SESSION: usize = 2_000;

pub const TIER_EXACT: &str = "exact";
pub const TIER_LIKELY: &str = "likely";

/// How well this join could name the commit that carries the hunk. It is
/// the CEILING on the tier: you cannot be `exact` about which session made
/// a change if you cannot say which commit made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitBasis {
    /// A commit in the range produced this EXACT hunk id in its own diff —
    /// a content address, so this is proof, not proximity.
    HunkExact,
    /// No single commit's diff reproduced the hunk (the cumulative diff
    /// merged two commits' edits, or the probe budget ran out), so the
    /// commit set is "everything in the range that touched this path".
    /// Wider, and therefore never enough for `exact`.
    PathInRange,
    /// The range yielded no commits at all for this path.
    None,
}

impl CommitBasis {
    pub fn caption(self) -> &'static str {
        match self {
            Self::HunkExact => {
                "a commit in this patchset's range reproduces this hunk's content address in its \
                 own diff — the commit is named, not guessed"
            }
            Self::PathInRange => {
                "no single commit's diff reproduces this hunk (its edits were merged across \
                 commits, or the probe budget was reached), so the commit set is every commit in \
                 range that touched this path — too wide for `exact`, so every match here is \
                 capped at `likely`"
            }
            Self::None => "no commit in this patchset's range touched this hunk's path",
        }
    }
}

/// One matched turn. Deliberately carries NO transcript prose: the tool
/// name, the path and the ranges are the bearer-visible half D9 describes,
/// and the assistant text around the edit stays where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TurnMatch {
    /// kb's own `t-<uuid12>` turn id, minted through kb-core's ONE
    /// derivation (`kb_core::sessions::view::turn_id_from_uuid`) so a link
    /// into `kb sessions read` addresses the same turn kb does.
    pub turn_id: String,
    pub session_id: String,
    pub uuid: String,
    /// Unix MILLIseconds, as `transcript_turns.ts` stores it.
    pub ts: i64,
    pub tool: String,
    /// The path the tool wrote, repo-relative when it is inside this repo.
    pub path: String,
    /// `exact` | `likely` — see the module doc.
    pub tier: &'static str,
    /// Why this tier and not the other. Always present.
    pub why: String,
    /// The commit this turn's session is joined to, when the join reached
    /// one. `None` on a `likely` match with no commit join.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// How the session↔commit join was made (`trailer`/`exact`/`fuzzy`),
    /// echoed from the existing ladder rather than re-derived.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_via: Option<String>,
    /// How many bytes of `old_string`/`new_string` matched — the evidence
    /// behind the claim, so a reader can judge it.
    pub matched_bytes: usize,
    /// `kb sessions read <session> --turn <turn_id>` — the read this
    /// daemon does not perform itself (kb owns transcript presentation).
    pub kb_read: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TurnsOut {
    pub schema: &'static str,
    pub review_id: i64,
    pub repo: String,
    pub ps_number: i64,
    pub hunk_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub commit_basis: CommitBasis,
    pub commit_basis_caption: &'static str,
    pub commits: Vec<String>,
    pub turns: Vec<TurnMatch>,
    /// Present when the list is empty — the honest reason, never silence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// True when a budget stopped the search before it was exhaustive.
    pub partial: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The kb sibling's own lane state — `ok` | `degraded`. A degrade
    /// weakens the commit join (and therefore caps tiers at `likely`); it
    /// never fails the request (the v6.0 per-lane precedent).
    pub kb_lane: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb_lane_reason: Option<String>,
}

// --- the pure core ---------------------------------------------------------

/// One candidate edit, already extracted from a transcript turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateEdit {
    pub session_id: String,
    pub uuid: String,
    pub ts: i64,
    pub tool: String,
    /// The path the tool named, as written (absolute or relative).
    pub raw_path: String,
    /// The repo-relative path, when it could be derived.
    pub rel_path: Option<String>,
    /// `(old_string, new_string)` pairs — one for `Edit`, several for
    /// `MultiEdit`, and `("", content)` for a `Write`.
    pub edits: Vec<(String, String)>,
}

/// The per-edit verdict. Pure, and the ONLY place a tier is minted.
///
/// `commit_joined` is "this edit's session is joined to a commit that
/// carries this hunk"; `basis` is how well that commit could be named.
/// `exact` needs BOTH, plus a path match, plus a byte match over the floor.
pub fn classify(
    hunk_path: &str,
    removed: &str,
    added: &str,
    edit: &CandidateEdit,
    commit_joined: Option<(&str, &str)>,
    basis: CommitBasis,
) -> Option<(&'static str, String, usize)> {
    let matched = best_match_bytes(removed, added, &edit.edits)?;
    let path_matches = edit.rel_path.as_deref() == Some(hunk_path);
    match (path_matches, commit_joined, basis) {
        (true, Some(_), CommitBasis::HunkExact) => Some((
            TIER_EXACT,
            "the tool wrote this path, its old/new strings appear byte-for-byte in this hunk, \
             and its session is joined to the commit whose diff reproduces this hunk"
                .to_string(),
            matched,
        )),
        (true, Some(_), _) => Some((
            TIER_LIKELY,
            format!(
                "byte-for-byte content match on this path, but the commit carrying this hunk \
                 could not be named exactly ({})",
                basis_word(basis)
            ),
            matched,
        )),
        (true, None, _) => Some((
            TIER_LIKELY,
            "byte-for-byte content match on this path, but no commit in this range joins back \
             to the session that made it"
                .to_string(),
            matched,
        )),
        (false, _, _) => Some((
            TIER_LIKELY,
            format!(
                "byte-for-byte content match, but the tool wrote {:?} and this hunk is in {:?} \
                 — the file moved, or the same change was made elsewhere",
                edit.raw_path, hunk_path
            ),
            matched,
        )),
    }
}

fn basis_word(basis: CommitBasis) -> &'static str {
    match basis {
        CommitBasis::HunkExact => "hunk_exact",
        CommitBasis::PathInRange => "path_in_range",
        CommitBasis::None => "none",
    }
}

/// The byte-match rule, isolated so it can be tested and argued about on
/// its own.
///
/// An edit matches when its `old_string` occurs verbatim in the hunk's
/// removed text AND its `new_string` occurs verbatim in the added text —
/// containment rather than equality, because git's `-U3` window routinely
/// groups several edits into one hunk, so an individual `Edit`'s strings
/// are a SUBSET of the hunk's changed region. A `Write` (empty
/// `old_string`) matches on the added side alone, which is what creating a
/// file looks like.
///
/// Returns the matched byte count (the evidence's own size), or `None`
/// when nothing matched or the match was under [`MIN_MATCH_BYTES`].
pub fn best_match_bytes(removed: &str, added: &str, edits: &[(String, String)]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (old, new) in edits {
        let old_t = old.trim_end_matches('\n');
        let new_t = new.trim_end_matches('\n');
        let old_ok = old_t.is_empty() || removed.contains(old_t);
        let new_ok = new_t.is_empty() || added.contains(new_t);
        if !old_ok || !new_ok {
            continue;
        }
        if old_t.is_empty() && new_t.is_empty() {
            continue;
        }
        let bytes = old_t.len().max(new_t.len());
        if bytes < MIN_MATCH_BYTES {
            continue;
        }
        best = Some(best.map_or(bytes, |b: usize| b.max(bytes)));
    }
    best
}

/// Pull the `(old, new)` pairs out of one tool_use `input` object. Total —
/// an input shape this does not recognise yields an empty list, and an
/// empty list can never match.
pub fn edits_from_input(tool: &str, input: &serde_json::Value) -> Vec<(String, String)> {
    let s = |v: Option<&serde_json::Value>| v.and_then(|x| x.as_str()).unwrap_or("").to_string();
    match tool {
        "Edit" => vec![(s(input.get("old_string")), s(input.get("new_string")))],
        "NotebookEdit" => vec![(s(input.get("old_source")), s(input.get("new_source")))],
        "Write" => vec![(String::new(), s(input.get("content")))],
        "MultiEdit" => input
            .get("edits")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|e| (s(e.get("old_string")), s(e.get("new_string"))))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The path a tool_use named, made repo-relative when it lies inside the
/// repo. Absolute paths are the common case (the harness writes them);
/// a relative one is taken as already repo-relative.
pub fn rel_path_in_repo(repo_root: &Path, raw: &str) -> Option<String> {
    let p = Path::new(raw);
    if p.is_relative() {
        return Some(raw.replace('\\', "/"));
    }
    p.strip_prefix(repo_root)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

// --- the impure shell ------------------------------------------------------

/// Every hunk of the patchset's diff, keyed by its `kbc-hunkid/1` address.
/// One `git diff` per changed file — the same read `GET /api/diff` makes,
/// through the same validated-revspec gate.
pub fn locate_hunk(
    repo_root: &Path,
    base_sha: &str,
    tip_sha: &str,
    paths: &[String],
    wanted: &str,
) -> Option<(String, DiffHunk)> {
    let base = crate::git::Revspec::trusted(base_sha.to_string());
    let tip = crate::git::Revspec::trusted(tip_sha.to_string());
    for path in paths {
        let Ok(text) = crate::diff::diff_file(repo_root, &base, Some(&tip), path) else {
            continue;
        };
        for hunk in review_hunks::parse_unified_diff(&text).hunks {
            if review_hunks::hunk_id(path, &hunk) == wanted {
                return Some((path.clone(), hunk));
            }
        }
    }
    None
}

/// The commits in `base..tip` that touched `path`, newest-first, capped.
/// Returns `(commits, truncated)`.
pub fn commits_touching(
    repo_root: &Path,
    base_sha: &str,
    tip_sha: &str,
    path: &str,
) -> (Vec<String>, bool) {
    let range = format!("{base_sha}..{tip_sha}");
    let Ok(out) =
        crate::history::run_git_raw(repo_root, &["log", "--format=%H", &range, "--", path])
    else {
        return (Vec::new(), false);
    };
    let all: Vec<String> = String::from_utf8_lossy(&out)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let truncated = all.len() > MAX_COMMIT_PROBES;
    (all.into_iter().take(MAX_COMMIT_PROBES).collect(), truncated)
}

/// Which of `commits` reproduces `wanted` in its OWN diff for `path`. This
/// is what turns "a commit in the range" into "the commit", and it is a
/// content address comparison, never a heuristic.
pub fn commits_carrying_hunk(
    repo_root: &Path,
    commits: &[String],
    path: &str,
    wanted: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for sha in commits {
        let parent = crate::git::Revspec::trusted(format!("{sha}^"));
        let this = crate::git::Revspec::trusted(sha.clone());
        let Ok(text) = crate::diff::diff_file(repo_root, &parent, Some(&this), path) else {
            continue;
        };
        if review_hunks::parse_unified_diff(&text)
            .hunks
            .iter()
            .any(|h| review_hunks::hunk_id(path, h) == wanted)
        {
            out.push(sha.clone());
        }
    }
    out
}

/// Collect the candidate edits from the sessions that touched `path` plus
/// the sessions the commit join names. Reads raw JSONL — loopback only.
#[allow(clippy::too_many_arguments)]
pub fn candidate_edits(
    store: &store::Store,
    transcripts_root: &Path,
    repo_root: &Path,
    abs_path: &str,
    extra_sessions: &BTreeSet<String>,
) -> (Vec<CandidateEdit>, bool) {
    let mut sessions: BTreeSet<String> = extra_sessions.clone();
    if let Ok(hits) = store.transcript_sessions_touching_path(abs_path, MAX_SESSIONS) {
        for h in hits {
            sessions.insert(h.session_id);
        }
    }
    let partial = sessions.len() > MAX_SESSIONS;
    let mut out: Vec<CandidateEdit> = Vec::new();
    for sid in sessions.iter().take(MAX_SESSIONS) {
        let Ok(turns) = store.transcript_turns_for_session(sid) else {
            continue;
        };
        for row in turns.into_iter().take(MAX_TURNS_PER_SESSION) {
            if row.kind != "tool_use" {
                continue;
            }
            let Some(tool) = row.tool_name.clone() else {
                continue;
            };
            if !EDIT_TOOLS.contains(&tool.as_str()) {
                continue;
            }
            let Some(raw_path) = row.file_paths.first().cloned() else {
                continue;
            };
            let Some(input) = crate::transcripts::search::read_turn_tool_input(
                transcripts_root,
                &row.src_file,
                &row.uuid,
                Some(tool.as_str()),
                row.byte_offset,
                row.byte_len,
            ) else {
                continue;
            };
            let edits = edits_from_input(&tool, &input);
            if edits.is_empty() {
                continue;
            }
            out.push(CandidateEdit {
                session_id: row.session_id,
                uuid: row.uuid,
                ts: row.ts,
                tool,
                rel_path: rel_path_in_repo(repo_root, &raw_path),
                raw_path,
                edits,
            });
        }
    }
    (out, partial)
}

/// `GET /api/reviews/{id}/hunks/{hunk}/turns` — LOOPBACK-ONLY. A thin
/// wrapper: the join itself is [`turns_for_hunk`], so the timeline's own
/// `turns` lane runs the SAME code rather than a second implementation.
pub async fn hunk_turns_route(
    axum::extract::State(state): axum::extract::State<SharedState>,
    axum::extract::Path((id, hunk)): axum::extract::Path<(i64, String)>,
    axum::extract::Query(params): axum::extract::Query<HunkTurnsParams>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    use axum::http::header;
    use axum::Json;
    let out = turns_for_hunk(&state, id, &hunk, params.ps.as_deref()).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// The join. Loopback enforcement lives at the ROUTER for the route, and
/// at the caller for the timeline lane — this function is the engine and
/// gates nothing itself, exactly like `review_comments`'s resolver.
pub async fn turns_for_hunk(
    state: &SharedState,
    id: i64,
    hunk: &str,
    ps_param: Option<&str>,
) -> Result<TurnsOut, ApiError> {
    let (_review, repo, repo_id) = crate::reviews::require_review(state, id).await?;
    if !crate::reviews::is_hunk_id(hunk) {
        return Err(ApiError::bad_request(format!(
            "hunk must be a kbc-hunkid/1 content address (16 lowercase hex digits), got {hunk:?}"
        )));
    }
    let hunk = hunk.to_string();
    let ps_param = ps_param.map(str::to_string);
    let ps = state
        .store
        .run_blocking(move |store| crate::reviews::resolve_ps(store, id, ps_param.as_deref()))
        .await?;

    let repo_root = repo.path.clone();
    let repo_name = repo.name.clone();
    let base = ps.base_sha.clone();
    let tip = ps.tip_sha.clone();
    let wanted = hunk.clone();

    // One blocking hop for every git + store read (the 2026-08-31
    // starvation lesson: coarse-wrap the sync ladder, keep async legs out).
    let root = repo_root.clone();
    let located = tokio::task::spawn_blocking({
        let base = base.clone();
        let tip = tip.clone();
        let wanted = wanted.clone();
        move || {
            let files = crate::reviews::files_changed(&root, &base, &tip).unwrap_or_default();
            let paths: Vec<String> = files.into_iter().map(|f| f.path).collect();
            locate_hunk(&root, &base, &tip, &paths, &wanted)
        }
    })
    .await
    .map_err(|e| ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some((path, diff_hunk)) = located else {
        return Ok(TurnsOut {
            schema: SCHEMA,
            review_id: id,
            repo: repo_name,
            ps_number: ps.ps_number,
            hunk_id: hunk,
            path: None,
            commit_basis: CommitBasis::None,
            commit_basis_caption: CommitBasis::None.caption(),
            commits: Vec::new(),
            turns: Vec::new(),
            reason: Some(
                "no hunk in this patchset's diff carries that content address — it may \
                     belong to another patchset, or the hunk's content changed (a kbc-hunkid/1 \
                     address is content-derived, so an edited hunk gets a new one)"
                    .into(),
            ),
            partial: false,
            notes: Vec::new(),
            kb_lane: "ok",
            kb_lane_reason: None,
        });
    };

    // Which commits carry this hunk?
    let root = repo_root.clone();
    let (range_commits, probe_truncated, carrying) = tokio::task::spawn_blocking({
        let base = base.clone();
        let tip = tip.clone();
        let path = path.clone();
        let wanted = wanted.clone();
        move || {
            let (commits, truncated) = commits_touching(&root, &base, &tip, &path);
            let carrying = commits_carrying_hunk(&root, &commits, &path, &wanted);
            (commits, truncated, carrying)
        }
    })
    .await
    .map_err(|e| ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (basis, commits) = if !carrying.is_empty() {
        (CommitBasis::HunkExact, carrying)
    } else if !range_commits.is_empty() {
        (CommitBasis::PathInRange, range_commits)
    } else {
        (CommitBasis::None, Vec::new())
    };

    // The session↔commit join, through the EXISTING ladder.
    //
    // The kb lane is reported by CONFIGURATION, not by guessing at a
    // ladder arm's name: `resolve_commit` folds an unreachable sibling into
    // its own `Confidence::None`, so from here "kb said nothing" and "kb
    // was never asked" are indistinguishable. Saying which one it is,
    // honestly, from the one fact this process knows, beats inventing a
    // degrade signal out of a `via` string — and either way the lane is
    // reported rather than silently empty (the v6.0 per-lane precedent).
    let mut joined: BTreeMap<String, (String, String)> = BTreeMap::new();
    let (kb_lane, kb_lane_reason) = if state.kb_client.is_enabled() {
        ("ok", None)
    } else {
        (
            "degraded",
            Some(
                "no kb sibling is configured, so the session↔commit join has only the local \
                 `Kb-Session:` trailer to work with — a match with no trailer is capped at \
                 `likely` because its commit witness is missing, not because the evidence was \
                 weak"
                    .to_string(),
            ),
        )
    };
    for sha in &commits {
        let att = ladder::resolve_commit(repo, repo_id, sha, &state.store, &state.kb_client).await;
        if att.confidence == ladder::Confidence::None {
            continue;
        }
        if let Some(sid) = att.session_id {
            joined.insert(sid, (att.sha.clone(), att.confidence.as_str().to_string()));
        }
    }

    let abs_path = repo_root.join(&path).to_string_lossy().to_string();
    let removed = diff_hunk.removed_text();
    let added = diff_hunk.added_text();
    let joined_sessions: BTreeSet<String> = joined.keys().cloned().collect();
    let transcripts_root = state.transcripts_root.clone();
    let root = repo_root.clone();
    let store = state.store.clone();
    let (candidates, sessions_truncated) = tokio::task::spawn_blocking(move || {
        candidate_edits(
            &store,
            &transcripts_root,
            &root,
            &abs_path,
            &joined_sessions,
        )
    })
    .await
    .map_err(|e| ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut turns: Vec<TurnMatch> = Vec::new();
    for c in &candidates {
        let join = joined.get(&c.session_id);
        let joined_ref = join.map(|(sha, via)| (sha.as_str(), via.as_str()));
        let Some((tier, why, matched_bytes)) =
            classify(&path, &removed, &added, c, joined_ref, basis)
        else {
            continue;
        };
        turns.push(TurnMatch {
            turn_id: kb_core::sessions::view::turn_id_from_uuid(&c.uuid),
            session_id: c.session_id.clone(),
            uuid: c.uuid.clone(),
            ts: c.ts,
            tool: c.tool.clone(),
            path: c.rel_path.clone().unwrap_or_else(|| c.raw_path.clone()),
            tier,
            why,
            commit: join.map(|(sha, _)| sha.clone()),
            join_via: join.map(|(_, via)| via.clone()),
            matched_bytes,
            kb_read: format!(
                "kb sessions read {} --turn {}",
                c.session_id,
                kb_core::sessions::view::turn_id_from_uuid(&c.uuid)
            ),
        });
    }
    // `exact` first, then newest — a deterministic order with no score.
    turns.sort_by(|a, b| {
        (a.tier != TIER_EXACT)
            .cmp(&(b.tier != TIER_EXACT))
            .then(b.ts.cmp(&a.ts))
            .then(a.uuid.cmp(&b.uuid))
    });

    let mut notes = Vec::new();
    if probe_truncated {
        notes.push(format!(
            "this path has more than {MAX_COMMIT_PROBES} commits in range; only the newest \
             {MAX_COMMIT_PROBES} were probed"
        ));
    }
    if sessions_truncated {
        notes.push(format!(
            "more than {MAX_SESSIONS} sessions touched this path; only {MAX_SESSIONS} were read"
        ));
    }
    let reason = turns.is_empty().then(|| {
        format!(
            "no captured {EDIT_TOOLS:?} turn reproduces this hunk's removed/added text \
             byte-for-byte with at least {MIN_MATCH_BYTES} matching bytes — the change may \
             predate transcript capture, may have been made outside the harness, or the match \
             fell under the floor that keeps a one-word coincidence from reading as provenance"
        )
    });

    Ok(TurnsOut {
        schema: SCHEMA,
        review_id: id,
        repo: repo_name,
        ps_number: ps.ps_number,
        hunk_id: hunk,
        path: Some(path),
        commit_basis: basis,
        commit_basis_caption: basis.caption(),
        commits,
        turns,
        reason,
        partial: probe_truncated || sessions_truncated,
        notes,
        kb_lane,
        kb_lane_reason,
    })
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct HunkTurnsParams {
    #[serde(default)]
    pub ps: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(path: &str, old: &str, new: &str) -> CandidateEdit {
        CandidateEdit {
            session_id: "s1".into(),
            uuid: "0189aa11-2233-4455-6677-8899aabbccdd".into(),
            ts: 1_700_000_000_000,
            tool: "Edit".into(),
            raw_path: path.into(),
            rel_path: Some(path.into()),
            edits: vec![(old.into(), new.into())],
        }
    }

    const OLD: &str = "    items.sum(&:price) # the legacy total";
    const NEW: &str = "    items.sum(&:price_cents) # the legacy total";

    #[test]
    fn exact_needs_all_three_witnesses() {
        let e = edit("app/models/order.rb", OLD, NEW);
        let (tier, why, bytes) = classify(
            "app/models/order.rb",
            OLD,
            NEW,
            &e,
            Some(("abc123", "trailer")),
            CommitBasis::HunkExact,
        )
        .expect("matches");
        assert_eq!(tier, TIER_EXACT);
        assert!(why.contains("byte-for-byte"));
        assert!(bytes >= MIN_MATCH_BYTES);
    }

    #[test]
    fn a_missing_commit_join_caps_at_likely() {
        let e = edit("app/models/order.rb", OLD, NEW);
        let (tier, why, _) = classify(
            "app/models/order.rb",
            OLD,
            NEW,
            &e,
            None,
            CommitBasis::HunkExact,
        )
        .unwrap();
        assert_eq!(tier, TIER_LIKELY);
        assert!(why.contains("no commit"));
    }

    #[test]
    fn a_wide_commit_basis_caps_at_likely_even_with_a_join() {
        let e = edit("app/models/order.rb", OLD, NEW);
        let (tier, why, _) = classify(
            "app/models/order.rb",
            OLD,
            NEW,
            &e,
            Some(("abc123", "trailer")),
            CommitBasis::PathInRange,
        )
        .unwrap();
        assert_eq!(tier, TIER_LIKELY);
        assert!(why.contains("path_in_range"));
    }

    #[test]
    fn a_moved_path_is_likely_and_says_both_paths() {
        let e = edit("app/models/old_order.rb", OLD, NEW);
        let (tier, why, _) = classify(
            "app/models/order.rb",
            OLD,
            NEW,
            &e,
            Some(("abc123", "trailer")),
            CommitBasis::HunkExact,
        )
        .unwrap();
        assert_eq!(tier, TIER_LIKELY);
        assert!(why.contains("old_order.rb") && why.contains("order.rb"));
    }

    #[test]
    fn nothing_is_claimed_below_the_byte_floor() {
        let e = edit("a.rb", "end", "end # x");
        assert!(classify("a.rb", "end", "end # x", &e, None, CommitBasis::None).is_none());
        assert!(best_match_bytes("end", "end # x", &[("end".into(), "end # x".into())]).is_none());
    }

    #[test]
    fn a_non_matching_edit_is_not_claimed_at_any_tier() {
        let e = edit(
            "app/models/order.rb",
            "something else entirely here",
            "and its replacement",
        );
        assert!(classify(
            "app/models/order.rb",
            OLD,
            NEW,
            &e,
            Some(("abc123", "trailer")),
            CommitBasis::HunkExact
        )
        .is_none());
    }

    #[test]
    fn a_write_matches_on_the_added_side_alone() {
        let content = "module Acme\n  class Retry\n  end\nend\n";
        let mut e = edit("lib/acme/retry.rb", "", content);
        e.tool = "Write".into();
        e.edits = vec![(String::new(), content.to_string())];
        let (tier, _, bytes) = classify(
            "lib/acme/retry.rb",
            "",
            content,
            &e,
            Some(("abc", "trailer")),
            CommitBasis::HunkExact,
        )
        .unwrap();
        assert_eq!(tier, TIER_EXACT);
        assert_eq!(bytes, content.trim_end_matches('\n').len());
    }

    #[test]
    fn containment_is_allowed_because_one_hunk_groups_several_edits() {
        let removed = format!("{OLD}\n    other_removed_line_that_is_long_enough");
        let added = format!("{NEW}\n    other_added_line_that_is_long_enough");
        let e = edit("a.rb", OLD, NEW);
        assert!(classify("a.rb", &removed, &added, &e, None, CommitBasis::None).is_some());
    }

    #[test]
    fn edits_are_extracted_from_every_declared_tool_shape() {
        let ed = edits_from_input(
            "Edit",
            &serde_json::json!({"old_string": "a", "new_string": "b"}),
        );
        assert_eq!(ed, vec![("a".to_string(), "b".to_string())]);

        let multi = edits_from_input(
            "MultiEdit",
            &serde_json::json!({"edits": [{"old_string": "a", "new_string": "b"},
                                          {"old_string": "c", "new_string": "d"}]}),
        );
        assert_eq!(multi.len(), 2);

        let write = edits_from_input("Write", &serde_json::json!({"content": "hello"}));
        assert_eq!(write, vec![(String::new(), "hello".to_string())]);

        // A tool outside the closed set yields nothing, so it can never
        // produce an attribution.
        assert!(edits_from_input("Bash", &serde_json::json!({"command": "ls"})).is_empty());
    }

    #[test]
    fn the_tool_set_is_closed() {
        assert_eq!(EDIT_TOOLS.len(), 4);
        assert!(!EDIT_TOOLS.contains(&"Bash"));
        assert!(!EDIT_TOOLS.contains(&"Read"));
    }

    #[test]
    fn a_repo_relative_path_is_derived_from_an_absolute_one() {
        let root = Path::new("/tmp/acme-app");
        assert_eq!(
            rel_path_in_repo(root, "/tmp/acme-app/app/models/order.rb"),
            Some("app/models/order.rb".into())
        );
        assert_eq!(
            rel_path_in_repo(root, "app/models/order.rb"),
            Some("app/models/order.rb".into())
        );
        assert_eq!(rel_path_in_repo(root, "/elsewhere/x.rb"), None);
    }

    #[test]
    fn every_basis_has_a_caption_that_explains_its_ceiling() {
        assert!(CommitBasis::HunkExact.caption().contains("named"));
        assert!(CommitBasis::PathInRange.caption().contains("likely"));
        assert!(CommitBasis::None.caption().contains("no commit"));
    }
}

use crate::entities::RouteContract;

fn turns_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("ps", "latest")] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<HunkTurnsParams>(serde_json::Value::Object(map)).is_ok()
}

pub const HUNK_TURNS_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/hunks/{hunk}/turns",
    handler: "review_turns::hunk_turns_route",
    required_params: &[],
    params_accept_without: turns_accept_without,
};
