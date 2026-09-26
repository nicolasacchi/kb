//! RS-U3 — `ReviewStores`: the daemon's per-boot handle on every review
//! store (README §5, D1–D4). One instance lives on `AppState`
//! (`state.review_stores`).
//!
//! It owns:
//!
//! * the resolved config ([`StoreSettings`]) and the hardened spawner
//!   ([`StoreGit`]);
//! * **registration** — the base-URL ladder + membership, run once per
//!   repo ([`ReviewStores::register_repo`]);
//! * **seeding** — [`ReviewStores::seed`] drives `seed::seed_store` and the
//!   DB state machine (`absent → seeding → ready`, or `broken`). A store
//!   reaches `ready` only over a credential that WORKED: a credential the
//!   operator supplied and that came back refused (D12) is
//!   [`StoreUnavailable::CredentialRefused`], and the row stays `absent`
//!   carrying the class — never a `ready` row over a base nobody fetched.
//! * **locks** — a per-(store, remote) FETCH mutex and a short per-store
//!   OPS mutex (design §4.2), plus the lifetime `flock` per store
//!   (`manifest::StoreLock`). `tokio::sync::Mutex`es: a fetch guard is held
//!   across the `spawn_blocking` await of the fetch itself.
//!   The two mutexes are per-process maps, so every path that WRITES a
//!   store admits through the `flock` as well — [`ReviewStores::open`],
//!   [`ReviewStores::seed`] and [`ReviewStores::import_pending_members`] —
//!   which is what makes one-writer-per-store hold across daemons: each
//!   of them puts the claim in `held` and KEEPS it there for the rest of
//!   this process's life, so the guarantee covers the whole write, not
//!   just an admission check. A `store-locked` refusal therefore means
//!   another PROCESS holds the store, never this one.
//!
//! # The API later units call
//!
//! * [`ReviewStores::handle_for_repo`] — a `ready`, manifest-verified,
//!   locked store for a repo name, or [`StoreUnavailable`] (reads fall back
//!   to the user repo, README §10 step 1).
//! * [`ReviewStores::admit_mutation`] — the same, but a store that is
//!   `seeding` refuses with 503 `urn:kb:errors:store-seeding` +
//!   `retry_after` ([`StoreRefusal`] renders it).
//! * [`ReviewStores::fetch_lock`] / [`ReviewStores::ops_lock`].
//! * [`ReviewStores::resolve_credential`] and
//!   [`ReviewStores::fetch_base`] — the credentialed base fetch.
//!
//! Every method that touches git or the DB is SYNCHRONOUS; call it from
//! `spawn_blocking`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use super::classify::FailureClass;
use super::cred::{
    resolve_fetch_credential, CredError, FetchCredential, GhCli, LiveProbes, Resolution,
};
use super::git::StoreGit;
use super::key::{https_url_for_key, local_store_key, split_key};
use super::ladder::{self, LadderInput, LadderOutcome, NoForkCheck, RefusedRemote, RemoteInfo};
use super::manifest::{self, ManifestProblem, StoreLock};
use super::seed::{self, BaseFetch, ExpectedPatchset, SeedMember, SeedPlan, SeedReport};
use super::settings::StoreSettings;
use super::url::{RemoteName, RemoteUrl};
use crate::config::{RepoEntry, ReviewSection};
use crate::store::{ReviewStoreRow, Store};

/// Seconds a caller is told to wait while a store seeds.
pub const SEEDING_RETRY_AFTER_SECS: u64 = 30;
/// `urn:kb:errors:store-seeding`.
pub const URN_STORE_SEEDING: &str = "urn:kb:errors:store-seeding";
/// Suffix on a persisted `cred_reason` when the token's scopes are broader
/// than fetch + GET need (D9: shown, never hidden).
pub const BROADER_THAN_NEEDED_MARK: &str = " [broader-than-needed]";

/// A configured repo, as the registry sees it.
#[derive(Debug, Clone)]
pub struct RepoRef {
    pub id: i64,
    pub name: String,
    pub root: PathBuf,
}

/// How a repo's registration went (kept in memory; a refusal has no DB
/// row to live in).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Registration {
    Member {
        store_id: i64,
        store_key: String,
        source: String,
        joined_existing: bool,
        /// Remotes REFUSED as unsafe while another remote still decided
        /// the store's project — the ladder's `remote-url-refused`, on
        /// the same code the all-refused case refuses with. A good remote
        /// keys the store regardless (refusing here would strand a
        /// working clone over a second remote's typo), so these are the
        /// only trace that remote is there at all: reported, never
        /// dropped, and never reclassified into the store's key.
        refused_remotes: Vec<RefusedRemote>,
    },
    Refused {
        code: String,
        reason: String,
        candidates: Vec<String>,
    },
    Error {
        code: String,
        detail: String,
    },
}

/// Why no usable store is available for a repo right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum StoreUnavailable {
    Disabled {
        detail: String,
    },
    NotRegistered,
    Absent,
    Seeding,
    Broken {
        code: String,
    },
    LockedElsewhere,
    /// The credential the operator configured for this store came back
    /// REFUSED (D12): a gh account that is not the pinned/recorded one,
    /// a PINNED rung that failed, or a `token_file` that was present,
    /// readable and owner-only but did not validate. `class` is the
    /// `FailureClass` slug, `detail` the (redacted) reason.
    ///
    /// A store-level ERROR, never a recorded base-fetch skip: the store
    /// is not brought up — or re-reported — on a credential that never
    /// worked, and no other rung is tried in its place.
    CredentialRefused {
        class: String,
        detail: String,
    },
    /// The store is ready, but THIS member joined after the seed and its
    /// refs are not imported yet (background import pending). Reads fall
    /// back to the user repo, exactly like an absent store.
    MemberPending,
    /// The installed git is older than the store needs.
    GitTooOld {
        found: String,
    },
    Error {
        detail: String,
    },
}

impl StoreUnavailable {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled { .. } => "store-disabled",
            Self::NotRegistered => "store-not-registered",
            Self::Absent => "store-absent",
            Self::Seeding => "store-seeding",
            Self::Broken { .. } => "store-broken",
            Self::LockedElsewhere => "store-locked",
            Self::MemberPending => "store-member-pending",
            Self::GitTooOld { .. } => "git-too-old",
            Self::CredentialRefused { .. } => "store-credential-refused",
            Self::Error { .. } => "store-error",
        }
    }
}

/// A mutation refused because of the store's state. Renders as HTTP.
#[derive(Debug, Clone)]
pub struct StoreRefusal(pub StoreUnavailable);

impl axum::response::IntoResponse for StoreRefusal {
    fn into_response(self) -> axum::response::Response {
        use axum::http::{header, HeaderValue, StatusCode};
        let (status, retry) = match &self.0 {
            StoreUnavailable::Seeding => (
                StatusCode::SERVICE_UNAVAILABLE,
                Some(SEEDING_RETRY_AFTER_SECS),
            ),
            StoreUnavailable::LockedElsewhere => (StatusCode::CONFLICT, None),
            StoreUnavailable::NotRegistered => (StatusCode::CONFLICT, None),
            StoreUnavailable::Error { .. } => (StatusCode::INTERNAL_SERVER_ERROR, None),
            StoreUnavailable::CredentialRefused { .. } => (StatusCode::FORBIDDEN, None),
            _ => (StatusCode::CONFLICT, None),
        };
        let code = self.0.code();
        // A refused credential is the ONE arm whose class and reason the
        // operator must read in the message itself, not only in `detail`:
        // the class is what says WHICH half of the ladder refused, and a
        // store that is not up is not something to go debug in a JSON blob.
        let error = match &self.0 {
            StoreUnavailable::CredentialRefused { class, detail } => format!(
                "review store unavailable: {code} — the configured credential was refused \
                 ({class}): {detail}"
            ),
            _ => format!("review store unavailable: {code}"),
        };
        let mut body = serde_json::json!({
            "error": error,
            "type": format!("urn:kb:errors:{code}"),
            "title": status.canonical_reason().unwrap_or("Error"),
            "status": status.as_u16(),
            "detail": self.0,
        });
        if let Some(r) = retry {
            body["retry_after"] = r.into();
        }
        let mut resp = (status, axum::Json(body)).into_response();
        let h = resp.headers_mut();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if let Some(r) = retry {
            h.insert(header::RETRY_AFTER, HeaderValue::from(r));
        }
        resp
    }
}

/// A `ready`, manifest-verified store this process holds the lock on.
#[derive(Debug, Clone, Serialize)]
pub struct StoreHandle {
    pub id: i64,
    pub uuid: String,
    pub git_dir: PathBuf,
    pub store_key: String,
    pub base_url: Option<String>,
    pub forge_kind: Option<String>,
}

type Lock = Arc<tokio::sync::Mutex<()>>;

