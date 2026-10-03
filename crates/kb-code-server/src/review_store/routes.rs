//! RS-U3 — the review-store HTTP surface (README §12).
//!
//! * `GET  /api/repos/{name}/store` — the store card (`kbc-store/1`):
//!   registration, state, members, disk facts, doctor findings.
//!   LOOPBACK-ONLY, like every other route in the RS-U3 store family
//!   below: the card reports `store.git_dir` — the ABSOLUTE path of the
//!   daemon's internal state dir — plus `store.uuid` and internal store
//!   row ids, and no bearer route in this crate hands out a kb-internal
//!   path. It carries no secret and no repo content; the gate is about
//!   the on-disk location, not the credentials.
//! * `GET  /api/repos/{name}/credentials` — the fetch credential as last
//!   RESOLVED (`kbc-credentials/1`): kind, account, reason, and the D9
//!   "broader than needed" flag. Never secret bytes; never runs `gh`. The
//!   `config` block also names the member entry the STORE resolved with
//!   (`settings_repo`) and any disagreement between members
//!   (`credential_disagreement`) — a store-wide pin is never read off one
//!   member in db order silently.
//! * `POST /api/repos/{name}/store/sync` — LOOPBACK-ONLY. Registers the
//!   repo if needed, seeds an `absent` store (with the base fetch unless
//!   `?offline=1`), or syncs a `ready` one. A `seeding` store refuses with
//!   503 `urn:kb:errors:store-seeding` + `retry_after`.
//! * `POST /api/repos/{name}/store/base-url` — LOOPBACK-ONLY. The ladder's
//!   explicit rung: registers an unregistered repo against `base_url`, or
//!   updates a member store's base URL when it names the SAME project
//!   (never a re-key: 409 `base-url-key-mismatch`).
//! * `POST /api/repos/{name}/credentials/test` — LOOPBACK-ONLY. Walks the
//!   fetch ladder live (runs `gh`), persists the answer, and asks the
//!   credential chain whether it answers for the forge host (no fetch).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::cred::{FetchCredential, ProfileKind};
use super::key::{split_key, store_key_for_url};
use super::registry::{
    state_code, Registration, ReviewStores, StoreCredentialSource, StoreRefusal, StoreUnavailable,
    BROADER_THAN_NEEDED_MARK,
};
use super::seed;
use super::url::RemoteName;
use crate::state::SharedState;
use crate::store::{ReviewStoreRow, Store};

/// `kbc-store/1`.
pub const STORE_SCHEMA: &str = "kbc-store/1";
/// `kbc-credentials/1`.
pub const CREDENTIALS_SCHEMA: &str = "kbc-credentials/1";

// One doctor finding. (Plain comments, not docs, on the ts-exported types in
// this block: the generated binding must stay stable.)
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ReviewStoreDoctorFinding")
)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DoctorFinding {
    // `error` | `warn` | `info`.
    #[cfg_attr(feature = "ts-export", ts(type = "\"error\" | \"warn\" | \"info\""))]
    pub level: &'static str,
    pub code: String,
    pub message: String,
    // `daemon` — a fact about the whole daemon's configuration (the same on
    // EVERY repo's card, so a dashboard shows it once) — or `repo`.
    #[cfg_attr(feature = "ts-export", ts(type = "\"daemon\" | \"repo\""))]
    pub scope: &'static str,
}

fn finding(level: &'static str, code: &str, message: impl Into<String>) -> DoctorFinding {
    DoctorFinding {
        level,
        code: code.into(),
        message: message.into(),
        scope: "repo",
    }
}

/// A finding that is true of the whole daemon (config), not of this repo.
fn daemon_finding(level: &'static str, code: &str, message: impl Into<String>) -> DoctorFinding {
    DoctorFinding {
        scope: "daemon",
        ..finding(level, code, message)
    }
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ReviewStoreMember")
)]
#[derive(Debug, Serialize)]
struct MemberView {
    repo_id: i64,
    name: Option<String>,
    remote: String,
}

