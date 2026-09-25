//! RS-U9 — store maintenance, scheduled GC, backup bundles and the restore
//! guard (README §5.4/§8, design-internal-store.md §8, plan doc "RS-U9").
//!
//! **Operator ruling (post-review, 2026-09-25 — supersedes this module's
//! first cut): the SCHEDULER never applies GC.** An independent review
//! found three data-loss-shaped gaps in "scheduled apply, guarded by a
//! sentinel" (see "Cadence choices" and "Restore-guard design" below for
//! the full accounting). [`run_pass_for_store`] (the scheduler, and
//! `store maintain`) ALWAYS computes the GC candidate list as a dry run
//! and records it (`state_json.last_gc_dry_run`); NEITHER ever reaches
//! [`super::gc::apply`]. That function has exactly ONE caller in the
//! crate — [`apply_gc_candidates`] — and that has exactly TWO entry
//! points, BOTH funnelling through [`run_gc_pass`]: the operator's
//! explicit `kb-code store gc --repo R --yes` ([`run_gc_now`], which is
//! what the `--yes` acknowledgement is for) and the legacy
//! `kb-code review refs gc --repo R --apply` route
//! ([`crate::reviews::gc_review_refs_inner`], which must NOT call
//! [`super::gc::apply`] itself — it hands its candidates to
//! [`run_gc_pass`]). Each takes the store's ops lock FIRST, re-checks
//! readiness and the restore guard UNDER it, runs the UNCONDITIONAL
//! (never `--yes`-bypassable) DB-truth check that no candidate names a
//! review id newer than this volume has ever assigned, and only then
//! takes the fresh bundle backup and applies.
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
//!   cadence (plus a per-cadence retry backoff after a failure, so a
//!   persistently-failing task is not retried every tick), so restarts
//!   never re-run a cadence early.
//! * **Scheduled store-wide GC, report-only** ([`super::gc`]'s engine,
//!   unchanged from RS-U5) plus the ps<n>/ps<n>-base ref INVARIANT check
//!   ([`super::seed::verify_connectivity`], reused verbatim — RS-U3/RS-U5
//!   already built the recreate-or-flag logic this unit schedules rather
//!   than reimplementing). Both run on the DAILY cadence (a design choice
//!   — see "Cadence choices" below).
//! * **Backup bundles** — `backups/store-<uuid>-<ts>.bundle`, written on a
//!   gated-epoch snapshot, `kb-code backup`, a detected restore, or
//!   immediately before every real apply; the last 3 per store are kept.
//!   The routine shape is `refs/kbc/*` minus what `refs/remotes/base/*`
//!   reaches ([`write_bundle`]). The PRE-APPLY shape is that PLUS every ref
//!   the apply is about to delete, whatever namespace it lives in
//!   ([`apply_gc_candidates`] → `write_bundle_covering`) — because GC's
//!   candidate set is not confined to `refs/kbc/*`. The invariant this
//!   buys, and the reason the extra refs are there: **no ref is ever
//!   deleted by an apply whose pre-apply bundle did not cover it.**
//! * **The restore guard** — [`restore_guard`]'s epoch-rollback detector
//!   plus a manual flag primitive, PER-STORE acknowledged (an operator
//!   acknowledging store R's restore suspicion via `gc --repo R --yes`
//!   never silently clears it for a DIFFERENT store S they have not
//!   looked at) — see "Restore-guard design" below.
//!
//! # Cadence choices (not pinned by the source docs — recorded here)
//!
//! README §5.4 pins the THREE git-housekeeping cadences exactly (daily/
//! weekly/monthly, each with its own task list) but says nothing about
//! when the STORE-WIDE GC or the ref invariant check should run — they are
//! listed as a separate bullet with no cadence of their own. This module
//! runs both on the DAILY pass, right after the daily git-maintenance
//! tasks: [`super::seed::verify_connectivity`] is already called on every
//! seed/sync/import — it is cheap (a `cat-file --batch-check` over
//! patchset shas plus, at most, one `update-ref --stdin` recreating
//! missing ps refs) and safe to run daily; the GC half is DRY-RUN ONLY on
//! this cadence (see the module-top ruling) — computing and recording the
//! candidate list daily costs nothing and keeps `store show`/doctor
//! honest about how large a real `--yes` apply would be, without ever
//! touching a ref itself.
//!
//! **The original "ref GC is safe to run unattended, because objects are
//! not reclaimed until the monthly cruft pass's 2-week window" reasoning
//! this section carried was WRONG** (review finding B3): a packed object
//! keeps its PACK's mtime, not the mtime of whatever event orphaned the
//! ref pointing at it — an object that has sat in an old pack for months
//! reads as months-old to `--cruft-expiration`, so an expiring cruft
//! repack run shortly after a GC apply can prune objects that same pass,
//! not two weeks later. [`run_monthly`] now takes an explicit
//! `allow_expire` the caller computes from `state_json.last_gc_apply`
//! (never expiring within 14 days of the last real apply, and never on a
//! store with no recorded apply at all — "first pass never expires").
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
//! text's "or a restore flag" names.
//!
//! **Three gaps a review found in the first cut, and how each is closed:**
//! (1) *same-epoch restores* (an operator restores an OLDER snapshot at
//! the SAME schema epoch) are invisible to epoch comparison by
//! construction — there is no epoch delta to observe. Closed by an
//! independent, unconditional mechanism that needs no epoch at all: see
//! [`Store::reviews_high_water_id`] and the `restore-suspected` check in
//! [`apply_gc_candidates`] (reached from both [`run_gc_now`] and
//! [`run_gc_pass`]). (2) *restoring the whole state directory* also rolls
//! the sentinel FILE back, defeating the "outside the volume" asymmetry
//! the design relies on. There is no filesystem-only fix for this (the
//! sentinel is, definitionally, part of "the state directory"); the
//! DB-truth check in (1) does not depend on the sentinel surviving at all,
//! so it still catches this case. (3) *fail-open on corruption/write
//! failure*: a corrupt or unreadable sentinel now reads as FLAGGED (not
//! the previous "degrades to unflagged"), and a failed WRITE sets an
//! in-process (not persisted — "for the process lifetime", per the
//! ruling) latch that forces every subsequent read in this process to
//! read flagged too, until a write succeeds again.
//!
//! [`Store::reviews_high_water_id`]: crate::store::Store::reviews_high_water_id

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

/// A failed task is retried no sooner than this (Should-fix review finding:
/// a task that fails every scheduler tick must not be RE-ATTEMPTED every
/// scheduler tick — that just repeats the same failure hourly forever).
/// Matches [`SCHEDULER_TICK`], so a backoff effectively means "skip the
/// next tick or two," not a long quarantine.
pub const TASK_RETRY_BACKOFF_SECS: i64 = 3600;

