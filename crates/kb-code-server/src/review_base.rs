//! RS-U6 — the review BASE MODEL (README "kb-code reviews that diff like
//! GitHub", §3/§6/§10 step 3/§12; D13–D16).
//!
//! A review stores a base POLICY, not a commit:
//!
//! * `track(B)` — follow branch `B` of the forge project, fetched into the
//!   review store as `refs/remotes/base/B`;
//! * `local(B)` — follow a member clone's branch, `refs/remotes/work-<id>/B`;
//! * `pin(sha)` — deliberately frozen.
//!
//! plus who set it (`auto` may be re-resolved by kb, `user` only by a
//! person, `legacy` is a pre-upgrade row that keeps its old behaviour).
//! Every capture resolves the policy to a tip `T` and records
//! `merge-base(T, head)`; a patchset is minted only when the PAIR
//! `(head tip, merge-base)` changes ([`decide_kind`]).
//!
//! This file is the PURE half — no git, no DB:
//!
//! * [`classify_base`] — the ONE `--base` grammar parser, shared by the CLI
//!   (which forwards the string verbatim) and every HTTP creation route
//!   (README §6 table). Context it cannot know by itself (which remotes map
//!   to the project, whether a branch exists, what a rev resolves to) comes
//!   from a [`BaseProbe`].
//! * [`legacy_spec`] — read-time classification of a pre-V0045 row (README
//!   §10 step 3). Pure; its answer is never persisted by a read.
//! * [`resolve_pr_base`] / [`resolve_non_pr_base`] / [`pick_default_branch`]
//!   — the resolution chain (README §6), every rung excluding the head's own
//!   branch.
//! * [`decide_kind`] — why a patchset exists (`initial` | `push` | `rebase`
//!   | `base-moved` | `base-corrected` | `retarget`).
//! * The additive envelope DTOs ([`ReviewBaseOut`], [`BaseWarningOut`]) and
//!   the stored `reviews.base_status` JSON ([`BaseStatus`]).
//!
//! The git/DB half (fetch into the store, capture under the ops lock) is
//! [`capture`].

pub mod capture;
#[cfg(test)]
mod tests;

use crate::git::Revspec;
use crate::review_store::url::RefName;
use crate::routes::ApiError;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

// --- error + warning vocabulary -------------------------------------------

/// `--base` input the grammar cannot turn into a policy (README §6: 400).
pub const URN_BASE_UNRESOLVED: &str = "urn:kb:errors:base-unresolved";
/// No rung of the resolution chain produced a base (and nothing may guess).
pub const URN_BASE_UNDETERMINED: &str = "urn:kb:errors:base-undetermined";
/// The store's forge has no PR head ref shape (`forge = "none"`).
pub const URN_PR_REFS_UNSUPPORTED: &str = "urn:kb:errors:pr-refs-unsupported";
/// A PR head fetch failed (bad number, unreachable forge).
pub const URN_PR_FETCH_FAILED: &str = "urn:kb:errors:pr-fetch-failed";
/// The head could not be resolved, or is not in the review store.
pub const URN_HEAD_UNAVAILABLE: &str = "urn:kb:errors:head-unavailable";
/// The policy's base tip is not available (never fetched, vanished).
pub const URN_BASE_UNAVAILABLE: &str = "urn:kb:errors:base-unavailable";
/// Head and base share no history.
pub const URN_NO_MERGE_BASE: &str = "urn:kb:errors:no-merge-base";
/// The capture itself failed (git or DB).
pub const URN_CAPTURE_FAILED: &str = "urn:kb:errors:capture-failed";

/// Warning codes carried in `warnings[]` (README §12, design-general
/// "Warning codes").
pub mod warn {
    pub const BASE_PINNED: &str = "base-pinned";
    pub const BASE_UPGRADED: &str = "base-upgraded";
    pub const PR_TARGET_ASSUMED: &str = "pr-target-assumed";
    pub const DEFAULT_BRANCH_GUESSED: &str = "default-branch-guessed";
    pub const RETARGETED: &str = "retargeted";
    pub const PR_TARGET_DIFFERS: &str = "pr-target-differs";
    pub const STALE_MIRROR: &str = "stale-mirror";
    pub const BASE_REFRESH_FAILED: &str = "base-refresh-failed";
    pub const BASE_OFFLINE: &str = "base-offline";
    pub const PR_REFRESH_FAILED: &str = "pr-refresh-failed";
}

