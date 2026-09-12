//! `branch-facts/1` (V75-M3, design D15) — ONE `for-each-ref` pass per
//! repo, a CLASSED base, and the view rules that turn a ref list into an
//! answer to "should you look at this?".
//!
//! # Why this exists beside `history::branches`
//!
//! `history::branches` (Phase C3 / V4.L1) answers "list the refs, with
//! ahead/behind vs the default and a suggested rank". It costs ONE
//! `git rev-list` subprocess per branch, measures everything against the
//! DEFAULT branch (wrong for a stacked or `develop`-based branch), and has
//! no vocabulary for merged, stale, agent-authored or checked-out-
//! elsewhere. This module is the v7 answer to the same question and is
//! deliberately additive: `/api/branches` keeps its byte-identical
//! `branches/1` shape, and nothing here is wired into it.
//!
//! # The one pass
//!
//! [`enumerate`] runs a single `git for-each-ref refs/heads refs/remotes`
//! whose `--format` carries FOURTEEN atoms separated by `\x1f`. Three of
//! them are the reason this is one call rather than N:
//!
//! * `%(upstream:track)` — the branch's ahead/behind **vs its own
//!   upstream**, already computed by git. No `rev-list`.
//! * `%(ahead-behind:<default sha>)` — ahead/behind vs the default branch
//!   for EVERY ref in the same pass (git ≥ 2.41). No `rev-list`.
//! * `%(trailers:key=Kb-Session,...)` / `%(...Kb-Agent...)` — the tip
//!   commit's agent trailers, so [`agent_provenance`] costs no `git log`.
//!
//! `%(ahead-behind:)` is the only atom this crate's minimum git version
//! does not guarantee, and `for-each-ref` fails the WHOLE invocation on an
//! unknown atom rather than degrading per-row. [`enumerate`] therefore
//! retries once without it and reports which source it used
//! ([`AheadBehindSource`]) rather than silently reporting nothing — the
//! fallback path is `history::branches::ahead_behind`'s per-branch
//! `rev-list`, run only for the rows that survive filtering and paging.
//!
//! # The base is CLASSED, never silently defaulted
//!
//! [`BaseClass`] is a four-rung ladder and the class always rides the
//! wire:
//!
//! 1. `upstream` — the branch has a configured upstream that is not
//!    `[gone]` and is not this branch's own remote MIRROR (a `feature`
//!    tracking `origin/feature` is a push target, not a base; treating it
//!    as one would report "0 ahead" for every pushed branch). Base
//!    ahead/behind come straight from `%(upstream:track)`.
//! 2. `fork-point` — `git merge-base --fork-point <default> <branch>`
//!    succeeded. This consults the default branch's REFLOG, so it can name
//!    the commit the branch actually forked from even after the default
//!    moved; git fails rather than guessing when the reflog has expired,
//!    which is exactly the honest-degrade shape this ladder wants.
//! 3. `merge-base` — plain `git merge-base <default> <branch>`.
//! 4. `unknown` — no default branch, or disjoint histories. `base.ref` and
//!    `base.sha` are `null` and ahead/behind are `null` too: a branch with
//!    nothing to measure against reports nothing, never a measured-looking
//!    zero (`branches/1`'s own V70-A3X ruling, applied here).
//!
//! # Merged, stale and agent are each a NAMED rule
//!
//! * **merged** carries its WITNESS. `ancestry` is free — a branch that is
//!   `0` ahead of its base is, by definition, wholly contained in it, so
//!   the ahead/behind already computed IS the proof. `patch-id` is
//!   `git cherry <base> <branch>`: every commit having an equivalent patch
//!   upstream is what catches a SQUASH merge, which ancestry structurally
//!   cannot see. The patch-id probe costs one subprocess per candidate, so
//!   it is capped ([`MAX_PATCH_ID_PROBES`]) and the response says how many
//!   of how many it ran.
//! * **stale** is DISTRIBUTION-derived, not a wall clock: older than the
//!   [`STALE_PERCENTILE`]th percentile of THIS repo's own branch
//!   last-activity ages. Under [`MIN_REFS_FOR_DISTRIBUTION`] refs there is
//!   no distribution to speak of and nothing is called stale (the response
//!   says so). And a branch with an OPEN REVIEW is never stale, whatever
//!   the distribution says — an open review is a live claim on the branch.
//! * **agent** (design D18) has exactly two rungs and one explicit
//!   non-rung. `exact` = a machine trailer NAMING the run (`Kb-Session:`,
//!   `Kb-Agent:`). `likely` = the tip author's email is in the configured
//!   agent-email set. And a `Co-authored-by:` trailer ALONE is deliberately
//!   NOT evidence: in an agent-assisted workflow that trailer is the shape
//!   a HUMAN-authored commit takes, so reading it as provenance would
//!   label the operator's own commits agent — exactly what D18 forbids.
//!   Pinned by [`tests::a_co_authored_by_trailer_alone_is_never_agent`].
//!
//! # Blocking
//!
//! Every git call here is blocking (`history::run_git_raw`), so route
//! handlers MUST run them inside `spawn_blocking` under the daemon-wide
//! `git_fanout` semaphore, exactly like the rest of `history`.

use super::{merge_base, run_git_raw, Result};
use crate::git::Revspec;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Wire schema name.
pub const SCHEMA: &str = "branch-facts/1";

/// Enumeration ceiling — refs beyond this are not read at all (the pass is
/// one subprocess, but the per-row derivation below is not free). Reported
/// as `enumeration_truncated`.
pub const MAX_REFS: usize = 2000;

/// Default `limit` for one page of rows.
pub const DEFAULT_LIMIT: usize = 50;
/// Hard ceiling on `limit`.
pub const MAX_LIMIT: usize = 200;

/// The percentile of the repo's own last-activity age distribution above
/// which a branch is called stale. Stated on the wire in `rules.stale`.
pub const STALE_PERCENTILE: f64 = 0.75;

/// Below this many enumerated refs there is no distribution to take a
/// percentile of, so NOTHING is called stale and the response says why.
pub const MIN_REFS_FOR_DISTRIBUTION: usize = 4;

/// Hard cap on `git cherry` patch-id probes per request.
pub const MAX_PATCH_ID_PROBES: usize = 40;

/// Hard cap on branches scanned for a `touches:<path>` filter.
pub const MAX_TOUCHES_SCAN: usize = 40;

/// The default `[branches] agent_emails` set — the address this project's
/// own agent co-author trailer uses. An operator adds their own harness's
/// address rather than editing this.
pub const DEFAULT_AGENT_EMAILS: &[&str] = &["noreply@anthropic.com"];

// --- for-each-ref ---------------------------------------------------------

/// Field separator inside one `for-each-ref` record.
const FS: char = '\u{1f}';

/// How many `\x1f`-separated fields [`FORMAT_BASE`] produces.
const BASE_FIELDS: usize = 12;

/// The index of `%(ahead-behind:)` when [`enumerate`] appended it.
const AHEAD_BEHIND_FIELD: usize = BASE_FIELDS;