/// See the module doc.
pub struct ReviewStores {
    settings: StoreSettings,
    git: Option<StoreGit>,
    git_error: Option<String>,
    repos: Vec<RepoRef>,
    fetch_locks: parking_lot::Mutex<HashMap<(i64, String), Lock>>,
    ops_locks: parking_lot::Mutex<HashMap<i64, Lock>>,
    /// uuid → the lifetime flock (held = opened/seeded by THIS process).
    held: parking_lot::Mutex<HashMap<String, StoreLock>>,
    /// uuids another process holds.
    locked_elsewhere: parking_lot::Mutex<BTreeSet<String>>,
    /// store ids being seeded by this process right now.
    seeding: parking_lot::Mutex<BTreeSet<i64>>,
    registrations: parking_lot::Mutex<BTreeMap<String, Registration>>,
    /// `Some(found)` once a probe showed git < 2.41 (lazy, first use).
    git_version: std::sync::OnceLock<Option<String>>,
    /// uuid → (taken at, stats): `store show` never walks a store more
    /// than once per [`STATS_TTL`].
    stats_cache: parking_lot::Mutex<HashMap<String, (std::time::Instant, seed::StoreStats)>>,
}

/// How long a store's disk walk is reused by `store show`.
pub const STATS_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// Is `s` a kb-minted v4 uuid (lower-case hex, 8-4-4-4-12, version 4)?
/// Checked before a DB-held uuid becomes a path or a config value.
pub fn is_store_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            14 => *c == b'4',
            _ => matches!(c, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// Record that `m`'s member import landed (README §5.2 step 2): the
/// per-member marker. `repo_stores.legacy_import_json` NULL = not imported.
fn mark_imported(store: &Store, m: &seed::MemberImport) {
    let li = serde_json::json!({
        "at": now(),
        "imported": m.review_refs,
        "heads": m.heads,
        "pruned": m.pruned,
        "conflicts": m.conflicts,
    });
    let _ = store.set_repo_store_legacy_import(m.repo_id, Some(&li.to_string()));
}

impl std::fmt::Debug for ReviewStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewStores")
            .field("root", &self.settings.root)
            .field("disabled", &self.settings.disabled)
            .finish_non_exhaustive()
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A fresh v4 uuid (store directory name).
pub fn new_store_uuid() -> std::io::Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| std::io::Error::other(e.to_string()))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

/// The canonical base URL for a ladder answer: the HTTPS form of the
/// remote/operator URL when it passes the allowlist (README §8), else the
/// allowlisted URL itself (an ssh remote with no https twin), else the
/// HTTPS form derived from the key (a remote whose written URL carried
/// userinfo — never stored).
pub fn canonical_base_url(raw: &str, store_key: &str) -> Option<String> {
    match RemoteUrl::parse_remote(raw.trim()) {
        Ok(u) => Some(u.https_equivalent().unwrap_or(u).as_str().to_string()),
        Err(_) => https_url_for_key(store_key),
    }
}

/// The base BRANCH a legacy/new review tracks, from `base_branch` (V0045)
/// or its legacy `base_ref`. `None` for a pinned sha.
pub fn base_branch_of(
    base_ref: &str,
    base_branch: Option<&str>,
    remotes: &[String],
) -> Option<String> {
    if let Some(b) = base_branch.map(str::trim).filter(|b| !b.is_empty()) {
        return Some(b.to_string());
    }
    let r = base_ref.trim();
    if r.is_empty()
        || ((r.len() == 40 || r.len() == 64) && r.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return None;
    }
    if let Some(rest) = r.strip_prefix("refs/remotes/") {
        return rest.split_once('/').map(|(_, b)| b.to_string());
    }
    if let Some(b) = r.strip_prefix("refs/heads/") {
        return Some(b.to_string());
    }
    if r.starts_with("refs/") {
        return None;
    }
    if let Some((first, rest)) = r.split_once('/') {
        if remotes.iter().any(|n| n == first) {
            return Some(rest.to_string());
        }
    }
    Some(r.to_string())
}

/// One member of a store, as `[[review.repos]]` declares it (or not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberSettings {
    pub repo: String,
    /// Does this member have a `[[review.repos]]` entry at all? A member
    /// without one contributes its DEFAULTS (`auto`, no `gh_user`), which
    /// is why it cannot silently out-vote a member that declares a pin.
    pub declared: bool,
    pub credential: super::cred::CredentialPin,
    pub gh_user: Option<String>,
}

/// Which member's `[[review.repos]]` entry a store's credential comes
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreCredentialSource {
    /// `repo`'s entry drives the store.
    Member(String),
    /// More than one member declares a store-wide credential setting and
    /// they do not agree — the `String` names EVERY member's value. No
    /// credential is resolved on a guess: a store-wide pin read off one
    /// member is exactly how repo B's `gh_user` gets silently ignored
    /// (D12 binds the store, not the member).
    Disagreement(String),
}

/// The member that drives a store's credential resolution, or the
/// disagreement that stops one being chosen.
///
/// Members are visited in store-member (repo id) order, which is NOT an
/// operator-visible priority: with a `[[review.repos]]` entry on repo A
/// (id 1) and a `gh_user` pin on repo B, "first entry wins" binds the
/// store to whatever A resolves — unbound, and so free to fall through.
/// So when the DECLARED members disagree on `credential` or `gh_user`,
/// none is chosen and every member's value is named.
pub fn credential_source(members: &[MemberSettings]) -> StoreCredentialSource {
    let declared: Vec<&MemberSettings> = members.iter().filter(|m| m.declared).collect();
    let first = declared.first().copied().or_else(|| members.first());
    let Some(first) = first else {
        return StoreCredentialSource::Member(String::new());
    };
    let same = |m: &MemberSettings| {
        m.credential == first.credential
            && match (&m.gh_user, &first.gh_user) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                (None, None) => true,
                _ => false,
            }
    };
    if declared.len() < 2 || declared.iter().all(|m| same(m)) {
        return StoreCredentialSource::Member(first.repo.clone());
    }
    let values: Vec<String> = members
        .iter()
        .map(|m| {
            format!(
                "{} credential={} gh_user={}",
                m.repo,
                m.credential_slug(),
                m.gh_user.as_deref().unwrap_or("<unset>")
            )
        })
        .collect();
    StoreCredentialSource::Disagreement(format!(
        "this store's members declare different credential settings ({}) — no \
         credential is resolved until they agree (db order, not priority: \
         `{}` would otherwise have won silently)",
        values.join("; "),
        first.repo
    ))
}

impl MemberSettings {
    fn credential_slug(&self) -> &'static str {
        match self.credential {
            super::cred::CredentialPin::Auto => "auto",
            super::cred::CredentialPin::GhCli => "gh-cli",
            super::cred::CredentialPin::DeployKey => "deploy-key",
            super::cred::CredentialPin::Token => "token",
            super::cred::CredentialPin::Anonymous => "anonymous",
            super::cred::CredentialPin::Inherit => "inherit",
            super::cred::CredentialPin::None => "none",
        }
    }
}

impl ReviewStores {
    /// Build from config. Never fails boot: a spawner that cannot be
    /// built, or a disabled root, leaves every store `unavailable` and
    /// reads fall back to the user repos.
    /// The read half of that promise is [`Self::reads_can_use_store`],
    /// which `bind_and_spawn` publishes onto the `Store` at boot.
    pub fn new(
        review: &ReviewSection,
        state_dir: &Path,
        repos: &[RepoEntry],
        repo_ids: &HashMap<String, i64>,
    ) -> Self {
        let settings = StoreSettings::resolve(review, state_dir, repos);
        for w in &settings.warnings {
            tracing::warn!(warning = %w, "review store config");
        }
        let (git, git_error) = if settings.disabled.is_some() {
            (None, None)
        } else {
            match StoreGit::new(&settings.git_home) {
                Ok(g) => (Some(g), None),
                Err(e) => {
                    tracing::warn!(error = %e, "review store: cannot build the store git spawner");
                    (None, Some(e.to_string()))
                }
            }
        };
        let repos = repos
            .iter()
            .filter_map(|r| {
                repo_ids.get(&r.name).map(|id| RepoRef {
                    id: *id,
                    name: r.name.clone(),
                    root: r.path.clone(),
                })
            })
            .collect();
        Self::from_parts(settings, git, git_error, repos)
    }

    /// A registry with the store switched off (fixtures that build
    /// `AppState` by hand).
    pub fn disabled(reason: &str) -> Self {
        let mut settings =
            StoreSettings::resolve(&ReviewSection::default(), Path::new("/nonexistent"), &[]);
        settings.disabled = Some(reason.to_string());
        Self::from_parts(settings, None, None, Vec::new())
    }

