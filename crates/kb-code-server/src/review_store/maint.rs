//! RS-U9 — store maintenance, scheduled GC, backup bundles and the restore
//! guard (README §5.4/§8, design-internal-store.md §8, plan doc "RS-U9").
//!
//! Four pieces, one file per the build plan's architecture table:
//!
//! * **Scheduled git housekeeping**, per store, on three cadences (README
//!   §5.4): daily (`git maintenance run` over `loose-objects`/
//!   `commit-graph`/`pack-refs`, plus a sweep of stale
//!   `objects/pack/tmp_pack_*`), weekly (`repack --geometric=2 -d
//!   --write-midx`) and monthly (`repack --cruft …` + `reflog expire
//!   --expire=14.days`). [`due_tasks`] is the pure scheduler; each store's
//!   `review_stores.state_json.last_maint` records the last run of each
//!   cadence, so restarts never re-run a cadence early.
//! * **Scheduled store-wide GC** ([`super::gc`]'s engine, unchanged from
//!   RS-U5) plus the ps<n>/ps<n>-base ref INVARIANT check
//!   ([`super::seed::verify_connectivity`], reused verbatim — RS-U3/RS-U5
//!   already built the recreate-or-flag logic this unit schedules rather
//!   than reimplementing). Both run on the DAILY cadence (a design choice
//!   — see "Cadence choices" below), guarded by the restore guard.
//! * **Backup bundles** — `backups/store-<uuid>-<ts>.bundle`
//!   (`refs/kbc/*` minus what `refs/remotes/base/*` reaches), written on a
//!   gated-epoch snapshot, `kb-code backup`, or a detected restore; the
//!   last 3 per store are kept.
//! * **The restore guard** — [`restore_guard`]'s epoch-rollback detector
//!   plus a manual flag primitive; while flagged, scheduled GC computes
//!   its candidate list but never calls [`super::gc::apply`], until an
//!   operator runs `kb-code store gc --repo R --yes` (which both applies
//!   and clears the flag).
//!
//! # Cadence choices (not pinned by the source docs — recorded here)
//!
//! README §5.4 pins the THREE git-housekeeping cadences exactly (daily/
//! weekly/monthly, each with its own task list) but says nothing about
//! when the STORE-WIDE GC or the ref invariant check should run — they are
//! listed as a separate bullet with no cadence of their own. This module
//! runs both on the DAILY pass, right after the daily git-maintenance
//! tasks, for two reasons: (1) [`super::seed::verify_connectivity`] is
//! already called on every seed/sync/import — it is cheap (a
//! `cat-file --batch-check` over patchset shas plus, at most, one
//! `update-ref --stdin` recreating missing ps refs) and safe to run daily;
//! (2) [`super::gc::apply`] only ever DELETES refs, never objects — the
//! objects an orphaned ref stops protecting are not actually reclaimed
//! until the MONTHLY cruft-repack's own `--cruft-expiration=2.weeks.ago`
//! window passes, so running ref GC daily costs nothing in safety margin
//! and shortens how long a stray ref survives. A future unit that finds a
//! reason to split them onto their own cadence can without a schema
//! change — `last_maint` already has independent daily/weekly/monthly
//! slots and GC's own `last_gc` is a fourth, separate key.
//!
//! # Restore-guard design (a build-time decision — the source docs name
//! two possible mechanisms, "the backup receipt/marker or a restore
//! flag", without picking one)
//!
//! [`restore_guard::observe_boot_epoch`] is the AUTOMATIC detector: it
//! persists, in a sentinel file OUTSIDE the sqlite volume (so restoring
//! `index.db` alone can never also roll this back — the same asymmetry
//! `backup.marker` relies on), the highest schema epoch any earlier boot
//! of this daemon has observed. A boot whose PRE-MIGRATION epoch is lower
//! than that high-water mark can only mean the operator restored an older
//! snapshot (refinery only ever migrates forward, so a normal restart's
//! epoch never regresses) — that boot flags the guard.
//! [`restore_guard::flag_manual`] is the extensibility point the design
//! text's "or a restore flag" names: any future restore tooling (a
//! `store restore --bundle` verb is explicitly OUT of RS-U9's scope, see
//! the unit's hand-off note) can flag the guard directly without going
//! through an epoch change at all — the more common real-world case,
//! since [`crate::backup::GATED_EPOCHS`] only fires on the rare schema
//! migration. Both mechanisms write the SAME sentinel and are read the
//! same way, so a later unit can wire the manual flag into a real
//! restore verb without touching this module again.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::git::{GitArgs, GitCall, StoreGit, StoreGitError};
use super::registry::ReviewStores;
use super::seed::{ExpectedPatchset, SeedMember};
use crate::state::SharedState;
use crate::store::{ReviewStoreRow, Store};

/// Local plumbing timeout for every maintenance git call (repack/cruft on
/// a large store can run for a while; still bounded — see [`super::git`]'s
/// own `StoreGit::run` contract for the process-group kill on expiry).
pub const MAINT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// `objects/pack/tmp_pack_*` older than this are a crashed/interrupted
/// pack-writer's leftovers (README §5.4, design-internal-store.md §8: "a
/// sweep of `objects/pack/tmp_pack_*` older than 1 hour" — a live
/// `pack-objects`/`index-pack` still writing one is always younger).
pub const STALE_TMP_PACK_AGE: Duration = Duration::from_secs(3600);

/// How many `store-<uuid>-*.bundle` files are kept per store (README §5.4:
/// "The last 3 are kept.").
pub const BUNDLES_KEPT: usize = 3;