// The store row as the card reports it. `git_dir` and `uuid` are the
// store's ABSOLUTE on-disk location and its identity, so this view only
// ever reaches a LOOPBACK caller — see `store_show_route`. Everything
// secret-adjacent is still omitted: `cred_reason`, `cred_account`,
// `key_fingerprint` and `key_read_only` never leave the store DB.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ReviewStoreRow")
)]
#[derive(Debug, Serialize)]
struct StoreRowView {
    id: i64,
    uuid: String,
    store_key: String,
    git_dir: String,
    base_url: Option<String>,
    base_url_source: Option<String>,
    forge_kind: Option<String>,
    forge_host: Option<String>,
    forge_slug: Option<String>,
    forge_verified: String,
    cred_kind: String,
    state: String,
    state_code: Option<String>,
    #[cfg_attr(feature = "ts-export", ts(type = "unknown"))]
    state_json: Option<serde_json::Value>,
    created_at: i64,
}

impl From<&ReviewStoreRow> for StoreRowView {
    fn from(r: &ReviewStoreRow) -> Self {
        Self {
            id: r.id,
            uuid: r.uuid.clone(),
            store_key: r.store_key.clone(),
            git_dir: r.git_dir.clone(),
            base_url: r.base_url.clone(),
            base_url_source: r.base_url_source.clone(),
            forge_kind: r.forge_kind.clone(),
            forge_host: r.forge_host.clone(),
            forge_slug: r.forge_slug.clone(),
            forge_verified: r.forge_verified.clone(),
            cred_kind: r.cred_kind.clone(),
            state: r.state.clone(),
            state_code: state_code(r.state_json.as_deref()),
            state_json: r
                .state_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
            created_at: r.created_at,
        }
    }
}

// The card's `runtime` block: process-wide seeding/lock facts for THIS store
// plus the process-wide git-read fallback counters.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ReviewStoreRuntime")
)]
#[derive(Debug, Serialize)]
pub struct StoreRuntimeOut {
    seeding: bool,
    locked_elsewhere: bool,
    git_fallbacks: crate::git::roots::GitFallbackSnapshot,
}

// `GET /api/repos/{name}/store`'s body.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ReviewStoreCard")
)]
#[derive(Debug, Serialize)]
pub struct StoreCard {
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    schema: &'static str,
    repo: String,
    store: Option<StoreRowView>,
    members: Vec<MemberView>,
    registration: Option<Registration>,
    runtime: StoreRuntimeOut,
    disk: Option<seed::StoreStats>,
    doctor: Vec<DoctorFinding>,
}

fn not_found(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("unknown repo `{name}`"),
            "type": "urn:kb:errors:not-found",
        })),
    )
        .into_response()
}

fn internal(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": e.to_string() })),
    )
        .into_response()
}