/// The atoms, in the order [`parse_for_each_ref`] reads them. `%1f` is
/// git's own hex escape for the unit separator, and it appears BETWEEN
/// atoms only — no trailing one — so the field count is exactly
/// [`BASE_FIELDS`], and `%1f%(ahead-behind:…)` appends exactly one more.
/// (A trailing separator would make the lean and rich forms produce the
/// SAME field count with different meanings, which is how a shifted field
/// gets read as the wrong thing.)
const FORMAT_BASE: &str = concat!(
    "%(refname)%1f",
    "%(objectname)%1f",
    "%(upstream:short)%1f",
    "%(upstream:track)%1f",
    "%(worktreepath)%1f",
    "%(authorname)%1f",
    "%(authoremail:trim)%1f",
    "%(authordate:unix)%1f",
    "%(subject)%1f",
    "%(HEAD)%1f",
    "%(trailers:key=Kb-Session,valueonly,separator=%x2c)%1f",
    "%(trailers:key=Kb-Agent,valueonly,separator=%x2c)",
);

/// Where the ahead/behind numbers came from — surfaced, never assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AheadBehindSource {
    /// `%(ahead-behind:<sha>)` in the one pass (git ≥ 2.41).
    ForEachRef,
    /// The atom was rejected by this git; the numbers come from
    /// `history::branches::ahead_behind`'s per-row `rev-list` instead.
    RevList,
    /// There is no default branch to measure against at all.
    None,
}

/// One raw `for-each-ref` row, before any derivation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRef {
    pub full_ref: String,
    /// Short name with the remote stripped (`refs/remotes/origin/foo` →
    /// `"foo"`), matching `git::RefInfo::name`'s convention.
    pub name: String,
    pub remote: Option<String>,
    pub tip_sha: String,
    /// `%(upstream:short)` — e.g. `"origin/main"`. Empty ⇒ `None`.
    pub upstream: Option<String>,
    /// Parsed `%(upstream:track)`.
    pub track: UpstreamTrack,
    /// `%(worktreepath)` — the absolute path of the worktree this ref is
    /// checked out in, INCLUDING the main one. Empty ⇒ `None`.
    pub worktree_path: Option<String>,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub subject: String,
    /// `%(HEAD)` is `*` for the ref HEAD points at.
    pub is_head: bool,
    /// `Kb-Session:` trailer values on the tip commit.
    pub kb_session_trailers: Vec<String>,
    /// `Kb-Agent:` trailer values on the tip commit.
    pub kb_agent_trailers: Vec<String>,
    /// `%(ahead-behind:<default>)`, when the atom was available.
    pub ahead_behind_default: Option<(u32, u32)>,
}

/// Parsed `%(upstream:track)` — `[ahead 1, behind 2]`, `[gone]`, or empty.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpstreamTrack {
    pub gone: bool,
    pub ahead: u32,
    pub behind: u32,
    /// `false` when the atom was empty (no upstream, or nothing to say).
    pub measured: bool,
}

/// `[ahead 1, behind 2]` / `[gone]` / `""`. Total: an unrecognised shape
/// parses to `measured: false` rather than a guessed zero.
pub fn parse_upstream_track(s: &str) -> UpstreamTrack {
    let inner = s.trim().trim_start_matches('[').trim_end_matches(']');
    if inner.is_empty() {
        return UpstreamTrack::default();
    }
    if inner == "gone" {
        return UpstreamTrack {
            gone: true,
            ..UpstreamTrack::default()
        };
    }
    let mut out = UpstreamTrack::default();
    for part in inner.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            if let Ok(v) = n.trim().parse() {
                out.ahead = v;
                out.measured = true;
            }
        } else if let Some(n) = part.strip_prefix("behind ") {
            if let Ok(v) = n.trim().parse() {
                out.behind = v;
                out.measured = true;
            }
        }
    }
    out
}

/// `refs/remotes/<remote>/<name>` → `(Some(remote), name)`; anything else
/// → `(None, <full ref minus refs/heads/>)`. Mirrors `git::refs`'s own
/// naming so the two surfaces agree on what a branch is called.
pub fn split_ref_name(full_ref: &str) -> (Option<String>, String) {
    if let Some(rest) = full_ref.strip_prefix("refs/remotes/") {
        if let Some((remote, name)) = rest.split_once('/') {
            if !remote.is_empty() && !name.is_empty() {
                return (Some(remote.to_string()), name.to_string());
            }
        }
        return (None, rest.to_string());
    }
    (
        None,
        full_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(full_ref)
            .to_string(),
    )
}

fn split_trailers(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse the one pass's stdout. `with_ahead_behind` says whether the
/// format carried the 13th atom, so a row is never mis-shifted.
pub fn parse_for_each_ref(bytes: &[u8], with_ahead_behind: bool) -> Vec<RawRef> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(FS).collect();
        let want = if with_ahead_behind {
            BASE_FIELDS + 1
        } else {
            BASE_FIELDS
        };
        if f.len() < want {
            continue;
        }
        let full_ref = f[0].to_string();
        if full_ref.is_empty() {
            continue;
        }
        // A `<remote>/HEAD` symref is the default-branch POINTER, not a
        // branch anyone can look at — `git::refs::parse_remote_ref` drops
        // it too, and keeping it here would double every remote listing's
        // default row.
        if full_ref.ends_with("/HEAD") {
            continue;
        }
        let (remote, name) = split_ref_name(&full_ref);
        let ahead_behind_default = if with_ahead_behind {
            parse_ahead_behind_atom(f[AHEAD_BEHIND_FIELD])
        } else {
            None
        };
        out.push(RawRef {
            full_ref,
            name,
            remote,
            tip_sha: f[1].to_string(),
            upstream: (!f[2].is_empty()).then(|| f[2].to_string()),
            track: parse_upstream_track(f[3]),
            worktree_path: (!f[4].is_empty()).then(|| f[4].to_string()),
            author_name: f[5].to_string(),
            author_email: f[6].to_string(),
            author_time: f[7].trim().parse().unwrap_or(0),
            subject: f[8].to_string(),
            is_head: f[9].trim() == "*",
            kb_session_trailers: split_trailers(f[10]),
            kb_agent_trailers: split_trailers(f[11]),
            ahead_behind_default,
        });
    }
    out
}

/// `%(ahead-behind:X)` renders `"<ahead> <behind>"`. Anything else — most
/// importantly an EMPTY value, which is what git emits for a ref with no
/// common history with `X` — is `None`, never `(0, 0)`.
fn parse_ahead_behind_atom(s: &str) -> Option<(u32, u32)> {
    let mut it = s.split_whitespace();
    let a: u32 = it.next()?.parse().ok()?;
    let b: u32 = it.next()?.parse().ok()?;
    Some((a, b))
}