// ── cadence scheduling (pure) ──────────────────────────────────────────

pub const DAY_SECS: i64 = 24 * 3600;
pub const WEEK_SECS: i64 = 7 * DAY_SECS;
pub const MONTH_SECS: i64 = 30 * DAY_SECS;

/// One of the three git-housekeeping cadences (README §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaintTask {
    Daily,
    Weekly,
    Monthly,
}

impl MaintTask {
    pub const ALL: [MaintTask; 3] = [MaintTask::Daily, MaintTask::Weekly, MaintTask::Monthly];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }

    fn period_secs(self) -> i64 {
        match self {
            Self::Daily => DAY_SECS,
            Self::Weekly => WEEK_SECS,
            Self::Monthly => MONTH_SECS,
        }
    }
}

/// `review_stores.state_json.last_maint` — the timestamp each cadence last
/// ran, per store. `None` = never run (always due).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastMaint {
    pub daily: Option<i64>,
    pub weekly: Option<i64>,
    pub monthly: Option<i64>,
}

impl LastMaint {
    /// Read `state_json.last_maint`; a missing/malformed key reads as
    /// "never run" (every cadence due), never an error — a maintenance
    /// scheduler that refuses to schedule on a parse hiccup is worse than
    /// one that runs a cadence slightly early.
    pub fn from_state_json(v: Option<&serde_json::Value>) -> Self {
        v.and_then(|v| v.get("last_maint"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    fn get(self, t: MaintTask) -> Option<i64> {
        match t {
            MaintTask::Daily => self.daily,
            MaintTask::Weekly => self.weekly,
            MaintTask::Monthly => self.monthly,
        }
    }

    fn set(&mut self, t: MaintTask, at: i64) {
        match t {
            MaintTask::Daily => self.daily = Some(at),
            MaintTask::Weekly => self.weekly = Some(at),
            MaintTask::Monthly => self.monthly = Some(at),
        }
    }
}

/// Pure: which cadences are due, ascending (daily, weekly, monthly), given
/// `now` and the LAST time each ran. A cadence that has never run is
/// always due. `now < last` (a clock that moved backward) is treated the
/// same as "not yet due" — never a negative-duration panic, never a
/// spurious immediate re-run.
pub fn due_tasks(now: i64, last: LastMaint) -> Vec<MaintTask> {
    MaintTask::ALL
        .into_iter()
        .filter(|&t| match last.get(t) {
            None => true,
            Some(l) => now >= l && now - l >= t.period_secs(),
        })
        .collect()
}

// ── restore guard ──────────────────────────────────────────────────────

pub mod restore_guard {
    //! See the parent module's doc ("Restore-guard design") for the full
    //! rationale. This submodule is deliberately small: read/write one
    //! sentinel file, plus the two ways it gets set and the one way it
    //! gets cleared.

    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};

    pub const SCHEMA: &str = "kbc-restore-guard/1";

    fn schema_default() -> String {
        SCHEMA.to_string()
    }

    /// The sentinel's on-disk (and wire) shape. `just_flagged` is NEVER
    /// persisted (`#[serde(skip)]`) — it is a per-call signal, true only
    /// on the [`observe_boot_epoch`]/[`flag_manual`] call that JUST set
    /// `flagged`, never on a later read that finds it still set. Callers
    /// use it to decide whether to ALSO take a fresh bundle-backup pass
    /// right now (README §8's "or a restore").
    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    pub struct RestoreGuardState {
        #[serde(default = "schema_default")]
        pub schema: String,
        pub high_water_epoch: Option<u32>,
        #[serde(default)]
        pub flagged: bool,
        pub flagged_at: Option<i64>,
        pub flagged_reason: Option<String>,
        pub cleared_at: Option<i64>,
        #[serde(skip)]
        pub just_flagged: bool,
    }

    /// `<state>/review-store-restore-guard.json` for a given state dir —
    /// the helper non-daemon callers (`Store::open`'s boot hook,
    /// `kb-code backup`) use; daemon-context callers already have this
    /// path cached on `StoreSettings::restore_guard_path`.
    pub fn path_for(state_dir: &Path) -> PathBuf {
        state_dir.join(super::super::settings::RESTORE_GUARD_FILE)
    }

    /// Read the sentinel at `guard_path`. Missing or corrupt reads as the
    /// all-`None`/unflagged default — the guard degrades to "never
    /// flagged" rather than erroring, matching [`LastMaint`]'s posture on
    /// a parse failure (`super::LastMaint`).
    pub fn read(guard_path: &Path) -> RestoreGuardState {
        std::fs::read_to_string(guard_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write(guard_path: &Path, state: &RestoreGuardState) -> std::io::Result<()> {
        if let Some(parent) = guard_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = guard_path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(state)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, guard_path)
    }

    /// The automatic detector: compare the volume's PRE-MIGRATION epoch at
    /// THIS boot against the highest epoch any earlier boot of this
    /// daemon (on this box) ever observed. A volume whose epoch just went
    /// BACKWARD relative to the high-water mark flags the guard (refinery
    /// only migrates forward, so that can only mean a restore); a volume
    /// at or above the mark bumps it (never lowers it) and leaves any
    /// EXISTING flag untouched — only [`acknowledge`] clears one. Call
    /// once per boot, before the migration runner touches the epoch.
    pub fn observe_boot_epoch(
        guard_path: &Path,
        current_epoch: Option<u32>,
        now: i64,
    ) -> RestoreGuardState {
        let mut state = read(guard_path);
        state.just_flagged = false;
        if let Some(cur) = current_epoch {
            match state.high_water_epoch {
                Some(hw) if cur < hw => {
                    if !state.flagged {
                        state.flagged = true;
                        state.flagged_at = Some(now);
                        state.flagged_reason = Some(format!(
                            "volume epoch regressed ({cur} < previously observed {hw}); the \
                             only known cause is a restored (older) sqlite snapshot"
                        ));
                        state.just_flagged = true;
                    }
                }
                Some(hw) if cur > hw => state.high_water_epoch = Some(cur),
                Some(_) => {}
                None => state.high_water_epoch = Some(cur),
            }
        }
        if let Err(e) = write(guard_path, &state) {
            tracing::warn!(
                error = %e,
                path = %guard_path.display(),
                "kb-code: could not persist the review-store restore guard"
            );
        }
        state
    }

    /// The extensibility point named in the design text's "or a restore
    /// flag": anything that knows a restore just happened by some means
    /// OTHER than an epoch rollback can flag the guard directly. Not
    /// wired to any route or CLI verb in RS-U9 (no restore-bundle verb
    /// ships yet — see the unit's hand-off note); exists so a later unit
    /// can call it without touching this module.
    pub fn flag_manual(
        guard_path: &Path,
        reason: &str,
        now: i64,
    ) -> std::io::Result<RestoreGuardState> {
        let mut state = read(guard_path);
        state.flagged = true;
        state.flagged_at = Some(now);
        state.flagged_reason = Some(reason.to_string());
        write(guard_path, &state)?;
        state.just_flagged = true;
        Ok(state)
    }

    /// The operator's explicit `kb-code store gc --repo R --yes`
    /// acknowledgement: clears the flag so the SCHEDULED pass may apply
    /// again. A no-op (but never an error) when nothing was flagged.
    pub fn acknowledge(guard_path: &Path, now: i64) -> std::io::Result<RestoreGuardState> {
        let mut state = read(guard_path);
        if state.flagged {
            state.flagged = false;
            state.cleared_at = Some(now);
            write(guard_path, &state)?;
        }
        Ok(state)
    }
}

// ── git-housekeeping tasks ─────────────────────────────────────────────

/// Daily: `git maintenance run` over loose-objects/commit-graph/pack-refs
/// (README §5.4), plus the stale-tmp_pack sweep. Returns how many stale
/// tmp packs were swept.
pub fn run_daily(git: &StoreGit, git_dir: &Path) -> Result<usize, StoreGitError> {
    git.run(
        GitCall::new(
            "maintenance-daily",
            GitArgs::new("maintenance")
                .flag("run")
                .flag("--task=loose-objects")
                .flag("--task=commit-graph")
                .flag("--task=pack-refs")
                .flag("--quiet"),
        )
        .git_dir(git_dir)
        .timeout(MAINT_TIMEOUT),
    )?;
    Ok(sweep_stale_pack_tmp(git_dir))
}

/// Weekly: `repack --geometric=2 -d --write-midx` (README §5.4).
pub fn run_weekly(git: &StoreGit, git_dir: &Path) -> Result<(), StoreGitError> {
    git.run(
        GitCall::new(
            "maintenance-weekly",
            GitArgs::new("repack")
                .flag("--geometric=2")
                .flag("-d")
                .flag("--write-midx")
                .flag("--quiet"),
        )
        .git_dir(git_dir)
        .timeout(MAINT_TIMEOUT),
    )?;
    Ok(())
}

/// Monthly: `repack --cruft -d --geometric=2 --cruft-expiration=2.weeks.ago`
/// plus `reflog expire --expire=14.days --all` (README §5.4,
/// design-internal-store.md §8's fuller flag list). `--all` (not in the
/// README's shorthand) is a deliberate build-time addition: without it
/// `git reflog expire` only ever touches the ref(s) named on its command
/// line, and a bare store has no branch reflog of its own to default to —
/// omitting it would make this call a byte-identical no-op.
pub fn run_monthly(git: &StoreGit, git_dir: &Path) -> Result<(), StoreGitError> {
    git.run(
        GitCall::new(
            "maintenance-monthly-cruft",
            GitArgs::new("repack")
                .flag("--cruft")
                .flag("-d")
                .flag("--geometric=2")
                .flag("--cruft-expiration=2.weeks.ago")
                .flag("--quiet"),
        )
        .git_dir(git_dir)
        .timeout(MAINT_TIMEOUT),
    )?;
    git.run(
        GitCall::new(
            "maintenance-monthly-reflog",
            GitArgs::new("reflog")
                .flag("expire")
                .flag("--expire=14.days")
                .flag("--all"),
        )
        .git_dir(git_dir)
        .timeout(MAINT_TIMEOUT),
    )?;
    Ok(())
}

/// The one-shot consolidation RS-U3's follow-up note calls for: after a
/// MULTI-member seed, the store has one pack per imported member (each
/// `import_member` fetch writes its own pack) — repack it down to one.
/// Safe as a full `repack -a -d` (unlike the old hardlinked-seed-pack
/// design, README §5.2 seeds by FETCH now, so there is no shared pack to
/// preserve — README §5.4: "Full `gc` is fine now, since there are no seed
/// packs to preserve.").
pub fn repack_full(git: &StoreGit, git_dir: &Path) -> Result<(), StoreGitError> {
    git.run(
        GitCall::new(
            "repack-full",
            GitArgs::new("repack").flag("-a").flag("-d").flag("--quiet"),
        )
        .git_dir(git_dir)
        .timeout(MAINT_TIMEOUT),
    )?;
    Ok(())
}

/// Remove `objects/pack/tmp_pack_*` older than [`STALE_TMP_PACK_AGE`] — a
/// crashed pack-writer's leftovers (mirrors [`super::seed::sweep_stale_tmp`]'s
/// shape for a different location/pattern). Returns how many were removed.
pub fn sweep_stale_pack_tmp(git_dir: &Path) -> usize {
    sweep_pack_tmp_older_than(git_dir, STALE_TMP_PACK_AGE)
}

/// [`sweep_stale_pack_tmp`]'s body, parameterized on the age threshold so
/// a test can pass [`Duration::ZERO`] (sweep everything already on disk)
/// without manipulating file mtimes.
fn sweep_pack_tmp_older_than(git_dir: &Path, max_age: Duration) -> usize {
    let dir = git_dir.join("objects").join("pack");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let cutoff = std::time::SystemTime::now().checked_sub(max_age);
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(s) = name.to_str() else { continue };
        if !s.starts_with("tmp_pack_") {
            continue;
        }
        let old_enough = match (cutoff, e.metadata().and_then(|m| m.modified())) {
            (Some(cutoff), Ok(mtime)) => mtime <= cutoff,
            _ => false,
        };
        if old_enough && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

// ── store-wide GC + the ref invariant (scheduled) ──────────────────────

/// The ps<n>/ps<n>-base ref invariant check's outcome (README §5.4: every
/// patchset tip has its ref, every non-null `base_tip_sha` has its
/// `-base` ref; a missing one is recreated from the objects present, or
/// the review is flagged `objects_state`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct InvariantReport {
    pub objects_missing: usize,
    pub objects_ok: usize,
    pub recovered_by_sha: usize,
    pub refs_recreated: usize,
}

/// Run [`super::seed::verify_connectivity`] and apply its `objects_state`
/// verdict to every review it looked at. `ops` is `None` when the caller
/// already holds the store's ops lock (a SECOND lock attempt on the same
/// `tokio::sync::Mutex` would deadlock — it is not reentrant).
pub fn invariant_check(
    store: &Store,
    git: &StoreGit,
    git_dir: &Path,
    members: &[SeedMember],
    patchsets: &[ExpectedPatchset],
) -> Result<InvariantReport, StoreGitError> {
    let (missing, ok, recovered, recreated) =
        super::seed::verify_connectivity(git, git_dir, members, patchsets, None)?;
    for id in &missing {
        let _ = store.set_review_objects_state(*id, Some(super::seed::OBJECTS_MISSING));
    }
    for id in &ok {
        if let Ok(Some(b)) = store.get_review_base(*id) {
            if b.objects_state.as_deref() == Some(super::seed::OBJECTS_MISSING) {
                let _ = store.set_review_objects_state(*id, None);
            }
        }
    }
    Ok(InvariantReport {
        objects_missing: missing.len(),
        objects_ok: ok.len(),
        recovered_by_sha: recovered,
        refs_recreated: recreated,
    })
}

/// One store-wide GC pass's outcome. `applied` is `false` for a dry run,
/// for a candidate-free pass, or when the restore guard blocked it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GcRunReport {
    pub candidates: usize,
    pub applied: bool,
    /// `"applied"` | `"dry-run"` | `"nothing-to-do"` | `"restore-guard"`.
    pub reason: &'static str,
}

/// Compute (and, when `apply` is true and nothing blocks it, perform) one
/// store-wide GC pass (README §5.4, [`super::gc`]'s engine — unchanged
/// from RS-U5). Pure classification plus, at most, ONE guarded
/// `update-ref --stdin` transaction; call under the store's ops lock.
#[allow(clippy::too_many_arguments)]
pub fn gc_pass(
    store: &Store,
    git: &StoreGit,
    git_dir: &Path,
    store_id: i64,
    registered_member_ids: &[i64],
    apply: bool,
) -> Result<GcRunReport, String> {
    let keep =
        super::gc::keep_set(store, store_id, registered_member_ids).map_err(|e| e.to_string())?;
    let refs = super::seed::list_refs(git, git_dir, &[]).map_err(|e| e.to_string())?;
    let attributed = super::gc::attribute(&refs, &keep);
    let candidates = super::gc::delete_candidates(&attributed);
    let n = candidates.len();
    let did_apply = apply && n > 0;
    if did_apply {
        super::gc::apply(git, git_dir, &candidates).map_err(|e| e.to_string())?;
    }
    let reason = if !apply {
        "dry-run"
    } else if n == 0 {
        "nothing-to-do"
    } else {
        "applied"
    };
    Ok(GcRunReport {
        candidates: n,
        applied: did_apply,
        reason,
    })
}

/// [`gather_store_scope`]'s result: everything a store-wide pass needs,
/// gathered with NO git calls (unlike [`ReviewStores::plan_for`], which
/// also reads each member's remotes — unneeded here and wasteful on a
/// routine pass). No member NAMES here on purpose: `keep_set` is
/// `store_id`-keyed since RS-U5's review fix (name-filtering a keep-set
/// is unsafe — see that function's own doc), and `registered_ids` is all
/// the invariant check + GC pass need beyond `patchsets`.
struct StoreScope {
    registered_ids: Vec<i64>,
    seed_members: Vec<SeedMember>,
    patchsets: Vec<ExpectedPatchset>,
}

fn gather_store_scope(
    rs: &ReviewStores,
    store: &Store,
    store_id: i64,
) -> Result<StoreScope, String> {
    let (members, _problems) = rs.members_of(store, store_id);
    // `Store::patchsets_for_repos` is still name-keyed (unlike
    // `gc::keep_set`) — a purely LOCAL name list, never stored or handed
    // to the keep-set query.
    let member_names: Vec<String> = members.iter().map(|(r, _)| r.name.clone()).collect();
    let registered_ids: Vec<i64> = members.iter().map(|(r, _)| r.id).collect();
    let seed_members: Vec<SeedMember> = members.into_iter().map(|(_, m)| m).collect();
    let patchsets: Vec<ExpectedPatchset> = store
        .patchsets_for_repos(&member_names)
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
    Ok(StoreScope {
        registered_ids,
        seed_members,
        patchsets,
    })
}

// ── backup bundles ──────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum MaintError {
    #[error(transparent)]
    Git(#[from] StoreGitError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum BundleOutcome {
    Written,
    Skipped { reason: &'static str },
}

/// `<backups>/store-<uuid>-<ts>.bundle`.
pub fn bundle_path(backups_dir: &Path, uuid: &str, ts: i64) -> PathBuf {
    backups_dir.join(format!("store-{uuid}-{ts}.bundle"))
}

/// Write ONE bundle: `refs/kbc/*` minus objects reachable from
/// `refs/remotes/base/*` (README §5.4 / design-internal-store.md §8 —
/// `git bundle create <f> --stdin` fed the kbc refs plus `^<base tips>`).
/// `Skipped { reason: "no-refs" }` when the store has no `refs/kbc/*` yet
/// (a freshly seeded store with no reviews) — `git bundle create` refuses
/// an empty ref list, and an empty bundle is not a useful backup anyway.
pub fn write_bundle(
    git: &StoreGit,
    git_dir: &Path,
    dest: &Path,
) -> Result<BundleOutcome, MaintError> {
    let kbc_refs = super::seed::list_refs(git, git_dir, &["refs/kbc/"])?;
    if kbc_refs.is_empty() {
        return Ok(BundleOutcome::Skipped { reason: "no-refs" });
    }
    let base_refs = super::seed::list_refs(git, git_dir, &["refs/remotes/base/"])?;
    let mut stdin = String::new();
    for (_, name) in &kbc_refs {
        stdin.push_str(name);
        stdin.push('\n');
    }
    for (_, name) in &base_refs {
        stdin.push('^');
        stdin.push_str(name);
        stdin.push('\n');
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let args = GitArgs::new("bundle")
        .flag("create")
        .flag("--stdin")
        .flag("--quiet")
        .end_of_options()
        .abs_path(dest)
        .map_err(|e| MaintError::Other(e.to_string()))?;
    git.run(
        GitCall::new("bundle-create", args)
            .git_dir(git_dir)
            .stdin(stdin.into_bytes())
            .timeout(MAINT_TIMEOUT),
    )?;
    Ok(BundleOutcome::Written)
}

/// Keep the newest `keep` `store-<uuid>-*.bundle` files under
/// `backups_dir`, removing the rest. Returns the removed paths.
pub fn prune_bundles(backups_dir: &Path, uuid: &str, keep: usize) -> std::io::Result<Vec<PathBuf>> {
    let prefix = format!("store-{uuid}-");
    let rd = match std::fs::read_dir(backups_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut found: Vec<(i64, PathBuf)> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(s) = name.to_str() else { continue };
        let Some(rest) = s.strip_prefix(&prefix) else {
            continue;
        };
        let Some(ts_str) = rest.strip_suffix(".bundle") else {
            continue;
        };
        let Ok(ts) = ts_str.parse::<i64>() else {
            continue;
        };
        found.push((ts, entry.path()));
    }
    found.sort_by_key(|b| std::cmp::Reverse(b.0));
    let mut removed = Vec::new();
    for (_, path) in found.into_iter().skip(keep) {
        if std::fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    Ok(removed)
}

/// `<state>/backups/`.
pub fn backups_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(super::settings::BACKUPS_DIR_NAME)
}

/// One store's bundle-backup outcome, for [`BackupAllReport`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct StoreBundleOutcome {
    pub uuid: String,
    pub written: Option<PathBuf>,
    pub pruned: Vec<PathBuf>,
    pub skipped_reason: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BackupAllReport {
    pub stores: Vec<StoreBundleOutcome>,
}

fn review_stores_table_exists(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'review_stores'",
        [],
        |r| r.get(0),
    )
    .unwrap_or(false)
}

/// Bundle-backup EVERY `ready` store on `conn`'s volume. Skips (returns an
/// empty report) when the `review_stores` table does not exist yet — the
/// FIRST V0045 crossing runs before that migration has landed, so there is
/// nothing to back up (README §10 step 1: "There is no git I/O at boot,
/// and all stores start `absent`"). Best-effort per store: one store's
/// failure never stops the rest.
pub fn backup_all_ready_stores(
    conn: &rusqlite::Connection,
    git: &StoreGit,
    state_dir: &Path,
    now: i64,
) -> BackupAllReport {
    let mut report = BackupAllReport::default();
    if !review_stores_table_exists(conn) {
        return report;
    }
    let rows: Vec<(String, String)> = match conn
        .prepare("SELECT uuid, git_dir FROM review_stores WHERE state = 'ready' ORDER BY id")
        .and_then(|mut stmt| {
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        }) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "kb-code: review-store backup: could not list ready stores");
            return report;
        }
    };
    let dest_dir = backups_dir(state_dir);
    for (uuid, git_dir) in rows {
        let mut outcome = StoreBundleOutcome {
            uuid: uuid.clone(),
            ..Default::default()
        };
        let dir = Path::new(&git_dir);
        let dest = bundle_path(&dest_dir, &uuid, now);
        match write_bundle(git, dir, &dest) {
            Ok(BundleOutcome::Written) => {
                outcome.written = Some(dest);
                match prune_bundles(&dest_dir, &uuid, BUNDLES_KEPT) {
                    Ok(p) => outcome.pruned = p,
                    Err(e) => outcome.error = Some(format!("prune: {e}")),
                }
            }
            Ok(BundleOutcome::Skipped { reason }) => {
                outcome.skipped_reason = Some(reason.to_string())
            }
            Err(e) => outcome.error = Some(e.to_string()),
        }
        report.stores.push(outcome);
    }
    report
}