/// Build the store card (blocking: reads the DB and walks the store dir).
pub fn store_card(rs: &ReviewStores, store: &Store, name: &str) -> Result<StoreCard, String> {
    let row = store.store_for_repo_name(name).map_err(|e| e.to_string())?;
    let mut doctor = Vec::new();
    if let Some(d) = &rs.settings().disabled {
        doctor.push(daemon_finding("error", "store-disabled", d.clone()));
    }
    for w in &rs.settings().warnings {
        doctor.push(daemon_finding("warn", "config", w.clone()));
    }
    let registration = rs.registration(name);
    if let Some(Registration::Refused { code, reason, .. }) = &registration {
        doctor.push(finding("error", code, reason.clone()));
    }
    if let Some(Registration::Error { code, detail }) = &registration {
        doctor.push(finding("error", code, detail.clone()));
    }

    // A refused remote on an otherwise-healthy store: the clone carries a
    // URL the ladder would not key, and the good remote that DID decide
    // the store means nothing else reports it. Without this finding a
    // hostile or typo'd remote is invisible on every boot after the one
    // that minted the store.
    if let Some(Registration::Member {
        refused_remotes, ..
    }) = &registration
    {
        for r in refused_remotes {
            doctor.push(finding(
                "warn",
                "remote-url-refused",
                format!("remote {:?}: {}", r.name, r.reason),
            ));
        }
    }
    // A credential the operator supplied and which was REFUSED leaves its
    // class on the row. Surfacing it here is the difference between "the
    // store is fine" and "the store is fine because we gave up on the
    // base" — the row is `absent` in that case, but an operator reading
    // only the state would not know why.
    if let Some(class) = row.as_ref().and_then(super::boot::refused_credential) {
        doctor.push(finding("error", "store-credential-refused", class));
    }
    let cfg = rs.settings().repo(name);
    let mut members = Vec::new();
    let mut disk = None;
    let mut runtime_seeding = false;
    let mut runtime_locked = false;
    match &row {
        None => {
            if registration.is_none() {
                doctor.push(finding(
                    "info",
                    "store-not-registered",
                    "no store yet: registration runs at boot for repos with reviews, or on `kb-code store sync`",
                ));
            }
        }
        Some(r) => {
            for id in store.store_members(r.id).map_err(|e| e.to_string())? {
                members.push(MemberView {
                    repo_id: id,
                    name: rs
                        .repos()
                        .iter()
                        .find(|x| x.id == id)
                        .map(|x| x.name.clone()),
                    remote: RemoteName::work(id).as_str().to_string(),
                });
            }
            let seeding = rs.is_seeding(r.id) || r.state == "seeding";
            let locked = rs.is_locked_elsewhere(&r.uuid);
            runtime_seeding = seeding;
            runtime_locked = locked;
            let dir = std::path::Path::new(&r.git_dir);
            if dir.is_dir() {
                disk = Some(rs.cached_stats(&r.uuid, dir));
            }
            match r.state.as_str() {
                "broken" => doctor.push(finding(
                    "error",
                    "store-broken",
                    format!(
                        "store is broken ({})",
                        state_code(r.state_json.as_deref()).unwrap_or_default()
                    ),
                )),
                "absent" => doctor.push(finding(
                    "warn",
                    "store-absent",
                    "store not seeded yet; reads fall back to the user repo",
                )),
                _ => {}
            }
            if locked {
                doctor.push(finding(
                    "error",
                    "store-locked",
                    "another kb-code process holds this store's lock",
                ));
            }
            if let Some(url) = &cfg.base_url {
                match store_key_for_url(url) {
                    Some(k) if k != r.store_key => doctor.push(finding(
                        "error",
                        "base-url-key-mismatch",
                        format!(
                            "[[review.repos]] base_url names `{k}` but this repo's store is `{}`; a store is never re-keyed",
                            r.store_key
                        ),
                    )),
                    None => doctor.push(finding(
                        "error",
                        "base-url-invalid",
                        "[[review.repos]] base_url is not a forge project URL",
                    )),
                    _ => {}
                }
            }
            if r.forge_verified != "verified" && r.forge_kind.is_some() {
                doctor.push(finding(
                    "info",
                    "forge-unverified",
                    format!(
                        "forge `{}` ships forge-unverified (D8)",
                        r.forge_kind.as_deref().unwrap_or("?")
                    ),
                ));
            }
            if r.cred_kind == "inherit" && r.cred_reason.is_some() {
                doctor.push(finding(
                    "warn",
                    "credential-inherit",
                    "fetches use the ambient environment (legacy, amber)",
                ));
            }
            // D12 — a store-wide pin read off ONE member, chosen by db
            // order, is the account-confusion axis itself: reported, and no
            // credential resolved on a guess.
            if let StoreCredentialSource::Disagreement(detail) =
                rs.credential_source_for(store, r.id)
            {
                doctor.push(finding("warn", "credential-settings-conflict", detail));
            }
            if r.cred_reason
                .as_deref()
                .is_some_and(|s| s.ends_with(BROADER_THAN_NEEDED_MARK))
            {
                doctor.push(finding(
                    "warn",
                    "credential-broader-than-needed",
                    "the gh token's scopes are broader than fetch + GET need (used read-only)",
                ));
            }
            let missing = r
                .state_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| {
                    v.get("objects_missing")
                        .and_then(|m| m.as_array().map(Vec::len))
                })
                .unwrap_or(0);
            if missing > 0 {
                doctor.push(finding(
                    "warn",
                    "objects-missing",
                    format!(
                        "{missing} review(s) have patchset commits that exist nowhere (read-only)"
                    ),
                ));
            }
        }
    }
    // Process-wide counters of review/PR git reads that could not use a
    // store (`unresolved`: no ready store; `odb_miss`: a ready store lacked
    // the object and the work tree served it). Acceptance gate 3 ("0 user-ODB
    // fallback hits once every store is ready") reads `odb_miss` here.
    let runtime = StoreRuntimeOut {
        seeding: runtime_seeding,
        locked_elsewhere: runtime_locked,
        git_fallbacks: store.git_fallback_stats(),
    };
    Ok(StoreCard {
        schema: STORE_SCHEMA,
        repo: name.to_string(),
        store: row.as_ref().map(StoreRowView::from),
        members,
        registration,
        runtime,
        disk,
        doctor,
    })
}