/// `review_stores.state_json.last_maint` — the timestamp each cadence last
/// SUCCEEDED, per store, plus (Should-fix) a per-cadence retry backoff
/// after a failure. `None` = never run (always due).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastMaint {
    pub daily: Option<i64>,
    pub weekly: Option<i64>,
    pub monthly: Option<i64>,
    #[serde(default)]
    pub daily_retry_after: Option<i64>,
    #[serde(default)]
    pub weekly_retry_after: Option<i64>,
    #[serde(default)]
    pub monthly_retry_after: Option<i64>,
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
        // A success clears any pending backoff for that SAME cadence.
        self.set_retry_after(t, None);
    }

    fn retry_after(self, t: MaintTask) -> Option<i64> {
        match t {
            MaintTask::Daily => self.daily_retry_after,
            MaintTask::Weekly => self.weekly_retry_after,
            MaintTask::Monthly => self.monthly_retry_after,
        }
    }

    fn set_retry_after(&mut self, t: MaintTask, at: Option<i64>) {
        match t {
            MaintTask::Daily => self.daily_retry_after = at,
            MaintTask::Weekly => self.weekly_retry_after = at,
            MaintTask::Monthly => self.monthly_retry_after = at,
        }
    }

    /// Record a FAILED attempt: no success timestamp, but a backoff so the
    /// next scheduler tick skips this cadence rather than retrying it
    /// immediately.
    fn record_failure(&mut self, t: MaintTask, now: i64) {
        self.set_retry_after(t, Some(now + TASK_RETRY_BACKOFF_SECS));
    }
}

/// Pure: which cadences are due, ascending (daily, weekly, monthly), given
/// `now` and the LAST time each ran (or failed). A cadence that has never
/// run is always due. `now < last` (a clock that moved backward) is
/// treated the same as "not yet due" — never a negative-duration panic,
/// never a spurious immediate re-run. A cadence whose PERIOD is due but
/// which failed recently stays not-due until its backoff elapses.
pub fn due_tasks(now: i64, last: LastMaint) -> Vec<MaintTask> {
    MaintTask::ALL
        .into_iter()
        .filter(|&t| {
            let period_due = match last.get(t) {
                None => true,
                Some(l) => now >= l && now - l >= t.period_secs(),
            };
            period_due && last.retry_after(t).is_none_or(|r| now >= r)
        })
        .collect()
}

// ── restore guard ──────────────────────────────────────────────────────