/// One `warnings[]` entry on a review envelope.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BaseWarningOut {
    pub code: String,
    pub message: String,
}

pub fn warning(code: &str, message: impl Into<String>) -> BaseWarningOut {
    BaseWarningOut {
        code: code.to_string(),
        message: message.into(),
    }
}

/// A typed refusal from base resolution or capture. Renders as RFC 7807
/// problem+json through [`ApiError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseError {
    pub status: u16,
    pub urn: &'static str,
    pub message: String,
}

impl BaseError {
    pub fn new(status: u16, urn: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            urn,
            message: message.into(),
        }
    }
    pub fn unresolved(message: impl Into<String>) -> Self {
        Self::new(400, URN_BASE_UNRESOLVED, message)
    }
    pub fn undetermined(message: impl Into<String>) -> Self {
        Self::new(400, URN_BASE_UNDETERMINED, message)
    }
    /// The URN's last segment (`base-unresolved`, …).
    pub fn code(&self) -> &'static str {
        self.urn.rsplit(':').next().unwrap_or(self.urn)
    }
}

impl std::fmt::Display for BaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message)
    }
}

impl From<BaseError> for ApiError {
    fn from(e: BaseError) -> Self {
        let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST);
        ApiError::new(status, e.message).with_problem_type(e.urn)
    }
}

// --- the policy ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseMode {
    Track,
    Local,
    Pin,
}

impl BaseMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Local => "local",
            Self::Pin => "pin",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "track" => Some(Self::Track),
            "local" => Some(Self::Local),
            "pin" => Some(Self::Pin),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetBy {
    Auto,
    User,
    Legacy,
}

impl SetBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::User => "user",
            Self::Legacy => "legacy",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "auto" => Self::Auto,
            "user" => Self::User,
            _ => Self::Legacy,
        }
    }
}

/// Which rung produced the policy (`base_status.source`, README §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseSource {
    Explicit,
    ForgeApi,
    Caller,
    MergeRef,
    DefaultAssumed,
    Upstream,
    StackParent,
    /// A pre-V0045 row, classified on read.
    Legacy,
}

impl BaseSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::ForgeApi => "forge-api",
            Self::Caller => "caller",
            Self::MergeRef => "merge-ref",
            Self::DefaultAssumed => "default-assumed",
            Self::Upstream => "upstream",
            Self::StackParent => "stack-parent",
            Self::Legacy => "legacy",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "explicit" => Self::Explicit,
            "forge-api" => Self::ForgeApi,
            "caller" => Self::Caller,
            "merge-ref" => Self::MergeRef,
            "default-assumed" => Self::DefaultAssumed,
            "upstream" => Self::Upstream,
            "stack-parent" => Self::StackParent,
            "legacy" => Self::Legacy,
            _ => return None,
        })
    }
    /// Human label for the one-line stderr summary (README §12).
    pub fn label(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::ForgeApi => "forge api",
            Self::Caller => "caller",
            Self::MergeRef => "merge ref",
            Self::DefaultAssumed => "default branch, assumed",
            Self::Upstream => "upstream",
            Self::StackParent => "stack parent",
            Self::Legacy => "legacy row",
        }
    }
}

/// A resolved base policy (README §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasePolicy {
    pub mode: BaseMode,
    /// `track`/`local`: the branch name (never `refs/…`).
    pub branch: Option<String>,
    /// `pin`: the full 40-hex commit.
    pub pin: Option<String>,
    /// `local`: the member repo id whose branch is followed (`None` = the
    /// review's own repo).
    pub member: Option<i64>,
    pub set_by: SetBy,
    pub source: BaseSource,
}

impl BasePolicy {
    pub fn track(branch: &str, set_by: SetBy, source: BaseSource) -> Self {
        Self {
            mode: BaseMode::Track,
            branch: Some(branch.to_string()),
            pin: None,
            member: None,
            set_by,
            source,
        }
    }
    pub fn local(branch: &str, set_by: SetBy, source: BaseSource) -> Self {
        Self {
            mode: BaseMode::Local,
            branch: Some(branch.to_string()),
            pin: None,
            member: None,
            set_by,
            source,
        }
    }
    pub fn pin(sha: &str, set_by: SetBy, source: BaseSource) -> Self {
        Self {
            mode: BaseMode::Pin,
            branch: None,
            pin: Some(sha.to_ascii_lowercase()),
            member: None,
            set_by,
            source,
        }
    }