/// `GET /api/repos/{name}/store` — LOOPBACK-ONLY (mounted on the
/// `transcripts_api` sub-router beside the store family's mutations): the
/// card carries `git_dir`/`uuid`, the daemon's internal state-dir path.
pub async fn store_show_route(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Response {
    if state.review_stores.repo(&name).is_none() {
        return not_found(&name);
    }
    let st = state.clone();
    match tokio::task::spawn_blocking(move || store_card(&st.review_stores, &st.store, &name)).await
    {
        Ok(Ok(card)) => Json(card).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

#[derive(Debug, Serialize)]
struct SkippedView {
    rung: &'static str,
    class: &'static str,
    reason: String,
}

/// `GET /api/repos/{name}/credentials` — the persisted resolution.
pub async fn credentials_route(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Response {
    if state.review_stores.repo(&name).is_none() {
        return not_found(&name);
    }
    let st = state.clone();
    let n = name.clone();
    let rs = state.review_stores.clone();
    let (row, source) = match tokio::task::spawn_blocking(move || {
        let row = st.store.store_for_repo_name(&n);
        // Which member's `[[review.repos]]` entry the STORE resolves with —
        // not necessarily this repo's, and never silently (D12).
        let source = row
            .as_ref()
            .ok()
            .and_then(|r| r.as_ref())
            .map(|r| rs.credential_source_for(&st.store, r.id));
        (row, source)
    })
    .await
    {
        Ok(v) => v,
        Err(e) => return internal(e),
    };
    let row = match row {
        Ok(r) => r,
        Err(e) => return internal(e),
    };
    let (settings_repo, disagreement) = match &source {
        Some(StoreCredentialSource::Member(m)) => ((!m.is_empty()).then(|| m.clone()), None),
        Some(StoreCredentialSource::Disagreement(d)) => (None, Some(d.clone())),
        None => (None, None),
    };
    let cfg = state.review_stores.settings().repo(&name);
    let (kind, reason, account, host) = match &row {
        Some(r) => (
            Some(r.cred_kind.clone()),
            r.cred_reason.clone(),
            r.cred_account.clone(),
            split_key(&r.store_key).map(|(h, _)| h.to_string()),
        ),
        None => (None, None, None, None),
    };
    let resolved = reason.is_some();
    let broader = reason
        .as_deref()
        .is_some_and(|s| s.ends_with(BROADER_THAN_NEEDED_MARK));
    Json(serde_json::json!({
        "schema": CREDENTIALS_SCHEMA,
        "repo": name,
        "fetch": {
            // `cred_kind` is the column DEFAULT until a resolution ran.
            "cred_kind": if resolved { kind } else { None },
            "account": account,
            "reason": reason.map(|r| r.trim_end_matches(BROADER_THAN_NEEDED_MARK).to_string()),
            "broader_than_needed": broader,
            "amber": resolved && row.as_ref().is_some_and(|r| r.cred_kind == "inherit"),
            "resolved": resolved,
            "host": host,
        },
        "config": {
            "credential": pin_slug(cfg.credential),
            "gh_user": cfg.gh_user,
            "token_file_configured": cfg.token_file.is_some(),
            "allow_inherited_credentials": state.review_stores.settings().allow_inherited_credentials,
            // Which member's entry the store actually used, and whether its
            // members disagreed (then nothing was resolved at all).
            "settings_repo": settings_repo,
            "credential_disagreement": disagreement,
        },
    }))
    .into_response()
}

fn pin_slug(p: super::cred::CredentialPin) -> &'static str {
    use super::cred::CredentialPin as P;
    match p {
        P::Auto => "auto",
        P::GhCli => "gh-cli",
        P::DeployKey => "deploy-key",
        P::Token => "token",
        P::Anonymous => "anonymous",
        P::Inherit => "inherit",
        P::None => "none",
    }
}

/// One audit line per store/credential mutation call (README §8: "key and
/// credential endpoints are loopback-only and audited"). Route, repo and
/// outcome only — never a URL body, token, or stderr.
fn audit(route: &'static str, repo: &str, status: StatusCode) {
    tracing::info!(
        target: "kb_code::audit",
        route,
        repo,
        status = status.as_u16(),
        "review store mutation"
    );
}

/// Import members that joined a ready store (BLOCKER-1 fix) in the
/// background: never on the request path.
fn spawn_pending_import(state: &SharedState, name: &str) {
    let st = state.clone();
    let n = name.to_string();
    tokio::spawn(async move {
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(Some(row)) = st.store.store_for_repo_name(&n) {
                // Both arms are visible: a store-level refusal is a code,
                // and a PER-MEMBER failure now comes back in the Ok value's
                // `errors` rather than only a warn-and-continue — without
                // this the caller of this background task could not tell a
                // clean import from a member that was skipped.
                match st.review_stores.import_pending_members(&st.store, row.id) {
                    Ok(imported) => {
                        for e in imported.errors {
                            tracing::warn!(
                                repo = %n,
                                error = %e,
                                "review store: background member import skipped a member"
                            );
                        }
                    }
                    Err(u) => {
                        tracing::warn!(repo = %n, code = u.code(), "review store: background member import failed");
                    }
                }
            }
        })
        .await;
    });
}

/// `?offline=1` on sync: skip the network base fetch.
#[derive(Debug, Default, Deserialize)]
pub struct SyncQuery {
    #[serde(default)]
    pub offline: bool,
}

fn registration_refusal(r: &Registration) -> Option<Response> {
    match r {
        Registration::Member { .. } => None,
        Registration::Refused {
            code,
            reason,
            candidates,
        } => Some(
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": reason,
                    "type": format!("urn:kb:errors:{code}"),
                    "code": code,
                    "candidates": candidates,
                })),
            )
                .into_response(),
        ),
        Registration::Error { code, detail } => Some(
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": detail,
                    "type": format!("urn:kb:errors:{code}"),
                    "code": code,
                })),
            )
                .into_response(),
        ),
    }
}