    /// Tests: an explicit spawner + repo list.
    pub fn from_parts(
        settings: StoreSettings,
        git: Option<StoreGit>,
        git_error: Option<String>,
        repos: Vec<RepoRef>,
    ) -> Self {
        Self {
            settings,
            git,
            git_error,
            repos,
            fetch_locks: Default::default(),
            ops_locks: Default::default(),
            held: Default::default(),
            locked_elsewhere: Default::default(),
            seeding: Default::default(),
            registrations: Default::default(),
            git_version: std::sync::OnceLock::new(),
            stats_cache: Default::default(),
        }
    }

    pub fn settings(&self) -> &StoreSettings {
        &self.settings
    }

    /// May a READ resolve a store root for this boot? The read path
    /// (`crate::git::roots::GitCtx`) sees only `&Store`, never
    /// `StoreSettings`, so the flag is PUSHED boot → `Store` once, in the
    /// same RS-artifact-parked-on-`Store` shape as the `git_fallbacks`
    /// counters. Covers both arms `unavailable_reason` reports as
    /// `Disabled` (no spawner, or a disabled root) and deliberately
    /// EXCLUDES its `GitTooOld` arm, which gates store MUTATION (a
    /// `git -C <store>` write), not a plain read.
    pub fn reads_can_use_store(&self) -> bool {
        self.settings.disabled.is_none() && self.git.is_some()
    }

    pub fn git(&self) -> Option<&StoreGit> {
        self.git.as_ref()
    }

    pub fn repos(&self) -> &[RepoRef] {
        &self.repos
    }

    pub fn repo(&self, name: &str) -> Option<&RepoRef> {
        self.repos.iter().find(|r| r.name == name)
    }

    fn repo_by_id(&self, id: i64) -> Option<&RepoRef> {
        self.repos.iter().find(|r| r.id == id)
    }

    fn unavailable_reason(&self) -> Option<StoreUnavailable> {
        if let Some(d) = &self.settings.disabled {
            return Some(StoreUnavailable::Disabled { detail: d.clone() });
        }
        let Some(git) = self.git.as_ref() else {
            return Some(StoreUnavailable::Disabled {
                detail: self
                    .git_error
                    .clone()
                    .unwrap_or_else(|| "store git spawner unavailable".into()),
            });
        };
        let too_old = self
            .git_version
            .get_or_init(|| seed::git_too_old(git, seed::MIN_GIT))
            .clone();
        too_old.map(|found| StoreUnavailable::GitTooOld { found })
    }

    /// `store show`'s disk facts, cached for [`STATS_TTL`].
    pub fn cached_stats(&self, uuid: &str, dir: &Path) -> seed::StoreStats {
        if let Some((at, s)) = self.stats_cache.lock().get(uuid) {
            if at.elapsed() < STATS_TTL {
                return s.clone();
            }
        }
        let s = seed::store_stats(dir);
        self.stats_cache
            .lock()
            .insert(uuid.to_string(), (std::time::Instant::now(), s.clone()));
        s
    }

    /// Store ids this process is seeding right now (boot must not reset
    /// their `seeding` rows).
    pub fn seeding_ids(&self) -> Vec<i64> {
        self.seeding.lock().iter().copied().collect()
    }

    /// Import every member of ready store `store_id` whose import marker is
    /// unset — a repo that JOINED after the seed (or while it ran). Each
    /// import runs under that member's fetch lock; the ps-ref recreate
    /// step under the store's ops lock. Blocking (uses `blocking_lock`):
    /// call from `spawn_blocking`, never while holding either lock.
    ///
    /// Admits through the same helper `open` uses before touching a
    /// single ref or config: this process then holds the store's
    /// lifetime `flock` for the WHOLE import, not just for a check, so
    /// a second daemon cannot take the store mid-import and prune the
    /// `refs/remotes/work-<id>/*` refs this just wrote. A store this
    /// process has not admitted yet is manifest-checked on the way in, so
    /// admitting it here is exactly as safe as opening it.
    ///
    /// Refused outright with `store-locked` when another process holds the
    /// store: `configure_remote`/`update-ref` here would contend with
    /// whichever daemon is driving it. The pending marker is only cleared
    /// on success, so the next boot pass or route trigger retries.
    ///
    /// A member whose import FAILS comes back in
    /// [`PendingImport::errors`], never in `members`: it is not marked
    /// imported, so it stays PENDING and the next boot pass or route
    /// trigger retries it. The reason travels as `"<repo>: <slug>"` —
    /// the shape `plan_for`'s dropped-member problems use, so a seed
    /// report carries both kinds of unimported member on ONE list. Its
    /// siblings are unaffected: one bad member never aborts the rest.
    pub fn import_pending_members(
        &self,
        store: &Store,
        store_id: i64,
    ) -> Result<PendingImport, StoreUnavailable> {
        if let Some(u) = self.unavailable_reason() {
            return Err(u);
        }
        let git = self.git.as_ref().expect("checked");
        let db = |e: crate::store::StoreError| StoreUnavailable::Error {
            detail: e.to_string(),
        };
        let row = store
            .get_review_store(store_id)
            .map_err(db)?
            .ok_or(StoreUnavailable::NotRegistered)?;
        if row.state != "ready" {
            return Ok(PendingImport::default());
        }
        // The per-member fetch lock and the per-store ops lock are
        // in-process maps, so they cannot see a second daemon writing the
        // same store. Admit through `open`'s admission, before touching
        // a single ref or config, and keep the claim for the import.
        if !is_store_uuid(&row.uuid) {
            return Err(StoreUnavailable::Broken {
                code: "bad-uuid".into(),
            });
        }
        self.admit_locked(store, &row)?;
        let dir = PathBuf::from(&row.git_dir);
        let (members, _) = self.members_of(store, store_id);
        let ops = self.ops_lock(store_id);
        let mut out = PendingImport::default();
        for (r, m) in members {
            let pending = store
                .repo_store(r.id)
                .map_err(db)?
                .is_some_and(|rs| rs.legacy_import_json.is_none());
            if !pending {
                continue;
            }
            let fl = self.fetch_lock(store_id, &RemoteName::work(r.id));
            let _g = fl.blocking_lock();
            let imp = match seed::import_member(git, &dir, &m, seed::SEED_FETCH_TIMEOUT) {
                Ok(i) => i,
                Err(e) => {
                    // This warn IS the reporting channel for the
                    // background route trigger, which owns no report to
                    // carry the reason; the callers that do own one (the
                    // seed report, the boot summary) get it in `errors`.
                    tracing::warn!(repo = %r.name, class = %e.class, "review store: member import failed");
                    out.errors.push(format!("{}: {}", r.name, e.slug()));
                    continue;
                }
            };
            let patchsets: Vec<ExpectedPatchset> = store
                .patchsets_for_repos(std::slice::from_ref(&r.name))
                .map_err(db)?
                .into_iter()
                .map(
                    |(review_id, ps_number, tip_sha, base_sha, base_tip_sha)| ExpectedPatchset {
                        review_id,
                        ps_number,
                        tip_sha,
                        base_sha,
                        base_tip_sha,
                    },
                )
                .collect();
            match seed::verify_connectivity(
                git,
                &dir,
                std::slice::from_ref(&m),
                &patchsets,
                Some(&ops),
            ) {
                Ok((missing, ok, _, _)) => self.apply_objects_state(store, &missing, &ok),
                Err(e) => {
                    tracing::warn!(repo = %r.name, class = %e.class, "review store: member connectivity check failed")
                }
            }
            mark_imported(store, &imp);
            out.members.push(imp);
        }
        Ok(out)
    }

    /// The in-memory registration outcome for `name`, if registration ran.
    pub fn registration(&self, name: &str) -> Option<Registration> {
        self.registrations.lock().get(name).cloned()
    }

    /// Is store `id` being seeded by this process right now?
    pub fn is_seeding(&self, id: i64) -> bool {
        self.seeding.lock().contains(&id)
    }

    pub fn is_locked_elsewhere(&self, uuid: &str) -> bool {
        self.locked_elsewhere.lock().contains(uuid)
    }

    // --- locks ----------------------------------------------------------

    /// The per-(store, remote) fetch mutex (design §4.2): fetches of one
    /// kind into one store serialize; different remotes run in parallel.
    pub fn fetch_lock(&self, store_id: i64, remote: &RemoteName) -> Arc<tokio::sync::Mutex<()>> {
        self.fetch_locks
            .lock()
            .entry((store_id, remote.as_str().to_string()))
            .or_default()
            .clone()
    }

    /// The short per-store ops mutex: capture (merge-base, one
    /// `update-ref --stdin`, the DB insert), pack-refs, reflog expire.
    /// Never held across a network fetch.
    pub fn ops_lock(&self, store_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        self.ops_locks.lock().entry(store_id).or_default().clone()
    }