/// `kb-code backup`'s entry point (README §5.4/§8: "on ... `kb-code
/// backup`"). Opens its OWN connection to `db_path` — `kb-code-cli`
/// carries no direct `rusqlite` dependency (see `default_kb_code_db`'s
/// doc in `kb-code-cli/src/main.rs`), and sqlite/WAL supports true
/// concurrent readers, so this is safe even while the daemon has the same
/// file open. `state_dir` is always `db_path`'s parent — the same
/// directory `backup.marker`/`index.db` live in.
pub fn backup_all_ready_stores_at(db_path: &Path) -> Result<BackupAllReport, String> {
    let conn = rusqlite::Connection::open(db_path).map_err(|e| e.to_string())?;
    let state_dir = db_path.parent().unwrap_or_else(|| Path::new("."));
    let git_home = state_dir.join(super::settings::GIT_HOME_DIR);
    let git = StoreGit::new(&git_home).map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp();
    Ok(backup_all_ready_stores(&conn, &git, state_dir, now))
}

// ── per-store scheduled pass (daily/weekly/monthly + GC + invariant) ───

/// One scheduled (or manually forced) maintenance pass over one store.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MaintPassReport {
    pub store_id: i64,
    pub uuid: String,
    pub tasks_run: Vec<&'static str>,
    pub swept_tmp_pack: usize,
    pub invariant: Option<InvariantReport>,
    pub gc: Option<GcRunReport>,
    pub restore_guard_flagged: bool,
    pub errors: Vec<String>,
}