/// `POST /api/repos/{name}/store/sync` (loopback-only).
pub async fn store_sync_route(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Query(q): Query<SyncQuery>,
) -> Response {
    let resp = store_sync_route_inner(State(state.clone()), Path(name.clone()), Query(q)).await;
    audit("store_sync_route", &name, resp.status());
    if resp.status().is_success() {
        spawn_pending_import(&state, &name);
    }
    resp
}

async fn store_sync_route_inner(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Query(q): Query<SyncQuery>,
) -> Response {
    if state.review_stores.repo(&name).is_none() {
        return not_found(&name);
    }
    // 1. registration + current row (blocking).
    let st = state.clone();
    let n = name.clone();
    let pre = tokio::task::spawn_blocking(move || {
        let reg = st.review_stores.register_repo(&st.store, &n, None);
        let row = st.store.store_for_repo_name(&n);
        (reg, row)
    })
    .await;
    let (reg, row) = match pre {
        Ok((reg, Ok(row))) => (reg, row),
        Ok((_, Err(e))) => return internal(e),
        Err(e) => return internal(e),
    };
    if let Some(resp) = registration_refusal(&reg) {
        return resp;
    }
    let Some(row) = row else {
        return internal("registration succeeded but no store row is visible");
    };
    if row.state == "seeding" || state.review_stores.is_seeding(row.id) {
        return StoreRefusal(StoreUnavailable::Seeding).into_response();
    }
    if row.state != "ready" {
        // 2a. seed (absent / broken-by-missing-dir).
        let st = state.clone();
        let network = !q.offline;
        let id = row.id;
        return match tokio::task::spawn_blocking(move || {
            st.review_stores.seed(&st.store, id, network)
        })
        .await
        {
            Ok(Ok(report)) => Json(serde_json::json!({
                "schema": "kbc-store-sync/1",
                "repo": name,
                "action": "seeded",
                "report": report,
            }))
            .into_response(),
            Ok(Err(u)) => StoreRefusal(u).into_response(),
            Err(e) => internal(e),
        };
    }
    // 2b. sync a ready store under its fetch locks.
    let st = state.clone();
    let handle =
        match tokio::task::spawn_blocking(move || st.review_stores.open(&st.store, &row)).await {
            Ok(Ok(h)) => h,
            Ok(Err(u)) => return StoreRefusal(u).into_response(),
            Err(e) => return internal(e),
        };
    let mut remotes = vec![RemoteName::base()];
    let st = state.clone();
    let hid = handle.id;
    let member_ids = tokio::task::spawn_blocking(move || st.store.store_members(hid))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    remotes.extend(member_ids.into_iter().map(RemoteName::work));
    // Fixed order (base, then work-<id> ascending) — no lock-order inversion.
    let locks: Vec<_> = remotes
        .iter()
        .map(|r| state.review_stores.fetch_lock(handle.id, r))
        .collect();
    let st = state.clone();
    let network = !q.offline;
    let res = run_under_fetch_locks(locks, move || {
        st.review_stores.sync_ready(&st.store, &handle, network)
    })
    .await;
    match res {
        Ok(Ok(report)) => Json(serde_json::json!({
            "schema": "kbc-store-sync/1",
            "repo": name,
            "action": "synced",
            "report": report,
        }))
        .into_response(),
        Ok(Err(u)) => StoreRefusal(u).into_response(),
        Err(e) => internal(e),
    }
}