    /// The `reviews.base_ref` value written for this policy — kept a
    /// git-resolvable ref in the MEMBER clone so every legacy reader (and
    /// the pre-store fallback) keeps working: `refs/remotes/<R>/B` for
    /// `track` (R = the member's remote that maps to the project, `origin`
    /// when none is known), `refs/heads/B` for `local`, the sha for `pin`.
    pub fn display_base_ref(&self, mapped_remote: Option<&str>) -> String {
        match self.mode {
            BaseMode::Pin => self.pin.clone().unwrap_or_default(),
            BaseMode::Local => format!("refs/heads/{}", self.branch.as_deref().unwrap_or("")),
            BaseMode::Track => format!(
                "refs/remotes/{}/{}",
                mapped_remote.unwrap_or("origin"),
                self.branch.as_deref().unwrap_or("")
            ),
        }
    }

    /// The pre-RS-U6 `base_source` label start-pr envelopes and
    /// `pr_meta_json` still carry (`explicit` | `merge-base` |
    /// `local-default`), mapped from the new source (design-general
    /// "Envelopes").
    pub fn legacy_base_source(&self) -> &'static str {
        match (self.source, self.mode) {
            (BaseSource::Explicit, _) => "explicit",
            (_, BaseMode::Local) => "local-default",
            _ => "merge-base",
        }
    }
}

/// What a review's base IS, as capture sees it: a policy, or — for a
/// legacy row [`legacy_spec`] could not classify — its `base_ref` string,
/// evaluated verbatim through rev-parse DWIM exactly as before the upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveBase {
    Policy(BasePolicy),
    Verbatim(String),
}

impl EffectiveBase {
    pub fn policy(&self) -> Option<&BasePolicy> {
        match self {
            Self::Policy(p) => Some(p),
            Self::Verbatim(_) => None,
        }
    }
}

// --- validation helpers ------------------------------------------------------

/// `git check-ref-format --branch` semantics: a name that is a valid
/// `refs/heads/<b>` ref name, not option-shaped (`Revspec` owns that
/// predicate — see `git/revspec.rs`), not `HEAD`, and not itself a full
/// `refs/` name.
pub fn valid_branch_name(b: &str) -> bool {
    !b.is_empty()
        && b != "HEAD"
        && !b.starts_with("refs/")
        && !b.contains("@{")
        && Revspec::parse(b).is_ok()
        && RefName::branch(b).is_ok()
}

fn is_hex40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// Split `<R>/<B>` by the LONGEST remote-name prefix in `remotes` (remote
/// names may themselves contain `/`). `None` when no remote matches or
/// the branch part is not a valid branch name.
pub fn split_remote_branch(s: &str, remotes: &[String]) -> Option<(String, String)> {
    let mut best: Option<(String, String)> = None;
    for r in remotes {
        if let Some(rest) = s.strip_prefix(r.as_str()).and_then(|t| t.strip_prefix('/')) {
            if valid_branch_name(rest) && best.as_ref().is_none_or(|(br, _)| r.len() > br.len()) {
                best = Some((r.clone(), rest.to_string()));
            }
        }
    }
    best
}

// --- the grammar ----------------------------------------------------------------

/// Context the grammar asks about (implemented over the review store and
/// the member clone by [`capture`]; over tables in tests).
pub trait BaseProbe {
    /// Member remote names whose URL maps to the review's forge project.
    fn mapped_remotes(&self) -> Vec<String>;
    /// Does the member clone have `refs/heads/<b>`?
    fn local_branch_exists(&self, b: &str) -> bool;
    /// Is `b` a known branch of the forge project (store `base/<b>`, or a
    /// mapped remote-tracking ref in the member)?
    fn remote_branch_exists(&self, b: &str) -> bool;
    /// `rev` → a full commit sha, if it resolves.
    fn resolve_rev(&self, rev: &Revspec) -> Option<String>;
}