fn state_json_value(row: &ReviewStoreRow) -> serde_json::Value {
    row.state_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Run every cadence in `tasks` (plus, on [`MaintTask::Daily`], the
/// invariant check and a store-wide GC pass — see the module doc's
/// "Cadence choices") against ONE store, then persist `last_maint`/
/// `last_gc` into `state_json`. Call under `rs.ops_lock(row.id)` — this
/// function does NOT take the lock itself (the scheduler and the
/// `store maintain` route hold it for the whole pass, README §5.4's
/// "under the store ops lock").
pub fn run_pass_for_store(
    rs: &ReviewStores,
    store: &Store,
    row: &ReviewStoreRow,
    tasks: &[MaintTask],
    gc_apply_requested: bool,
    now: i64,
) -> MaintPassReport {
    let mut report = MaintPassReport {
        store_id: row.id,
        uuid: row.uuid.clone(),
        ..Default::default()
    };
    let Some(git) = rs.git() else {
        report.errors.push("store git spawner unavailable".into());
        return report;
    };
    let dir = Path::new(&row.git_dir);
    let guard = restore_guard::read(&rs.settings().restore_guard_path);
    report.restore_guard_flagged = guard.flagged;

    if tasks.contains(&MaintTask::Daily) {
        match run_daily(git, dir) {
            Ok(swept) => {
                report.tasks_run.push("daily");
                report.swept_tmp_pack = swept;
            }
            Err(e) => report.errors.push(format!("daily: {e}")),
        }
        match gather_store_scope(rs, store, row.id) {
            Ok(scope) => {
                match invariant_check(store, git, dir, &scope.seed_members, &scope.patchsets) {
                    Ok(inv) => report.invariant = Some(inv),
                    Err(e) => report.errors.push(format!("invariant: {e}")),
                }
                // The restore guard blocks APPLY only — the dry-run scan
                // still runs and is still reported, so a flagged store is
                // never silent about what GC would have done.
                let apply = gc_apply_requested && !guard.flagged;
                match gc_pass(store, git, dir, row.id, &scope.registered_ids, apply) {
                    Ok(mut g) => {
                        if gc_apply_requested && guard.flagged {
                            g.reason = "restore-guard";
                        }
                        report.gc = Some(g);
                    }
                    Err(e) => report.errors.push(format!("gc: {e}")),
                }
            }
            Err(e) => report.errors.push(format!("scope: {e}")),
        }
    }
    if tasks.contains(&MaintTask::Weekly) {
        match run_weekly(git, dir) {
            Ok(()) => report.tasks_run.push("weekly"),
            Err(e) => report.errors.push(format!("weekly: {e}")),
        }
    }
    if tasks.contains(&MaintTask::Monthly) {
        match run_monthly(git, dir) {
            Ok(()) => report.tasks_run.push("monthly"),
            Err(e) => report.errors.push(format!("monthly: {e}")),
        }
    }

    let mut sj = state_json_value(row);
    let mut last = LastMaint::from_state_json(Some(&sj));
    for t in report.tasks_run.iter().copied() {
        if let Some(task) = MaintTask::parse(t) {
            last.set(task, now);
        }
    }
    sj["last_maint"] = serde_json::to_value(last).unwrap_or_default();
    if let Some(g) = &report.gc {
        sj["last_gc"] = serde_json::json!({
            "at": now,
            "candidates": g.candidates,
            "applied": g.applied,
            "reason": g.reason,
        });
    }
    let _ = store.set_review_store_state(row.id, &row.state, Some(&sj.to_string()));
    report
}

/// `kb-code store gc --repo R [--yes]`'s core (route + CLI share this).
/// `bypass_guard` is true ONLY for an explicit operator `--yes` — that is
/// the acknowledgement the restore guard is waiting for, and a successful
/// apply while flagged clears it.
pub fn run_gc_now(
    rs: &ReviewStores,
    store: &Store,
    row: &ReviewStoreRow,
    apply_requested: bool,
    bypass_guard: bool,
    now: i64,
) -> Result<GcRunReport, String> {
    let Some(git) = rs.git() else {
        return Err("store git spawner unavailable".into());
    };
    let dir = Path::new(&row.git_dir);
    let guard_path = &rs.settings().restore_guard_path;
    let guard = restore_guard::read(guard_path);
    let blocked = guard.flagged && !bypass_guard;
    let apply = apply_requested && !blocked;
    // Only `registered_ids` is needed here (`keep_set` is `store_id`-keyed
    // since RS-U5's review fix) — no `gather_store_scope` (which also
    // computes patchsets this pass never reads).
    let registered_ids = store.store_members(row.id).map_err(|e| e.to_string())?;
    let mut report = gc_pass(store, git, dir, row.id, &registered_ids, apply)?;
    if apply_requested && blocked {
        report.reason = "restore-guard";
    }
    if apply && guard.flagged && bypass_guard {
        if let Err(e) = restore_guard::acknowledge(guard_path, now) {
            tracing::warn!(error = %e, "kb-code: could not clear the restore guard after an acknowledged gc --yes");
        }
    }
    let mut sj = state_json_value(row);
    sj["last_gc"] = serde_json::json!({
        "at": now,
        "candidates": report.candidates,
        "applied": report.applied,
        "reason": report.reason,
    });
    let _ = store.set_review_store_state(row.id, &row.state, Some(&sj.to_string()));
    Ok(report)
}

// ── background scheduler ────────────────────────────────────────────────

/// The periodic pass' polling interval. Cadences due are computed against
/// wall-clock time ([`due_tasks`]), so the exact tick granularity only
/// bounds how LATE a due cadence can start running — an hour is generous
/// against day-granularity cadences.
pub const SCHEDULER_TICK: Duration = Duration::from_secs(3600);
/// Upper bound on the random jitter added before each tick's work, so
/// many stores (and, on a box running several kb-code daemons, many
/// PROCESSES) don't all hit disk on the exact same wall-clock second.
pub const SCHEDULER_JITTER: Duration = Duration::from_secs(5 * 60);

fn jitter(max: Duration) -> Duration {
    let max_ms = max.as_millis().min(u128::from(u64::MAX)) as u64;
    if max_ms == 0 {
        return Duration::ZERO;
    }
    let mut b = [0u8; 8];
    let ms = if getrandom::fill(&mut b).is_ok() {
        u64::from_le_bytes(b) % max_ms
    } else {
        0
    };
    Duration::from_millis(ms)
}

/// One pass over every `ready` store: compute due cadences, run them.
/// Blocking — call from `spawn_blocking`.
pub fn run_scheduler_pass(rs: &ReviewStores, store: &Store, now: i64) -> Vec<MaintPassReport> {
    let rows = match store.list_review_stores() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "kb-code: review-store maintenance: could not list stores");
            return vec![];
        }
    };
    let mut out = Vec::new();
    for row in rows.into_iter().filter(|r| r.state == "ready") {
        let state_json_val: Option<serde_json::Value> = row
            .state_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        let last = LastMaint::from_state_json(state_json_val.as_ref());
        let due = due_tasks(now, last);
        if due.is_empty() {
            continue;
        }
        // README §5.4/task text: "under the store ops lock". `blocking_lock`
        // is safe here — this function is only ever called from a
        // `spawn_blocking` closure (the scheduler worker below, or a test).
        let ops = rs.ops_lock(row.id);
        let _guard = ops.blocking_lock();
        let report = run_pass_for_store(rs, store, &row, &due, true, now);
        if !report.errors.is_empty() {
            tracing::warn!(store = %row.store_key, errors = ?report.errors, "kb-code: review-store maintenance pass had errors");
        }
        out.push(report);
    }
    out
}