/// Run `work` on the blocking pool while holding every lock in `locks`
/// (taken in the given order).
///
/// The locks are acquired INSIDE the blocking closure, so their lifetime
/// equals the work's: a dropped request future (client disconnect, CLI
/// timeout) abandons the `.await` but not the blocking task, and the guards
/// are released only when `work` returns — never while it is still
/// fetching. (Acquiring on the async side and moving the guards in would
/// release them on drop of the future.)
///
/// Bound on the cost: the acquire is `blocking_lock_owned`, so while
/// another holder has a lock this task PARKS one blocking-pool thread. The
/// wait is at most the other holder's own work, which is itself bounded by
/// the git deadlines (`WORK_FETCH_TIMEOUT` per local fetch, one
/// `base_fetch_timeout` per base pass, `SEED_FETCH_TIMEOUT` per seed), and
/// the number of parked threads by the number of concurrent requests for
/// ONE store (the blocking pool's cap, 512 by default, is shared with every
/// other `spawn_blocking` user; a try-lock-and-retry loop would trade the
/// parked thread for a poll without shortening the wait).
pub(crate) async fn run_under_fetch_locks<T: Send + 'static>(
    locks: Vec<std::sync::Arc<tokio::sync::Mutex<()>>>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let _guards: Vec<_> = locks.into_iter().map(|l| l.blocking_lock_owned()).collect();
        work()
    })
    .await
}