    /// Take the lifetime flock on `uuid`. PRECONDITION: the caller holds
    /// the `held` guard for the whole check-acquire-insert, so two
    /// same-process writers can never race each other into a
    /// self-inflicted `store-locked`. Every call site is
    /// [`Self::admit_locked`] or `seed`'s own guarded block.
    fn acquire_lock(&self, uuid: &str) -> Result<StoreLock, StoreUnavailable> {
        std::fs::create_dir_all(&self.settings.root).map_err(|e| StoreUnavailable::Error {
            detail: format!("store root: {}", e.kind()),
        })?;
        match StoreLock::try_acquire(&self.settings.root, uuid) {
            Ok(Some(l)) => {
                self.locked_elsewhere.lock().remove(uuid);
                Ok(l)
            }
            Ok(None) => {
                self.locked_elsewhere.lock().insert(uuid.to_string());
                Err(StoreUnavailable::LockedElsewhere)
            }
            Err(e) => Err(StoreUnavailable::Error {
                detail: format!("store lock: {}", e.kind()),
            }),
        }
    }

    /// Admit `row` as a store this process owns: our lifetime `flock` on
    /// its uuid, plus a manifest check of its directory. Idempotent — a
    /// uuid already in `held` was admitted (and verified) by whoever put
    /// it there, so it is admitted as-is.
    ///
    /// The check, the `try_acquire` and the insert all happen under the
    /// `held` guard, as [`Self::acquire_lock`] requires: two same-process
    /// writers serialize here instead of one of them contending with the
    /// other's own claim, which is what would otherwise turn this
    /// process's transient lock into a `store-locked` refusal and a
    /// sticky `locked_elsewhere` entry for a store we own.
    ///
    /// The claim is KEPT in `held` for the rest of this process's life,
    /// which is the point: a caller about to write a store must hold the
    /// one-writer claim for the whole write, not just for the check. A
    /// manifest check runs before the insert, so a store admitted here is
    /// exactly as verified as one `open` admits — nothing reaches
    /// `held` unverified, and `open` still skips its own re-check only
    /// for stores this process has already verified.
    ///
    /// Refusals: `LockedElsewhere` when another PROCESS holds the store
    /// (`acquire_lock` records it in `locked_elsewhere`, so a refusal is
    /// reported and then retried on the next attempt), and
    /// `Absent`/`Broken` with the row's state corrected when the
    /// directory is gone or does not match the row.
    fn admit_locked(&self, store: &Store, row: &ReviewStoreRow) -> Result<(), StoreUnavailable> {
        let mut held = self.held.lock();
        if held.contains_key(&row.uuid) {
            return Ok(());
        }
        let lock = self.acquire_lock(&row.uuid)?;
        let dir = PathBuf::from(&row.git_dir);
        if let Err(p) = manifest::check(&dir, &row.uuid, &row.store_key) {
            drop(lock);
            let code = p.code();
            let _ = store.set_review_store_state(
                row.id,
                if matches!(p, ManifestProblem::DirMissing) {
                    "absent"
                } else {
                    "broken"
                },
                Some(&serde_json::json!({ "code": code, "detail": p.to_string() }).to_string()),
            );
            return Err(if matches!(p, ManifestProblem::DirMissing) {
                StoreUnavailable::Absent
            } else {
                StoreUnavailable::Broken { code: code.into() }
            });
        }
        held.insert(row.uuid.clone(), lock);
        Ok(())
    }

    // --- handles ----------------------------------------------------------

    /// A usable store for `repo_name`. See the module doc.
    pub fn handle_for_repo(
        &self,
        store: &Store,
        repo_name: &str,
    ) -> Result<StoreHandle, StoreUnavailable> {
        if let Some(u) = self.unavailable_reason() {
            return Err(u);
        }
        let row = store
            .store_for_repo_name(repo_name)
            .map_err(|e| StoreUnavailable::Error {
                detail: e.to_string(),
            })?
            .ok_or(StoreUnavailable::NotRegistered)?;
        let handle = self.open(store, &row)?;
        if let Some(r) = self.repo(repo_name) {
            let pending = store
                .repo_store(r.id)
                .map_err(|e| StoreUnavailable::Error {
                    detail: e.to_string(),
                })?
                .is_some_and(|m| m.legacy_import_json.is_none());
            if pending {
                return Err(StoreUnavailable::MemberPending);
            }
        }
        Ok(handle)
    }

    /// Mutation admission: `Ok(Some(handle))` for a ready store, `Ok(None)`
    /// when the repo has no usable store yet (absent / not registered /
    /// disabled — the caller keeps today's user-repo behaviour), and a
    /// [`StoreRefusal`] while the store is `seeding` (503) or held by
    /// another daemon.
    pub fn admit_mutation(
        &self,
        store: &Store,
        repo_name: &str,
    ) -> Result<Option<StoreHandle>, StoreRefusal> {
        match self.handle_for_repo(store, repo_name) {
            Ok(h) => Ok(Some(h)),
            Err(u @ (StoreUnavailable::Seeding | StoreUnavailable::LockedElsewhere)) => {
                Err(StoreRefusal(u))
            }
            Err(StoreUnavailable::Error { detail }) => {
                Err(StoreRefusal(StoreUnavailable::Error { detail }))
            }
            Err(_) => Ok(None),
        }
    }

    /// Open a store row: state must be `ready`, the manifest must match,
    /// and this process must hold (or take) its lock.
    pub fn open(
        &self,
        store: &Store,
        row: &ReviewStoreRow,
    ) -> Result<StoreHandle, StoreUnavailable> {
        if self.is_seeding(row.id) || row.state == "seeding" {
            return Err(StoreUnavailable::Seeding);
        }
        match row.state.as_str() {
            "ready" => {}
            "broken" => {
                return Err(StoreUnavailable::Broken {
                    code: state_code(row.state_json.as_deref()).unwrap_or_else(|| "broken".into()),
                })
            }
            _ => return Err(StoreUnavailable::Absent),
        }
        if !is_store_uuid(&row.uuid) {
            return Err(StoreUnavailable::Broken {
                code: "bad-uuid".into(),
            });
        }
        let dir = PathBuf::from(&row.git_dir);
        self.admit_locked(store, row)?;
        Ok(StoreHandle {
            id: row.id,
            uuid: row.uuid.clone(),
            git_dir: dir,
            store_key: row.store_key.clone(),
            base_url: row.base_url.clone(),
            forge_kind: row.forge_kind.clone(),
        })
    }

    // --- registration (README §5.1) ------------------------------------

    /// Register `name` (a configured repo) with its store: join an existing
    /// store, or mint one, per the ladder. Idempotent: a repo that is
    /// already a member returns its membership unless `explicit` is given,
    /// in which case the URL must name the SAME project (never a re-key;
    /// `base-url-key-mismatch` otherwise) and updates the store's base URL.
    pub fn register_repo(&self, store: &Store, name: &str, explicit: Option<&str>) -> Registration {
        let r = self.register_inner(store, name, explicit);
        self.registrations
            .lock()
            .insert(name.to_string(), r.clone());
        r
    }