/// The grammar's answer. `policy: None` = `auto` (run the chain).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub policy: Option<BasePolicy>,
    pub warnings: Vec<BaseWarningOut>,
    /// A bare `<B>` produced this policy (an old SPA build sends the PR's
    /// own target this way — [`resolve_pr_base`] treats it as the forge
    /// rung when it equals the API's `base.ref`).
    pub from_bare: bool,
}

impl Classified {
    fn of(policy: BasePolicy) -> Self {
        Self {
            policy: Some(policy),
            warnings: vec![],
            from_bare: false,
        }
    }
}

fn pinned_warning(sha: &str, what: &str) -> BaseWarningOut {
    warning(
        warn::BASE_PINNED,
        format!(
            "{what} pins this review to commit {} — it will not follow its target branch; pass a branch name to track it",
            &sha[..sha.len().min(12)]
        ),
    )
}

/// The ONE `--base` parser (README §6 table; first match wins):
///
/// | input | result |
/// |---|---|
/// | absent / `auto` | run the chain (`policy: None`) |
/// | `pin:<rev>` / `local:<B>` / `track:<B>` | explicit mode, `user` |
/// | 40-hex | `pin` + `base-pinned` |
/// | `refs/remotes/<R>/<B>` / `<R>/<B>`, R mapping to the project | `track(B)` |
/// | `refs/heads/<B>` | `local(B)` |
/// | bare `<B>` on a PR | `track(B)` |
/// | bare `<B>` otherwise | `local(B)` if the member has it, else `track(B)` if the forge has it |
/// | any other rev (short sha, tag, `HEAD~3`) | `pin` + `base-pinned` |
/// | otherwise | 400 `base-unresolved` |
pub fn classify_base(
    input: Option<&str>,
    is_pr: bool,
    probe: &dyn BaseProbe,
) -> Result<Classified, BaseError> {
    let s = input.map(str::trim).unwrap_or("");
    if s.is_empty() || s == "auto" {
        return Ok(Classified {
            policy: None,
            warnings: vec![],
            from_bare: false,
        });
    }
    let user = |p: BasePolicy| Ok(Classified::of(p));
    if let Some(rev) = s.strip_prefix("pin:") {
        let spec = Revspec::parse(rev)
            .map_err(|_| BaseError::unresolved(format!("pin:{rev:?} is not a valid revision")))?;
        let sha = probe.resolve_rev(&spec).ok_or_else(|| {
            BaseError::unresolved(format!("pin:{rev} does not resolve to a commit"))
        })?;
        return user(BasePolicy::pin(&sha, SetBy::User, BaseSource::Explicit));
    }
    if let Some(b) = s.strip_prefix("local:") {
        if !valid_branch_name(b) {
            return Err(BaseError::unresolved(format!(
                "local:{b:?} is not a valid branch name"
            )));
        }
        return user(BasePolicy::local(b, SetBy::User, BaseSource::Explicit));
    }
    if let Some(b) = s.strip_prefix("track:") {
        if !valid_branch_name(b) {
            return Err(BaseError::unresolved(format!(
                "track:{b:?} is not a valid branch name"
            )));
        }
        return user(BasePolicy::track(b, SetBy::User, BaseSource::Explicit));
    }
    if is_hex40(s) {
        let sha = s.to_ascii_lowercase();
        let spec = Revspec::parse(&sha).map_err(|_| BaseError::unresolved("bad sha"))?;
        if probe.resolve_rev(&spec).is_none() {
            return Err(BaseError::unresolved(format!(
                "commit {sha} is not known to this repo"
            )));
        }
        return Ok(Classified {
            warnings: vec![pinned_warning(&sha, "a commit sha")],
            ..Classified::of(BasePolicy::pin(&sha, SetBy::User, BaseSource::Explicit))
        });
    }
    let remotes = probe.mapped_remotes();
    if let Some(rest) = s.strip_prefix("refs/remotes/") {
        if let Some((_, b)) = split_remote_branch(rest, &remotes) {
            return user(BasePolicy::track(&b, SetBy::User, BaseSource::Explicit));
        }
        return other_rev(s, probe);
    }
    if let Some(b) = s.strip_prefix("refs/heads/") {
        if valid_branch_name(b) {
            return user(BasePolicy::local(b, SetBy::User, BaseSource::Explicit));
        }
        return Err(BaseError::unresolved(format!(
            "{s:?} is not a valid branch ref"
        )));
    }
    if s.starts_with("refs/") {
        return other_rev(s, probe);
    }
    // `<R>/<B>` — only when no LOCAL branch has that literal name.
    if let Some((_, b)) = split_remote_branch(s, &remotes) {
        if !probe.local_branch_exists(s) {
            return user(BasePolicy::track(&b, SetBy::User, BaseSource::Explicit));
        }
    }
    if valid_branch_name(s) {
        let bare = |p: BasePolicy| {
            Ok(Classified {
                from_bare: true,
                ..Classified::of(p)
            })
        };
        if is_pr {
            return bare(BasePolicy::track(s, SetBy::User, BaseSource::Explicit));
        }
        if probe.local_branch_exists(s) {
            return bare(BasePolicy::local(s, SetBy::User, BaseSource::Explicit));
        }
        if probe.remote_branch_exists(s) {
            return bare(BasePolicy::track(s, SetBy::User, BaseSource::Explicit));
        }
    }
    other_rev(s, probe)
}