#[derive(Debug, Deserialize)]
pub struct BaseUrlBody {
    pub base_url: String,
}

/// `POST /api/repos/{name}/store/base-url` (loopback-only).
pub async fn store_base_url_route(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(body): Json<BaseUrlBody>,
) -> Response {
    let resp =
        store_base_url_route_inner(State(state.clone()), Path(name.clone()), Json(body)).await;
    audit("store_base_url_route", &name, resp.status());
    if resp.status().is_success() {
        spawn_pending_import(&state, &name);
    }
    resp
}

async fn store_base_url_route_inner(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(body): Json<BaseUrlBody>,
) -> Response {
    if state.review_stores.repo(&name).is_none() {
        return not_found(&name);
    }
    if store_key_for_url(&body.base_url).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "base_url is not a forge project URL",
                "type": "urn:kb:errors:base-url-invalid",
            })),
        )
            .into_response();
    }
    let st = state.clone();
    let n = name.clone();
    let res = tokio::task::spawn_blocking(move || {
        if let Ok(Some(row)) = st.store.store_for_repo_name(&n) {
            if row.state == "seeding" || st.review_stores.is_seeding(row.id) {
                return Err(StoreRefusal(StoreUnavailable::Seeding));
            }
        }
        let reg = st
            .review_stores
            .register_repo(&st.store, &n, Some(&body.base_url));
        let card = store_card(&st.review_stores, &st.store, &n).ok();
        Ok((reg, card))
    })
    .await;
    match res {
        Ok(Ok((reg, card))) => match registration_refusal(&reg) {
            Some(r) => r,
            None => Json(serde_json::json!({ "registration": reg, "store": card })).into_response(),
        },
        Ok(Err(refusal)) => refusal.into_response(),
        Err(e) => internal(e),
    }
}