    fn register_inner(&self, store: &Store, name: &str, explicit: Option<&str>) -> Registration {
        let err = |code: &str, detail: String| Registration::Error {
            code: code.into(),
            detail,
        };
        if let Some(u) = self.unavailable_reason() {
            return err(u.code(), format!("{u:?}"));
        }
        let Some(repo) = self.repo(name).cloned() else {
            return err(
                "repo-not-found",
                format!("`{name}` is not a configured repo"),
            );
        };
        let existing = match store.repo_store(repo.id) {
            Ok(v) => v,
            Err(e) => return err("db", e.to_string()),
        };
        if let Some(m) = existing {
            let row = match store.get_review_store(m.store_id) {
                Ok(Some(r)) => r,
                Ok(None) => return err("db", "membership points at a missing store".into()),
                Err(e) => return err("db", e.to_string()),
            };
            if let Some(url) = explicit {
                let Some(key) = super::key::store_key_for_url(url) else {
                    return Registration::Refused {
                        code: ladder::BASE_URL_INVALID.into(),
                        reason: "the explicit base url is not a forge project URL".into(),
                        candidates: vec![],
                    };
                };
                if key != row.store_key {
                    return Registration::Refused {
                        code: "base-url-key-mismatch".into(),
                        reason: format!(
                            "`{name}` belongs to store `{}`; a store is never re-keyed",
                            row.store_key
                        ),
                        candidates: vec![row.store_key.clone()],
                    };
                }
                let base = canonical_base_url(url, &key);
                if let Err(e) =
                    store.set_review_store_base_url(row.id, base.as_deref(), Some("explicit"))
                {
                    return err("db", e.to_string());
                }
                return Registration::Member {
                    store_id: row.id,
                    store_key: row.store_key,
                    source: "explicit".into(),
                    joined_existing: true,
                    refused_remotes: vec![],
                };
            }
            return Registration::Member {
                store_id: row.id,
                store_key: row.store_key,
                source: row.base_url_source.unwrap_or_else(|| "member".into()),
                joined_existing: true,
                refused_remotes: vec![],
            };
        }

        let git = self.git.as_ref().expect("checked by unavailable_reason");
        let common = match seed::common_dir_of(&repo.root) {
            Ok(c) => c,
            Err(e) => return err("not-a-repo", format!("{}: {}", repo.name, e.kind())),
        };
        let _ = git.allow_local_source(&common);
        let remotes = match seed::read_remotes(git, &common) {
            Ok(r) => r,
            Err(e) => return err(e.slug(), e.detail),
        };
        let slugs = store.review_pr_slugs(name).unwrap_or_default();
        let keys: Vec<String> = match store.list_review_stores() {
            Ok(rows) => rows.into_iter().map(|r| r.store_key).collect(),
            Err(e) => return err("db", e.to_string()),
        };
        let cfg = self.settings.repo(name);
        let outcome = ladder::resolve(&LadderInput {
            explicit,
            config_base_url: cfg.base_url.as_deref(),
            pr_slugs: &slugs,
            remotes: &remotes,
            existing_keys: &keys,
            // Rung 6 is inert, so a fork-shaped clone refuses with
            // `base-url-ambiguous` and the operator sets `base_url`. See
            // `NoForkCheck`'s doc for the two things that have to exist
            // first (a `GET /repos/{o}/{r}` fork read on `GithubClient`,
            // and a way to reach the api credential from this
            // sync, `&Store`-only, pre-store-row path).
            fork_check: &NoForkCheck,
        });
        let (key, base_url, source, refused) = match outcome {
            LadderOutcome::Resolved {
                store_key,
                url,
                source,
                refused,
                ..
            } => {
                let base = canonical_base_url(&url, &store_key);
                // A remote refused as unsafe while a GOOD remote still
                // decided the project: the store is keyed from the good
                // one, and the refusal is reported rather than lost with
                // it (it is the same `remote-url-refused` the all-refused
                // clone refuses with).
                for r in &refused {
                    tracing::warn!(
                        repo = %name,
                        remote = %r.name,
                        code = ladder::REMOTE_URL_REFUSED,
                        reason = %r.reason,
                        "kb-code: a remote was refused as unsafe; the store is keyed from \
                         another remote"
                    );
                }
                (store_key, base, source.slug(), refused)
            }
            LadderOutcome::NoForgeRemote => {
                let uuid = match new_store_uuid() {
                    Ok(u) => u,
                    Err(e) => return err("failed", e.to_string()),
                };
                return self.create_and_join(
                    store,
                    &repo,
                    &uuid,
                    &local_store_key(&uuid),
                    None,
                    "local",
                    // Unreachable in fact: a single refused remote stops
                    // the ladder above, so nothing was refused here.
                    vec![],
                );
            }
            LadderOutcome::Refused {
                code,
                reason,
                candidates,
            } => {
                return Registration::Refused {
                    code: code.into(),
                    reason,
                    candidates,
                }
            }
        };
        match store.get_review_store_by_key(&key) {
            Ok(Some(row)) => {
                if let Err(e) = store.add_repo_to_store(repo.id, row.id) {
                    return err("db", e.to_string());
                }
                Registration::Member {
                    store_id: row.id,
                    store_key: row.store_key,
                    source: source.into(),
                    joined_existing: true,
                    refused_remotes: refused,
                }
            }
            Ok(None) => {
                let uuid = match new_store_uuid() {
                    Ok(u) => u,
                    Err(e) => return err("failed", e.to_string()),
                };
                self.create_and_join(
                    store,
                    &repo,
                    &uuid,
                    &key,
                    base_url.as_deref(),
                    source,
                    refused,
                )
            }
            Err(e) => err("db", e.to_string()),
        }
    }

    // Eight parameters, and the eighth is the refused-remote list: it is
    // not part of the store's identity, so bundling it would mean a
    // struct that exists only to satisfy a lint.
    #[allow(clippy::too_many_arguments)]
    fn create_and_join(
        &self,
        store: &Store,
        repo: &RepoRef,
        uuid: &str,
        key: &str,
        base_url: Option<&str>,
        source: &str,
        // Remotes the ladder refused as unsafe while `key` was still
        // decided by another remote — reported on the registration,
        // never folded into the key.
        refused: Vec<RefusedRemote>,
    ) -> Registration {
        let git_dir = seed::store_dir(&self.settings.root, uuid);
        let id = match store.create_review_store(
            uuid,
            key,
            &git_dir.to_string_lossy(),
            base_url,
            Some(source),
            now(),
        ) {
            Ok(id) => id,
            // UNIQUE(store_key): another registration minted it first — join.
            Err(_) => match store.get_review_store_by_key(key) {
                Ok(Some(row)) => row.id,
                Ok(None) => {
                    return Registration::Error {
                        code: "db".into(),
                        detail: "could not create the store row".into(),
                    }
                }
                Err(e) => {
                    return Registration::Error {
                        code: "db".into(),
                        detail: e.to_string(),
                    }
                }
            },
        };
        let cfg = self.settings.repo(&repo.name);
        let (host, slug) = split_key(key).map_or((None, None), |(h, s)| (Some(h), Some(s)));
        let forge = cfg.forge.detect(host);
        let _ = store.set_review_store_forge(
            id,
            forge.map(|f| f.slug()),
            host,
            slug,
            if forge.is_some_and(|f| f.verified()) {
                "verified"
            } else {
                "unverified"
            },
        );
        if let Err(e) = store.add_repo_to_store(repo.id, id) {
            return Registration::Error {
                code: "db".into(),
                detail: e.to_string(),
            };
        }
        Registration::Member {
            store_id: id,
            store_key: key.to_string(),
            source: source.into(),
            joined_existing: false,
            refused_remotes: refused,
        }
    }

    // --- seeding ----------------------------------------------------------

    /// The member clones of a store (skipping any whose path no longer
    /// resolves — reported, never fatal).
    pub fn members_of(
        &self,
        store: &Store,
        store_id: i64,
    ) -> (Vec<(RepoRef, SeedMember)>, Vec<String>) {
        let mut out = Vec::new();
        let mut problems = Vec::new();
        for id in store.store_members(store_id).unwrap_or_default() {
            let Some(r) = self.repo_by_id(id) else {
                problems.push(format!("member repo id {id} is not configured"));
                continue;
            };
            match seed::common_dir_of(&r.root) {
                Ok(common_dir) => out.push((
                    r.clone(),
                    SeedMember {
                        repo_id: id,
                        common_dir,
                    },
                )),
                Err(e) => problems.push(format!("{}: {}", r.name, e.kind())),
            }
        }
        (out, problems)
    }

    /// Build the seed plan for store `row`.
    pub fn plan_for(
        &self,
        store: &Store,
        row: &ReviewStoreRow,
    ) -> Result<(SeedPlan, Vec<String>), String> {
        let git = self.git.as_ref().ok_or("store git spawner unavailable")?;
        let (members, problems) = self.members_of(store, row.id);
        let names: Vec<String> = members.iter().map(|(r, _)| r.name.clone()).collect();
        let mut remote_names: BTreeSet<String> = BTreeSet::new();
        for (_, m) in &members {
            if let Ok(rs) = seed::read_remotes(git, &m.common_dir) {
                remote_names.extend(rs.into_iter().map(|r: RemoteInfo| r.name));
            }
        }
        let remote_names: Vec<String> = remote_names.into_iter().collect();
        let reviews = store.reviews_for_repos(&names).map_err(|e| e.to_string())?;
        let mut branches = BTreeSet::new();
        for (_, _, state, base_ref, base_branch) in &reviews {
            if state == "open" {
                if let Some(b) = base_branch_of(base_ref, base_branch.as_deref(), &remote_names) {
                    branches.insert(b);
                }
            }
        }
        let patchsets = store
            .patchsets_for_repos(&names)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(
                |(review_id, ps_number, tip_sha, base_sha, base_tip_sha)| ExpectedPatchset {
                    review_id,
                    ps_number,
                    tip_sha,
                    base_sha,
                    base_tip_sha,
                },
            )
            .collect();
        let base_url = row
            .base_url
            .as_deref()
            .and_then(|u| RemoteUrl::parse_remote(u).ok());
        Ok((
            SeedPlan {
                root: self.settings.root.clone(),
                uuid: row.uuid.clone(),
                store_key: row.store_key.clone(),
                base_url,
                members: members.into_iter().map(|(_, m)| m).collect(),
                base_branches: branches.into_iter().collect(),
                patchsets,
                fail_after_members: None,
            },
            problems,
        ))
    }

