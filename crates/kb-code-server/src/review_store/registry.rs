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
//!   DB state machine (`absent → seeding → ready`, or `broken`);
//! * **locks** — a per-(store, remote) FETCH mutex and a short per-store
//!   OPS mutex (design §4.2), plus the lifetime `flock` per store
//!   (`manifest::StoreLock`). `tokio::sync::Mutex`es: a fetch guard is held
//!   across the `spawn_blocking` await of the fetch itself.
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

use super::cred::{
    resolve_fetch_credential, CredError, FetchCredential, GhCli, LiveProbes, Resolution,
};
use super::git::StoreGit;
use super::key::{https_url_for_key, local_store_key, split_key};
use super::ladder::{self, LadderInput, LadderOutcome, NoForkCheck, RemoteInfo};
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
            _ => (StatusCode::CONFLICT, None),
        };
        let code = self.0.code();
        let mut body = serde_json::json!({
            "error": format!("review store unavailable: {code}"),
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

impl ReviewStores {
    /// Build from config. Never fails boot: a spawner that cannot be
    /// built, or a disabled root, leaves every store `unavailable` and
    /// reads fall back to the user repos.
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
    pub fn import_pending_members(
        &self,
        store: &Store,
        store_id: i64,
    ) -> Result<Vec<seed::MemberImport>, StoreUnavailable> {
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
            return Ok(vec![]);
        }
        let dir = PathBuf::from(&row.git_dir);
        let (members, _) = self.members_of(store, store_id);
        let ops = self.ops_lock(store_id);
        let mut out = Vec::new();
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
                    tracing::warn!(repo = %r.name, class = %e.class, "review store: member import failed");
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
            out.push(imp);
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

    /// Take the lifetime flock on `uuid`. The caller holds the `held`
    /// guard for the whole check-acquire-insert, so two same-process opens
    /// can never race each other into a self-inflicted `store-locked`.
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
        let mut held = self.held.lock();
        if !held.contains_key(&row.uuid) {
            let lock = self.acquire_lock(&row.uuid)?;
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
        }
        drop(held);
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
                };
            }
            return Registration::Member {
                store_id: row.id,
                store_key: row.store_key,
                source: row.base_url_source.unwrap_or_else(|| "member".into()),
                joined_existing: true,
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
            fork_check: &NoForkCheck,
        });
        let (key, base_url, source) = match outcome {
            LadderOutcome::Resolved {
                store_key,
                url,
                source,
                ..
            } => {
                let base = canonical_base_url(&url, &store_key);
                (store_key, base, source.slug())
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
                }
            }
            Ok(None) => {
                let uuid = match new_store_uuid() {
                    Ok(u) => u,
                    Err(e) => return err("failed", e.to_string()),
                };
                self.create_and_join(store, &repo, &uuid, &key, base_url.as_deref(), source)
            }
            Err(e) => err("db", e.to_string()),
        }
    }

    fn create_and_join(
        &self,
        store: &Store,
        repo: &RepoRef,
        uuid: &str,
        key: &str,
        base_url: Option<&str>,
        source: &str,
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

    /// The first member with a `[[review.repos]]` entry, else the first
    /// member — whose settings drive store-wide credential resolution.
    pub fn settings_repo_for(&self, store: &Store, store_id: i64) -> Option<String> {
        let names: Vec<String> = store
            .store_members(store_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|id| self.repo_by_id(id).map(|r| r.name.clone()))
            .collect();
        names
            .iter()
            .find(|n| self.settings.repos.contains_key(*n))
            .or(names.first())
            .cloned()
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
                    // Re-verify connectivity for the adopted store.
                    let (plan, _) = self
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
                    // An adopted directory may predate some members.
                    let _ = self.import_pending_members(store, row.id);
                    Ok(SeedReport {
                        git_dir: dir.clone(),
                        members: vec![],
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
        let mut cred_note = None;
        let cred = if network && plan.base_url.is_some() {
            let who = self.settings_repo_for(store, row.id).unwrap_or_default();
            match self.resolve_credential(store, &row, &who) {
                Ok(r) => Some(r.credential),
                Err(e) => {
                    cred_note = Some(e.class().slug());
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
                // plan snapshot: import them now.
                let _ = self.import_pending_members(store, row.id);
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
            let who = self.settings_repo_for(store, row.id).unwrap_or_default();
            match self.resolve_credential(store, &row, &who) {
                Ok(r) => seed::fetch_base_branches(
                    git,
                    &handle.git_dir,
                    &plan.base_branches,
                    &r.credential,
                ),
                Err(e) => BaseFetch::Skipped {
                    code: e.class().slug().into(),
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
        let mut sj: serde_json::Value = row
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

struct SeedClaim<'a> {
    set: &'a parking_lot::Mutex<BTreeSet<i64>>,
    id: i64,
}

impl Drop for SeedClaim<'_> {
    fn drop(&mut self) {
        self.set.lock().remove(&self.id);
    }
}

/// `state_json.code`, if any.
pub fn state_code(state_json: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(state_json?).ok()?;
    v.get("code")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
