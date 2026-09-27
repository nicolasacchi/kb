//! RS-U3 — the boot seeding job (D4, README §10 steps 1–2).
//!
//! Spawned AFTER `AppState` is built and never awaited: there is no git
//! I/O on the boot critical path. Stores start `absent` (V0045's column
//! default), and until a store is `ready` every read falls back to the
//! user repo exactly as before the store existed.
//!
//! The job, in order, on a blocking thread:
//!
//! 1. sweep `.seed-*.tmp` leftovers whose lock is free (a crash mid-seed);
//! 2. put stores a dead process left `seeding` back to `absent`;
//! 3. register every configured repo that HAS REVIEWS (D4) with its store
//!    (the ladder, README §5.1) — repos without reviews register lazily,
//!    on their first store action;
//! 4. open every `ready` store (manifest check + lifetime flock), and — when
//!    `[review.store] seed_on_boot` — seed every `absent` one, LOCAL ONLY
//!    (no credential, no network: base branches arrive with the first
//!    explicit `store sync`/capture). One `absent` row is EXEMPT: one
//!    whose last attempt was refused on a CREDENTIAL. A local pass cannot
//!    resolve a credential, so re-seeding it would restore the store to
//!    `ready` over a credential that is still broken — the state the
//!    refused arm in [`super::registry::ReviewStores::seed`] exists to
//!    prevent. Those rows keep their `absent` + class, are reported in
//!    [`BootSummary::refused`], and come up on the first explicit
//!    `store sync`, which does resolve one.
//!
//! Steps 1–2 are CRASH RECOVERY and need no git spawner, so they run
//! BEFORE the spawner gate that guards steps 3–4: skipping them when the
//! spawner is unavailable would leave every boot re-skipping the recovery
//! and wedge for good a store a dead process left `seeding`. That gate
//! logs `store git spawner unavailable` and stops there — with the
//! recovery counts still in the `BootSummary` it logged.

use std::collections::BTreeSet;

use serde::Serialize;

use super::registry::{Registration, ReviewStores};
use super::seed;
use crate::state::SharedState;
use crate::store::Store;

/// What one boot pass did (logged; returned for tests).
#[derive(Debug, Default, Clone, Serialize)]
pub struct BootSummary {
    pub swept_tmp: usize,
    pub reset_seeding: usize,
    pub registered: Vec<String>,
    /// Refusals the pass reported, each `"<what>: <why>"`: a
    /// registration that refused, a member's clone carrying a remote
    /// refused as unsafe, and a store whose last attempt was refused on
    /// a CREDENTIAL and was therefore not re-seeded (step 4). One list,
    /// because all three are the same thing to an operator reading the
    /// boot log: something about this store needs a human.
    pub refused: Vec<String>,
    pub opened: usize,
    pub seeded: usize,
    /// Members imported into an already-ready store.
    pub imported: usize,
    pub failed: Vec<String>,
    pub skipped_reason: Option<String>,
}

/// Spawn the job. Returns the task handle (tests await it; the daemon
/// drops it).
pub fn spawn_boot_job(state: SharedState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let st = state.clone();
        match tokio::task::spawn_blocking(move || run_boot(&st.review_stores, &st.store)).await {
            Ok(s) => tracing::info!(
                registered = s.registered.len(),
                refused = s.refused.len(),
                opened = s.opened,
                seeded = s.seeded,
                failed = s.failed.len(),
                swept_tmp = s.swept_tmp,
                reset_seeding = s.reset_seeding,
                skipped = ?s.skipped_reason,
                "review store boot job done"
            ),
            Err(e) => tracing::warn!(error = %e, "review store boot job panicked"),
        }
    })
}