fn other_rev(s: &str, probe: &dyn BaseProbe) -> Result<Classified, BaseError> {
    let Ok(spec) = Revspec::parse(s) else {
        return Err(BaseError::unresolved(format!(
            "--base {s:?} is neither a branch, a ref nor a revision"
        )));
    };
    match probe.resolve_rev(&spec) {
        Some(sha) => Ok(Classified {
            warnings: vec![pinned_warning(&sha, &format!("--base {s}"))],
            ..Classified::of(BasePolicy::pin(&sha, SetBy::User, BaseSource::Explicit))
        }),
        None => Err(BaseError::unresolved(format!(
            "--base {s:?} does not name a branch, a ref or a commit of this repo"
        ))),
    }
}

// --- legacy rows ---------------------------------------------------------------

/// A legacy row's read-time class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyClass {
    pub effective: EffectiveBase,
    pub warnings: Vec<BaseWarningOut>,
    /// `true` when the row would be UPGRADED (a `track(B)`, `auto` answer)
    /// — persisted only by the next explicit action after a successful
    /// fetch, never by a read.
    pub upgraded: bool,
}

/// Classify a pre-V0045 review on read (README §10 step 3 — never
/// rewritten silently):
///
/// * a 40-hex `base_ref` stays `pin`, `set_by=legacy`, with an amber
///   `base-pinned` warning;
/// * `refs/remotes/R/B` or `R/B`, R a remote mapping to the project →
///   `track(B)`, `auto`;
/// * a bare `B` on a PR-bound row (the SPA's `"main"`), when it agrees with
///   the PR's recorded target (or none is recorded) → `track(B)`, `auto`,
///   with a `base-upgraded` warning;
/// * anything else → evaluated verbatim, exactly as before.
pub fn legacy_spec(
    base_ref: &str,
    pr_bound: bool,
    pr_meta_base_ref: Option<&str>,
    mapped_remotes: &[String],
) -> LegacyClass {
    let r = base_ref.trim();
    let verbatim = LegacyClass {
        effective: EffectiveBase::Verbatim(r.to_string()),
        warnings: vec![],
        upgraded: false,
    };
    if is_hex40(r) {
        let sha = r.to_ascii_lowercase();
        return LegacyClass {
            warnings: vec![pinned_warning(&sha, "this legacy review's base")],
            effective: EffectiveBase::Policy(BasePolicy::pin(
                &sha,
                SetBy::Legacy,
                BaseSource::Legacy,
            )),
            upgraded: false,
        };
    }
    let upgraded = |b: &str, warnings| LegacyClass {
        effective: EffectiveBase::Policy(BasePolicy::track(b, SetBy::Auto, BaseSource::Legacy)),
        warnings,
        upgraded: true,
    };
    if let Some(rest) = r.strip_prefix("refs/remotes/") {
        return match split_remote_branch(rest, mapped_remotes) {
            Some((_, b)) => upgraded(&b, vec![]),
            None => verbatim,
        };
    }
    if r.starts_with("refs/") {
        return verbatim;
    }
    if let Some((_, b)) = split_remote_branch(r, mapped_remotes) {
        return upgraded(&b, vec![]);
    }
    if pr_bound && valid_branch_name(r) && pr_meta_base_ref.is_none_or(|m| m == r) {
        return upgraded(
            r,
            vec![warning(
                warn::BASE_UPGRADED,
                format!(
                    "this PR review's base {r:?} named the local branch; it now tracks the forge's {r} (pass --base local:{r} to keep the local branch)"
                ),
            )],
        );
    }
    verbatim
}