/// `POST /api/repos/{name}/credentials/test` (loopback-only).
pub async fn credential_test_route(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Response {
    let resp = credential_test_route_inner(State(state.clone()), Path(name.clone())).await;
    audit("credential_test_route", &name, resp.status());
    if resp.status().is_success() {
        spawn_pending_import(&state, &name);
    }
    resp
}

async fn credential_test_route_inner(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Response {
    if state.review_stores.repo(&name).is_none() {
        return not_found(&name);
    }
    let st = state.clone();
    let n = name.clone();
    let res = tokio::task::spawn_blocking(move || {
        let rs = &st.review_stores;
        if let Registration::Member { .. } = rs.register_repo(&st.store, &n, None) {
        } else {
            return Err((
                StatusCode::CONFLICT,
                "store-not-registered".to_string(),
                None,
            ));
        }
        let row = match st.store.store_for_repo_name(&n) {
            Ok(Some(r)) => r,
            _ => return Err((StatusCode::CONFLICT, "store-not-registered".into(), None)),
        };
        let r = match rs.resolve_store_credential(&st.store, &row) {
            Ok(r) => r,
            Err(e) => {
                return Err((
                    StatusCode::CONFLICT,
                    e.class().slug().to_string(),
                    Some(e.to_string()),
                ))
            }
        };
        let host = split_key(&row.store_key).map(|(h, _)| h.to_string());
        let answers = match (rs.git(), r.credential.auth(), &host) {
            (Some(g), Some(auth), Some(h))
                if !matches!(r.credential, FetchCredential::Anonymous) =>
            {
                g.credential_answers(auth, "https", h).ok()
            }
            _ => None,
        };
        let broader = matches!(&r.credential, FetchCredential::GhCli(g) if g.broader_than_needed());
        let skipped: Vec<SkippedView> = r
            .skipped
            .iter()
            .map(|s| SkippedView {
                rung: s.rung.slug(),
                class: s.class.slug(),
                reason: s.reason.clone(),
            })
            .collect();
        Ok(serde_json::json!({
            "schema": CREDENTIALS_SCHEMA,
            "repo": n,
            "tested": true,
            "fetch": {
                "cred_kind": r.credential.kind().slug(),
                "account": r.credential.account(),
                "reason": r.reason,
                "broader_than_needed": broader,
                "amber": r.credential.is_amber(),
                "helper_answers": answers,
                "skipped": skipped,
                "host": host,
                "network_capable": r.credential.kind() != ProfileKind::None,
            },
        }))
    })
    .await;
    match res {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err((status, code, detail))) => (
            status,
            Json(serde_json::json!({
                "error": detail.clone().unwrap_or_else(|| code.clone()),
                "type": format!("urn:kb:errors:{code}"),
                "code": code,
            })),
        )
            .into_response(),
        Err(e) => internal(e),
    }
}

#[cfg(test)]
mod card_wire_tests {
    use super::*;

    /// N6 — config/disabled findings are daemon-scoped (the same on every
    /// repo's card), per-repo findings are repo-scoped; the wire carries it.
    #[test]
    fn findings_are_tagged_with_their_scope_on_the_wire() {
        let d = serde_json::to_value(daemon_finding("warn", "config", "w")).unwrap();
        assert_eq!(d["scope"], "daemon");
        let r = serde_json::to_value(finding("error", "store-broken", "b")).unwrap();
        assert_eq!(r["scope"], "repo");
    }

    /// N4 — `runtime` is a typed struct and carries `git_fallbacks`.
    #[test]
    fn the_runtime_block_is_typed_and_carries_git_fallbacks() {
        let rt = StoreRuntimeOut {
            seeding: false,
            locked_elsewhere: true,
            git_fallbacks: crate::git::roots::GitFallbackSnapshot {
                unresolved: 2,
                odb_miss: 3,
            },
        };
        let v = serde_json::to_value(rt).unwrap();
        assert_eq!(v["locked_elsewhere"], true);
        assert_eq!(v["git_fallbacks"]["unresolved"], 2);
        assert_eq!(v["git_fallbacks"]["odb_miss"], 3);
    }

    /// A5-6 — a dropped request future mid-sync must NOT release the fetch
    /// locks while the work runs. The work blocks on a channel; the request
    /// task is aborted (what a client disconnect does to the handler);
    /// the lock must still be held until the work returns, and a second
    /// request for the same lock must wait for it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_request_future_keeps_the_fetch_locks_while_the_work_runs() {
        use std::sync::Arc;
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let req = tokio::spawn(run_under_fetch_locks(vec![lock.clone()], move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            done_tx.send(()).unwrap();
        }));
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        assert!(lock.try_lock().is_err(), "held while the work runs");
        // The client goes away: the handler future is dropped mid-work.
        req.abort();
        let _ = req.await;
        assert!(
            lock.try_lock().is_err(),
            "dropping the request future released the fetch lock while the work was still running"
        );
        // A second request for the same lock waits for the first's work.
        let l2 = lock.clone();
        let second = tokio::spawn(run_under_fetch_locks(vec![l2], || ()));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(!second.is_finished(), "the second request must wait");
        release_tx.send(()).unwrap();
        second.await.unwrap().unwrap();
        done_rx.recv().unwrap();
        assert!(lock.try_lock().is_ok(), "released once the work returned");
    }
}