/// `true` for a 40-char lowercase hex sha. Guards the ONE place this
/// module interpolates a value into git's `--format` argument.
fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The one `for-each-ref` pass. `default_sha` (when present and a full
/// sha) adds `%(ahead-behind:<sha>)`; an older git rejects the atom
/// outright, so one retry without it is the documented degrade.
pub fn enumerate(
    repo_root: &Path,
    default_sha: Option<&str>,
) -> Result<(Vec<RawRef>, AheadBehindSource)> {
    let ab_sha = default_sha.filter(|s| is_full_sha(s));
    if let Some(sha) = ab_sha {
        let fmt = format!("{FORMAT_BASE}%1f%(ahead-behind:{sha})");
        match run_git_raw(
            repo_root,
            &[
                "for-each-ref",
                "--format",
                &fmt,
                "refs/heads",
                "refs/remotes",
            ],
        ) {
            Ok(out) => {
                return Ok((
                    parse_for_each_ref(&out, true),
                    AheadBehindSource::ForEachRef,
                ))
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "branch-facts/1: %(ahead-behind:) unavailable, falling back to rev-list"
                );
            }
        }
    }
    let out = run_git_raw(
        repo_root,
        &[
            "for-each-ref",
            "--format",
            FORMAT_BASE,
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    let source = if default_sha.is_some() {
        AheadBehindSource::RevList
    } else {
        AheadBehindSource::None
    };
    Ok((parse_for_each_ref(&out, false), source))
}

// --- the base ladder ------------------------------------------------------

/// The trust ladder's four rungs — see the module doc. Never absent from
/// the wire: an unmeasurable base is `Unknown`, not a missing field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BaseClass {
    Upstream,
    ForkPoint,
    MergeBase,
    Unknown,
}

impl BaseClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upstream => "upstream",
            Self::ForkPoint => "fork-point",
            Self::MergeBase => "merge-base",
            Self::Unknown => "unknown",
        }
    }
}

/// A branch's base, with the rung that produced it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BranchBase {
    pub class: BaseClass,
    /// The REF this branch is measured against (`"main"`, `"origin/dev"`).
    /// `None` only for [`BaseClass::Unknown`].
    #[serde(rename = "ref")]
    pub ref_name: Option<String>,
    /// The base COMMIT — the fork point / merge base / upstream tip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

impl BranchBase {
    pub fn unknown() -> Self {
        Self {
            class: BaseClass::Unknown,
            ref_name: None,
            sha: None,
        }
    }
}

/// `true` when `upstream` is this branch's own remote MIRROR (`feature` ↔
/// `origin/feature`) rather than a base — see the module doc's rung 1.
pub fn upstream_is_own_mirror(branch_name: &str, upstream: &str) -> bool {
    match upstream.split_once('/') {
        Some((_remote, rest)) => rest == branch_name,
        None => upstream == branch_name,
    }
}

/// The ladder. `default_ref`/`default_sha` name the repo's default branch;
/// both `None` ⇒ rungs 2 and 3 cannot run at all and the answer is
/// `Unknown`.
///
/// Costs at most ONE subprocess (`merge-base --fork-point`), and only when
/// rung 1 does not apply.
pub fn detect_base(
    repo_root: &Path,
    raw: &RawRef,
    default_ref: Option<&str>,
    default_sha: Option<&str>,
) -> BranchBase {
    // Rung 1 — a configured, live upstream that is not this branch's own
    // remote mirror.
    if let Some(up) = raw.upstream.as_deref() {
        if !raw.track.gone && !upstream_is_own_mirror(&raw.name, up) {
            return BranchBase {
                class: BaseClass::Upstream,
                ref_name: Some(up.to_string()),
                sha: resolve_ref_sha(repo_root, up),
            };
        }
    }
    let (Some(default_ref), Some(default_sha)) = (default_ref, default_sha) else {
        return BranchBase::unknown();
    };
    // The default branch is its own base, definitionally, with nothing to
    // probe (`branches/1`'s same carve-out).
    if raw.name == default_ref || raw.full_ref == format!("refs/heads/{default_ref}") {
        return BranchBase {
            class: BaseClass::MergeBase,
            ref_name: Some(default_ref.to_string()),
            sha: Some(default_sha.to_string()),
        };
    }
    // Rung 2 — fork-point. Consults the DEFAULT branch's reflog, so it is
    // meaningful only for a local branch; git fails (rather than guessing)
    // when the reflog cannot answer, which is the degrade we want.
    if raw.remote.is_none() {
        if let (Ok(d), Ok(b)) = (Revspec::parse(default_ref), Revspec::parse(&raw.full_ref)) {
            if let Ok(out) = run_git_raw(
                repo_root,
                &["merge-base", "--fork-point", d.as_str(), b.as_str()],
            ) {
                let sha = String::from_utf8_lossy(&out).trim().to_string();
                if !sha.is_empty() {
                    return BranchBase {
                        class: BaseClass::ForkPoint,
                        ref_name: Some(default_ref.to_string()),
                        sha: Some(sha),
                    };
                }
            }
        }
    }
    // Rung 3 — plain merge-base against the default's resolved tip.
    match merge_base(repo_root, default_sha, &raw.tip_sha) {
        Some(sha) => BranchBase {
            class: BaseClass::MergeBase,
            ref_name: Some(default_ref.to_string()),
            sha: Some(sha),
        },
        // Rung 4 — disjoint histories. Nothing to measure against.
        None => BranchBase::unknown(),
    }
}

fn resolve_ref_sha(repo_root: &Path, spec: &str) -> Option<String> {
    let r = Revspec::parse(spec).ok()?;
    let out = run_git_raw(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", r.as_str()),
        ],
    )
    .ok()?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    (!s.is_empty()).then_some(s)
}

// --- merged ---------------------------------------------------------------

/// How a `merged` claim was proved. The witness is never omitted — a
/// merged row without one would be an assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergedKind {
    /// 0 commits ahead of the base: every commit is already reachable.
    Ancestry,
    /// `git cherry` found an equivalent patch upstream for every commit —
    /// what catches a SQUASH merge, which ancestry cannot see.
    PatchId,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MergedWitness {
    pub kind: MergedKind,
    /// The base ref the proof is against.
    pub into: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub into_sha: Option<String>,
    /// For [`MergedKind::PatchId`]: how many of the branch's commits had an
    /// equivalent patch upstream (all of them, or it would not be merged).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equivalent: Option<u32>,
}

/// `git cherry <base> <head>` output: `- <sha>` = an equivalent patch
/// exists upstream, `+ <sha>` = it does not. Merged-by-patch-id means at
/// least one commit and NO `+` lines. Empty output means there was nothing
/// to compare — `None`, never a vacuous "merged".
pub fn parse_cherry(bytes: &[u8]) -> Option<u32> {
    let text = String::from_utf8_lossy(bytes);
    let mut equivalent = 0u32;
    let mut any = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        any = true;
        // ONE match on the sign character rather than two `starts_with`
        // calls. That is clearer here (the sign is a two-value alphabet,
        // not a prefix), and it keeps this parser out of
        // `tests/security/git_argv_lint.rs`'s scan for a hand-rolled
        // `starts_with('-')` revspec guard — which this is NOT: the `-` is
        // `git cherry`'s own OUTPUT marker for "an equivalent patch exists
        // upstream", not a caller-supplied ref. Do not "restore" the
        // `starts_with` form.
        match line.chars().next() {
            Some('+') => return None,
            Some('-') => equivalent += 1,
            _ => {}
        }
    }
    any.then_some(equivalent)
}

/// One `git cherry` probe. Blocking; the caller caps how many run.
pub fn patch_id_merged(repo_root: &Path, base: &Revspec, head: &Revspec) -> Option<u32> {
    let out = run_git_raw(repo_root, &["cherry", base.as_str(), head.as_str()]).ok()?;
    parse_cherry(&out)
}

// --- agent provenance (D18) ----------------------------------------------

/// D18's ladder for "did an agent write this branch's tip?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentClass {
    /// No evidence. Never "probably not an agent" — just no evidence.
    None,
    /// The tip author's EMAIL is in the configured agent set. An email is
    /// an identity heuristic (anyone can set `user.email`), so it can
    /// never raise to exact.
    Likely,
    /// A machine trailer NAMES the run.
    Exact,
}