/// The effective base of a stored row: its V0045 policy columns when set,
/// else [`legacy_spec`] over `base_ref`.
#[allow(clippy::too_many_arguments)]
pub fn effective_base(
    base_ref: &str,
    base_mode: Option<&str>,
    base_branch: Option<&str>,
    base_member: Option<i64>,
    base_set_by: &str,
    base_source: Option<&str>,
    pr_bound: bool,
    pr_meta_base_ref: Option<&str>,
    mapped_remotes: &[String],
) -> LegacyClass {
    let mode = base_mode.and_then(BaseMode::parse);
    let set_by = SetBy::parse(base_set_by);
    let source = base_source
        .and_then(BaseSource::parse)
        .unwrap_or(BaseSource::Explicit);
    let from_columns = match mode {
        Some(BaseMode::Pin) if is_hex40(base_ref.trim()) => {
            Some(BasePolicy::pin(base_ref.trim(), set_by, source))
        }
        Some(m @ (BaseMode::Track | BaseMode::Local)) => base_branch
            .filter(|b| valid_branch_name(b))
            .map(|b| BasePolicy {
                mode: m,
                branch: Some(b.to_string()),
                pin: None,
                member: base_member,
                set_by,
                source,
            }),
        _ => None,
    };
    match from_columns {
        Some(p) => {
            let warnings = if p.mode == BaseMode::Pin {
                vec![pinned_warning(p.pin.as_deref().unwrap_or(""), "the base")]
            } else {
                vec![]
            };
            LegacyClass {
                effective: EffectiveBase::Policy(p),
                warnings,
                upgraded: false,
            }
        }
        None => legacy_spec(base_ref, pr_bound, pr_meta_base_ref, mapped_remotes),
    }
}

// --- the resolution chain -----------------------------------------------------------

/// The default-branch ladder (README §6): config → the forge's `HEAD`
/// symref (probed and cached by the caller) → exactly one of
/// `main`/`master`/`trunk`/`develop` among `candidates` (with
/// `default-branch-guessed`) → refuse. The head's own branch is never an
/// answer.
pub fn pick_default_branch(
    config: Option<&str>,
    symref: Option<&str>,
    candidates: &[String],
    head_branch: Option<&str>,
) -> Result<(String, Vec<BaseWarningOut>), BaseError> {
    let usable = |b: &str| valid_branch_name(b) && Some(b) != head_branch;
    if let Some(c) = config.map(str::trim).filter(|c| usable(c)) {
        return Ok((c.to_string(), vec![]));
    }
    if let Some(s) = symref.map(str::trim).filter(|s| usable(s)) {
        return Ok((s.to_string(), vec![]));
    }
    let found: Vec<&str> = ["main", "master", "trunk", "develop"]
        .into_iter()
        .filter(|c| candidates.iter().any(|x| x == c) && usable(c))
        .collect();
    if found.len() == 1 {
        return Ok((
            found[0].to_string(),
            vec![warning(
                warn::DEFAULT_BRANCH_GUESSED,
                format!(
                    "the project's default branch could not be read; guessed {:?} (set [[review.repos]] default_branch to pin it)",
                    found[0]
                ),
            )],
        ));
    }
    Err(BaseError::undetermined(if found.is_empty() {
        "no default branch could be determined (no forge HEAD, no main/master/trunk/develop) — pass --base <branch>".to_string()
    } else {
        format!(
            "the default branch is ambiguous ({}) — pass --base <branch> or set [[review.repos]] default_branch",
            found.join(", ")
        )
    }))
}

/// Inputs to [`resolve_pr_base`].
#[derive(Debug, Clone, Default)]
pub struct PrChain {
    pub explicit: Option<Classified>,
    /// Forge API `base.ref`, when it could be read.
    pub forge_api: Option<String>,
    /// Caller-supplied target (`caller_base_ref`).
    pub caller: Option<String>,
    /// The PR's head branch name, when known (never a base).
    pub head_branch: Option<String>,
}