pub mod restore_guard {
    //! See the parent module's doc ("Restore-guard design") for the full
    //! rationale, including the three review-found gaps this cut closes:
    //! same-epoch restores (closed elsewhere — see
    //! [`crate::store::Store::reviews_high_water_id`] and
    //! [`super::apply_gc_candidates`]),
    //! a restored state directory rolling this sentinel back too (same
    //! answer), and fail-OPEN on corruption/write failure (closed here:
    //! [`read`] now treats a corrupt/unreadable-but-PRESENT sentinel as
    //! FLAGGED, and a failed [`write`] latches [`PERSIST_FAILED`] for the
    //! rest of this process so every subsequent read stays flagged even if
    //! the file itself never gets the news).

    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};

    use serde::{Deserialize, Serialize};

    pub const SCHEMA: &str = "kbc-restore-guard/1";

    fn schema_default() -> String {
        SCHEMA.to_string()
    }

    /// Set when a [`write`] fails. Sticky for the rest of THIS process
    /// (never persisted — "fail closed for the process lifetime" per the
    /// review ruling, not across restarts, since a restart re-derives
    /// `high_water_epoch` from whatever the sentinel last durably held
    /// anyway). [`read`] forces `flagged = true` whenever this is set.
    static PERSIST_FAILED: AtomicBool = AtomicBool::new(false);

    /// The sentinel's on-disk (and wire) shape. `just_flagged` is NEVER
    /// persisted (`#[serde(skip)]`) — it is a per-call signal, true only
    /// on the [`observe_boot_epoch`]/[`flag_manual`] call that JUST set
    /// `flagged`, never on a later read that finds it still set. Callers
    /// use it to decide whether to ALSO take a fresh bundle-backup pass
    /// right now (README §8's "or a restore"). `acknowledged_stores` is
    /// PER-STORE (review finding, Should-fix): acknowledging via
    /// `gc --repo R --yes` adds R's uuid here and nothing else — `flagged`
    /// itself is never cleared by an acknowledgement, so a different store
    /// nobody has looked at yet stays blocked.
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
        #[serde(default)]
        pub acknowledged_stores: BTreeSet<String>,
        #[serde(skip)]
        pub just_flagged: bool,
    }

    impl RestoreGuardState {
        /// Is store `uuid` blocked from a real GC apply right now?
        pub fn blocks(&self, store_uuid: &str) -> bool {
            self.flagged && !self.acknowledged_stores.contains(store_uuid)
        }
    }

    /// `<state>/review-store-restore-guard.json` for a given state dir —
    /// the helper non-daemon callers (`Store::open`'s boot hook,
    /// `kb-code backup`) use; daemon-context callers already have this
    /// path cached on `StoreSettings::restore_guard_path`.
    pub fn path_for(state_dir: &Path) -> PathBuf {
        state_dir.join(super::super::settings::RESTORE_GUARD_FILE)
    }

    /// Read the sentinel at `guard_path`. Fail-CLOSED (review finding B2):
    /// a MISSING file (the honest "never flagged, nothing has ever run
    /// here" first-boot case) reads as the default — but a file that
    /// EXISTS and fails to parse (truncated write, disk corruption) reads
    /// as FLAGGED, never silently "never flagged". A prior [`write`]
    /// failure in THIS process latches the same fail-closed reading
    /// regardless of what the file says.
    pub fn read(guard_path: &Path) -> RestoreGuardState {
        let mut state = match std::fs::read_to_string(guard_path) {
            Ok(text) => match serde_json::from_str::<RestoreGuardState>(&text) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %guard_path.display(),
                        "kb-code: review-store restore-guard sentinel is corrupt — \
                         treating as FLAGGED (fail closed)"
                    );
                    RestoreGuardState {
                        flagged: true,
                        flagged_reason: Some(format!("corrupt restore-guard sentinel: {e}")),
                        ..Default::default()
                    }
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RestoreGuardState::default(),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %guard_path.display(),
                    "kb-code: could not read the review-store restore-guard sentinel — \
                     treating as FLAGGED (fail closed)"
                );
                RestoreGuardState {
                    flagged: true,
                    flagged_reason: Some(format!("unreadable restore-guard sentinel: {e}")),
                    ..Default::default()
                }
            }
        };
        if PERSIST_FAILED.load(Ordering::SeqCst) && !state.flagged {
            state.flagged = true;
            state
                .flagged_reason
                .get_or_insert_with(|| "a prior write to this sentinel failed this process".into());
        }
        state
    }

    fn write(guard_path: &Path, state: &RestoreGuardState) -> std::io::Result<()> {
        let attempt = || -> std::io::Result<()> {
            if let Some(parent) = guard_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = guard_path.with_extension("json.tmp");
            let text = serde_json::to_string_pretty(state)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, guard_path)
        };
        match attempt() {
            Ok(()) => {
                PERSIST_FAILED.store(false, Ordering::SeqCst);
                Ok(())
            }
            Err(e) => {
                // Should-fix: fail CLOSED, not just a warning — every
                // subsequent `read` in this process now returns flagged
                // regardless of what is (or is not) on disk.
                PERSIST_FAILED.store(true, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    /// The automatic detector: compare the volume's PRE-MIGRATION epoch at
    /// THIS boot against the highest epoch any earlier boot of this
    /// daemon (on this box) ever observed. A volume whose epoch just went
    /// BACKWARD relative to the high-water mark flags the guard (refinery
    /// only migrates forward, so that can only mean a restore); a volume
    /// at or above the mark bumps it (never lowers it). A NEW flagging
    /// event (not a repeat of an already-flagged one) clears
    /// `acknowledged_stores` — a previous incident's acknowledgements do
    /// not carry over to a new one. Call once per boot, before the
    /// migration runner touches the epoch.
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
                        state.acknowledged_stores.clear();
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
    /// OTHER than an epoch rollback can flag the guard directly (always a
    /// NEW incident — clears `acknowledged_stores`). Not wired to any
    /// route or CLI verb in RS-U9 (no restore-bundle verb ships yet — see
    /// the unit's hand-off note); exists so a later unit can call it
    /// without touching this module.
    pub fn flag_manual(
        guard_path: &Path,
        reason: &str,
        now: i64,
    ) -> std::io::Result<RestoreGuardState> {
        let mut state = read(guard_path);
        state.flagged = true;
        state.flagged_at = Some(now);
        state.flagged_reason = Some(reason.to_string());
        state.acknowledged_stores.clear();
        write(guard_path, &state)?;
        state.just_flagged = true;
        Ok(state)
    }

    /// The operator's explicit `kb-code store gc --repo R --yes`
    /// acknowledgement for STORE `store_uuid` only (review finding,
    /// Should-fix — this never clears `flagged` globally, and never
    /// acknowledges any OTHER store). A no-op (but never an error) when
    /// nothing is flagged.
    pub fn acknowledge(
        guard_path: &Path,
        store_uuid: &str,
        now: i64,
    ) -> std::io::Result<RestoreGuardState> {
        let mut state = read(guard_path);
        if state.flagged {
            state.acknowledged_stores.insert(store_uuid.to_string());
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

/// Below this many seconds since the store's last recorded REAL GC apply
/// (`state_json.last_gc_apply`), [`run_monthly`] must not expire any cruft
/// object (review finding B3 — see the module doc's "Cadence choices").
pub const CRUFT_EXPIRE_COOLDOWN_SECS: i64 = 14 * DAY_SECS;

/// Monthly: `repack --cruft -d --cruft-expiration=<…>` plus
/// `reflog expire --expire=14.days --all` (README §5.4,
/// design-internal-store.md §8's fuller flag list).
///
/// **Design-doc correction:** the source docs' flag list also names
/// `--geometric=2` on the cruft repack. Real git refuses that combination
/// outright (`fatal: options '--geometric' and '-A/-a' cannot be used
/// together` — `--cruft` implies an `-A`-style whole-repo repack, which is
/// a different STRATEGY from `--geometric`'s incremental one; confirmed
/// against CI's git by a failing fixture test), so it is dropped here.
/// `--all` on the reflog expire (not in the README's shorthand) is a
/// deliberate build-time addition: without it `git reflog expire` only
/// ever touches the ref(s) named on its command line, and a bare store
/// has no branch reflog of its own to default to — omitting it would make
/// that call a byte-identical no-op.
///
/// **`allow_expire` (review finding B3, data-loss blocker):** a packed
/// object keeps its PACK's mtime, not the mtime of whatever event orphaned
/// the ref that used to protect it — an object that sat in an old pack for
/// months reads as months-old to `--cruft-expiration` the INSTANT it
/// becomes unreachable, not two weeks later. Running an EXPIRING cruft
/// repack shortly after a GC apply can prune objects that same pass. The
/// caller computes `allow_expire` from `state_json.last_gc_apply` (never
/// within [`CRUFT_EXPIRE_COOLDOWN_SECS`] of the last real apply, and never
/// when there is no recorded apply at all — a store with no `last_gc_apply`
/// gets `--cruft-expiration=never`, "first pass never expires"). This
/// function does not consult `state_json` itself — being handed the
/// decision keeps it a pure git-argv builder, testable without a `Store`.
pub fn run_monthly(
    git: &StoreGit,
    git_dir: &Path,
    allow_expire: bool,
) -> Result<(), StoreGitError> {
    let expiration: &'static str = if allow_expire {
        "--cruft-expiration=2.weeks.ago"
    } else {
        "--cruft-expiration=never"
    };
    git.run(
        GitCall::new(
            "maintenance-monthly-cruft",
            GitArgs::new("repack")
                .flag("--cruft")
                .flag("-d")
                .flag(expiration)
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
/// verdict to every review it looked at. Always passes `None` for
/// `verify_connectivity`'s own `ops` parameter — the CALLER already holds
/// the store's ops lock for this whole section (a second lock attempt on
/// the same `tokio::sync::Mutex` would deadlock; it is not reentrant).
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
        if let Err(e) = store.set_review_objects_state(*id, Some(super::seed::OBJECTS_MISSING)) {
            tracing::warn!(review = *id, error = %e, "kb-code: could not record objects-missing");
        }
    }
    for id in &ok {
        match store.get_review_base(*id) {
            Ok(Some(b)) if b.objects_state.as_deref() == Some(super::seed::OBJECTS_MISSING) => {
                if let Err(e) = store.set_review_objects_state(*id, None) {
                    tracing::warn!(review = *id, error = %e, "kb-code: could not clear objects-missing");
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(review = *id, error = %e, "kb-code: could not re-read review base")
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

/// One store-wide GC pass's outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GcRunReport {
    pub candidates: usize,
    pub applied: bool,
    /// True when one or more members could not be resolved this pass
    /// (`StoreScope::problems`, B1) — their refs are still correctly kept
    /// (membership for `keep_set` is DB-truth, never config-filtered), but
    /// the pass could not fully verify them, so it is reported honestly
    /// rather than silently treated as complete.
    pub partial: bool,
    /// `"applied"` | `"dry-run"` | `"nothing-to-do"` | `"restore-guard"` |
    /// `"restore-suspected"` | `"backup-failed"`.
    pub reason: &'static str,
    pub member_problems: Vec<String>,
    pub detail: Option<String>,
}

/// Classify every store ref (README §5.4, [`super::gc`]'s engine —
/// unchanged from RS-U5): `keep_set` + `list_refs` + `attribute` +
/// `delete_candidates`. Pure read — never mutates anything. Returns the
/// dry-run report ALONGSIDE the raw candidates, so a caller that goes on
/// to apply (only ever [`apply_gc_candidates`], reached from
/// [`run_gc_pass`]) can run its guard checks against the exact same
/// classification rather than re-deriving it.
///
/// `registered_member_ids` MUST come from DB truth
/// (`Store::store_members`), never from a config-filtered list (RS-U9
/// review fix B1 — RS-U3's `ReviewStores::members_of` silently drops a
/// member whose clone fails `common_dir_of` or has left `[[repos]]`,
/// discarding it from `problems`; using that filtered list here would
/// misclassify that member's still-live `work-<id>`/`hint/<id>` refs as
/// orphan and delete them). Call under the store's ops lock.
pub fn gc_pass(
    store: &Store,
    git: &StoreGit,
    git_dir: &Path,
    store_id: i64,
    registered_member_ids: &[i64],
    member_problems: &[String],
) -> Result<(GcRunReport, Vec<super::gc::GcCandidate>), String> {
    let keep =
        super::gc::keep_set(store, store_id, registered_member_ids).map_err(|e| e.to_string())?;
    let refs = super::seed::list_refs(git, git_dir, &[]).map_err(|e| e.to_string())?;
    let attributed = super::gc::attribute(&refs, &keep);
    let candidates = super::gc::delete_candidates(&attributed);
    let report = GcRunReport {
        candidates: candidates.len(),
        applied: false,
        partial: !member_problems.is_empty(),
        reason: "dry-run",
        member_problems: member_problems.to_vec(),
        detail: None,
    };
    Ok((report, candidates))
}

/// The first delete candidate whose refname encodes a review id ABOVE
/// `high_water` — RS-U9 review fix B2's DB-truth restore-suspected check,
/// independent of the sentinel-file restore guard (catches a same-epoch
/// restore, which never trips an epoch comparison). `None` when every
/// candidate is safe. `high_water` is
/// [`crate::store::Store::reviews_high_water_id`] — a review id above it
/// can only have been minted by a database this volume has since been
/// rolled BEHIND.
fn restore_suspected_review_id(
    candidates: &[super::gc::GcCandidate],
    high_water: i64,
) -> Option<i64> {
    candidates
        .iter()
        .find_map(|c| match crate::reviews::parse_kbc_ref(&c.refname) {
            Some(crate::reviews::KbcRef::Patchset { review_id, .. })
            | Some(crate::reviews::KbcRef::PatchsetBase { review_id, .. })
                if review_id > high_water =>
            {
                Some(review_id)
            }
            _ => None,
        })
}

/// [`gc_pass`]'s guard-checked apply half — the ONLY function in this
/// crate that calls [`super::gc::apply`], and therefore the ONLY place a
/// ref-delete transaction can originate. Reached from exactly two entry
/// points, both [`run_gc_pass`]: the operator's `store gc --yes` and the
/// `review refs gc --apply` route. In order: nothing to do →
/// sentinel restore-guard (bypassable only by the caller having already
/// acknowledged THIS store, [`run_gc_now`]) → the UNCONDITIONAL,
/// never-bypassable DB-truth `restore_suspected_review_id` check → a
/// fresh bundle backup (the operator ruling: every real apply is preceded
/// by a bundle of the pre-apply state) that COVERS every ref in
/// `candidates` — not merely the `refs/kbc/*` ones, since GC's candidate
/// set also spans `refs/remotes/work-<id>/*` — → the actual guarded
/// `update-ref --stdin` transaction. Call under the store's ops lock, with
/// `report` fresh from [`gc_pass`] (same candidates the caller is about to
/// apply — never re-derive between the two).
#[allow(clippy::too_many_arguments)]
fn apply_gc_candidates(
    store: &Store,
    git: &StoreGit,
    git_dir: &Path,
    backups_dir: &Path,
    store_uuid: &str,
    candidates: &[super::gc::GcCandidate],
    mut report: GcRunReport,
    guard_blocked: bool,
    now: i64,
) -> Result<GcRunReport, String> {
    if report.candidates == 0 {
        report.reason = "nothing-to-do";
        return Ok(report);
    }
    if guard_blocked {
        report.reason = "restore-guard";
        return Ok(report);
    }
    let high_water = store.reviews_high_water_id().map_err(|e| e.to_string())?;
    if let Some(bad_id) = restore_suspected_review_id(candidates, high_water) {
        report.reason = "restore-suspected";
        report.detail = Some(format!(
            "a delete candidate names review {bad_id}, above this volume's high-water review id {high_water}"
        ));
        return Ok(report);
    }
    // The bundle MUST cover every ref this apply is about to delete, not
    // just the `refs/kbc/*` ones: `super::gc::attribute` also marks a
    // de-registered member's `refs/remotes/work-<id>/*` mirror refs
    // orphan, and `delete_candidates` has no `kind` filter, so those are
    // in `candidates` too. A `refs/kbc/*`-only bundle would let a real
    // apply delete refs nothing had backed up.
    //
    // `cover` carries `(oid, refname)` straight from `candidates` — the
    // exact values this apply deletes, guarded by `old_oid` — so the
    // bundle stays a point-in-time snapshot of the SAME classification
    // `report` came from, with no second scan that could disagree with
    // it. It is bounded by the candidate count, itself bounded by the
    // store's ref count (the same bound the `refs/kbc/*` argv already
    // relies on — see [`write_bundle`]'s ARG_MAX note).
    let cover: Vec<(String, String)> = candidates
        .iter()
        .map(|c| (c.old_oid.clone(), c.refname.clone()))
        .collect();
    match write_bundle_covering(
        git,
        git_dir,
        &bundle_path(backups_dir, store_uuid, now),
        &cover,
    ) {
        Ok(BundleOutcome::Written) => {
            if let Err(e) = prune_bundles(backups_dir, store_uuid, BUNDLES_KEPT) {
                tracing::warn!(error = %e, store = store_uuid, "kb-code: bundle prune before gc apply failed (non-fatal)");
            }
        }
        Ok(BundleOutcome::Skipped { reason: "no-refs" }) => {
            // Structurally unreachable — `cover` is non-empty whenever
            // `report.candidates` is (it IS that list, and the
            // `nothing-to-do` arm above returned otherwise), so the
            // bundle's ref list cannot be empty. Fail CLOSED rather than
            // delete refs an empty bundle did not cover: this arm makes
            // the invariant hold by construction, not by argument.
            report.reason = "backup-failed";
            report.detail = Some(
                "bundle reported no-refs despite a non-empty delete candidate list".to_string(),
            );
            return Ok(report);
        }
        // `fully-reachable-from-base`: git refused a genuinely empty
        // bundle because EVERY ref — the candidates included — is
        // already reachable from `refs/remotes/base/*`. Deleting such a
        // ref orphans no object, and the `.refs` manifest written beside
        // the (absent) bundle still records every ref name and oid.
        Ok(BundleOutcome::Skipped { .. }) => {}
        Err(e) => {
            report.reason = "backup-failed";
            report.detail = Some(e.to_string());
            return Ok(report);
        }
    }
    super::gc::apply(git, git_dir, candidates).map_err(|e| e.to_string())?;
    report.applied = true;
    report.reason = "applied";
    Ok(report)
}

/// [`gather_store_scope`]'s result: everything a store-wide pass needs,
/// gathered with NO git calls (unlike [`ReviewStores::plan_for`], which
/// also reads each member's remotes — unneeded here and wasteful on a
/// routine pass). `registered_ids` is DB TRUTH (`Store::store_members`,
/// B1's fix) — NEVER the config-filtered `members_of` list, which is only
/// safe for `seed_members`/`patchsets` (the invariant check's own scope,
/// where a member that cannot be resolved on disk genuinely cannot be
/// checked). `problems` is `members_of`'s own report of which members
/// could not be resolved, surfaced into [`GcRunReport::partial`].
struct StoreScope {
    registered_ids: Vec<i64>,
    seed_members: Vec<SeedMember>,
    patchsets: Vec<ExpectedPatchset>,
    problems: Vec<String>,
}

fn gather_store_scope(
    rs: &ReviewStores,
    store: &Store,
    store_id: i64,
) -> Result<StoreScope, String> {
    let (members, problems) = rs.members_of(store, store_id);
    // `Store::patchsets_for_repos` is still name-keyed — a purely LOCAL
    // name list, never stored or handed to the (DB-truth, `store_id`-keyed)
    // keep-set query below.
    let member_names: Vec<String> = members.iter().map(|(r, _)| r.name.clone()).collect();
    let seed_members: Vec<SeedMember> = members.into_iter().map(|(_, m)| m).collect();
    // B1 fix: DB truth, not `members_of`'s config-filtered id list.
    let registered_ids = store.store_members(store_id).map_err(|e| e.to_string())?;
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
        problems,
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
/// `refs/remotes/base/*` (README §5.4 / design-internal-store.md §8) —
/// the routine-backup shape ([`backup_all_ready_stores`], the boot /
/// `kb-code backup` passes). GC's PRE-APPLY bundle is a different, wider
/// shape: see `write_bundle_covering`.
///
/// `Skipped { reason: "no-refs" }` when the store has no `refs/kbc/*` yet
/// (a freshly seeded store with no reviews) — `git bundle create` refuses
/// an empty ref list, and an empty bundle is not a useful backup anyway.
///
/// **Implementation note, corrected post-review:** the source docs
/// describe this as `git bundle create <f> --stdin` fed the refs on
/// stdin. An earlier cut of this function claimed real git has no such
/// flag — that claim was WRONG: `--stdin` has existed since git 2.31, but
/// only works when it appears AFTER `<file>` (it is one of the
/// `<git-rev-list-args>`, not a `bundle create`-level flag like `-q`/
/// `--progress`/`--version=<n>`) — a positional detail the original CI
/// failure (`error: unknown option 'stdin'`, from putting it BEFORE
/// `<file>`) was actually about. This function instead passes every ref
/// as a POSITIONAL rev-list argument directly (the same grammar `--stdin`
/// would feed into), which is exactly how `git rev-list`/`git log` take a
/// ref list too, and sidesteps the ordering subtlety entirely. The
/// store's own ref-count scale sits comfortably under Linux's ARG_MAX with
/// `env_clear()`'s already-tiny environment, so there is no practical case
/// this overflows argv.
pub fn write_bundle(
    git: &StoreGit,
    git_dir: &Path,
    dest: &Path,
) -> Result<BundleOutcome, MaintError> {
    write_bundle_covering(git, git_dir, dest, &[])
}

/// [`write_bundle`] PLUS every ref in `cover` (`(oid, refname)` pairs) —
/// the bundle shape [`apply_gc_candidates`] takes immediately before a
/// real GC apply.
///
/// **Ref classes covered:** all `refs/kbc/*` (whatever their binding
/// status) plus every ref the apply is about to delete, whatever its
/// namespace. That second clause is load-bearing and is why this function
/// exists at all: [`super::gc::attribute`] classifies
/// `refs/remotes/work-<id>/<branch>` mirror refs as `kind: "work"`, and
/// [`super::gc::delete_candidates`] filters on `status == "orphan"` with
/// NO `kind` filter — so a de-registered member's mirror refs ARE delete
/// candidates while living entirely outside `refs/kbc/*`. A
/// `refs/kbc/*`-only bundle would let such an apply destroy refs that no
/// backup had ever captured.
///
/// **Ref classes EXCLUDED, and why that is safe:** `refs/remotes/base/*`
/// is passed as `^<ref>` (never a bundle head), per README §5.4 — those
/// objects are re-fetchable from the base remote, so a bundle need not
/// carry them. `refs/remotes/work-<id>/*` for a STILL-REGISTERED member
/// is not a delete candidate, so it is covered only incidentally, when it
/// also appears in `cover`. `refs/heads/*` and any other namespace this
/// store's ref scan never classifies are likewise neither heads nor
/// candidates. The invariant this maintains: **no ref is ever deleted by
/// an apply whose pre-apply bundle did not cover it.**
///
/// `cover` is a point-in-time list supplied by the caller (the exact
/// `(old_oid, refname)` the apply is guarded on), not a re-scan, so it
/// cannot disagree with the classification that produced the apply. It
/// is bounded by the store's ref count — the same bound the `refs/kbc/*`
/// argv already relies on above.
fn write_bundle_covering(
    git: &StoreGit,
    git_dir: &Path,
    dest: &Path,
    cover: &[(String, String)],
) -> Result<BundleOutcome, MaintError> {
    let kbc_refs = super::seed::list_refs(git, git_dir, &["refs/kbc/"])?;
    // Union of the `refs/kbc/*` scan and the caller's `cover`, deduped by
    // refname (a candidate in the `kbc` namespace is listed by both). The
    // scan's own oid wins the duplicate: `git bundle create` resolves the
    // ref by name anyway, so the manifest records the value the bundle
    // actually carries.
    let mut refs = kbc_refs;
    for (oid, name) in cover {
        if !refs.iter().any(|(_, existing)| existing == name) {
            refs.push((oid.clone(), name.clone()));
        }
    }
    if refs.is_empty() {
        return Ok(BundleOutcome::Skipped { reason: "no-refs" });
    }
    let base_refs = super::seed::list_refs(git, git_dir, &["refs/remotes/base/"])?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
        // Should-fix: the backups dir holds bundle contents of the whole
        // store — same 0700 posture as `git_home` (`StoreGit::new`), same
        // Linux/WSL2-only assumption the rest of this crate already makes.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut args = GitArgs::new("bundle")
        .flag("create")
        .flag("--quiet")
        .end_of_options()
        .abs_path(dest)
        .map_err(|e| MaintError::Other(e.to_string()))?;
    for (_, name) in &refs {
        let r = super::url::RefName::parse(name)
            .map_err(|_| MaintError::Other(format!("unparseable bundled ref: {name}")))?;
        args = args.refname(&r);
    }
    for (_, name) in &base_refs {
        let r = super::url::RefName::parse(name)
            .map_err(|_| MaintError::Other(format!("unparseable base ref: {name}")))?;
        args = args.exclude_ref(&r);
    }
    // Should-fix: "nothing to bundle" (every covered ref is already
    // reachable from `refs/remotes/base/*`) is a recorded no-op, not an
    // error — git refuses to write a genuinely empty bundle. Either way, a
    // small refs manifest beside the bundle path keeps every ref NAME (and
    // the oid it pointed at) on record, even for a ref whose tip needed no
    // new objects because base already carries it.
    match git.run(
        GitCall::new("bundle-create", args)
            .git_dir(git_dir)
            .timeout(MAINT_TIMEOUT),
    ) {
        Ok(_) => {
            write_refs_manifest(dest, &refs)?;
            Ok(BundleOutcome::Written)
        }
        Err(e) if e.detail.to_ascii_lowercase().contains("empty bundle") => {
            write_refs_manifest(dest, &refs)?;
            Ok(BundleOutcome::Skipped {
                reason: "fully-reachable-from-base",
            })
        }
        Err(e) => Err(e.into()),
    }
}

/// `<dest>.refs`: one `<oid>\t<refname>` line per ref the bundle (attempted
/// to) carry — see [`write_bundle`]'s doc.
fn write_refs_manifest(dest: &Path, refs: &[(String, String)]) -> std::io::Result<()> {
    let manifest = PathBuf::from(format!("{}.refs", dest.display()));
    let mut body = String::new();
    for (oid, name) in refs {
        body.push_str(oid);
        body.push('\t');
        body.push_str(name);
        body.push('\n');
    }
    std::fs::write(manifest, body)
}

/// Keep the newest `keep` `store-<uuid>-*.bundle` files (and their `.refs`
/// manifests) under `backups_dir`, removing the rest. Returns the removed
/// bundle paths (a manifest removal failure is logged, never fatal to the
/// prune pass — nit: a REMOVE failure on the bundle itself is now logged
/// too, not silently dropped).
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
        match std::fs::remove_file(&path) {
            Ok(()) => {
                let manifest = PathBuf::from(format!("{}.refs", path.display()));
                let _ = std::fs::remove_file(&manifest);
                removed.push(path);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "kb-code: could not remove a pruned backup bundle"
                );
            }
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
    let rows: Vec<(i64, String, String, Option<String>)> = match conn
        .prepare(
            "SELECT id, uuid, git_dir, state_json FROM review_stores \
             WHERE state = 'ready' ORDER BY id",
        )
        .and_then(|mut stmt| {
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        }) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "kb-code: review-store backup: could not list ready stores");
            return report;
        }
    };
    let dest_dir = backups_dir(state_dir);
    for (id, uuid, git_dir, state_json) in rows {
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
        // Should-fix: record the outcome (never just log-and-drop) so
        // `store show`/doctor can surface a failing boot/backup-cmd bundle
        // pass without needing the daemon's own log.
        let mut sj: serde_json::Value = state_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        sj["last_backup"] = serde_json::json!({
            "at": now,
            "written": outcome.written.is_some(),
            "skipped_reason": outcome.skipped_reason,
            "error": outcome.error,
        });
        if let Err(e) = conn.execute(
            "UPDATE review_stores SET state_json = ?1 WHERE id = ?2",
            rusqlite::params![sj.to_string(), id],
        ) {
            tracing::warn!(error = %e, store = %uuid, "kb-code: could not record last_backup in state_json");
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

// ── boot-time bundle backup, deferred off the boot critical path ───────

/// Marker file name: `Store::open`'s cheap, filesystem-only restore-guard
/// observation writes this when it decides a boot-time bundle backup is
/// warranted (a gated-epoch crossing, or a freshly detected restore) —
/// Should-fix review finding: "NO git I/O on the boot path." `Store::open`
/// itself never spawns git; [`spawn_boot_bundle_backup`] does, AFTER the
/// daemon has bound and started serving.
pub const BOOT_BACKUP_PENDING_FILE: &str = "review-store-boot-backup-pending";

/// Called from `Store::open` (filesystem only — no git, no network).
pub fn mark_boot_backup_pending(state_dir: &Path, reason: &str) -> std::io::Result<()> {
    std::fs::write(state_dir.join(BOOT_BACKUP_PENDING_FILE), reason)
}

/// Spawn the deferred boot-time bundle-backup check. Spawned UNCONDITIONALLY
/// (mirrors every other worker in this module), AFTER the daemon binds —
/// never awaited by boot. A no-op, cheaply (one `try_exists`), on the
/// overwhelming majority of boots that never marked anything pending.
pub fn spawn_boot_bundle_backup(state: SharedState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(state_dir) = state
            .review_stores
            .settings()
            .git_home
            .parent()
            .map(Path::to_path_buf)
        else {
            return;
        };
        let marker = state_dir.join(BOOT_BACKUP_PENDING_FILE);
        let db_path = state_dir.join("index.db");
        let res = tokio::task::spawn_blocking(move || {
            if !marker.exists() {
                return None;
            }
            let reason = std::fs::read_to_string(&marker).unwrap_or_default();
            let report = backup_all_ready_stores_at(&db_path);
            let _ = std::fs::remove_file(&marker);
            Some((reason, report))
        })
        .await;
        match res {
            Ok(Some((reason, Ok(report)))) => {
                let errors: Vec<&str> = report
                    .stores
                    .iter()
                    .filter_map(|s| s.error.as_deref())
                    .collect();
                if errors.is_empty() {
                    tracing::info!(
                        reason,
                        stores = report.stores.len(),
                        "kb-code: boot-time review-store bundle backup done"
                    );
                } else {
                    tracing::warn!(
                        reason,
                        ?errors,
                        "kb-code: boot-time review-store bundle backup had errors"
                    );
                }
            }
            Ok(Some((reason, Err(e)))) => {
                tracing::warn!(reason, error = %e, "kb-code: boot-time review-store bundle backup failed");
            }
            Ok(None) => {} // nothing was pending — the common case.
            Err(e) => {
                tracing::warn!(error = %e, "kb-code: boot-time review-store bundle backup task panicked")
            }
        }
    })
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
    /// Always a DRY RUN — the scheduler (and `store maintain`) NEVER call
    /// [`super::gc::apply`] (operator ruling, see the module doc's top).
    /// The only paths that apply are [`run_gc_now`] and [`run_gc_pass`]
    /// (the `review refs gc --apply` route), and neither is reachable
    /// from here.
    pub gc: Option<GcRunReport>,
    /// Would a real apply be blocked for THIS store right now (either the
    /// per-store restore guard, or a stale/not-ready row found when the
    /// GC/invariant section re-checked under the lock)?
    pub restore_guard_blocks_apply: bool,
    pub errors: Vec<String>,
}

fn state_json_value(row: &ReviewStoreRow) -> serde_json::Value {
    parse_state_json(row.state_json.as_deref())
}

fn parse_state_json(s: Option<&str>) -> serde_json::Value {
    s.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

fn log_state_write_failure(store_uuid: &str, context: &'static str, e: &crate::store::StoreError) {
    tracing::warn!(error = %e, store = store_uuid, context, "kb-code: could not persist review_stores.state_json");
}

/// Pure (review finding B3): may a MONTHLY pass expire cruft objects right
/// now? Only when `state_json.last_gc_apply.at` is recorded AND at least
/// [`CRUFT_EXPIRE_COOLDOWN_SECS`] old — never on a store with NO recorded
/// apply at all ("first pass never expires": an absent key is exactly as
/// unsafe as a RECENT one, since either way this process cannot prove 14
/// days have passed since any possible apply).
fn cruft_allow_expire(sj: &serde_json::Value, now: i64) -> bool {
    let last_apply_at = sj
        .get("last_gc_apply")
        .and_then(|v| v.get("at"))
        .and_then(serde_json::Value::as_i64);
    last_apply_at.is_some_and(|at| now.saturating_sub(at) >= CRUFT_EXPIRE_COOLDOWN_SECS)
}

/// Run every cadence in `tasks` against ONE store: daily's git-housekeeping
/// task plus, separately and under the ops lock, the ref invariant check
/// and a store-wide GC DRY RUN (see the module doc's "Cadence choices");
/// weekly/monthly repack (NOT under the ops lock — Should-fix review
/// finding: a long repack must not hold the same lock a capture or a real
/// `gc --yes` apply needs). Persists `last_maint` (plus a per-cadence retry
/// backoff after a failure) and `last_gc_dry_run` into `state_json`. Takes
/// the ops lock ITSELF, only around the GC/invariant section — callers
/// must NOT also hold it (not reentrant).
pub fn run_pass_for_store(
    rs: &ReviewStores,
    store: &Store,
    row: &ReviewStoreRow,
    tasks: &[MaintTask],
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
    let mut sj = state_json_value(row);
    let mut last = LastMaint::from_state_json(Some(&sj));

    if tasks.contains(&MaintTask::Daily) {
        match run_daily(git, dir) {
            Ok(swept) => {
                report.tasks_run.push("daily");
                report.swept_tmp_pack = swept;
                last.set(MaintTask::Daily, now);
            }
            Err(e) => {
                report.errors.push(format!("daily: {e}"));
                last.record_failure(MaintTask::Daily, now);
            }
        }

        // GC/invariant section: ops lock held for exactly this, Should-fix
        // review finding ("hold the ops lock only around the GC/ref
        // section, not the long repacks" — weekly/monthly below run
        // UNLOCKED). A dry run never mutates refs on its own, but
        // `invariant_check` CAN (recreating a missing ps ref), and reading
        // a consistent snapshot alongside a possible recreate wants the
        // same lock a real apply would take.
        {
            let ops = rs.ops_lock(row.id);
            let _guard = ops.blocking_lock();
            // Should-fix: re-read the row UNDER the lock and skip this
            // section (never silently proceed on stale state) unless it
            // is still `ready` — a concurrent re-seed/break could have
            // moved it since the caller's own snapshot.
            match store.get_review_store(row.id) {
                Ok(Some(fresh)) if fresh.state == "ready" => {
                    let guard = restore_guard::read(&rs.settings().restore_guard_path);
                    report.restore_guard_blocks_apply = guard.blocks(&fresh.uuid);
                    match gather_store_scope(rs, store, row.id) {
                        Ok(scope) => {
                            match invariant_check(
                                store,
                                git,
                                dir,
                                &scope.seed_members,
                                &scope.patchsets,
                            ) {
                                Ok(inv) => report.invariant = Some(inv),
                                Err(e) => report.errors.push(format!("invariant: {e}")),
                            }
                            match gc_pass(
                                store,
                                git,
                                dir,
                                row.id,
                                &scope.registered_ids,
                                &scope.problems,
                            ) {
                                Ok((g, _candidates)) => report.gc = Some(g),
                                Err(e) => report.errors.push(format!("gc: {e}")),
                            }
                        }
                        Err(e) => report.errors.push(format!("scope: {e}")),
                    }
                }
                Ok(Some(fresh)) => report.errors.push(format!(
                    "gc/invariant: store is no longer ready (state={})",
                    fresh.state
                )),
                Ok(None) => report
                    .errors
                    .push("gc/invariant: store row vanished".into()),
                Err(e) => report
                    .errors
                    .push(format!("gc/invariant: could not re-read store row: {e}")),
            }
        }
    }
    if tasks.contains(&MaintTask::Weekly) {
        match run_weekly(git, dir) {
            Ok(()) => {
                report.tasks_run.push("weekly");
                last.set(MaintTask::Weekly, now);
            }
            Err(e) => {
                report.errors.push(format!("weekly: {e}"));
                last.record_failure(MaintTask::Weekly, now);
            }
        }
    }
    if tasks.contains(&MaintTask::Monthly) {
        let allow_expire = cruft_allow_expire(&sj, now);
        match run_monthly(git, dir, allow_expire) {
            Ok(()) => {
                report.tasks_run.push("monthly");
                last.set(MaintTask::Monthly, now);
            }
            Err(e) => {
                report.errors.push(format!("monthly: {e}"));
                last.record_failure(MaintTask::Monthly, now);
            }
        }
    }

    sj["last_maint"] = serde_json::to_value(last).unwrap_or_default();
    if let Some(g) = &report.gc {
        sj["last_gc_dry_run"] = serde_json::json!({
            "at": now,
            "candidates": g.candidates,
            "reasons": [g.reason],
            "partial": g.partial,
            "member_problems": g.member_problems,
        });
    }
    // Should-fix: never write back a STALE `row.state` — re-read once more
    // (cheap) so a state transition that happened during this whole pass
    // is not silently overwritten with what the caller saw at entry.
    let write_state = store
        .get_review_store(row.id)
        .ok()
        .flatten()
        .map(|f| f.state)
        .unwrap_or_else(|| row.state.clone());
    if let Err(e) = store.set_review_store_state(row.id, &write_state, Some(&sj.to_string())) {
        log_state_write_failure(&row.uuid, "run_pass_for_store", &e);
        report.errors.push(format!("state_json write: {e}"));
    }
    report
}

/// THE store-wide GC entry point — the single funnel every store-backed
/// apply goes through, and therefore the only route to
/// [`super::gc::apply`]. Two callers, and they MUST stay the only two:
///
/// * [`run_gc_now`] — `kb-code store gc --repo R [--yes]`, the operator's
///   explicit verb (route + CLI share it). Its `--yes` is what
///   `bypass_guard` acknowledges.
/// * [`crate::reviews::gc_review_refs_inner`] — the legacy
///   `kb-code review refs gc --repo R --apply` route, which passes
///   `bypass_guard = false` (it has no `--yes`) and MUST delegate here
///   rather than call [`super::gc::apply`] itself: an unguarded apply on
///   that route deleted store refs with no backup, no restore-guard check
///   and no high-water check, and left `state_json.last_gc_apply`
///   stale so the monthly cruft cooldown was judged from a lie.
///
/// `bypass_guard` is true ONLY for an explicit operator `--yes`; it
/// acknowledges THIS store's restore-guard suspicion (never another
/// store's) and, when the apply actually proceeds, that acknowledgement
/// is what unblocks it. Takes the ops lock itself for the whole
/// classify-guard-apply sequence.
///
/// Returns the report ALONGSIDE the classified candidates' refnames (the
/// exact list the pass decided on — also what a refusal leaves intact),
/// so a caller can render per-ref output without re-deriving anything.
pub fn run_gc_pass(
    rs: &ReviewStores,
    store: &Store,
    row: &ReviewStoreRow,
    apply_requested: bool,
    bypass_guard: bool,
    now: i64,
) -> Result<(GcRunReport, Vec<String>), String> {
    let Some(git) = rs.git() else {
        return Err("store git spawner unavailable".into());
    };
    let ops = rs.ops_lock(row.id);
    let _guard = ops.blocking_lock();
    // Should-fix: re-read the row UNDER the lock; bail rather than act on
    // a possibly-stale admission snapshot.
    let fresh = store
        .get_review_store(row.id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "store row vanished between admission and apply".to_string())?;
    if fresh.state != "ready" {
        return Err(format!(
            "store `{}` is no longer ready (state={})",
            fresh.uuid, fresh.state
        ));
    }
    let dir = Path::new(&fresh.git_dir);
    let guard_path = &rs.settings().restore_guard_path;
    // An explicit `--yes` acknowledges THIS store's suspicion FIRST (per
    // store, review finding), so the guard state used below already
    // reflects it.
    let guard = if apply_requested && bypass_guard {
        match restore_guard::acknowledge(guard_path, &fresh.uuid, now) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, store = %fresh.uuid, "kb-code: could not persist restore-guard acknowledgement");
                restore_guard::read(guard_path)
            }
        }
    } else {
        restore_guard::read(guard_path)
    };
    let guard_blocked = guard.blocks(&fresh.uuid);

    let (_members, problems) = rs.members_of(store, fresh.id);
    let registered_ids = store.store_members(fresh.id).map_err(|e| e.to_string())?;
    let (dry_run_report, candidates) =
        gc_pass(store, git, dir, fresh.id, &registered_ids, &problems)?;

    let final_report = if apply_requested {
        apply_gc_candidates(
            store,
            git,
            dir,
            &rs.settings().backups_dir,
            &fresh.uuid,
            &candidates,
            dry_run_report,
            guard_blocked,
            now,
        )?
    } else {
        dry_run_report
    };
    // The refnames this pass classified, captured BEFORE any decision was
    // acted on: a guard refusal must still report what it refused.
    let refnames: Vec<String> = candidates.iter().map(|c| c.refname.clone()).collect();

    let mut sj = state_json_value(&fresh);
    if final_report.applied {
        sj["last_gc_apply"] = serde_json::json!({
            "at": now,
            "candidates": final_report.candidates,
            "reason": final_report.reason,
        });
    } else {
        sj["last_gc_dry_run"] = serde_json::json!({
            "at": now,
            "candidates": final_report.candidates,
            "reasons": [final_report.reason],
            "partial": final_report.partial,
            "member_problems": final_report.member_problems,
        });
    }
    if let Err(e) = store.set_review_store_state(fresh.id, &fresh.state, Some(&sj.to_string())) {
        log_state_write_failure(&fresh.uuid, "run_gc_pass", &e);
    }
    Ok((final_report, refnames))
}

/// `kb-code store gc --repo R [--yes]`'s core (route + CLI share this) —
/// [`run_gc_pass`]'s report half, discarding the candidate refnames the
/// operator verb has no use for.
pub fn run_gc_now(
    rs: &ReviewStores,
    store: &Store,
    row: &ReviewStoreRow,
    apply_requested: bool,
    bypass_guard: bool,
    now: i64,
) -> Result<GcRunReport, String> {
    run_gc_pass(rs, store, row, apply_requested, bypass_guard, now).map(|(report, _)| report)
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
        // `run_pass_for_store` takes the ops lock ITSELF, scoped to only
        // the GC/invariant section (Should-fix review finding) — never
        // held here across the weekly/monthly repacks it also runs.
        let report = run_pass_for_store(rs, store, &row, &due, now);
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
                    // Should-fix: log a panicked pass instead of silently
                    // dropping the JoinError — a swallowed panic here would
                    // otherwise look identical to "nothing was due".
                    if let Err(e) = tokio::task::spawn_blocking(move || {
                        run_scheduler_pass(&st.review_stores, &st.store, now)
                    })
                    .await
                    {
                        tracing::warn!(error = %e, "kb-code: review-store maintenance scheduler pass panicked");
                    }
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
/// is a DRY RUN — candidates computed, nothing deleted. `yes: true` is the
/// ONLY apply path in this crate: it acknowledges this store's restore
/// guard, takes a fresh bundle backup, then applies (with old-value
/// guards, README §5.4) unless the unconditional DB-truth
/// restore-suspected check refuses it.
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
        let now = chrono::Utc::now().timestamp();
        // `run_gc_now` takes the ops lock ITSELF, for the whole
        // classify-guard-apply sequence — never taken here.
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
        // `run_pass_for_store` takes the ops lock ITSELF, scoped to only
        // the GC/invariant section, and NEVER applies GC (the scheduler's
        // own report-only posture — `store maintain` shares this entry
        // point on purpose, so it can never apply either).
        Ok::<_, Box<Response>>(run_pass_for_store(
            &st.review_stores,
            &st.store,
            &row,
            &tasks,
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