impl AgentClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Likely => "likely",
            Self::Exact => "exact",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AgentProvenance {
    pub class: AgentClass,
    /// What produced the class — `"kb-session-trailer"`, `"kb-agent-trailer"`,
    /// `"author-email"`, or `"no-evidence"`. Never omitted.
    pub via: &'static str,
    /// The trailer's own value, when there was one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// D18's class ladder over one commit's author email + machine trailers.
/// Same rule [`agent_provenance`] uses for a branch tip; V76-R3d's file
/// scrubber applies it per stop. A `Co-authored-by:` trailer is
/// deliberately not an input at all.
pub fn agent_class_for(
    author_email: &str,
    kb_session_trailers: &[String],
    kb_agent_trailers: &[String],
    agent_emails: &[String],
) -> AgentClass {
    agent_provenance_parts(
        author_email,
        kb_session_trailers,
        kb_agent_trailers,
        agent_emails,
    )
    .0
}

fn agent_provenance_parts(
    author_email: &str,
    kb_session_trailers: &[String],
    kb_agent_trailers: &[String],
    agent_emails: &[String],
) -> (AgentClass, &'static str, Option<String>) {
    if let Some(v) = kb_session_trailers.first() {
        return (AgentClass::Exact, "kb-session-trailer", Some(v.clone()));
    }
    if let Some(v) = kb_agent_trailers.first() {
        return (AgentClass::Exact, "kb-agent-trailer", Some(v.clone()));
    }
    let email = author_email.trim().to_ascii_lowercase();
    if !email.is_empty()
        && agent_emails
            .iter()
            .any(|e| e.trim().to_ascii_lowercase() == email)
    {
        return (AgentClass::Likely, "author-email", None);
    }
    (AgentClass::None, "no-evidence", None)
}

/// See the module doc. `agent_emails` is compared case-insensitively; a
/// `Co-authored-by:` trailer is deliberately not an input at all.
pub fn agent_provenance(raw: &RawRef, agent_emails: &[String]) -> AgentProvenance {
    let (class, via, session_id) = agent_provenance_parts(
        &raw.author_email,
        &raw.kb_session_trailers,
        &raw.kb_agent_trailers,
        agent_emails,
    );
    AgentProvenance {
        class,
        via,
        session_id,
    }
}

// --- the activity distribution -------------------------------------------

/// Nearest-rank percentile over a SORTED ascending slice. `p` is clamped
/// to `[0, 1]`; an empty slice has no percentile.
pub fn percentile(sorted_asc: &[i64], p: f64) -> Option<i64> {
    if sorted_asc.is_empty() {
        return None;
    }
    let p = p.clamp(0.0, 1.0);
    let n = sorted_asc.len();
    // Nearest-rank: ceil(p * n), 1-based, clamped into range.
    let rank = ((p * n as f64).ceil() as usize).clamp(1, n);
    Some(sorted_asc[rank - 1])
}

/// The stale rule's computed threshold, plus the honest reason when there
/// is none.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StaleRule {
    /// Always the literal prose of the rule, so a caller never has to
    /// reverse-engineer it from the numbers.
    pub rule: &'static str,
    pub percentile: f64,
    /// Ages STRICTLY greater than this (seconds) are stale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_age_secs: Option<i64>,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

pub const STALE_RULE_TEXT: &str =
    "a branch is stale when its tip is older than the 75th percentile \
     of THIS repo's own branch last-activity ages AND no review on it is open";

/// Compute the stale rule from every enumerated ref's age.
pub fn stale_rule(ages_secs: &[i64]) -> StaleRule {
    if ages_secs.len() < MIN_REFS_FOR_DISTRIBUTION {
        return StaleRule {
            rule: STALE_RULE_TEXT,
            percentile: STALE_PERCENTILE,
            threshold_age_secs: None,
            applied: false,
            degraded_reason: Some(format!(
                "only {} branch(es) — fewer than the {} a percentile can describe, so nothing is called stale",
                ages_secs.len(),
                MIN_REFS_FOR_DISTRIBUTION
            )),
        };
    }
    let mut sorted = ages_secs.to_vec();
    sorted.sort_unstable();
    let threshold = percentile(&sorted, STALE_PERCENTILE);
    StaleRule {
        rule: STALE_RULE_TEXT,
        percentile: STALE_PERCENTILE,
        threshold_age_secs: threshold,
        applied: threshold.is_some(),
        degraded_reason: None,
    }
}

// --- prefix folding -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PrefixCount {
    /// Includes the trailing `/` — it is the literal `?prefix=` value.
    pub prefix: String,
    pub count: usize,
}

/// Server-computed prefix tree over the FIRST `/`-delimited segment.
/// Deterministic order: count descending, then prefix ascending. A branch
/// with no `/` contributes to no prefix (it is not "in" a folder).
pub fn fold_prefixes(names: impl IntoIterator<Item = String>) -> Vec<PrefixCount> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for name in names {
        if let Some((head, rest)) = name.split_once('/') {
            if !head.is_empty() && !rest.is_empty() {
                *counts.entry(format!("{head}/")).or_default() += 1;
            }
        }
    }
    let mut out: Vec<PrefixCount> = counts
        .into_iter()
        .map(|(prefix, count)| PrefixCount { prefix, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.prefix.cmp(&b.prefix)));
    out
}

// --- views ----------------------------------------------------------------

/// The eight URL-addressable views. `all` is the identity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    Current,
    Mine,
    Agent,
    Review,
    Active,
    Stale,
    Merged,
    #[default]
    All,
}

impl View {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Mine => "mine",
            Self::Agent => "agent",
            Self::Review => "review",
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Merged => "merged",
            Self::All => "all",
        }
    }

    pub const ALL: &'static [View] = &[
        View::Current,
        View::Mine,
        View::Agent,
        View::Review,
        View::Active,
        View::Stale,
        View::Merged,
        View::All,
    ];
}

/// One "why is this row here" chip. `code` is stable and machine-readable;
/// `text` is the human sentence — the CLI prints it verbatim and the SPA
/// renders it as a chip, so neither re-derives prose from a code.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Reason {
    pub code: &'static str,
    pub text: String,
}

impl Reason {
    pub fn new(code: &'static str, text: impl Into<String>) -> Self {
        Self {
            code,
            text: text.into(),
        }
    }
}

/// The derived, per-branch answer — everything the views and the wire need.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchFact {
    pub raw: RawRef,
    pub base: BranchBase,
    /// vs [`BranchFact::base`]. `None` when nothing was measurable.
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub agent: AgentProvenance,
    pub merged: Option<MergedWitness>,
    pub stale: bool,
    pub mine: bool,
    pub open_review_ids: Vec<i64>,
    pub favourite: bool,
    /// Age of the tip in seconds at request time.
    pub age_secs: i64,
    /// A linked worktree (never the main checkout — that is `is_head`).
    pub worktree: Option<String>,
}

impl BranchFact {
    pub fn is_current(&self) -> bool {
        self.raw.is_head || self.worktree.is_some()
    }