/// PR/MR target (README §6): explicit → forge API `base.ref` → caller →
/// [merge-ref inference: Phase 2] → default branch with a loud
/// `pr-target-assumed`. `default_branch` is evaluated lazily — only when
/// every earlier rung missed (it may need a network probe).
pub fn resolve_pr_base(
    chain: &PrChain,
    default_branch: impl FnOnce() -> Result<(String, Vec<BaseWarningOut>), BaseError>,
) -> Result<(BasePolicy, Vec<BaseWarningOut>), BaseError> {
    let head = chain.head_branch.as_deref();
    let ok = |b: &str| valid_branch_name(b) && Some(b) != head;
    if let Some(c) = &chain.explicit {
        if let Some(p) = &c.policy {
            // An old SPA build sends the PR's own target as a bare name:
            // that IS the forge rung, and kb may follow it (set_by auto).
            if c.from_bare
                && p.mode == BaseMode::Track
                && chain.forge_api.as_deref() == p.branch.as_deref()
            {
                let b = p.branch.clone().unwrap_or_default();
                return Ok((
                    BasePolicy::track(&b, SetBy::Auto, BaseSource::ForgeApi),
                    c.warnings.clone(),
                ));
            }
            return Ok((p.clone(), c.warnings.clone()));
        }
    }
    if let Some(b) = chain.forge_api.as_deref().filter(|b| ok(b)) {
        return Ok((
            BasePolicy::track(b, SetBy::Auto, BaseSource::ForgeApi),
            vec![],
        ));
    }
    if let Some(b) = chain.caller.as_deref().map(str::trim).filter(|b| ok(b)) {
        return Ok((
            BasePolicy::track(b, SetBy::Auto, BaseSource::Caller),
            vec![],
        ));
    }
    let (d, mut warnings) = default_branch()?;
    warnings.push(warning(
        warn::PR_TARGET_ASSUMED,
        format!(
            "the PR's target branch could not be read from the forge; assumed the default branch {d:?} — pass --base <branch> (or retrack) if it targets another branch"
        ),
    ));
    Ok((
        BasePolicy::track(&d, SetBy::Auto, BaseSource::DefaultAssumed),
        warnings,
    ))
}

/// Inputs to [`resolve_non_pr_base`].
#[derive(Debug, Clone, Default)]
pub struct NonPrChain {
    pub explicit: Option<Classified>,
    /// The stack parent branch, when one was detected.
    pub stack_parent: Option<String>,
    /// The head branch's `@{upstream}` as `(branch, maps_to_project)`.
    pub upstream: Option<(String, bool)>,
    pub head_branch: Option<String>,
    /// Does the project have a forge the store can `track`? (`false` → the
    /// default branch is followed `local`ly.)
    pub has_forge: bool,
}

/// Non-PR review (README §6): explicit → stack parent → `@{upstream}`
/// (`track` when it maps to the project, else `local`) → the default
/// branch. D14: the default is the FORGE's default branch, tracked —
/// never the member's possibly stale local `main` — whenever the project
/// has a forge.
pub fn resolve_non_pr_base(
    chain: &NonPrChain,
    default_branch: impl FnOnce() -> Result<(String, Vec<BaseWarningOut>), BaseError>,
) -> Result<(BasePolicy, Vec<BaseWarningOut>), BaseError> {
    let head = chain.head_branch.as_deref();
    let ok = |b: &str| valid_branch_name(b) && Some(b) != head;
    if let Some(c) = &chain.explicit {
        if let Some(p) = &c.policy {
            return Ok((p.clone(), c.warnings.clone()));
        }
    }
    if let Some(p) = chain.stack_parent.as_deref().filter(|b| ok(b)) {
        return Ok((
            BasePolicy::local(p, SetBy::Auto, BaseSource::StackParent),
            vec![],
        ));
    }
    if let Some((b, maps)) = &chain.upstream {
        if ok(b) {
            let p = if *maps {
                BasePolicy::track(b, SetBy::Auto, BaseSource::Upstream)
            } else {
                BasePolicy::local(b, SetBy::Auto, BaseSource::Upstream)
            };
            return Ok((p, vec![]));
        }
    }
    let (d, warnings) = default_branch()?;
    let p = if chain.has_forge {
        BasePolicy::track(&d, SetBy::Auto, BaseSource::DefaultAssumed)
    } else {
        BasePolicy::local(&d, SetBy::Auto, BaseSource::DefaultAssumed)
    };
    Ok((p, warnings))
}