/// Spawn the background scheduler. Spawned UNCONDITIONALLY, mirroring
/// `doclens::sync::spawn_doclens_sync_worker`'s own shape (never on the
/// boot critical path — this is `tokio::spawn`, not awaited by boot).
pub fn spawn_maintenance_worker(state: SharedState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if state.review_stores.settings().disabled.is_some() {
            tracing::info!("kb-code: review-store maintenance scheduler idle (store disabled)");
            return;
        }
        tokio::time::sleep(jitter(SCHEDULER_JITTER)).await;
        let mut ticker = tokio::time::interval(SCHEDULER_TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // burn the immediate first tick
        let shutdown = crate::shutdown_signal();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    tokio::time::sleep(jitter(SCHEDULER_JITTER)).await;
                    let st = state.clone();
                    let now = chrono::Utc::now().timestamp();
                    let _ = tokio::task::spawn_blocking(move || {
                        run_scheduler_pass(&st.review_stores, &st.store, now)
                    })
                    .await;
                }
                _ = &mut shutdown => {
                    tracing::info!("kb-code: review-store maintenance scheduler stopping (shutdown)");
                    break;
                }
            }
        }
    })
}

// ── HTTP routes (loopback-only; registered in router.rs) ───────────────

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

/// Same audited-mutation shape as `review_store::routes::audit` (README
/// §8: "key and credential endpoints are loopback-only and audited"; this
/// unit extends the same posture to `store/gc` and `store/maintain`).
fn audit(route: &'static str, repo: &str, status: StatusCode) {
    tracing::info!(
        target: "kb_code::audit",
        route,
        repo,
        status = status.as_u16(),
        "review store maintenance mutation"
    );
}