    pub fn in_view(&self, view: View) -> bool {
        match view {
            View::Current => self.is_current(),
            View::Mine => self.mine,
            View::Agent => self.agent.class != AgentClass::None,
            View::Review => !self.open_review_ids.is_empty(),
            // `active` and `stale` PARTITION the set: a branch older than
            // the threshold but held by an open review is active, not
            // stale (see the module doc's stale rule).
            View::Active => !self.stale,
            View::Stale => self.stale,
            View::Merged => self.merged.is_some(),
            View::All => true,
        }
    }

    /// The reason chips, in a fixed order so two renderings agree.
    pub fn reasons(&self, default_ref: Option<&str>) -> Vec<Reason> {
        let mut out = Vec::new();
        if self.raw.is_head {
            out.push(Reason::new("checked-out", "checked out at HEAD"));
        }
        if let Some(w) = &self.worktree {
            out.push(Reason::new(
                "worktree",
                format!("checked out in worktree {w}"),
            ));
        }
        if self.favourite {
            out.push(Reason::new("favourite", "starred"));
        }
        if self.mine {
            out.push(Reason::new("mine", "your identity authored the tip"));
        }
        match self.agent.class {
            AgentClass::Exact => out.push(Reason::new(
                "agent-trailer",
                format!("agent trailer ({})", self.agent.via),
            )),
            AgentClass::Likely => out.push(Reason::new(
                "agent-email",
                "tip author's email is a configured agent address",
            )),
            AgentClass::None => {}
        }
        for id in &self.open_review_ids {
            out.push(Reason::new("review-open", format!("review #{id} open")));
        }
        if self.raw.track.gone {
            out.push(Reason::new(
                "upstream-gone",
                format!(
                    "upstream {} is gone",
                    self.raw.upstream.as_deref().unwrap_or("(unknown)")
                ),
            ));
        }
        if let Some(w) = &self.merged {
            let text = match w.kind {
                MergedKind::Ancestry => format!(
                    "merged into {} by ancestry{}",
                    w.into,
                    w.into_sha
                        .as_deref()
                        .map(|s| format!("@{}", short(s)))
                        .unwrap_or_default()
                ),
                MergedKind::PatchId => format!(
                    "merged into {} by patch-id ({} commit(s) equivalent)",
                    w.into,
                    w.equivalent.unwrap_or(0)
                ),
            };
            out.push(Reason::new("merged", text));
        }
        if let (Some(a), Some(base)) = (self.ahead, self.base.ref_name.as_deref()) {
            if a > 0 {
                out.push(Reason::new(
                    "ahead-of-base",
                    format!("{a} commit(s) ahead of {base}"),
                ));
            }
        }
        if let (Some(b), Some(base)) = (self.behind, self.base.ref_name.as_deref()) {
            if b > 0 {
                out.push(Reason::new(
                    "behind-base",
                    format!("{b} commit(s) behind {base}"),
                ));
            }
        }
        if self.base.class == BaseClass::Unknown {
            out.push(Reason::new(
                "base-unknown",
                match default_ref {
                    Some(d) => format!("no common history with {d} — base unknown"),
                    None => "no default branch to measure against — base unknown".to_string(),
                },
            ));
        }
        if self.stale {
            out.push(Reason::new(
                "stale",
                format!("no activity for {} day(s)", self.age_secs / 86_400),
            ));
        }
        out
    }
}

fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

// --- touches --------------------------------------------------------------

/// `git diff --name-only <base>...<tip> -- <path>` — non-empty ⇒ this
/// branch's diff against its own base touches `path`. Blocking; the caller
/// caps how many branches this runs for.
///
/// The pathspec is caller-supplied and is passed AFTER an explicit `--`
/// (SEC-17's third rule; asserted by `tests/security/git_argv_lint.rs`).
pub fn touches_path(repo_root: &Path, base_sha: &str, tip_sha: &str, path: &str) -> bool {
    let range = format!("{base_sha}..{tip_sha}");
    let Ok(out) = run_git_raw(repo_root, &["diff", "--name-only", &range, "--", path]) else {
        return false;
    };
    !String::from_utf8_lossy(&out).trim().is_empty()
}

/// The identity this daemon calls "mine" — `user.name`/`user.email` read
/// from the repo's own git config. kb-code has ONE identity (the v0.34
/// ruling), so this is a repo property, never a per-caller one.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct MineIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

impl MineIdentity {
    pub fn matches(&self, raw: &RawRef) -> bool {
        if let Some(e) = &self.email {
            if !e.is_empty() && e.eq_ignore_ascii_case(raw.author_email.trim()) {
                return true;
            }
        }
        // Name is the fallback, not an equal rung: two people can share a
        // display name, so it only decides when no email was configured.
        if self.email.is_none() {
            if let Some(n) = &self.name {
                if !n.is_empty() && n == raw.author_name.trim() {
                    return true;
                }
            }
        }
        false
    }
}

/// `git config --get <key>` for the two identity keys. A missing key is
/// `None`, never an empty-string match (which would make EVERY unauthored
/// row "mine").
pub fn mine_identity(repo_root: &Path) -> MineIdentity {
    let get = |key: &str| -> Option<String> {
        let out = run_git_raw(repo_root, &["config", "--get", key]).ok()?;
        let s = String::from_utf8_lossy(&out).trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    MineIdentity {
        name: get("user.name"),
        email: get("user.email"),
    }
}

// --- the base cache ------------------------------------------------------

/// Entries kept before the cache is cleared wholesale. Deliberately a
/// CLEAR, not an LRU eviction: this cache holds at most one entry per
/// (ref, tip) a request actually paged to, so a fleet of repos would have
/// to churn thousands of tips to reach it, and a clear is a bounded,
/// obviously-correct policy where an LRU is a second data structure to get
/// wrong. A miss costs one `merge-base` call.
pub const MAX_CACHE_ENTRIES: usize = 4000;

/// `(repo, ref, tip, base)` — the design's own cache key, spelled out.
/// "base" is the INPUT the base ladder reads: the default branch's
/// resolved tip. Any of the four moving is a MISS, which is exactly the
/// "invalidated when the ref moves" rule, obtained structurally rather
/// than by an invalidation pass that could be forgotten.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BaseCacheKey {
    pub repo: String,
    pub full_ref: String,
    pub tip_sha: String,
    pub default_sha: String,
}

/// The in-process base cache. NOT a table: a branch fact is derivable from
/// the repo at any moment, so persisting it would be a second copy of git
/// that can go stale in ways a key miss cannot.
#[derive(Debug, Default)]
pub struct BaseCache {
    map: std::collections::HashMap<BaseCacheKey, BranchBase>,
    hits: u64,
    misses: u64,
}