// --- patchset kind -----------------------------------------------------------------

/// Why a patchset was minted (`review_patchsets.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchsetKind {
    Initial,
    Push,
    Rebase,
    BaseMoved,
    BaseCorrected,
    Retarget,
}

impl PatchsetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Push => "push",
            Self::Rebase => "rebase",
            Self::BaseMoved => "base-moved",
            Self::BaseCorrected => "base-corrected",
            Self::Retarget => "retarget",
        }
    }
}

/// Should a capture mint, and as what? `prev` is the latest patchset's
/// `(tip, merge-base)`. `None` = skip (the pair is unchanged and `force`
/// is off — README D13; a base branch merely advancing never mints).
pub fn decide_kind(
    prev: Option<(&str, &str)>,
    tip: &str,
    merge_base: &str,
    hint: Option<PatchsetKind>,
    force: bool,
) -> Option<PatchsetKind> {
    let Some((ptip, pmb)) = prev else {
        return Some(PatchsetKind::Initial);
    };
    let tip_changed = ptip != tip;
    let mb_changed = pmb != merge_base;
    if !tip_changed && !mb_changed && !force {
        return None;
    }
    if let Some(h) = hint {
        return Some(h);
    }
    Some(match (tip_changed, mb_changed) {
        (true, true) => PatchsetKind::Rebase,
        (true, false) => PatchsetKind::Push,
        (false, true) => PatchsetKind::BaseMoved,
        // force on an identical pair — the old always-mint snapshot.
        (false, false) => PatchsetKind::Push,
    })
}

// --- stored status + envelope DTOs ----------------------------------------------------

/// `reviews.base_status` JSON (README §5.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseStatus {
    /// `ok` | `cached` | `offline` | `refresh-failed` | `base-vanished` |
    /// `unavailable` | `pinned`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `fetched` | `cached` | `failed` | `offline` | `skipped`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fetch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    /// How the last fetch authenticated (`gh-cli (login)`, `local`, …) —
    /// never a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// A failure class slug, when the last fetch failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl BaseStatus {
    pub fn parse(json: Option<&str>) -> Self {
        json.and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// The additive `base{…}` block on review envelopes (README §12).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewBaseOut {
    pub mode: Option<String>,
    pub branch: Option<String>,
    pub set_by: String,
    pub source: Option<String>,
    pub state: Option<String>,
    pub merge_base: Option<String>,
    pub fetched_at: Option<i64>,
    pub last_fetch: Option<String>,
    pub fetched_via: Option<String>,
}

/// Compose [`ReviewBaseOut`] for an effective base, its stored status and
/// the latest patchset's merge-base.
pub fn base_out(
    effective: &EffectiveBase,
    set_by_column: &str,
    status: &BaseStatus,
    merge_base: Option<&str>,
) -> ReviewBaseOut {
    let (mode, branch, set_by, source, default_state) = match effective {
        EffectiveBase::Policy(p) => (
            Some(p.mode.as_str().to_string()),
            p.branch.clone(),
            p.set_by.as_str().to_string(),
            Some(
                status
                    .source
                    .clone()
                    .unwrap_or_else(|| p.source.as_str().to_string()),
            ),
            if p.mode == BaseMode::Pin {
                "pinned"
            } else {
                "unverified"
            },
        ),
        EffectiveBase::Verbatim(_) => (
            None,
            None,
            SetBy::parse(set_by_column).as_str().to_string(),
            status.source.clone(),
            "legacy",
        ),
    };
    ReviewBaseOut {
        mode,
        branch,
        set_by,
        source,
        state: Some(
            status
                .state
                .clone()
                .unwrap_or_else(|| default_state.to_string()),
        ),
        merge_base: merge_base.map(str::to_string),
        fetched_at: status.fetched_at,
        last_fetch: status.last_fetch.clone(),
        fetched_via: status.via.clone(),
    }
}