/// The job body. Blocking.
pub fn run_boot(rs: &ReviewStores, store: &Store) -> BootSummary {
    let mut s = BootSummary::default();
    if let Some(d) = &rs.settings().disabled {
        s.skipped_reason = Some(d.clone());
        return s;
    }
    // Steps 1–2 (RS-U3) are CRASH RECOVERY, not store traffic: neither
    // `sweep_stale_tmp` nor `reset_interrupted_seeding` spawns git, reads
    // a ref, or touches a store directory's object database — both are a
    // `read_dir` and a `Store` write. They therefore run BEFORE the git
    // gate below, not behind it: a daemon whose store spawner is
    // unavailable (no `git` on PATH, spawn budget exhausted) would
    // otherwise skip them on EVERY boot, so a row a dead process left
    // `seeding` would wedge that store's actions forever with no boot log
    // naming the cause. The exclusion list is still THIS process's
    // in-flight ids (`ReviewStores::seeding_ids`, read at the same
    // instant relative to everything else — no seeding starts between
    // here and where this used to run, so the set is the same one it was),
    // so a seed genuinely in flight in this process is never reset here;
    // a boot is a recovery pass, not a reaper.
    s.swept_tmp = seed::sweep_stale_tmp(&rs.settings().root);
    s.reset_seeding = store
        .reset_interrupted_seeding(&rs.seeding_ids())
        .unwrap_or(0);

    if rs.git().is_none() {
        s.skipped_reason = Some("store git spawner unavailable".into());
        return s;
    }

    let with_reviews: BTreeSet<String> = store
        .repos_with_reviews()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut store_ids = BTreeSet::new();
    for name in rs
        .repos()
        .iter()
        .map(|r| r.name.clone())
        .filter(|n| with_reviews.contains(n))
        .collect::<Vec<_>>()
    {
        match rs.register_repo(store, &name, None) {
            Registration::Member {
                store_id,
                refused_remotes,
                ..
            } => {
                store_ids.insert(store_id);
                // A member's clone carrying a remote refused as unsafe
                // is a refusal too, and the ladder never runs for a repo
                // that is already a member — so this is the only place
                // the boot pass can see it. Counted, not swallowed.
                for r in refused_remotes {
                    s.refused.push(format!(
                        "{name}: {} on remote `{}` ({})",
                        super::ladder::REMOTE_URL_REFUSED,
                        r.name,
                        r.reason
                    ));
                }
                s.registered.push(name);
            }
            Registration::Refused { code, .. } => s.refused.push(format!("{name}: {code}")),
            Registration::Error { code, .. } => s.failed.push(format!("{name}: {code}")),
        }
    }

    for id in store_ids {
        let Ok(Some(row)) = store.get_review_store(id) else {
            continue;
        };
        match row.state.as_str() {
            "ready" => match rs.open(store, &row) {
                Ok(_) => {
                    s.opened += 1;
                    // Members that joined after the seed (BLOCKER 1).
                    // One that fails to import is a failure of THIS
                    // store's import pass, not a silently missing member,
                    // and the boot job reports through `failed` — the
                    // same list every store-level refusal below lands
                    // in, which the boot log counts.
                    match rs.import_pending_members(store, id) {
                        Ok(v) => {
                            s.imported += v.members.len();
                            for e in v.errors {
                                s.failed.push(format!("{}: {}", row.store_key, e));
                            }
                        }
                        Err(u) => s.failed.push(format!("{}: {}", row.store_key, u.code())),
                    }
                }
                // The directory vanished: `open` put the row back to
                // `absent`; re-seed it now rather than a boot later —
                // unless the row's LAST recorded state is a refused
                // credential, which is what the arm below is about.
                Err(super::registry::StoreUnavailable::Absent) if rs.settings().seed_on_boot => {
                    if let Some(why) = refused_credential(&row) {
                        s.refused
                            .push(format!("{}: {why} (not re-seeded)", row.store_key));
                        continue;
                    }
                    match rs.seed(store, id, false) {
                        Ok(_) => s.seeded += 1,
                        Err(u) => s.failed.push(format!("{}: {}", row.store_key, u.code())),
                    }
                }
                Err(u) => s.failed.push(format!("{}: {}", row.store_key, u.code())),
            },
            // A boot seed is LOCAL ONLY (D4): no credential, no network.
            // So it cannot find out whether the credential that stopped
            // the last attempt has been fixed — and re-seeding anyway
            // brought the row straight back to `ready` with
            // `base: offline-seed`, over a base nobody fetched and a
            // credential still broken: the exact state the refused arm
            // in `registry::seed` exists to prevent, undone by the next
            // boot. The recorded refusal is therefore honoured: the row
            // stays `absent` with the class on it, and an explicit
            // `store sync` (which does resolve a credential) re-seeds it
            // the moment the operator has fixed it.
            "absent" if rs.settings().seed_on_boot => match refused_credential(&row) {
                Some(why) => s
                    .refused
                    .push(format!("{}: {why} (not re-seeded)", row.store_key)),
                None => match rs.seed(store, id, false) {
                    Ok(_) => s.seeded += 1,
                    Err(u) => s.failed.push(format!("{}: {}", row.store_key, u.code())),
                },
            },
            _ => {}
        }
    }
    s
}

/// The credential class this store's LAST attempt was refused on, read
/// off the row the refusal left behind — or `None` for a row that was
/// never refused on one (including the `seed-failed` codes, which name
/// no class at all).
///
/// `FailureClass::from_slug` + `is_auth` is the same pairing
/// `registry::seed` refuses on and the CLI reads off the wire, so
/// "refused on a credential" is defined in exactly one place and this
/// pass cannot drift from the write it is honouring. A store whose
/// refusal is NOT a credential fault (offline, timeout — transient, by
/// design) keeps the local re-seed: a boot seed needs no network, so
/// there is nothing about it a stale network failure should block.
/// The credential-refusal class recorded on a store row, if the row is
/// `absent` because a credential the operator SUPPLIED was refused (as
/// opposed to simply never having been seeded). Shared with the doctor
/// card so an operator sees the class where they look, not only in the
/// boot log.
pub(crate) fn refused_credential(row: &crate::store::ReviewStoreRow) -> Option<String> {
    use super::classify::FailureClass;
    let code = super::registry::state_code(row.state_json.as_deref())?;
    FailureClass::from_slug(&code)
        .filter(|c| c.is_auth())
        .map(|c| c.slug().to_string())
}