    /// Resolve (and persist, D12) the fetch credential for store `row`,
    /// using `repo_name`'s `[[review.repos]]` settings. Blocking (runs gh).
    pub fn resolve_credential(
        &self,
        store: &Store,
        row: &ReviewStoreRow,
        repo_name: &str,
    ) -> Result<Resolution, CredError> {
        let git = self
            .git
            .as_ref()
            .ok_or_else(|| CredError::Refused("store git spawner unavailable".into()))?;
        let url = row
            .base_url
            .as_deref()
            .and_then(|u| RemoteUrl::parse_remote(u).ok())
            .ok_or_else(|| CredError::Refused("this store has no forge base url".into()))?;
        let cfg = self
            .settings
            .fetch_credential_config(repo_name, row.cred_account.as_deref());
        let gh = GhCli::from_process_env();
        let res = resolve_fetch_credential(&cfg, &url, &LiveProbes { gh: &gh, git })?;
        let mut reason = res.reason.clone();
        if let FetchCredential::GhCli(g) = &res.credential {
            if g.broader_than_needed() {
                reason.push_str(BROADER_THAN_NEEDED_MARK);
            }
        }
        let _ = store.set_review_store_credential(
            row.id,
            res.credential.kind().slug(),
            Some(&reason),
            res.credential.account().or(row.cred_account.as_deref()),
        );
        Ok(res)
    }

    /// The member whose `[[review.repos]]` entry drives store-wide
    /// credential resolution — or the DISAGREEMENT that stops one being
    /// chosen (see [`credential_source_for`]).
    pub fn credential_source_for(&self, store: &Store, store_id: i64) -> StoreCredentialSource {
        let members: Vec<MemberSettings> = store
            .store_members(store_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|id| self.repo_by_id(id))
            .map(|r| {
                let s = self.settings.repo(&r.name);
                MemberSettings {
                    repo: r.name.clone(),
                    declared: self.settings.repos.contains_key(&r.name),
                    credential: s.credential,
                    gh_user: s.gh_user,
                }
            })
            .collect();
        credential_source(&members)
    }

    /// The store's credential settings, refusing to guess when members
    /// disagree. Returns the member name to resolve with, or a
    /// [`StoreCredentialSource::Disagreement`] naming every member's value
    /// — reported (tracing + the store card), never resolved on.
    pub fn resolve_settings_member(
        &self,
        store: &Store,
        row: &ReviewStoreRow,
    ) -> Result<String, String> {
        match self.credential_source_for(store, row.id) {
            StoreCredentialSource::Member(name) => Ok(name),
            StoreCredentialSource::Disagreement(detail) => {
                tracing::warn!(
                    store = %row.store_key,
                    warning = %detail,
                    "review store config"
                );
                Err(detail)
            }
        }
    }