/// `Box`ed error per `clippy::result_large_err` — a bare `axum::http::
/// Response<Body>` is 128+ bytes, too large to carry unboxed in a
/// `Result` (`reviews::normalize_report_shape`'s own doc names the same
/// convention this crate's lint gate already enforces elsewhere).
fn store_row_for(state: &SharedState, name: &str) -> Result<ReviewStoreRow, Box<Response>> {
    match state.review_stores.handle_for_repo(&state.store, name) {
        Ok(h) => state
            .store
            .get_review_store(h.id)
            .map_err(internal)?
            .ok_or_else(|| Box::new(internal("store row vanished between open and read"))),
        Err(u) => Err(Box::new(super::registry::StoreRefusal(u).into_response())),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct GcBody {
    #[serde(default)]
    pub yes: bool,
}

/// `POST /api/repos/{name}/store/gc` (loopback-only). Default (no `yes`)
/// is a DRY RUN — candidates computed, nothing deleted. `yes: true` applies
/// (with old-value guards, README §5.4) and, if the restore guard was
/// flagged, acknowledges it.
pub async fn store_gc_route(
    State(state): State<SharedState>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<GcBody>,
) -> Response {
    let resp = store_gc_route_inner(state.clone(), name.clone(), body).await;
    audit("store_gc_route", &name, resp.status());
    resp
}

async fn store_gc_route_inner(state: SharedState, name: String, body: GcBody) -> Response {
    if state.review_stores.repo(&name).is_none() {
        return not_found(&name);
    }
    let st = state.clone();
    let n = name.clone();
    let res = tokio::task::spawn_blocking(move || {
        let row = store_row_for(&st, &n)?;
        let ops = st.review_stores.ops_lock(row.id);
        let _guard = ops.blocking_lock();
        let now = chrono::Utc::now().timestamp();
        run_gc_now(&st.review_stores, &st.store, &row, body.yes, body.yes, now)
            .map_err(internal)
            .map_err(Box::new)
    })
    .await;
    match res {
        Ok(Ok(report)) => Json(serde_json::json!({
            "schema": "kbc-store-gc/1",
            "repo": name,
            "report": report,
        }))
        .into_response(),
        Ok(Err(resp)) => *resp,
        Err(e) => internal(e),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct MaintainBody {
    /// `daily`|`weekly`|`monthly` — force exactly that cadence to run now,
    /// regardless of when it last ran. Omitted = whatever [`due_tasks`]
    /// says is due.
    pub task: Option<String>,
}

/// `POST /api/repos/{name}/store/maintain` (loopback-only).
pub async fn store_maintain_route(
    State(state): State<SharedState>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<MaintainBody>,
) -> Response {
    let resp = store_maintain_route_inner(state.clone(), name.clone(), body).await;
    audit("store_maintain_route", &name, resp.status());
    resp
}

async fn store_maintain_route_inner(
    state: SharedState,
    name: String,
    body: MaintainBody,
) -> Response {
    if state.review_stores.repo(&name).is_none() {
        return not_found(&name);
    }
    if let Some(t) = &body.task {
        if MaintTask::parse(t).is_none() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("unknown task `{t}` (want daily|weekly|monthly)"),
                    "type": "urn:kb:errors:bad-request",
                })),
            )
                .into_response();
        }
    }
    let st = state.clone();
    let n = name.clone();
    let res = tokio::task::spawn_blocking(move || {
        let row = store_row_for(&st, &n)?;
        let ops = st.review_stores.ops_lock(row.id);
        let _guard = ops.blocking_lock();
        let now = chrono::Utc::now().timestamp();
        let tasks: Vec<MaintTask> = match &body.task {
            Some(t) => vec![MaintTask::parse(t).expect("validated above")],
            None => {
                let state_json_val: Option<serde_json::Value> = row
                    .state_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok());
                due_tasks(now, LastMaint::from_state_json(state_json_val.as_ref()))
            }
        };
        Ok::<_, Box<Response>>(run_pass_for_store(
            &st.review_stores,
            &st.store,
            &row,
            &tasks,
            true,
            now,
        ))
    })
    .await;
    match res {
        Ok(Ok(report)) => Json(serde_json::json!({
            "schema": "kbc-store-maintain/1",
            "repo": name,
            "report": report,
        }))
        .into_response(),
        Ok(Err(resp)) => *resp,
        Err(e) => internal(e),
    }
}

#[cfg(test)]
#[path = "maint/tests.rs"]
mod tests;