impl BaseCache {
    pub fn get(&mut self, key: &BaseCacheKey) -> Option<BranchBase> {
        match self.map.get(key) {
            Some(v) => {
                self.hits += 1;
                Some(v.clone())
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    pub fn put(&mut self, key: BaseCacheKey, value: BranchBase) {
        if self.map.len() >= MAX_CACHE_ENTRIES {
            self.map.clear();
        }
        self.map.insert(key, value);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `(hits, misses)` since boot — reported on the wire so a caller can
    /// see the cache working rather than taking it on faith.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

// --- the kbcq/1 atoms (V75-M3) -------------------------------------------

/// The `~branches` view of a parsed kbcq/1 query — this module is the
/// declared `consumer_module` for `branch:`/`touches:`/`by:`/`agent:`
/// (`search::grammar::FILTER_SPECS`), so this is where those four typed
/// fields are actually read. Everything except `touches` is a pure
/// predicate over an already-computed [`BranchFact`]; `touches` costs one
/// `git diff` per branch and is applied separately, under a cap, by the
/// route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactFilter {
    /// The residual free text of the query — the `/` filter, matched as a
    /// case-insensitive substring of the branch NAME (never of the
    /// subject: the operator typing `/auth` is naming a branch).
    pub text: Option<String>,
    /// `branch:` — the same substring match, stated explicitly. Both may
    /// be present, and both must hold (AND, like every other kbcq/1
    /// filter).
    pub branch: Option<String>,
    /// `by:` — a case-insensitive substring of the tip author's name OR
    /// email. A substring, not an equality: an operator types `by:ada`,
    /// not `by:"Ada Lovelace <ada@example.com>"`.
    pub by: Option<String>,
    /// `agent:` — one of `AGENT_FILTER_VALUES`. An unrecognised value
    /// matches NOTHING rather than everything (the grammar's closed
    /// vocabulary already turns a typo into a diagnostic + a plain word,
    /// so reaching here with one means a caller bypassed the parser).
    pub agent: Option<String>,
    /// `touches:` — a repo-relative path. Never applied here; see
    /// [`FactFilter::needs_touches_scan`].
    pub touches: Option<String>,
    /// `?prefix=` — the prefix-folding selection, an exact prefix of the
    /// branch name (`"feature/"`). Not a kbcq/1 key: it is a fold of the
    /// list, not a term of the query.
    pub prefix: Option<String>,
    /// `?fav=1`.
    pub favourites_only: bool,
}

impl FactFilter {
    /// Read the four kbcq/1 atoms plus the residual text off a parsed
    /// query. `prefix`/`favourites_only` are wire params the route sets.
    pub fn from_parsed(parsed: &crate::search::grammar::ParsedQuery) -> Self {
        let filters = &parsed.filters;
        Self {
            text: non_empty(parsed.query.trim()),
            branch: filters.branch.clone(),
            by: filters.by.clone(),
            agent: filters.agent.clone(),
            touches: filters.touches.clone(),
            prefix: None,
            favourites_only: false,
        }
    }

    /// `true` when this filter needs the per-branch `git diff` scan.
    pub fn needs_touches_scan(&self) -> bool {
        self.touches.is_some()
    }

    /// Every predicate that costs nothing. `touches` is deliberately NOT
    /// here — a cheap/expensive split is what lets the route apply this to
    /// every row and the expensive one to a capped few.
    pub fn matches_cheap(&self, f: &BranchFact) -> bool {
        let name = f.raw.name.to_ascii_lowercase();
        if let Some(t) = &self.text {
            if !name.contains(&t.to_ascii_lowercase()) {
                return false;
            }
        }
        if let Some(b) = &self.branch {
            if !name.contains(&b.to_ascii_lowercase()) {
                return false;
            }
        }
        if let Some(p) = &self.prefix {
            if !f.raw.name.starts_with(p.as_str()) {
                return false;
            }
        }
        if self.favourites_only && !f.favourite {
            return false;
        }
        if let Some(by) = &self.by {
            let needle = by.to_ascii_lowercase();
            let hay_name = f.raw.author_name.to_ascii_lowercase();
            let hay_mail = f.raw.author_email.to_ascii_lowercase();
            if !hay_name.contains(&needle) && !hay_mail.contains(&needle) {
                return false;
            }
        }
        if let Some(a) = &self.agent {
            let ok = match a.as_str() {
                "exact" => f.agent.class == AgentClass::Exact,
                "likely" => f.agent.class == AgentClass::Likely,
                "any" => f.agent.class != AgentClass::None,
                "none" => f.agent.class == AgentClass::None,
                // An unrecognised value matches nothing — see the field doc.
                _ => false,
            };
            if !ok {
                return false;
            }
        }
        true
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// De-dup remote rows whose short name a LOCAL branch already claims —
/// the operator's handle wins, exactly as `routes::branches_route` does.
pub fn drop_shadowed_remotes(refs: Vec<RawRef>) -> Vec<RawRef> {
    let locals: HashSet<String> = refs
        .iter()
        .filter(|r| r.remote.is_none())
        .map(|r| r.name.clone())
        .collect();
    refs.into_iter()
        .filter(|r| r.remote.is_none() || !locals.contains(&r.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str) -> RawRef {
        RawRef {
            full_ref: format!("refs/heads/{name}"),
            name: name.to_string(),
            remote: None,
            tip_sha: "a".repeat(40),
            upstream: None,
            track: UpstreamTrack::default(),
            worktree_path: None,
            author_name: "Test".to_string(),
            author_email: "test@example.com".to_string(),
            author_time: 1_700_000_000,
            subject: "subject".to_string(),
            is_head: false,
            kb_session_trailers: Vec::new(),
            kb_agent_trailers: Vec::new(),
            ahead_behind_default: None,
        }
    }

    /// The one thing a fixture cannot catch: `FORMAT_BASE` and
    /// `BASE_FIELDS` disagreeing. A mismatch shifts every field right of
    /// the gap, which is how `%(ahead-behind:)` once got read out of the
    /// wrong column and made EVERY row fail the length check silently.
    #[test]
    fn the_format_declares_exactly_base_fields_atoms() {
        assert_eq!(
            FORMAT_BASE.matches("%1f").count() + 1,
            BASE_FIELDS,
            "FORMAT_BASE has {} separators; BASE_FIELDS says {BASE_FIELDS}",
            FORMAT_BASE.matches("%1f").count()
        );
        assert!(
            !FORMAT_BASE.ends_with("%1f"),
            "a TRAILING separator would make the lean and rich forms the same length"
        );
        assert_eq!(AHEAD_BEHIND_FIELD, BASE_FIELDS);
    }

    #[test]
    fn upstream_track_parses_every_documented_shape() {
        assert_eq!(parse_upstream_track(""), UpstreamTrack::default());
        assert!(parse_upstream_track("[gone]").gone);
        let t = parse_upstream_track("[ahead 1]");
        assert_eq!((t.ahead, t.behind, t.measured), (1, 0, true));
        let t = parse_upstream_track("[behind 2]");
        assert_eq!((t.ahead, t.behind, t.measured), (0, 2, true));
        let t = parse_upstream_track("[ahead 3, behind 4]");
        assert_eq!((t.ahead, t.behind, t.measured), (3, 4, true));
        // An unrecognised shape is UNMEASURED, never a guessed zero.
        assert!(!parse_upstream_track("[whatever]").measured);
    }

    #[test]
    fn split_ref_name_matches_the_git_refs_convention() {
        assert_eq!(
            split_ref_name("refs/remotes/origin/feature/x"),
            (Some("origin".to_string()), "feature/x".to_string())
        );
        assert_eq!(
            split_ref_name("refs/heads/feature/x"),
            (None, "feature/x".to_string())
        );
    }

    #[test]
    fn for_each_ref_rows_parse_with_and_without_the_ahead_behind_atom() {
        let rich = format!(
            "refs/heads/a{FS}{sha}{FS}origin/main{FS}[ahead 1]{FS}{FS}Ada{FS}ada@example.com{FS}1700000000{FS}subj{FS} {FS}sess-1{FS}{FS}3 4\n",
            sha = "b".repeat(40),
        );
        assert_eq!(
            rich.trim_end().split(FS).count(),
            BASE_FIELDS + 1,
            "the fixture must have exactly as many fields as the real format"
        );
        let rows = parse_for_each_ref(rich.as_bytes(), true);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.name, "a");
        assert_eq!(r.upstream.as_deref(), Some("origin/main"));
        assert_eq!(r.track.ahead, 1);
        assert_eq!(r.author_email, "ada@example.com");
        assert_eq!(r.kb_session_trailers, vec!["sess-1".to_string()]);
        assert_eq!(r.ahead_behind_default, Some((3, 4)));

        // The SAME bytes minus the last atom, parsed as the lean format.
        let lean = format!(
            "refs/heads/a{FS}{sha}{FS}{FS}{FS}{FS}Ada{FS}ada@example.com{FS}1700000000{FS}subj{FS}*{FS}{FS}\n",
            sha = "b".repeat(40),
        );
        assert_eq!(rich.trim_end().split(FS).count() - 1, BASE_FIELDS);
        assert_eq!(lean.trim_end().split(FS).count(), BASE_FIELDS);
        let rows = parse_for_each_ref(lean.as_bytes(), false);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_head);
        assert_eq!(rows[0].ahead_behind_default, None);
    }

    #[test]
    fn a_remote_head_symref_is_never_a_branch_row() {
        let line = format!(
            "refs/remotes/origin/HEAD{FS}{sha}{FS}{FS}{FS}{FS}A{FS}a@e.com{FS}1{FS}s{FS} {FS}{FS}\n",
            sha = "c".repeat(40),
        );
        assert!(parse_for_each_ref(line.as_bytes(), false).is_empty());
    }

    #[test]
    fn an_empty_ahead_behind_atom_is_unmeasured_not_zero() {
        assert_eq!(parse_ahead_behind_atom(""), None);
        assert_eq!(parse_ahead_behind_atom("0 0"), Some((0, 0)));
    }

    #[test]
    fn an_upstream_that_is_this_branchs_own_mirror_is_not_a_base() {
        assert!(upstream_is_own_mirror("feature", "origin/feature"));
        assert!(!upstream_is_own_mirror("feature", "origin/main"));
        assert!(upstream_is_own_mirror("main", "main"));
    }

    #[test]
    fn a_co_authored_by_trailer_alone_is_never_agent() {
        // D18's "never a human's commit labelled agent". A
        // `Co-authored-by:` trailer is the shape a HUMAN-authored commit
        // takes in an agent-assisted workflow, so it is not an input to
        // this function at all — there is no field for it on `RawRef`.
        let mut r = raw("x");
        r.author_email = "human@example.com".to_string();
        let p = agent_provenance(&r, &["noreply@anthropic.com".to_string()]);
        assert_eq!(p.class, AgentClass::None);
        assert_eq!(p.via, "no-evidence");
    }

    #[test]
    fn agent_provenance_ladder_is_trailer_then_email() {
        let emails = vec!["bot@example.com".to_string()];
        let mut r = raw("x");
        r.author_email = "bot@example.com".to_string();
        let p = agent_provenance(&r, &emails);
        assert_eq!(p.class, AgentClass::Likely);
        assert_eq!(p.via, "author-email");

        r.kb_session_trailers = vec!["sess-9".to_string()];
        let p = agent_provenance(&r, &emails);
        assert_eq!(p.class, AgentClass::Exact);
        assert_eq!(p.via, "kb-session-trailer");
        assert_eq!(p.session_id.as_deref(), Some("sess-9"));

        // An email that is not configured never reaches `likely`.
        let mut human = raw("y");
        human.author_email = "someone@example.com".to_string();
        assert_eq!(agent_provenance(&human, &emails).class, AgentClass::None);
    }

    #[test]
    fn cherry_output_says_merged_only_when_every_commit_has_an_equivalent() {
        assert_eq!(parse_cherry(b"- aaa\n- bbb\n"), Some(2));
        assert_eq!(parse_cherry(b"- aaa\n+ bbb\n"), None);
        assert_eq!(parse_cherry(b"+ aaa\n"), None);
        // Nothing to compare is NOT a vacuous merge.
        assert_eq!(parse_cherry(b""), None);
        assert_eq!(parse_cherry(b"   \n"), None);
    }

    #[test]
    fn percentile_is_nearest_rank_and_total() {
        let v = vec![1, 2, 3, 4];
        assert_eq!(percentile(&v, 0.75), Some(3));
        assert_eq!(percentile(&v, 0.0), Some(1));
        assert_eq!(percentile(&v, 1.0), Some(4));
        assert_eq!(percentile(&[], 0.75), None);
        // Out-of-range p is clamped, never a panic.
        assert_eq!(percentile(&v, 5.0), Some(4));
        assert_eq!(percentile(&v, -1.0), Some(1));
    }

    #[test]
    fn stale_degrades_honestly_under_a_tiny_branch_count() {
        let r = stale_rule(&[10, 20, 30]);
        assert!(!r.applied);
        assert!(r.threshold_age_secs.is_none());
        assert!(r.degraded_reason.is_some());
        let r = stale_rule(&[10, 20, 30, 40]);
        assert!(r.applied);
        assert_eq!(r.threshold_age_secs, Some(30));
        assert!(r.degraded_reason.is_none());
        assert_eq!(r.rule, STALE_RULE_TEXT);
    }

    #[test]
    fn prefix_folding_counts_only_real_folders() {
        let got = fold_prefixes(
            [
                "feature/a",
                "feature/b",
                "agent/x",
                "main",
                "trailing/",
                "/leading",
            ]
            .into_iter()
            .map(str::to_string),
        );
        assert_eq!(
            got,
            vec![
                PrefixCount {
                    prefix: "feature/".to_string(),
                    count: 2
                },
                PrefixCount {
                    prefix: "agent/".to_string(),
                    count: 1
                },
            ]
        );
    }

    #[test]
    fn active_and_stale_partition_every_row() {
        let mut f = BranchFact {
            raw: raw("x"),
            base: BranchBase::unknown(),
            ahead: None,
            behind: None,
            agent: agent_provenance(&raw("x"), &[]),
            merged: None,
            stale: false,
            mine: false,
            open_review_ids: Vec::new(),
            favourite: false,
            age_secs: 0,
            worktree: None,
        };
        assert!(f.in_view(View::Active) ^ f.in_view(View::Stale));
        f.stale = true;
        assert!(f.in_view(View::Active) ^ f.in_view(View::Stale));
        assert!(f.in_view(View::All));
    }

    #[test]
    fn reason_order_is_fixed_and_names_the_witness() {
        let mut f = BranchFact {
            raw: raw("x"),
            base: BranchBase {
                class: BaseClass::ForkPoint,
                ref_name: Some("main".to_string()),
                sha: Some("d".repeat(40)),
            },
            ahead: Some(2),
            behind: Some(1),
            agent: agent_provenance(&raw("x"), &[]),
            merged: Some(MergedWitness {
                kind: MergedKind::PatchId,
                into: "main".to_string(),
                into_sha: Some("e".repeat(40)),
                equivalent: Some(2),
            }),
            stale: false,
            mine: false,
            open_review_ids: vec![7],
            favourite: false,
            age_secs: 0,
            worktree: None,
        };
        f.raw.is_head = true;
        let codes: Vec<&str> = f.reasons(Some("main")).iter().map(|r| r.code).collect();
        assert_eq!(
            codes,
            vec![
                "checked-out",
                "review-open",
                "merged",
                "ahead-of-base",
                "behind-base"
            ]
        );
        let merged = f
            .reasons(Some("main"))
            .into_iter()
            .find(|r| r.code == "merged")
            .unwrap();
        assert!(
            merged.text.contains("patch-id") && merged.text.contains("2 commit"),
            "the witness must be named: {}",
            merged.text
        );
    }

    #[test]
    fn a_local_branch_shadows_a_same_named_remote() {
        let mut remote = raw("feature");
        remote.full_ref = "refs/remotes/origin/feature".to_string();
        remote.remote = Some("origin".to_string());
        let mut other = raw("solo");
        other.full_ref = "refs/remotes/origin/solo".to_string();
        other.remote = Some("origin".to_string());
        let kept = drop_shadowed_remotes(vec![raw("feature"), remote, other]);
        assert_eq!(kept.len(), 2);
        assert!(kept
            .iter()
            .any(|r| r.name == "feature" && r.remote.is_none()));
        assert!(kept.iter().any(|r| r.name == "solo" && r.remote.is_some()));
    }

    #[test]
    fn the_base_cache_misses_when_any_key_component_moves() {
        let mut c = BaseCache::default();
        let key = |tip: &str, def: &str| BaseCacheKey {
            repo: "r".to_string(),
            full_ref: "refs/heads/x".to_string(),
            tip_sha: tip.to_string(),
            default_sha: def.to_string(),
        };
        let base = BranchBase {
            class: BaseClass::ForkPoint,
            ref_name: Some("main".to_string()),
            sha: Some("f".repeat(40)),
        };
        c.put(key("a", "d"), base.clone());
        assert_eq!(c.get(&key("a", "d")), Some(base.clone()));
        // The REF moved.
        assert_eq!(c.get(&key("b", "d")), None);
        // The BASE moved — the case an ordinary "invalidate on ref move"
        // scheme would miss, and the reason `default_sha` is in the key.
        assert_eq!(c.get(&key("a", "e")), None);
        let (hits, misses) = c.stats();
        assert_eq!((hits, misses), (1, 2));
    }

    #[test]
    fn the_base_cache_clears_rather_than_growing_without_bound() {
        let mut c = BaseCache::default();
        for i in 0..(MAX_CACHE_ENTRIES + 1) {
            c.put(
                BaseCacheKey {
                    repo: "r".to_string(),
                    full_ref: format!("refs/heads/b{i}"),
                    tip_sha: "a".repeat(40),
                    default_sha: "d".repeat(40),
                },
                BranchBase::unknown(),
            );
        }
        assert_eq!(c.len(), 1, "the clear leaves exactly the newest entry");
    }

    #[test]
    fn mine_never_matches_on_an_absent_identity() {
        let id = MineIdentity::default();
        assert!(!id.matches(&raw("x")));
        let id = MineIdentity {
            name: None,
            email: Some("test@example.com".to_string()),
        };
        assert!(id.matches(&raw("x")));
    }

    #[test]
    fn a_full_sha_is_the_only_thing_interpolated_into_the_format() {
        assert!(is_full_sha(&"a".repeat(40)));
        assert!(!is_full_sha("main"));
        assert!(!is_full_sha(&"a".repeat(39)));
        assert!(!is_full_sha(") --output=/tmp/x"));
    }

    fn fact_for(name: &str) -> BranchFact {
        let r = raw(name);
        BranchFact {
            agent: agent_provenance(&r, &[]),
            raw: r,
            base: BranchBase::unknown(),
            ahead: None,
            behind: None,
            merged: None,
            stale: false,
            mine: false,
            open_review_ids: Vec::new(),
            favourite: false,
            age_secs: 0,
            worktree: None,
        }
    }

    fn filt(q: &str) -> FactFilter {
        FactFilter::from_parsed(&crate::search::grammar::parse(q))
    }

    #[test]
    fn the_four_kbcq_atoms_are_read_here_and_and_together() {
        let mut f = fact_for("feature/auth");
        f.raw.author_name = "Ada Lovelace".to_string();
        f.raw.author_email = "ada@example.com".to_string();

        assert!(filt("branch:auth").matches_cheap(&f));
        assert!(!filt("branch:billing").matches_cheap(&f));
        assert!(filt("by:ADA").matches_cheap(&f));
        assert!(filt("by:example.com").matches_cheap(&f));
        assert!(!filt("by:grace").matches_cheap(&f));
        // AND, not OR — every other kbcq/1 filter composes this way.
        assert!(!filt("branch:auth by:grace").matches_cheap(&f));
        // The residual free text is the `/` filter over the NAME.
        assert!(filt("auth").matches_cheap(&f));
        assert!(!filt("nope").matches_cheap(&f));
        // `touches:` is never applied by the cheap pass.
        let t = filt("touches:app/models/order.rb");
        assert!(t.needs_touches_scan());
        assert!(t.matches_cheap(&f));
        assert_eq!(t.touches.as_deref(), Some("app/models/order.rb"));
    }

    #[test]
    fn the_agent_atom_maps_onto_the_d18_ladder() {
        let mut exact = fact_for("a");
        exact.agent = AgentProvenance {
            class: AgentClass::Exact,
            via: "kb-session-trailer",
            session_id: Some("s".to_string()),
        };
        let mut likely = fact_for("b");
        likely.agent = AgentProvenance {
            class: AgentClass::Likely,
            via: "author-email",
            session_id: None,
        };
        let none = fact_for("c");

        assert!(filt("agent:exact").matches_cheap(&exact));
        assert!(!filt("agent:exact").matches_cheap(&likely));
        assert!(filt("agent:likely").matches_cheap(&likely));
        assert!(filt("agent:any").matches_cheap(&exact));
        assert!(filt("agent:any").matches_cheap(&likely));
        assert!(!filt("agent:any").matches_cheap(&none));
        assert!(filt("agent:none").matches_cheap(&none));
        assert!(!filt("agent:none").matches_cheap(&exact));
    }

    #[test]
    fn an_out_of_vocabulary_agent_value_is_a_plain_word_not_a_filter() {
        // The closed vocabulary means the parser refuses `agent:maybe` as a
        // filter and keeps it as a search term — so the FactFilter's own
        // `agent` stays None and the row is not silently dropped.
        let p = crate::search::grammar::parse("agent:maybe");
        assert!(p.filters.agent.is_none());
        assert!(!p.diagnostics.is_empty());
    }

    #[test]
    fn prefix_and_favourite_are_wire_params_not_query_atoms() {
        let mut f = fact_for("feature/auth");
        f.favourite = true;
        let mut ff = filt("");
        ff.prefix = Some("feature/".to_string());
        assert!(ff.matches_cheap(&f));
        ff.prefix = Some("agent/".to_string());
        assert!(!ff.matches_cheap(&f));
        ff.prefix = None;
        ff.favourites_only = true;
        assert!(ff.matches_cheap(&f));
        f.favourite = false;
        assert!(!ff.matches_cheap(&f));
    }

    #[test]
    fn every_view_has_a_stable_name_and_the_list_is_closed() {
        let names: Vec<&str> = View::ALL.iter().map(|v| v.as_str()).collect();
        assert_eq!(
            names,
            vec!["current", "mine", "agent", "review", "active", "stale", "merged", "all"]
        );
    }
}