    /// Seed store `store_id` (README §5.2). `network = false` is the boot
    /// job's local-only seed (D4); `true` also fetches base branches with
    /// the resolved credential (offline-degradable). Blocking.
    pub fn seed(
        &self,
        store: &Store,
        store_id: i64,
        network: bool,
    ) -> Result<SeedReport, StoreUnavailable> {
        if let Some(u) = self.unavailable_reason() {
            return Err(u);
        }
        let git = self.git.as_ref().expect("checked");
        if !self.seeding.lock().insert(store_id) {
            return Err(StoreUnavailable::Seeding);
        }
        let _claim = SeedClaim {
            set: &self.seeding,
            id: store_id,
        };
        let db = |e: crate::store::StoreError| StoreUnavailable::Error {
            detail: e.to_string(),
        };
        let row = store
            .get_review_store(store_id)
            .map_err(db)?
            .ok_or(StoreUnavailable::NotRegistered)?;
        if !is_store_uuid(&row.uuid) {
            return Err(StoreUnavailable::Broken {
                code: "bad-uuid".into(),
            });
        }
        {
            let mut held = self.held.lock();
            if !held.contains_key(&row.uuid) {
                let lock = self.acquire_lock(&row.uuid)?;
                held.insert(row.uuid.clone(), lock);
            }
        }
        let dir = PathBuf::from(&row.git_dir);
        if dir.exists() {
            // Already on disk (a DB left `absent`, e.g. after a restore):
            // adopt only if the manifest matches.
            return match manifest::check(&dir, &row.uuid, &row.store_key) {
                Ok(_) => {
                    // Re-verify connectivity for the adopted store. The
                    // plan's SECOND return — the members it dropped — is
                    // this call's only evidence that the adopted directory
                    // is not a complete mirror, so it rides out on the
                    // report (it used to be dropped here, leaving a 200 that
                    // said `ready` over a store that had silently never
                    // imported a member).
                    let (plan, problems) = self
                        .plan_for(store, &row)
                        .map_err(|d| StoreUnavailable::Error { detail: d })?;
                    let ops = self.ops_lock(row.id);
                    let (missing, ok, rec, rr) = seed::verify_connectivity(
                        git,
                        &dir,
                        &plan.members,
                        &plan.patchsets,
                        Some(&ops),
                    )
                    .map_err(|e| StoreUnavailable::Error {
                        detail: e.to_string(),
                    })?;
                    self.apply_objects_state(store, &missing, &ok);
                    store
                        .set_review_store_state(
                            row.id,
                            "ready",
                            Some(
                                &serde_json::json!({
                                    "code": "adopted-existing",
                                    "at": now(),
                                    "objects_missing": missing,
                                })
                                .to_string(),
                            ),
                        )
                        .map_err(db)?;
                    // An adopted directory may predate some members; the
                    // imports it performs are this call's member work, so
                    // they are reported rather than thrown away. A refusal
                    // here (another daemon holds the store) is non-fatal,
                    // exactly as the post-seed repack further down this
                    // function is — but never silent: an unlogged `Err`
                    // would make this report's empty member list read as
                    // "nothing to import" rather than "the import did not
                    // run". A member whose import FAILS is in the same
                    // condition as one the plan dropped — neither imported
                    // nor connectivity-checked, and this store still goes
                    // `ready` — so its reason joins the plan's problems on
                    // the report's one `member_problems` list.
                    let imported = match self.import_pending_members(store, row.id) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                code = e.code(),
                                uuid = %row.uuid,
                                "kb-code: adopted-store member import failed (non-fatal)"
                            );
                            PendingImport::default()
                        }
                    };
                    let mut member_problems = problems;
                    member_problems.extend(imported.errors);
                    Ok(SeedReport {
                        git_dir: dir.clone(),
                        members: imported.members,
                        member_problems,
                        base: BaseFetch::Skipped {
                            code: "adopted-existing".into(),
                        },
                        objects_missing: missing,
                        objects_ok: ok,
                        recovered_by_sha: rec,
                        refs_recreated: rr,
                        elapsed_ms: 0,
                    })
                }
                Err(p) => {
                    let _ = store.set_review_store_state(
                        row.id,
                        "broken",
                        Some(
                            &serde_json::json!({"code": p.code(), "detail": p.to_string()})
                                .to_string(),
                        ),
                    );
                    Err(StoreUnavailable::Broken {
                        code: p.code().into(),
                    })
                }
            };
        }
        store
            .set_review_store_state(
                row.id,
                "seeding",
                Some(&serde_json::json!({"code": "seeding", "started_at": now()}).to_string()),
            )
            .map_err(db)?;
        let (plan, problems) = match self.plan_for(store, &row) {
            Ok(p) => p,
            Err(d) => {
                let _ = store.set_review_store_state(
                    row.id,
                    "absent",
                    Some(
                        &serde_json::json!({"code": "seed-failed", "stage": "plan", "detail": d})
                            .to_string(),
                    ),
                );
                return Err(StoreUnavailable::Error { detail: d });
            }
        };
        // D12 — a credential the operator SUPPLIED came back refused. The
        // ladder already stops on these (`cred.rs`: an account that is not
        // the pinned/recorded one, any gh failure on a bound store, a
        // `token_file` that was present and readable but did not validate,
        // a pinned rung that failed), so this call site must not put the
        // fatality back: seeding the store with NO credential and filing
        // the class as a `BaseFetch::Skipped` note is what left a store
        // `ready` over a base that was never fetched, as a DIFFERENT
        // identity than the operator asked for. `FailureClass::is_auth`
        // is the one grouping both sides already read, so the split is
        // never spelled twice.
        let mut cred_note = None;
        let cred = if network && plan.base_url.is_some() {
            match self.resolve_settings_member(store, &row) {
                Ok(who) => match self.resolve_credential(store, &row, &who) {
                    Ok(r) => Some(r.credential),
                    Err(e) if e.class().is_auth() => {
                        // Never `ready`: the row goes back to `absent`
                        // carrying the class, so the next boot re-seeds it
                        // once the operator fixes the credential.
                        let u = credential_refused(&row, &e);
                        let _ = store.set_review_store_state(
                            row.id,
                            "absent",
                            Some(&refused_state(&e)),
                        );
                        return Err(u);
                    }
                    // A network-shaped failure (offline, timeout) is not a
                    // credential fault: recorded as a skip, exactly as
                    // before, and the store still comes up on cached refs.
                    Err(e) => {
                        cred_note = Some(e.class().slug());
                        None
                    }
                },
                // Members disagree: no credential is resolved on a guess.
                // No credential was supplied and none was refused, so this
                // is not the arm above — the pass continues and the store
                // is reported with the reason.
                Err(_) => {
                    cred_note = Some(FailureClass::NoCredentials.slug());
                    None
                }
            }
        } else {
            None
        };
        let result = seed::seed_store(git, &plan, cred.as_ref(), now());
        match result {
            Ok(mut report) => {
                if let (Some(code), BaseFetch::Skipped { .. }) = (cred_note, &report.base) {
                    if network {
                        report.base = BaseFetch::Skipped { code: code.into() };
                    }
                }
                self.apply_objects_state(store, &report.objects_missing, &report.objects_ok);
                for m in &report.members {
                    mark_imported(store, m);
                }
                // `problems` is off the same `members_of` call as the plan's
                // members (`plan_for`), i.e. the members this seed did NOT
                // import. Put it on the report — the same value goes into
                // `state_json` just below, so the response and the durable
                // record are one list, never two — and carry it here by
                // clone because `sj` below consumes the original.
                report.member_problems = problems.clone();
                let sj = serde_json::json!({
                    "code": "ready",
                    "seeded_at": now(),
                    "base": report.base,
                    "last_base_fetch": matches!(report.base, BaseFetch::Fetched { .. }).then(now),
                    "last_work_fetch": now(),
                    "objects_missing": report.objects_missing,
                    "member_problems": problems,
                    "elapsed_ms": report.elapsed_ms as u64,
                });
                store
                    .set_review_store_state(row.id, "ready", Some(&sj.to_string()))
                    .map_err(db)?;
                // Repos that joined WHILE this seed ran were not in its
                // plan snapshot: import them now. One that fails to
                // import is reported like a plan-dropped member — its
                // refs are just as unimported and this pass still says
                // `ready` — and stays PENDING for the next boot pass or
                // route trigger. `state_json` above was written from the
                // plan's problems alone, so this late reason is
                // response-only.
                match self.import_pending_members(store, row.id) {
                    Ok(late) => report.member_problems.extend(late.errors),
                    Err(e) => {
                        tracing::warn!(
                            code = e.code(),
                            uuid = %row.uuid,
                            "kb-code: post-seed member import failed (non-fatal)"
                        );
                    }
                }
                // RS-U9 (RS-U3's own follow-up note): a multi-member seed
                // leaves one pack per imported member — repack to one now,
                // rather than waiting for the next scheduled weekly/monthly
                // pass. Best-effort: a repack failure never fails the seed
                // that just succeeded.
                if plan.members.len() > 1 {
                    if let Err(e) = super::maint::repack_full(git, &report.git_dir) {
                        tracing::warn!(
                            error = %e,
                            uuid = %row.uuid,
                            "kb-code: post-seed repack-to-one failed (non-fatal)"
                        );
                    }
                }
                Ok(report)
            }
            Err(e) => {
                let _ = store.set_review_store_state(
                    row.id,
                    "absent",
                    Some(&serde_json::json!({"code": "seed-failed", "stage": e.stage, "class": e.class, "detail": e.detail}).to_string()),
                );
                Err(StoreUnavailable::Error {
                    detail: e.to_string(),
                })
            }
        }
    }

    fn apply_objects_state(&self, store: &Store, missing: &[i64], ok: &[i64]) {
        for id in missing {
            let _ = store.set_review_objects_state(*id, Some(seed::OBJECTS_MISSING));
        }
        for id in ok {
            if let Ok(Some(b)) = store.get_review_base(*id) {
                if b.objects_state.as_deref() == Some(seed::OBJECTS_MISSING) {
                    let _ = store.set_review_objects_state(*id, None);
                }
            }
        }
    }

    /// `store sync` on a ready store: every member's heads + review refs
    /// (local), then every open review's base branch from `base` (network,
    /// offline-degradable). Blocking; the caller holds the fetch locks.
    pub fn sync_ready(
        &self,
        store: &Store,
        handle: &StoreHandle,
        network: bool,
    ) -> Result<SyncReport, StoreUnavailable> {
        let git = self.git.as_ref().ok_or(StoreUnavailable::Disabled {
            detail: "store git spawner unavailable".into(),
        })?;
        let row = store
            .get_review_store(handle.id)
            .map_err(|e| StoreUnavailable::Error {
                detail: e.to_string(),
            })?
            .ok_or(StoreUnavailable::NotRegistered)?;
        let (plan, problems) = self
            .plan_for(store, &row)
            .map_err(|d| StoreUnavailable::Error { detail: d })?;
        let mut members = Vec::new();
        let mut member_errors = Vec::new();
        for m in &plan.members {
            match seed::import_member(git, &handle.git_dir, m, super::git::WORK_FETCH_TIMEOUT) {
                Ok(i) => {
                    mark_imported(store, &i);
                    members.push(i)
                }
                Err(e) => member_errors.push(format!("work-{}: {}", m.repo_id, e.slug())),
            }
        }
        let base = if plan.base_url.is_none() {
            BaseFetch::Skipped {
                code: "no-base-remote".into(),
            }
        } else if !network {
            BaseFetch::Skipped {
                code: "offline".into(),
            }
        } else {
            match self.resolve_settings_member(store, &row) {
                Ok(who) => match self.resolve_credential(store, &row, &who) {
                    Ok(r) => seed::fetch_base_branches(
                        git,
                        &handle.git_dir,
                        &plan.base_branches,
                        &r.credential,
                    ),
                    // D12, the same rule the seed path applies: a
                    // credential the operator supplied and that was
                    // REFUSED stops the pass. Returning here — before
                    // the `ready` write further down — is the point: the
                    // store keeps the base status it last really had
                    // instead of having a refused credential recorded
                    // over it as its current one, and the caller gets
                    // the refusal rather than a 200 carrying a warning.
                    // A network-shaped failure still degrades to a skip.
                    Err(e) if e.class().is_auth() => {
                        return Err(credential_refused(&row, &e));
                    }
                    Err(e) => BaseFetch::Skipped {
                        code: e.class().slug().into(),
                    },
                },
                // Members disagree: no credential is resolved on a guess.
                Err(_) => BaseFetch::Skipped {
                    code: FailureClass::NoCredentials.slug().into(),
                },
            }
        };
        let ops = self.ops_lock(handle.id);
        let (missing, ok, _, _) = seed::verify_connectivity(
            git,
            &handle.git_dir,
            &plan.members,
            &plan.patchsets,
            Some(&ops),
        )
        .map_err(|e| StoreUnavailable::Error {
            detail: e.to_string(),
        })?;
        self.apply_objects_state(store, &missing, &ok);
        // Merged into a FRESH read, never the `row` snapshot taken above:
        // the member fetches, base fetch and connectivity verify above run
        // for MINUTES, and a `json_set` that lands meanwhile — the forge
        // probe's `default_branch`, a backup pass's `last_backup` — would
        // be silently dropped by a write derived from the stale copy. The
        // read is here, immediately before the write, for that reason.
        let fresh = store
            .get_review_store(handle.id)
            .map_err(|e| StoreUnavailable::Error {
                detail: e.to_string(),
            })?
            .ok_or(StoreUnavailable::NotRegistered)?;
        let mut sj: serde_json::Value = fresh
            .state_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        sj["code"] = "ready".into();
        sj["base"] = serde_json::to_value(&base).unwrap_or_default();
        sj["last_work_fetch"] = now().into();
        if matches!(base, BaseFetch::Fetched { .. }) {
            sj["last_base_fetch"] = now().into();
        }
        sj["objects_missing"] = serde_json::to_value(&missing).unwrap_or_default();
        let _ = store.set_review_store_state(row.id, "ready", Some(&sj.to_string()));
        Ok(SyncReport {
            members,
            member_errors,
            member_problems: problems,
            base,
            objects_missing: missing,
        })
    }

    /// Fetch `branches` of the store's forge project into
    /// `refs/remotes/base/*` with an already-resolved credential. Blocking;
    /// take [`Self::fetch_lock`] for `RemoteName::base()` first.
    pub fn fetch_base(
        &self,
        handle: &StoreHandle,
        branches: &[String],
        cred: &FetchCredential,
    ) -> BaseFetch {
        match self.git.as_ref() {
            Some(git) => seed::fetch_base_branches(git, &handle.git_dir, branches, cred),
            None => BaseFetch::Skipped {
                code: "store-disabled".into(),
            },
        }
    }
}

/// `store sync`'s report.
#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub members: Vec<seed::MemberImport>,
    pub member_errors: Vec<String>,
    pub member_problems: Vec<String>,
    pub base: BaseFetch,
    pub objects_missing: Vec<i64>,
}

/// What [`ReviewStores::import_pending_members`] did: the members it
/// imported, and the ones it could not. `members` alone would read
/// "every pending member is in the store now" over a member whose
/// fetch hard-failed.
#[derive(Debug, Default, Clone)]
pub struct PendingImport {
    pub members: Vec<seed::MemberImport>,
    /// One `"<repo>: <slug>"` per member whose import failed. Such a
    /// member is not marked imported — it stays PENDING and is retried —
    /// so this is the only record of it until an import succeeds.
    pub errors: Vec<String>,
}

struct SeedClaim<'a> {
    set: &'a parking_lot::Mutex<BTreeSet<i64>>,
    id: i64,
}

impl Drop for SeedClaim<'_> {
    fn drop(&mut self) {
        self.set.lock().remove(&self.id);
    }
}

/// The store-level refusal for a credential the operator supplied and
/// that was REFUSED (D12), logged on the way out. The class is
/// [`FailureClass::is_auth`]'s — the grouping `cred.rs` stops the ladder
/// on and the CLI already reads off the wire — so "a credential fault"
/// is defined in exactly one place and this call site only asks the
/// question.
fn credential_refused(row: &ReviewStoreRow, e: &CredError) -> StoreUnavailable {
    let class = e.class();
    tracing::warn!(
        store = %row.store_key,
        code = class.slug(),
        detail = %e,
        "kb-code: the store's configured credential was refused; the store is not brought \
         up on another rung (D12)"
    );
    StoreUnavailable::CredentialRefused {
        class: class.slug().into(),
        detail: e.to_string(),
    }
}

/// The `state_json` a row is left with when its credential was refused:
/// the class as `code`, so `store show`/doctor name the same slug the
/// refusal did instead of a bare "not seeded yet".
fn refused_state(e: &CredError) -> String {
    let class = e.class();
    serde_json::json!({
        "code": class.slug(),
        "class": class.slug(),
        "detail": e.to_string(),
        "at": now(),
    })
    .to_string()
}

/// `state_json.code`, if any.
pub fn state_code(state_json: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(state_json?).ok()?;
    v.get("code")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_store::cred::CredentialPin;

    fn member(
        repo: &str,
        declared: bool,
        credential: CredentialPin,
        gh_user: Option<&str>,
    ) -> MemberSettings {
        MemberSettings {
            repo: repo.to_string(),
            declared,
            credential,
            gh_user: gh_user.map(str::to_string),
        }
    }

    /// D12 — one member's entry drives a store-wide credential, and it is
    /// the one that DECLARES one (a member without an entry contributes
    /// defaults and never out-votes a member that pins an account).
    #[test]
    fn the_declaring_member_drives_the_store() {
        assert_eq!(
            credential_source(&[
                member("a", false, CredentialPin::Auto, None),
                member("b", true, CredentialPin::GhCli, Some("alice")),
            ]),
            StoreCredentialSource::Member("b".into())
        );
        // No member declares an entry: the first member's defaults, as
        // before.
        assert_eq!(
            credential_source(&[
                member("a", false, CredentialPin::Auto, None),
                member("b", false, CredentialPin::Auto, None),
            ]),
            StoreCredentialSource::Member("a".into())
        );
    }

    /// The reported defect: repo A (id 1) has an entry with no pin, repo B
    /// pins `gh_user`. "First entry wins" binds the store to whatever A
    /// resolves — unbound, and so free to fall through. No member is
    /// chosen; every member's value is named.
    #[test]
    fn a_pin_on_a_later_member_is_never_silently_dropped() {
        let src = credential_source(&[
            member("a", true, CredentialPin::Auto, None),
            member("b", true, CredentialPin::Auto, Some("alice")),
        ]);
        let StoreCredentialSource::Disagreement(detail) = src else {
            panic!("a store-wide pin must not be resolved off another member");
        };
        assert!(
            detail.contains("a credential=auto gh_user=<unset>"),
            "{detail}"
        );
        assert!(
            detail.contains("b credential=auto gh_user=alice"),
            "{detail}"
        );
    }

    #[test]
    fn only_a_real_disagreement_stops_a_resolution() {
        // The same pin, spelled differently, is agreement (D12 compares
        // logins case-insensitively).
        assert_eq!(
            credential_source(&[
                member("a", true, CredentialPin::GhCli, Some("alice")),
                member("b", true, CredentialPin::GhCli, Some("ALICE")),
            ]),
            StoreCredentialSource::Member("a".into())
        );
        // A different `credential` IS a disagreement, pin or not.
        let src = credential_source(&[
            member("a", true, CredentialPin::GhCli, Some("alice")),
            member("b", true, CredentialPin::Token, Some("alice")),
        ]);
        let StoreCredentialSource::Disagreement(detail) = src else {
            panic!("two members pinning different rungs must not be resolved");
        };
        assert!(detail.contains("b credential=token"), "{detail}");
    }

    #[test]
    fn uuids_are_v4_shaped_and_distinct() {
        let a = new_store_uuid().unwrap();
        let b = new_store_uuid().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert!(a.bytes().all(|c| c == b'-' || c.is_ascii_hexdigit()));
    }

    #[test]
    fn canonical_base_url_prefers_https_and_never_keeps_userinfo() {
        assert_eq!(
            canonical_base_url("git@github.com:acme/widgets.git", "github.com/acme/widgets")
                .unwrap(),
            "https://github.com/acme/widgets.git"
        );
        assert_eq!(
            canonical_base_url(
                "https://x-access-token:ghp_FAKE@github.com/acme/widgets.git",
                "github.com/acme/widgets"
            )
            .unwrap(),
            "https://github.com/acme/widgets.git"
        );
        assert_eq!(
            canonical_base_url(
                "ssh://git@git.example.com:7999/acme/widgets.git",
                "git.example.com:7999/acme/widgets"
            )
            .unwrap(),
            "ssh://git@git.example.com:7999/acme/widgets.git"
        );
    }

    #[test]
    fn base_branch_of_reads_every_legacy_shape() {
        let remotes = vec!["origin".to_string(), "mine".to_string()];
        let b = |r: &str| base_branch_of(r, None, &remotes);
        assert_eq!(b("refs/remotes/origin/main").as_deref(), Some("main"));
        assert_eq!(
            b("refs/remotes/origin/feature/x").as_deref(),
            Some("feature/x")
        );
        assert_eq!(b("refs/heads/develop").as_deref(), Some("develop"));
        assert_eq!(b("origin/main").as_deref(), Some("main"));
        assert_eq!(b("feature/x").as_deref(), Some("feature/x"));
        assert_eq!(b("main").as_deref(), Some("main"));
        assert_eq!(b(&"a".repeat(40)), None);
        assert_eq!(b("refs/kbc/pr/3"), None);
        assert_eq!(
            base_branch_of("x", Some("release"), &remotes).as_deref(),
            Some("release")
        );
    }

    #[test]
    fn seeding_refusal_is_a_503_with_retry_after() {
        use axum::response::IntoResponse;
        let resp = StoreRefusal(StoreUnavailable::Seeding).into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.headers().get(axum::http::header::RETRY_AFTER).unwrap(),
            &SEEDING_RETRY_AFTER_SECS.to_string()
        );
        assert_eq!(StoreUnavailable::Seeding.code(), "store-seeding");
        assert_eq!(URN_STORE_SEEDING, "urn:kb:errors:store-seeding");
    }

    #[test]
    fn reads_can_use_store_is_false_on_every_refusal_arm() {
        // Arm 1 — the CONFIGURED refusal: the documented defect this whole
        // predicate exists for. A relative/overlapping
        // `[review.store] root` sets `disabled` with no spawner involved.
        assert!(
            !ReviewStores::disabled("fixture").reads_can_use_store(),
            "a configured refusal must stop reads resolving a store root"
        );

        // Arm 2 — an unbuildable store git spawner with NOTHING
        // configured wrong: `git` is `None` and the error is carried
        // separately, exactly as `unavailable_reason` reports it
        // (`Disabled`). This arm is a deliberate, PINNED behaviour change
        // for reads: reads shell out through `history::run_git_raw` with
        // the ambient environment and never use the scrubbed `StoreGit`,
        // so a `read_store_only` `refs/kbc/*` lookup that used to resolve
        // off a stale `ready` row now falls back to the user repo and
        // misses. Mirroring `unavailable_reason` is the point; leaving it
        // unpinned is not.
        let settings =
            StoreSettings::resolve(&ReviewSection::default(), Path::new("/nonexistent"), &[]);
        assert!(
            settings.disabled.is_none(),
            "control: this arm is about the spawner, not a configured refusal"
        );
        let unbuildable = ReviewStores::from_parts(
            settings,
            None,
            Some("store git spawner unavailable".to_string()),
            Vec::new(),
        );
        assert!(
            !unbuildable.reads_can_use_store(),
            "a store whose git spawner could not be built must not resolve for reads"
        );
    }
}
