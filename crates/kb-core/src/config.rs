//! Config file parsing — `kb.toml` (per-daemon) and `daemons.toml` (cluster-4
//! multi-daemon list). Topic 02 §Decisions consolidates the schema.

use crate::types::KbName;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Top-level `kb.toml`. Parsed via `KbConfig::load`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KbConfig {
    #[serde(default)]
    pub daemon: DaemonSection,

    #[serde(default)]
    pub server: ServerSection,

    #[serde(default)]
    pub ui: UiSection,

    /// v0.7.x — indexer/watcher knobs (debounce, reconcile interval).
    /// Env vars `KB_DEBOUNCE_MS` / `KB_RECONCILE_SECS` override these
    /// when set; the defaults match the pre-config behaviour.
    #[serde(default)]
    pub indexer: IndexerSection,

    /// Daemon-wide lance storage tuning — per-connection cache caps +
    /// search-index rebuild throttle. Absent (the default) resolves to the
    /// capped shipped defaults; each knob opts back out to pre-knob lance
    /// behavior with an explicit `0`. See `StorageSection`.
    #[serde(default)]
    pub storage: StorageSection,

    /// `kb share` publishing config (Cloudflare Pages + Access, GitHub
    /// Pages). Empty by default — only set when sharing is configured.
    #[serde(default)]
    pub share: ShareSection,

    /// X1 — reactive event→webhook bridge. `None` (the default) spawns no
    /// task. When set, one daemon-wide subscriber POSTs every event whose
    /// `type` is in `types` to `url`. Read-only + post-emit: it only reads
    /// the bus and makes an outbound request, so it adds no inbound surface
    /// and can't write storage — it respects every security invariant. See
    /// `docs/extending.md`.
    #[serde(default)]
    pub webhooks: Option<WebhooksSection>,

    /// D1 — daemon-wide defaults. Currently holds only `embedding_model`,
    /// which fills in for any `[kb.*]` section that omits it. The full
    /// precedence chain is per-kb → `[defaults]` → registry default
    /// (`kb_core::embed::default_model`); see
    /// `KbConfig::resolved_embedding_model`.
    #[serde(default)]
    pub defaults: DefaultsSection,

    /// R3 (v0.24) — opt-in retention windows. Daemon-wide, off by default:
    /// an absent `[retention]` section (or one with every window unset)
    /// changes NOTHING — history + reading rows are kept forever, exactly
    /// as before. When a window IS set, the daemon spawns one background
    /// task that periodically deletes rows older than the window. History
    /// pruning is invariant #8's documented retention exception (a delete
    /// of OLD rows, never an edit). See `RetentionSection`.
    #[serde(default)]
    pub retention: RetentionSection,

    /// v0.34 X1 — multi-user attribution (kb-users/1). Daemon-wide like
    /// `[retention]`/`[defaults]`: the operator name + trusted header are
    /// host-level choices, not per-corpus. Users are ATTRIBUTION strings
    /// only (not authorization). Absent (the default) yields
    /// `operator = "operator"` + `header = "Remote-User"` + empty
    /// `users` — every pre-multi-user deployment attributes as the
    /// operator until configured. See `IdentitySection`.
    #[serde(default)]
    pub identity: IdentitySection,

    /// GC-B4 — optional off-host copy step run after a successful
    /// `kb backup`. Daemon-wide (not per-kb): the remote target is an
    /// operator/host-level choice, matching the `[retention]`/`[defaults]`
    /// precedent. Absent (the default) is a no-op — `kb backup` behaves
    /// exactly as before, writing only the local tarball. See
    /// `BackupSection`.
    #[serde(default)]
    pub backup: BackupSection,

    /// MI-W2.1 — daemon-wide agent-memory scoring knobs. Same shape/
    /// rationale as `[retention]`/`[backup]`: one operator-level choice,
    /// not per-corpus. See `MemorySection`.
    #[serde(default)]
    pub memory: MemorySection,

    /// Per-kb config keyed by kb name.
    #[serde(default)]
    pub kb: BTreeMap<KbName, KbSection>,

    /// W3.A (sessions-rethink designs/projects.md P3) — the declarative
    /// `[projects.<id>]` registry: a READ-TIME relabel/merge layer over the
    /// pure `derive_project` ladder, mirroring `[kb.*]`'s CorpusMount
    /// precedent. Keyed by a stable operator-chosen id (`[projects.kb]` →
    /// `"kb"`). Installed daemon-wide via `kb_core::sessions::
    /// set_project_registry` (kb-server's `install_project_registry`,
    /// beside `install_corpus_mounts`) and re-installed on a config-reload
    /// restart (#13) — a config change takes effect on the NEXT request,
    /// never a reindex. Empty by default: zero config still gives a useful
    /// projects home via auto-projects (basenames of the derived key).
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectSection>,

    /// W7 (sessions-rethink R15/LF-1) — `[sessions]` daemon-wide live-follow
    /// config. Daemon-global like `[defaults]`/`[retention]` (NOT per-kb —
    /// live transcripts aren't corpus-bound). Absent (the default) leaves
    /// Tier-1 liveness entirely off: the presence probe answers the stable
    /// `{"enabled":false,"live":[]}` and `/live` 404s, so every existing
    /// deployment (prod, phone) is byte-unchanged until an operator opts in.
    #[serde(default)]
    pub sessions: SessionsSection,
}

/// W7/LF-1 — Tier-1 "the transcript IS the presence signal" config. See
/// `KbConfig::sessions`. Both fields use the `Option` + accessor-method
/// pattern (`CaptureSection`/`AttachmentsSection` precedent) rather than a
/// `#[serde(default = "fn")]` field default, because THIS struct also
/// `#[derive(Default)]`s (for `#[serde(default)] pub sessions: SessionsSection`
/// on `KbConfig` above) — a derived `Default` bypasses per-field serde
/// defaults, so a bare `u64` field would silently resolve to 0 (every
/// transcript instantly reading "not live") whenever `[sessions]` is absent
/// from kb.toml entirely, not just when `live_window_secs` is.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionsSection {
    /// Directory holding `<claude-project-slug>/<session_id>.jsonl` live
    /// transcripts — the operator points this at `~/.claude/projects`
    /// (Claude Code's own transcript dir). `~` / `~/rest` is tilde-expanded
    /// at read time via `resolved_live_dir` (see there for exactly what's
    /// NOT handled); anything else is used verbatim. `None` (the default,
    /// and the shape of every deployment before W7) disables Tier-1
    /// entirely — D-LF1 recommends leaving this unset in prod (the host/dev
    /// daemon is where the operator actually sits during a live session).
    #[serde(default)]
    pub live_transcripts_dir: Option<PathBuf>,

    /// Seconds since a live transcript's last write before the presence
    /// probe drops it from the `live` set. Default 120s (see
    /// `live_window_secs()`) — long enough to survive an in-flight `Bash`
    /// call, short enough that a genuinely stalled session reads "active —
    /// no output" (Tier-0 style) rather than a false `live`.
    #[serde(default)]
    pub live_window_secs: Option<u64>,
}

impl SessionsSection {
    /// Mirrors the W0-ratified `sessions::LIVE_WINDOW_SECS` (120s) — ONE
    /// literal, not two: that constant is the spec-of-record value, this is
    /// just its `u64` accessor-default twin (the config field is `u64`
    /// because a byte-offset/seconds knob should never be negative; the
    /// engine constant stays `i64` for its own arithmetic).
    pub const DEFAULT_LIVE_WINDOW_SECS: u64 = crate::sessions::LIVE_WINDOW_SECS as u64;

    pub fn live_window_secs(&self) -> u64 {
        self.live_window_secs
            .filter(|&s| s > 0)
            .unwrap_or(Self::DEFAULT_LIVE_WINDOW_SECS)
    }

    /// `live_transcripts_dir`, tilde-expanded. `~` alone or a `~/...` prefix
    /// expands against `$HOME`; anything else (a bare relative path, an
    /// already-absolute path, or `~otheruser/...`) passes through unchanged
    /// — kb has no portable way to resolve another user's home, and a
    /// relative path is resolved against the daemon's cwd like every other
    /// kb.toml path (the same posture as every other `PathBuf` config
    /// field in this file — none of them tilde-expand either; this one
    /// does because the natural value, `~/.claude/projects`, is always
    /// typed with the tilde).
    pub fn resolved_live_dir(&self) -> Option<PathBuf> {
        self.live_transcripts_dir.as_deref().map(expand_tilde)
    }
}

/// Expand a leading `~` (bare or `~/...`) against `$HOME`. See
/// `SessionsSection::resolved_live_dir` for the exact contract.
fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    path.to_path_buf()
}

/// W3.A — one `[projects.<id>]` entry. `roots` declares every path prefix
/// that belongs to this project (worktrees, renamed subdirs — a trailing
/// `/*` segment is accepted as documentation ("this covers a subtree") but
/// is semantically identical to a bare prefix, since a directory prefix
/// already covers everything beneath it — see `kb_core::sessions::projects`
/// for the matching rule). `kb`/`code_url`/`code_repo` are optional
/// integration hooks (P7/P8): when set, the project card can deep-link into
/// its corpus and its kb-code SPA.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectSection {
    /// Display label. Falls back to the registry id when absent.
    pub label: Option<String>,
    /// Absolute path prefixes this project owns. Empty is legal but useless
    /// (the entry never matches anything) — `validate()` warns.
    #[serde(default)]
    pub roots: Vec<String>,
    /// Primary corpus (`[kb.<name>]`) this project maps to, if any.
    pub kb: Option<String>,
    /// kb-code SPA base URL (e.g. `https://kbc.example.com`), if any.
    pub code_url: Option<String>,
    /// The `:repo` segment kb-code routes use for this project, if any.
    pub code_repo: Option<String>,
}

/// D1 — daemon-wide defaults. Currently two fields; structured as a
/// section so future daemon-wide knobs (e.g. default outbound policy,
/// default decay policy) can land here without another schema break.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DefaultsSection {
    /// Daemon-wide embedding-model fallback. Used when a `[kb.*]` section
    /// omits `embedding_model`. Must match a `kb_core::embed::SUPPORTED_MODELS`
    /// entry; an unknown name here is treated the same as `None` by the
    /// daemon (the registry-default fallback kicks in unless
    /// `disable_embedder_fallback` is set). The bake-off at
    /// `docs/research/foundation/14-embedding-bakeoff-2026-05-19.html`
    /// recommends `"bge-large-en-v1.5"` for technical-English corpora;
    /// the safe default stays `"bge-small-en-v1.5"`.
    pub embedding_model: Option<String>,

    /// D2 — suppress the registry-default fallback in
    /// `resolved_embedding_model`. When `true`, a kb that omits
    /// `embedding_model` AND has no daemon-wide `embedding_model` set
    /// here will resolve to `None` (lexical-only search, no embedder
    /// spawned). Per-kb `embedding_model` and `[defaults] embedding_model`
    /// still apply when set — this knob only governs whether the
    /// daemon auto-selects the registry default when nothing else fires.
    /// Useful in two cases: tests that bring up a daemon without
    /// downloading model weights, and production operators who
    /// deliberately run a no-embedder daemon.
    #[serde(default)]
    pub disable_embedder_fallback: bool,
}

/// Indexer + watcher tuning. Daemon-wide (not per-kb) for every field
/// EXCEPT `reconcile_secs`, which since PF-I1 has a per-kb override at
/// `KbSection::reconcile_secs` (see `resolved_reconcile_secs_for`) — a kb
/// with an unusually large/quiet/noisy source tree can run its
/// reconciliation walk on its own cadence. The watcher's debounce window,
/// watch backend, and indexer nice value stay symmetric across kbs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexerSection {
    /// notify debouncer window. `None` → `kb_core::watcher::DEFAULT_DEBOUNCE_MS`
    /// (400 ms). Env `KB_DEBOUNCE_MS` takes precedence when set.
    pub debounce_ms: Option<u64>,

    /// Periodic reconciliation interval. `None` → 60 s. Setting `Some(0)`
    /// disables the background walk entirely (the explicit
    /// `POST /api/kb/{kb}/reindex` endpoint still works). Env
    /// `KB_RECONCILE_SECS` takes precedence when set.
    ///
    /// The reconciler walks the watched source folder and emits
    /// `watch.modify` envelopes for every `*.html`/`*.htm` file; the
    /// indexer's content-hash dedup gate makes byte-identical files
    /// cheap, so the pass is near-free when nothing changed. Its job
    /// is to catch anything notify missed — inotify queue overflow,
    /// NFS/FUSE mounts that don't deliver events, the small race
    /// between the watcher's initial walk and its arm-debouncer step,
    /// dropped broadcast lag, or a dead drain thread.
    ///
    /// PF-I1 — a kb can override this daemon-wide default via
    /// `[kb.<name>] reconcile_secs` (`KbSection::reconcile_secs`); see
    /// [`IndexerSection::resolved_reconcile_secs_for`].
    pub reconcile_secs: Option<u64>,

    /// I1 — nice value for the `kb-embedder` subprocess the daemon
    /// spawns per kb. Default 20 (lowest priority — the embedder uses
    /// all cores when active but yields to anything else). `None` or
    /// `Some(20)` are equivalent; `Some(0)` disables de-prioritisation.
    /// Negative values are clamped to 0 (only root can raise priority,
    /// which the daemon doesn't expect). Env `KB_INDEXER_NICE` takes
    /// precedence when set.
    ///
    /// The nice value is applied inside the forked child between fork
    /// and the binary takeover (via Unix `pre_exec`), so ONNX worker
    /// threads inherit the priority from birth — no TOCTOU window
    /// where threads start at the parent's priority.
    pub indexer_nice: Option<i32>,

    /// Filesystem-watch backend: `auto` (default — native inotify
    /// with a WSL `/mnt/*` heads-up), `native`, or `poll`. Use `poll` for
    /// filesystems where native events don't fire: WSL2 `/mnt/*` (DrvFs/9p),
    /// SMB/NFS. The reconcile pass is the correctness backstop regardless.
    /// Env `KB_WATCH_MODE` takes precedence.
    pub watch_mode: Option<String>,

    /// X1 (v0.24) — daemon-wide default indexable-extension map: a TOML table
    /// of bare extension → parse-pipeline name (`"html"` or `"markdown"`),
    /// e.g. `[indexer.indexable_extensions]` with `txt = "markdown"`. `None`
    /// (the default) inherits the built-in set (`html`/`htm` → HTML,
    /// `md`/`markdown` → Markdown — identical to pre-X1 behaviour). Any
    /// `[kb.*] indexable_extensions` overrides this for that kb. Resolved via
    /// [`KbConfig::resolved_extension_map`] into a
    /// [`crate::extmap::ExtensionMap`]; `validate` HARD-rejects an unknown
    /// pipeline name or an empty/dotted extension key. D1: the two pipelines
    /// are the entire set — this configures WHICH extensions parse, never adds
    /// a new parser.
    #[serde(default)]
    pub indexable_extensions: Option<BTreeMap<String, String>>,
}

impl IndexerSection {
    pub const DEFAULT_RECONCILE_SECS: u64 = 60;
    /// I1 — default nice for the embedder subprocess. 20 = lowest
    /// priority. Matches the user's choice in the I1 plan.
    pub const DEFAULT_INDEXER_NICE: i32 = 20;

    /// Resolved debounce window — env wins, then config, then default.
    pub fn resolved_debounce_ms(&self) -> u64 {
        std::env::var("KB_DEBOUNCE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .or(self.debounce_ms)
            .unwrap_or(crate::watcher::DEFAULT_DEBOUNCE_MS)
    }

    /// Resolved reconcile interval. `0` means disabled.
    pub fn resolved_reconcile_secs(&self) -> u64 {
        std::env::var("KB_RECONCILE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .or(self.reconcile_secs)
            .unwrap_or(Self::DEFAULT_RECONCILE_SECS)
    }

    /// PF-I1 — per-kb-aware variant of [`Self::resolved_reconcile_secs`]:
    /// same ladder, with `kb`'s own `reconcile_secs` override inserted
    /// between the env var and the daemon-wide `[indexer] reconcile_secs`.
    /// Precedence (highest wins): `KB_RECONCILE_SECS` env → per-kb
    /// `[kb.<name>] reconcile_secs` → daemon-wide `[indexer] reconcile_secs`
    /// → `DEFAULT_RECONCILE_SECS` (60). The env var is checked FIRST and
    /// unconditionally — it's the existing daemon-wide test/container escape
    /// hatch, and a per-kb override must never let one kb re-enable a pass
    /// an operator globally disabled via env. `0` (from any layer) disables
    /// the pass for this kb.
    pub fn resolved_reconcile_secs_for(&self, kb: &KbSection) -> u64 {
        std::env::var("KB_RECONCILE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .or(kb.reconcile_secs)
            .or(self.reconcile_secs)
            .unwrap_or(Self::DEFAULT_RECONCILE_SECS)
    }

    /// I1 — resolved nice value for the embedder subprocess. Env wins,
    /// then config, then default (20). Negative values clamp to 0.
    pub fn resolved_indexer_nice(&self) -> i32 {
        let raw = std::env::var("KB_INDEXER_NICE")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .or(self.indexer_nice)
            .unwrap_or(Self::DEFAULT_INDEXER_NICE);
        raw.clamp(0, 19)
    }

    /// Resolved watch backend — env (`KB_WATCH_MODE`) wins, then config,
    /// then `auto`. An empty/whitespace env var is ignored (falls through to
    /// config) rather than masking a configured `watch_mode` with `auto`.
    pub fn resolved_watch_mode(&self) -> crate::watcher::WatchMode {
        std::env::var("KB_WATCH_MODE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.watch_mode.clone())
            .map(|s| crate::watcher::WatchMode::parse(&s))
            .unwrap_or_default()
    }

    /// Resolved poll interval (ms) for `watch_mode = "poll"` — env
    /// `KB_POLL_INTERVAL_MS` wins, else the watcher default. Coarse by design
    /// (polling re-stats the whole tree); a large SMB/NFS mount may want it
    /// coarser still, while reconcile remains the correctness backstop.
    pub fn resolved_poll_interval_ms(&self) -> u64 {
        std::env::var("KB_POLL_INTERVAL_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&ms| ms > 0)
            .unwrap_or(crate::watcher::DEFAULT_POLL_INTERVAL_MS)
    }
}

/// Daemon-wide lance storage tuning (cache caps + search-index rebuild
/// throttle). Daemon-wide like `[retention]`/`[backup]` — these are
/// host-level resource guards, not per-corpus policy. Every field is
/// `Option<u64>`; an absent `[storage]` section resolves to the capped
/// `DEFAULT_*` values below. Each knob shares one opt-out convention: an
/// explicit `0` restores the pre-knob lance behavior (uncapped caches /
/// rebuild-on-every-dirty).
///
/// Why these exist: lance 4.0.0 defaults its byte-weighted per-connection
/// caches to 6 GiB (index) + 1 GiB (metadata), and every dirty FTS/vector
/// flag used to trigger a full `replace=true` retrain on the next search —
/// on a high-churn corpus that grew the daemon heap to ~10 GB and produced a
/// ~217 MB FTS rebuild every few minutes (plus orphaned `_indices` dirs that
/// lance never reaps; those are swept by kb's own GC, not a knob).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageSection {
    /// Cap for lance's per-connection index cache, in MiB. `None` → 256
    /// (lance's own default is 6 GiB PER kb connection). `Some(0)` → lance's
    /// default.
    pub lance_index_cache_mb: Option<u64>,

    /// Cap for lance's per-connection metadata cache, in MiB. `None` → 64
    /// (lance's own default is 1 GiB per connection). `Some(0)` → lance's
    /// default.
    pub lance_metadata_cache_mb: Option<u64>,

    /// Minimum seconds between search-triggered FTS rebuilds, and
    /// independently between vector (IVF-PQ) rebuilds. The dirty flags stay
    /// the trigger; this only rate-limits the expensive full retrains. The
    /// first build after open is always immediate, and freshness is
    /// preserved while throttled (lance scans fragments newer than the index
    /// — see the Fix 2 comment in `storage::lance::ensure_fts_index`).
    /// `None` → 300. `Some(0)` → rebuild whenever dirty (pre-throttle
    /// behavior).
    pub index_rebuild_min_secs: Option<u64>,
}

impl StorageSection {
    pub const DEFAULT_LANCE_INDEX_CACHE_MB: u64 = 256;
    pub const DEFAULT_LANCE_METADATA_CACHE_MB: u64 = 64;
    pub const DEFAULT_INDEX_REBUILD_MIN_SECS: u64 = 300;

    /// Resolved index-cache cap (MiB). `0` opts out to lance's default.
    pub fn resolved_lance_index_cache_mb(&self) -> u64 {
        self.lance_index_cache_mb
            .unwrap_or(Self::DEFAULT_LANCE_INDEX_CACHE_MB)
    }

    /// Resolved metadata-cache cap (MiB). `0` opts out to lance's default.
    pub fn resolved_lance_metadata_cache_mb(&self) -> u64 {
        self.lance_metadata_cache_mb
            .unwrap_or(Self::DEFAULT_LANCE_METADATA_CACHE_MB)
    }

    /// Resolved rebuild throttle (seconds). `0` disables throttling.
    pub fn resolved_index_rebuild_min_secs(&self) -> u64 {
        self.index_rebuild_min_secs
            .unwrap_or(Self::DEFAULT_INDEX_REBUILD_MIN_SECS)
    }
}

/// R3 (v0.24) — opt-in retention windows. Daemon-wide (not per-kb): the
/// day windows are uniform across corpora — there is no reason to vary a
/// visit-history retention policy per kb, and the existing daemon-wide
/// sections (`[indexer]`, `[defaults]`) are the precedent.
///
/// Every field is `Option<u32>` days; `None` (the default for every field)
/// means "keep forever", so an absent `[retention]` section is a no-op. A
/// set window arms one background prune task (see `spawn_retention_prune`
/// in kb-server) that deletes rows OLDER than `now - window` on a daily
/// tick (and once at boot).
///
/// Only two tables are prunable by age: `history` (keyed on `started_at`)
/// and `reading_sections` (keyed on `last_at`). `reading_sections` is a FK
/// child of `history` — pruning a history row already cascades its section
/// rows away — so `reading_sections_days` is only useful to prune section
/// rows MORE aggressively than their parent visit. `edges` has NO timestamp
/// column, so it CANNOT be time-pruned and is deliberately absent here
/// (dangling edges are swept by the R2 delete cascade + reconcile orphan
/// pass, not by age).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetentionSection {
    /// `history` window in days. `None` → keep history forever (the
    /// default). When set, the prune task deletes every history row whose
    /// `started_at` is older than `now - history_days`, cascading each
    /// row's child `reading_sections`. `Some(0)` is rejected by `validate`
    /// (it would delete ALL history on the next tick).
    pub history_days: Option<u32>,

    /// `reading_sections` window in days. `None` → section rows share their
    /// parent visit's lifetime (the default). When set, the prune task
    /// deletes section rows whose `last_at` is older than `now -
    /// reading_sections_days` WITHOUT touching the parent history row — an
    /// independent, more-aggressive section prune. `Some(0)` is rejected by
    /// `validate`.
    pub reading_sections_days: Option<u32>,
}

impl RetentionSection {
    /// Fixed prune cadence: the background task wakes once per day (plus
    /// once at boot). Retention windows are coarse (whole days), so a finer
    /// cadence buys nothing and a daily wake is negligible. Not operator-
    /// configurable by design — mirrors the metrics ticker's fixed period.
    pub const PRUNE_INTERVAL_SECS: u64 = 24 * 60 * 60;

    /// GC-B6 — bound on `PRAGMA incremental_vacuum(N)` at the end of every
    /// prune run: a per-tick reclaim ceiling (~20 MiB at the default 4 KiB
    /// page size) so a database with a huge one-time backlog of freed pages
    /// can't stall the (single-threaded, one-actor-per-kb) storage actor for
    /// long in one prune tick — it just reclaims the rest on the next daily
    /// tick. Not operator-configurable, same rationale as
    /// `PRUNE_INTERVAL_SECS`.
    pub const INCREMENTAL_VACUUM_PAGES: i64 = 5_000;

    /// GC-B6 — minimum file size before an EXISTING (pre-retention,
    /// `auto_vacuum=NONE`) database pays for the one-time `VACUUM` that
    /// switches it to incremental auto-vacuum. `auto_vacuum` can only be
    /// changed on a database with no data yet, short of a full `VACUUM`
    /// rewrite (see `Db::ensure_incremental_auto_vacuum`); a freshly
    /// migrated kb sqlite file is already ~300 KiB of schema, so the
    /// threshold sits comfortably above that baseline — a brand-new or
    /// lightly used kb never pays for a VACUUM it doesn't need yet.
    pub const AUTO_VACUUM_UPGRADE_MIN_BYTES: i64 = 2 * 1024 * 1024;

    const SECS_PER_DAY: i64 = 24 * 60 * 60;

    /// The `history` window in seconds, or `None` to keep history forever.
    pub fn history_max_age_secs(&self) -> Option<i64> {
        self.history_days.map(|d| d as i64 * Self::SECS_PER_DAY)
    }

    /// The `reading_sections` window in seconds, or `None` (section rows
    /// then share their parent visit's lifetime).
    pub fn reading_sections_max_age_secs(&self) -> Option<i64> {
        self.reading_sections_days
            .map(|d| d as i64 * Self::SECS_PER_DAY)
    }

    /// True when at least one window is set. The daemon spawns the prune
    /// task ONLY when this holds — no idle task when the feature is off.
    pub fn any_window_set(&self) -> bool {
        self.history_days.is_some() || self.reading_sections_days.is_some()
    }
}

/// v0.34 X1 — multi-user attribution (kb-users/1). Daemon-wide like
/// `[retention]`/`[defaults]`: the operator name and trusted identity
/// header are host-level choices. Users are plain username strings used
/// for ATTRIBUTION only (not authorization); unknown users still
/// attribute verbatim via the header/token ladder.
///
/// `operator` and each `users[].name` must already be lowercase-valid
/// (`identity::username_is_valid`) — config validation hard-rejects
/// uppercase rather than silently folding (case forks in append-only
/// history are permanent). Pure helpers live in [`crate::identity`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentitySection {
    /// Attribution for loopback + legacy-shared-token callers.
    /// Default `"operator"`.
    #[serde(default = "default_identity_operator")]
    pub operator: String,

    /// Trusted identity header name (e.g. Traefik/Authelia `Remote-User`).
    /// Default `"Remote-User"`.
    #[serde(default = "default_identity_header")]
    pub header: String,

    /// Optional display metadata; unknown users still attribute verbatim.
    #[serde(default)]
    pub users: Vec<crate::identity::IdentityUser>,
}

fn default_identity_operator() -> String {
    crate::identity::DEFAULT_OPERATOR.to_string()
}

fn default_identity_header() -> String {
    crate::identity::DEFAULT_HEADER.to_string()
}

impl Default for IdentitySection {
    fn default() -> Self {
        Self {
            operator: default_identity_operator(),
            header: default_identity_header(),
            users: Vec::new(),
        }
    }
}

/// GC-B4 — off-host copy step for `kb backup`. `kb backup` on its own
/// only ever writes a local tarball under `<state>/exports/` — a SPOF
/// (see docs/self-host.md "Backups must leave the box"). When both
/// fields are set, the CLI runs `remote_cmd` (with `{src}`/`{dest}`
/// substituted) right after a successful local backup, as a best-effort
/// extra step: a failed remote copy is loudly reported but never fails
/// the backup itself (the local tarball is still the source of truth).
///
/// Both fields are `None` by default — an absent `[backup]` section (or
/// one with either field unset) changes nothing; no remote command ever
/// runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackupSection {
    /// Explicit argv template for the remote-copy command — NOT a shell
    /// string, so no shell is ever invoked and there is no quoting/
    /// injection surface. `{src}` and `{dest}` are substituted (see
    /// `BackupSection::build_argv`) with the local tarball path and
    /// `remote_dest` respectively. E.g. `["rclone", "copyto", "{src}",
    /// "{dest}"]` or `["scp", "{src}", "{dest}"]`.
    #[serde(default)]
    pub remote_cmd: Option<Vec<String>>,

    /// Destination argument substituted for `{dest}`, e.g.
    /// `remote:bucket/path` (rclone) or `user@host:/path/to/backups/`
    /// (scp/rsync). Opaque to kb — passed through verbatim.
    #[serde(default)]
    pub remote_dest: Option<String>,
}

/// MI-W2.1 — `[memory]`: daemon-wide agent-memory scoring knobs. Same
/// precedent as `[retention]`/`[backup]` — an operator-level toggle, not a
/// per-corpus one (a memory's `RecallHit` already spans corpora inside one
/// `/api/memory/recall` fan-out, so a per-kb override would have to pick
/// "whose flag wins" for every cross-corpus merge; the daemon-wide cell
/// sidesteps that entirely).
///
/// **MI-W5.R (2026-08-08 operator ruling, on W5.1 bench evidence)** — the
/// single `scoring_v2` flag was SPLIT into two independent flags because the
/// W5.1 live-corpus bench measured its two factors unequally: **relevance**
/// (25 queries, 14 better / 11 wash / 0 worse, 23/25 returning a different id
/// set, +3% mean latency) is strongly evidenced and now ships ON by default;
/// **stability** (the FSRS term) was NOT exercised at all — every hit had
/// `recall_count == 0` because the `memory_recalls` ledger lives in the
/// sessions corpus, which the probe never loaded — so it stays fixture-tested
/// only and OFF by default pending a bench that actually loads that corpus.
/// A single flag gating both would have shipped the unmeasured factor
/// alongside the measured one the moment an operator flipped it on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySection {
    /// MI-W5.R split of MI-W2.1's relevance term — **default `true`**: the
    /// W5.1 bench measured a real, positive effect on the live corpus (see
    /// the struct doc above), so this factor ships on. `false` drops the
    /// per-corpus min-max normalized search-engine relevance factor from
    /// `kb_core::memory::rerank_with_policy_scored` entirely (not multiplied
    /// by a neutral `1.0` — skipped, so no rounding can creep in).
    #[serde(default = "default_scoring_v2_relevance")]
    pub scoring_v2_relevance: bool,

    /// MI-W5.R split of MI-W2.2's stability term — **default `false`**: the
    /// W5.1 bench never exercised this factor (see the struct doc above), so
    /// it stays off pending a bench that loads the sessions corpus. `true`
    /// folds in the FSRS-inspired stability multiplier derived from the
    /// memory's own W1 `memory_recalls` ledger.
    #[serde(default)]
    pub scoring_v2_stability: bool,

    /// DEPRECATED (MI-W5.R) — the original single flag that gated both
    /// terms together. Kept ONLY as a backward-compatible alias so an
    /// existing `kb.toml` that still sets `scoring_v2 = <bool>` doesn't
    /// silently change meaning now that the two replacement flags have
    /// DIFFERENT defaults: when present, this OVERRIDES both
    /// `scoring_v2_relevance` and `scoring_v2_stability` to its own value
    /// (reproducing the pre-split "one flag gates both" behavior bit-for-
    /// bit) and logs a one-line `tracing::warn!` at parse time
    /// (`KbConfig::from_toml_str` → `MemorySection::apply_deprecated_alias`)
    /// naming the two keys to migrate to. `None` (the key absent — the
    /// normal case for any config written after this split) is a no-op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoring_v2: Option<bool>,
}

fn default_scoring_v2_relevance() -> bool {
    true
}

impl Default for MemorySection {
    fn default() -> Self {
        Self {
            scoring_v2_relevance: true,
            scoring_v2_stability: false,
            scoring_v2: None,
        }
    }
}

impl MemorySection {
    /// MI-W5.R — apply the deprecated `scoring_v2` alias, if present: force
    /// both new flags to its value and log a one-line deprecation warning.
    /// Called once, at parse time. See the `scoring_v2` field doc for the
    /// full rationale.
    fn apply_deprecated_alias(&mut self) {
        if let Some(v) = self.scoring_v2 {
            tracing::warn!(
                value = v,
                "config: [memory] scoring_v2 is deprecated — set scoring_v2_relevance and \
                 scoring_v2_stability independently instead; both are forced to {v} for now"
            );
            self.scoring_v2_relevance = v;
            self.scoring_v2_stability = v;
        }
    }
}

impl BackupSection {
    /// True only when BOTH knobs are set — a partially-configured
    /// section (one field present, the other absent) never runs a
    /// remote copy (see `KbConfig::validate`, which warns on that case).
    pub fn is_configured(&self) -> bool {
        self.remote_cmd.as_ref().is_some_and(|c| !c.is_empty()) && self.remote_dest.is_some()
    }

    /// Build the concrete argv by substituting every `{src}`/`{dest}`
    /// occurrence in each template argument. Returns `None` when the
    /// section isn't fully configured (see `is_configured`).
    pub fn build_argv(&self, src: &Path) -> Option<Vec<String>> {
        if !self.is_configured() {
            return None;
        }
        let src_str = src.to_string_lossy();
        let dest = self.remote_dest.as_deref().unwrap_or_default();
        Some(
            self.remote_cmd
                .as_ref()
                .unwrap()
                .iter()
                .map(|arg| arg.replace("{src}", &src_str).replace("{dest}", dest))
                .collect(),
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonSection {
    /// Override for the daemon name (defaults to short hostname when absent).
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSection {
    /// Listen address. Default `127.0.0.1:4000` per topic 11.
    #[serde(default = "ServerSection::default_addr")]
    pub addr: String,

    /// v0.4 C1 — opt-in mDNS daemon advertise. When `true`, the daemon
    /// publishes `_kb._tcp.local.` with TXT record `v=<crate-version>`,
    /// so other kb daemons / fleet tooling on the same LAN can
    /// auto-discover. Off by default — single-host operators don't
    /// pay the multicast traffic cost. See docs/self-host.md.
    #[serde(default)]
    pub mdns: bool,

    /// v0.5 Q1 — per-route rate-limit overrides. `None` (the default)
    /// keeps the v0.4 baked-in 60/min/token policy on /api/search,
    /// /api/kb/{kb}/atlas/recompute, and /api/kb/{kb}/review/{id}
    /// (POST). Operators can dial individual buckets up or down via
    /// kb.toml `[server.rate_limit]`.
    #[serde(default)]
    pub rate_limit: Option<RateLimitSection>,

    /// v0.6 — host suffix that identifies artifact subdomains. Default
    /// `.artifacts.localhost` matches the local-dev origin. Set to e.g.
    /// `.artifacts.example.com` in production (with a wildcard cert +
    /// DNS in front). See docs/self-host.md.
    #[serde(default = "ServerSection::default_artifact_host_suffix")]
    pub artifact_host_suffix: String,

    /// v0.6 — parent origin (scheme+host[+port]) the SPA is served from.
    /// Used by the origin allowlist for the "trusted parent" case; the
    /// same-origin (Origin==Host) path keeps working regardless. Default
    /// `http://localhost:4000`.
    #[serde(default = "ServerSection::default_parent_origin")]
    pub parent_origin: String,

    /// v0.7.1 — reverse-proxy IPs the daemon trusts to set
    /// `X-Forwarded-For`. Empty by default: the local-only deployment
    /// has no proxy, so the TCP peer IP is authoritative. When the
    /// daemon runs behind a reverse proxy, `X-Forwarded-For` is consulted
    /// only once the immediate peer is a trusted hop — loopback always
    /// counts (a same-host Traefik/Caddy/nginx), and any IP listed here
    /// counts too (e.g. a dockerised proxy reaching the host over a
    /// bridge network). The header is then walked right-to-left, skipping
    /// loopback + listed proxies, so a client cannot spoof the leftmost
    /// entry to claim a loopback origin. Entries that don't parse as an
    /// IP address are dropped with a warn log at boot. See
    /// docs/self-host.md.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,

    /// Y-track — comment attachment limits. `None` (the default) keeps the
    /// built-in defaults (10 MiB/file, 20/comment, 24 h staged-GC grace).
    /// Configured via kb.toml `[server.attachments]`. See docs/self-host.md.
    #[serde(default)]
    pub attachments: Option<AttachmentsSection>,

    /// U1 (v0.25 quick capture) — quick-capture upload limits + the Web
    /// Share Target destination kb. `None` (the default) keeps the
    /// built-in default (10 MiB/file) and no `default_kb` (U2's share-target
    /// route then falls back to the first configured kb). Configured via
    /// kb.toml `[server.capture]`. See docs/self-host.md.
    #[serde(default)]
    pub capture: Option<CaptureSection>,

    /// TM-track — opt-in detailed metrics. Off by default. The coarse
    /// per-route latency histograms + `metrics.tick` SSE are ALWAYS on (cheap
    /// lock-free atomics); this flag additionally enables the richer,
    /// slightly-more-expensive layer surfaced via `GET /api/metrics`:
    /// per-search-stage (embed/bm25/vector/hybrid) + per-kb request timing,
    /// and the ingest-pipeline timing (indexer throughput, index-side embed
    /// latency, storage-actor queue-wait + handler-time). View with `kb
    /// metrics` or the SPA Settings → Traffic tab. See docs/configuration.md.
    #[serde(default)]
    pub metrics: bool,

    /// L2 (v0.24) — retention window, in days, for the daemon's ndjson log
    /// files under `<state>/log/` (see `tracing_init`). Files whose mtime
    /// is older than the window are deleted at boot + daily by the
    /// log-retention sweep. Default 14. Unlike `[retention]` (user data,
    /// opt-in, keep-forever default) this is ALWAYS on: logs are the
    /// daemon's own diagnostics, and an unbounded daily-rolled ndjson dir
    /// is the L1 disk-fill risk — the same rationale that pins the file
    /// layer to `info`. Must be ≥ 1 (`validate()` hard-rejects 0 — it
    /// would reap every log file, including today's active one).
    #[serde(default = "ServerSection::default_log_retention_days")]
    pub log_retention_days: u32,

    /// PF-R1 — concurrency cap for federated (`scope=all`) read handlers'
    /// per-corpus fan-out (invariant #28's `routes::buffered_join`).
    /// Default 8 (matches the pre-existing hardcoded `routes::FANOUT_CAP`
    /// kb-server ships with — see docs/configuration.md). Bounds the
    /// `spawn_blocking` pool + embedder IPC pressure a single scope=all
    /// request puts on the daemon; raising it trades that headroom for
    /// lower fan-out latency on many-kb fleets. `0` is accepted (the
    /// consumer clamps to at least 1 — see `buffered_join`), never a
    /// panic or unbounded concurrency.
    #[serde(default = "ServerSection::default_fanout_cap")]
    pub fanout_cap: usize,
}

/// Per-route rate-limit overrides. Each field overrides the v0.4
/// default of 60 req/min/token; `None` keeps the default.
///
/// The `history` family (open/scroll/search) has a HIGHER default
/// because scroll updates are debounced to 1/s in the SPA — under
/// active reading we expect ~60-180/min legitimately, and the
/// limiter exists to bound an adversarial / buggy client, not throttle
/// real use. Deep-review M7 added the gate; pre-M7 these routes were
/// unrate-limited and a runaway tab could fan out hundreds of QPS into
/// sqlite writes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RateLimitSection {
    pub search: Option<u32>,
    pub atlas_recompute: Option<u32>,
    pub review_post: Option<u32>,
    /// Per-token cap for `/api/kb/{kb}/history/{open,scroll,search}`.
    /// Defaults to 600/min (~10/s) — well above legitimate use.
    pub history_post: Option<u32>,
}

impl RateLimitSection {
    pub const DEFAULT_PER_MIN: u32 = 60;
    pub const DEFAULT_HISTORY_PER_MIN: u32 = 600;
    /// R5 — the review family fans one logical edit into several
    /// fine-grained requests (add + reply + resolve + edit are now
    /// separate calls, not one whole-document POST). A burst of
    /// annotations on a non-loopback (token-authed) daemon would 429
    /// under the shared 60/min cap, so the review family gets a higher
    /// default. Loopback (local CLI/SPA) bypasses rate-limiting entirely.
    pub const DEFAULT_REVIEW_PER_MIN: u32 = 240;

    pub fn search_per_min(&self) -> u32 {
        self.search.unwrap_or(Self::DEFAULT_PER_MIN)
    }

    pub fn atlas_per_min(&self) -> u32 {
        self.atlas_recompute.unwrap_or(Self::DEFAULT_PER_MIN)
    }

    pub fn review_per_min(&self) -> u32 {
        self.review_post.unwrap_or(Self::DEFAULT_REVIEW_PER_MIN)
    }

    pub fn history_per_min(&self) -> u32 {
        self.history_post.unwrap_or(Self::DEFAULT_HISTORY_PER_MIN)
    }
}

/// Y-track — comment attachment limits (`[server.attachments]`). Each
/// `None` field falls back to the built-in default in `crate::attachments`.
/// The allowed content types are a fixed allowlist in v1 (raster images +
/// PDF + UTF-8 text), not operator-configurable — see `attachments::sniff_allowed`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AttachmentsSection {
    /// Max bytes for a single uploaded attachment. Default 10 MiB.
    pub max_file_bytes: Option<u64>,
    /// Max attachments adopted onto one comment or reply. Default 20.
    pub max_per_comment: Option<usize>,
    /// Hours a staged-but-never-adopted blob survives before GC. Default 24.
    pub gc_grace_hours: Option<i64>,
}

impl AttachmentsSection {
    pub fn max_file_bytes(&self) -> u64 {
        self.max_file_bytes
            .unwrap_or(crate::attachments::DEFAULT_MAX_FILE_BYTES)
    }

    pub fn max_per_comment(&self) -> usize {
        self.max_per_comment
            .unwrap_or(crate::attachments::DEFAULT_MAX_PER_COMMENT)
    }

    pub fn gc_grace_hours(&self) -> i64 {
        self.gc_grace_hours
            .unwrap_or(crate::attachments::DEFAULT_GC_GRACE_HOURS)
    }
}

/// U1 (v0.25 quick capture) — `[server.capture]` upload limits + the Web
/// Share Target destination kb.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptureSection {
    /// Max bytes for a SINGLE captured file. Default 10 MiB (same default
    /// as `[server.attachments]`, a distinct knob — captures and comment
    /// attachments are unrelated upload surfaces).
    pub max_file_bytes: Option<u64>,
    /// U2 follow-up — max COMBINED bytes for one capture request (the
    /// endpoint accepts a multi-file batch, so this is a distinct budget
    /// from `max_file_bytes`, which bounds only one file within the
    /// batch). Default 64 MiB. Sets the `DefaultBodyLimit` on both capture
    /// routes (`router.rs`) — `max(max_request_bytes, max_file_bytes +
    /// slack)`, so a generous `max_file_bytes` still widens the request
    /// cap even if this key is left at its default.
    pub max_request_bytes: Option<u64>,
    /// Destination kb for the Web Share Target route (`POST /capture`,
    /// U2), which carries no `{kb}` path segment. `None` → the daemon's
    /// first configured kb (`KbConfig.kb`'s first `BTreeMap` entry, i.e.
    /// lexicographically-first name — deterministic without this key set).
    pub default_kb: Option<String>,
}

impl CaptureSection {
    pub fn max_file_bytes(&self) -> u64 {
        self.max_file_bytes
            .unwrap_or(crate::capture::DEFAULT_MAX_FILE_BYTES)
    }

    pub fn max_request_bytes(&self) -> u64 {
        self.max_request_bytes
            .unwrap_or(crate::capture::DEFAULT_MAX_REQUEST_BYTES)
    }
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            addr: Self::default_addr(),
            mdns: false,
            rate_limit: None,
            artifact_host_suffix: Self::default_artifact_host_suffix(),
            parent_origin: Self::default_parent_origin(),
            trusted_proxies: Vec::new(),
            attachments: None,
            capture: None,
            metrics: false,
            log_retention_days: Self::default_log_retention_days(),
            fanout_cap: Self::default_fanout_cap(),
        }
    }
}

impl ServerSection {
    fn default_addr() -> String {
        "127.0.0.1:4000".to_string()
    }

    /// L2 — two weeks of daily ndjson files. Enough to debug "it was slow
    /// last Tuesday" without letting the log dir grow forever.
    fn default_log_retention_days() -> u32 {
        14
    }

    /// PF-R1 — mirrors kb-server's pre-existing hardcoded `routes::FANOUT_CAP`
    /// so an absent/default config is byte-identical to today's behaviour.
    fn default_fanout_cap() -> usize {
        8
    }

    fn default_artifact_host_suffix() -> String {
        ".artifacts.localhost".to_string()
    }

    fn default_parent_origin() -> String {
        Self::DEFAULT_PARENT_ORIGIN.to_string()
    }

    /// The `parent_origin` default. Exposed so kb-server can recognise
    /// it as the "not configured for production" sentinel — see the
    /// `postmessage_target` / `artifact_csp_header` helpers in
    /// `routes/artifact.rs`. The dev SPA reaches the daemon from
    /// either `localhost:4000` or `127.0.0.1:4000`; treating either
    /// of those as a strict postMessage target would silently drop
    /// messages whenever the user happened to be at the other.
    pub const DEFAULT_PARENT_ORIGIN: &'static str = "http://localhost:4000";
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiSection {
    /// Theme: `paper` or `ink`.
    pub theme: Option<String>,
    /// Accent identifier from a fixed palette.
    pub accent: Option<String>,
    /// Density: `compact`, `normal`, `spacious`.
    pub density: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KbSection {
    /// Source folder watched by this kb.
    pub path: PathBuf,

    /// Glob patterns to skip during indexing (e.g. `.git`, `*.tmp`).
    #[serde(default)]
    pub skip_patterns: Vec<String>,

    /// Per-kb UI overrides.
    #[serde(default)]
    pub ui: UiSection,

    /// Embedding model name (per topic 02 §Decisions). Must match a
    /// `kb_core::embed::SUPPORTED_MODELS` entry. If `None`, this kb has no
    /// embedder — indexer skips the embed step (Doc.embedding stays null),
    /// and `/api/search?mode=hybrid|semantic` returns 400.
    pub embedding_model: Option<String>,

    /// SQ4 — optional cross-encoder reranker name (must match a
    /// `kb_core::embed::SUPPORTED_RERANKERS` entry). When set, the daemon
    /// loads the reranker in-process and re-orders the fused top-N of
    /// hybrid/semantic search results. Opt-in per kb (the model is ~1 GB);
    /// `None` = no reranking. Failure to load is non-fatal — the daemon
    /// logs and search continues unranked-by-cross-encoder.
    #[serde(default)]
    pub reranker_model: Option<String>,

    /// SQ5 — opt-in passage/chunk embeddings. When true, the indexer also
    /// embeds each document's passages into a sibling `artifact_chunks`
    /// table and semantic/hybrid search vector-queries the chunks (fixing
    /// the 512-token whole-body truncation for long artifacts). Off by
    /// default — the extra embeds multiply reindex cost; enable per kb and
    /// run `kb reindex` to populate the chunk table.
    #[serde(default)]
    pub chunked_embeddings: bool,

    /// GS-track — opt-in graph-degree ranking signal. When set, hybrid
    /// search adds a small additive post-fusion boost per hit:
    /// `weight/60 × sqrt(in_degree)/sqrt(max_in_degree)` over the kb's
    /// `edges` table (the same in-degree behind the backlinks chip).
    /// Query-time only — flipping it needs no reindex. Bench-gated: the
    /// ship rule is "no regression on `kb bench`, or it stays off";
    /// `None` (the default) = off. Sane weights are (0, 4]; values
    /// outside warn at validate time and still apply verbatim.
    #[serde(default)]
    pub graph_boost: Option<f32>,

    /// v0.3 G3 — outbound scrubbing rules applied when an artifact's
    /// HTML leaves the daemon for a non-loopback origin (e.g. when
    /// proxied to an outside reviewer or pasted into an external
    /// service). Loopback requests skip the layer entirely. `None`
    /// means "no scrubbing" (the v0.0.1 behavior).
    #[serde(default)]
    pub outbound: Option<OutboundSection>,

    /// v0.5 P2 — per-kb atlas overrides. `None` keeps the v0.3
    /// defaults (UMAP layout, √n cluster count). Set k explicitly
    /// to surface a fixed cluster count in the SPA legend; pick
    /// "pca" when the corpus's UMAP layout collapses (rare, but
    /// the fallback is useful as an explicit override).
    #[serde(default)]
    pub atlas: Option<AtlasSection>,

    /// v0.7 N1 — named HTML templates resolvable by `kb new --template
    /// <name>`. Keys are short names (e.g. "idea", "fix"); values are
    /// paths to template files on disk. Path resolution is verbatim —
    /// relative paths are interpreted against the current working
    /// directory at `kb new` invocation, so prefer absolute paths.
    /// Templates contain `{{title}}` / `{{date}}` / `{{slug}}` / `{{key}}`
    /// placeholders; see `kb new --help` for the substitution rules.
    #[serde(default)]
    pub templates: BTreeMap<String, PathBuf>,

    /// v0.9 M2 — marks this corpus as an agent-memory corpus and sets
    /// its scope: `"global"` (cross-project memories) or `"project"`
    /// (this-project memories). `None` → an ordinary artifact corpus.
    /// Read by `GET /api/memory/recall` to resolve the fan-out set.
    #[serde(default)]
    pub memory_scope: Option<String>,

    /// R0-opt-in — per-kb default for `GET /api/search`'s `category` filter
    /// (`routes/search.rs` `Filters::keep`). Sessions are hidden from search
    /// unless the caller explicitly asks for them
    /// (`?category=memory-session`); that's correct for `scope=all`
    /// (federated search must not get polluted with transcript noise) but
    /// bad UX once a user has directly scoped to this kb. Set this to
    /// `"memory-session"` on a sessions corpus and a `?category`-less
    /// `scope=one` request against it defaults to that category, so
    /// transcripts are searchable without typing it every time. `None` →
    /// no default (today's behavior). Applies ONLY to the single-kb search
    /// path; `scope=all` never resolves or applies it (R0 stays
    /// default-exclude there).
    #[serde(default)]
    pub default_search_category: Option<String>,

    /// DCB — base URL of the kb-code daemon that indexes the repos this
    /// corpus's docs cite (e.g. `https://kbc.example.com`, or
    /// `http://127.0.0.1:4747` for a colocated local dev daemon). kb NEVER
    /// opens an HTTP client to it (invariant #2/#4 — one live call
    /// direction, kb-code→kb): this value is INERT config, surfaced on
    /// `GET /api/kbs` so the SPA's Code section knows where to send the
    /// browser's own fetch. `None` ⇒ the Code section renders extracted
    /// refs as inert rows with "not linked to a code repo" — never an
    /// error, never a blocked tab.
    ///
    /// Field shape copied from `ProjectSection::code_url`.
    /// `ProjectSection::code_repo` is DELIBERATELY NOT carried over here: a
    /// singular repo name fights the multi-checkout design and reintroduces
    /// exactly the config-file repo mapping Decision 1 forbids — the repo
    /// LIST comes from kb-code's own scorecard, picked per read-time by a
    /// human, never pinned in config.
    #[serde(default)]
    pub code_url: Option<String>,

    /// v0.13 — per-kb decay-policy override. Accepts `"strict"` /
    /// `"balanced"` / `"loose"`; anything else logs a warn and falls
    /// back to the daemon-wide default. Only meaningful on a memory
    /// corpus; ignored on regular kbs (the recall route only consults
    /// it for in-scope memory corpora). `None` → use the daemon cell
    /// from `<state>/memory-policy.json`.
    #[serde(default)]
    pub decay_policy: Option<String>,

    /// Track V — per-kb source for the artifact Versions/Diff timeline:
    /// `"auto"` (default) / `"git"` / `"index"` / `"both"` / `"off"`.
    /// `auto` uses git history when the file is tracked and falls back to
    /// kb index snapshots otherwise; `both` unions them; `git`/`index`
    /// force one source; `off` disables the feature for this kb. Unknown
    /// values warn at validate time and the daemon falls back to `auto`.
    /// `None` → `auto`. Resolved to `kb_core::vcs::VersionsMode` at boot
    /// and memoised on `KbContext`.
    #[serde(default)]
    pub versions: Option<String>,

    /// RP-track — per-kb reading-progress capture toggle. Default ON
    /// (`None` → true). When false, `POST …/history/reading` no-ops (204)
    /// so nothing per-section is stored; scroll-resume + the TOC mini-spy
    /// are unaffected (they predate this and aren't gated). `kb history
    /// purge` clears any captured rows.
    #[serde(default)]
    pub reading_progress: Option<bool>,

    /// GC-D1 — spike: opt-in typo tolerance for this kb's keyword (BM25)
    /// search arm. See `SearchSection`. Off by default — a bare `[kb.*]`
    /// (or one with an empty `[kb.*.search]`) behaves exactly as before.
    #[serde(default)]
    pub search: SearchSection,

    /// X1 (v0.24) — per-kb indexable-extension override: a TOML table of bare
    /// extension → parse-pipeline name (`"html"` / `"markdown"`). `None` (the
    /// default) inherits the daemon-wide `[indexer] indexable_extensions`,
    /// else the built-in default set. When set, it REPLACES (does not merge
    /// with) the inherited map for this kb — so a kb that maps only
    /// `txt = "markdown"` indexes `.txt` and nothing else. Resolved by
    /// [`KbConfig::resolved_extension_map`]; `validate` HARD-rejects unknown
    /// pipeline names / empty|dotted keys. See [`crate::extmap`].
    #[serde(default)]
    pub indexable_extensions: Option<BTreeMap<String, String>>,

    /// PF-I1 — per-kb override of the daemon-wide `[indexer] reconcile_secs`
    /// periodic-reconciliation interval (see `IndexerSection::reconcile_secs`
    /// for what the pass does — same semantics apply here: `Some(0)` disables
    /// the background walk for JUST this kb, and the explicit
    /// `POST /api/kb/{kb}/reindex` endpoint still works regardless). `None`
    /// (the default) inherits the daemon-wide value. Resolved by
    /// [`IndexerSection::resolved_reconcile_secs_for`], where the
    /// `KB_RECONCILE_SECS` env var STILL trumps everything — including this
    /// per-kb override — since it's the daemon-wide test/container escape
    /// hatch and a per-kb knob must never weaken it. The periodic
    /// auto-compact ticker (kb-server's `compact_check_interval_secs`) is
    /// ALSO spawned per kb and follows this same resolved value for free.
    #[serde(default)]
    pub reconcile_secs: Option<u64>,

    /// U1 (v0.25 quick capture) — per-kb subfolder captures land in,
    /// relative to `path`. `None` → [`crate::capture::DEFAULT_CAPTURE_DIR`]
    /// (`"capture"`). Resolved by [`KbSection::resolved_capture_dir`].
    #[serde(default)]
    pub capture_dir: Option<String>,

    /// W2.9 — per-kb override of the resurfacing queue's scoring weights
    /// (`kb_core::resurface::ResurfaceWeights`). `None` (or any absent
    /// field within) keeps the shipped defaults. Tuning-only — like
    /// `graph_boost`, a nonsensical value warns at `validate()` time and
    /// boot still applies the shipped default for that ONE field (never
    /// fails to boot). Resolved by
    /// [`crate::resurface::ResurfaceWeights::from_section`] and cached on
    /// `KbContext` at daemon bring-up.
    #[serde(default)]
    pub resurface: Option<ResurfaceSection>,

    /// CT-F5 — per-kb corpus-health SLO targets. Its own section (the
    /// `[kb.<name>.atlas]`/`[kb.<name>.search]`/`[kb.<name>.resurface]`
    /// precedent) rather than four loose `KbSection` fields, because the four
    /// keys are one concept and a future indicator belongs beside them.
    ///
    /// EVERY key is optional and the whole section is optional: a kb with no
    /// `[slo]` still gets a full four-indicator report from
    /// `GET /api/kb/{kb}/slo` — every indicator MEASURED, every status
    /// `unknown` for want of a target. Configuring a target adds a verdict; it
    /// never adds an indicator, and it never changes behaviour anywhere
    /// (surfaced, never enforced — see [`crate::slo`]).
    #[serde(default)]
    pub slo: Option<SloSection>,
}

/// CT-F5 — `[kb.<name>.slo]`: the four operator-seeded corpus-health targets.
///
/// Each key is named EXACTLY for its indicator
/// ([`crate::slo::SloKey::as_str`]) — the config key, the JSON `key`, the
/// `slo_snapshots.indicator` value and the CLI column header are one string,
/// so there is never a translation table between "what I configured" and
/// "what the report calls it". The DIRECTION each target reads (minimum vs
/// maximum) is a property of the indicator, not of the config, and lives on
/// [`crate::slo::SloKey::direction`]; it is documented per field below and in
/// `docs/configuration.md`.
///
/// Nonsensical values (a negative count, a percentage outside 0..=100) warn
/// at `validate()` time and are still applied VERBATIM — the `graph_boost`
/// precedent. A target is an operator's claim about their own corpus; kb
/// reports against it rather than second-guessing it, and an out-of-range
/// target that pins an indicator permanently to `warn` (or permanently to
/// `ok`) is a self-correcting mistake, never a boot failure.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct SloSection {
    /// MINIMUM acceptable percentage of extracted code-ref hints that carry a
    /// local-tree path shape. `None` = measured, unjudged.
    #[serde(default)]
    pub coderef_resolution_pct: Option<f64>,

    /// MAXIMUM acceptable number of docs whose `kb_session` resolves to no
    /// `sessions` row anywhere on this daemon. `None` = measured, unjudged.
    #[serde(default)]
    pub orphan_kb_sessions: Option<f64>,

    /// MAXIMUM acceptable percentage of injected recall hits that parsed via
    /// neither the `kb-recall/1` marker nor the free-text fallback. `None` =
    /// measured, unjudged.
    #[serde(default)]
    pub ledger_parse_failure_pct: Option<f64>,

    /// MAXIMUM acceptable hours between now and the newest captured session's
    /// `started_at`. `None` = measured, unjudged.
    #[serde(default)]
    pub capture_freshness_hours: Option<f64>,
}

impl SloSection {
    /// Project onto the pure [`crate::slo::SloTargets`] the indicator
    /// computation takes. A straight field copy — the two types are kept
    /// separate so `kb_core::slo` stays free of any config/serde dependency
    /// and every indicator test can build targets without a TOML round-trip.
    pub fn targets(&self) -> crate::slo::SloTargets {
        crate::slo::SloTargets {
            coderef_resolution_pct: self.coderef_resolution_pct,
            orphan_kb_sessions: self.orphan_kb_sessions,
            ledger_parse_failure_pct: self.ledger_parse_failure_pct,
            capture_freshness_hours: self.capture_freshness_hours,
        }
    }
}

/// GC-D1 — `[kb.<name>.search]`: query-time keyword-search knobs. Currently
/// one field; structured as its own section (mirroring `[kb.<name>.atlas]`/
/// `[kb.<name>.outbound]`) so future search-tuning knobs land here without
/// another schema break.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchSection {
    /// When `true`, the BM25 arm (`mode=keyword`, and the keyword half of
    /// `mode=hybrid`) matches terms through lance's native fuzzy full-text
    /// query (`FullTextSearchQuery::new_fuzzy`, max edit distance 1 — a
    /// Levenshtein automaton over the FTS index's own term dictionary, not
    /// a hand-rolled one) instead of the exact-match query. `false` (the
    /// default) is byte-identical to the pre-GC-D1 query — deterministic
    /// tie order (root invariant #1/GC-B1) is unaffected either way.
    /// Bench findings: `docs/research/typo-tolerance-spike-2026-07.html`.
    #[serde(default)]
    pub typo_tolerance: bool,
}

impl KbSection {
    /// Resolved reading-progress capture flag (default ON when unset).
    pub fn reading_progress_enabled(&self) -> bool {
        self.reading_progress.unwrap_or(true)
    }

    /// U1 — resolved capture subfolder (default `"capture"`).
    pub fn resolved_capture_dir(&self) -> &str {
        self.capture_dir
            .as_deref()
            .unwrap_or(crate::capture::DEFAULT_CAPTURE_DIR)
    }
}

/// Per-kb atlas overrides. Both fields optional — leaving either
/// `None` falls back to the v0.3 defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AtlasSection {
    /// Override the √n cluster count (clamped to [1, MAX_CLUSTERS]).
    pub k: Option<usize>,
    /// `"umap"` (default) or `"pca"`. Unknown values fall back to
    /// `"umap"` with a warn log at boot time.
    pub layout: Option<String>,
}

/// W2.9 — `[kb.<name>.resurface]`: per-kb override of the resurfacing
/// queue's scoring weights. All five fields independently optional
/// (mirroring `AtlasSection`) — an absent field keeps the shipped
/// default for THAT knob alone. See `kb_core::resurface::ResurfaceWeights`
/// for the semantics of each field and `ResurfaceWeights::from_section`
/// for the warn+fallback resolution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResurfaceSection {
    /// Weight of the open-comments term. Default `0.6`.
    pub comment_weight: Option<f32>,
    /// Weight of the unfinished-read term. Default `0.4`.
    pub read_weight: Option<f32>,
    /// Open-comment count where the comment term saturates at 1.0.
    /// Default `4`.
    pub comment_saturation: Option<u32>,
    /// Half-life (days) of the unfinished-read term's decay. Default
    /// `45.0`.
    pub read_halflife_days: Option<f32>,
    /// Items scoring below this are dropped entirely. Default `0.05`.
    pub score_floor: Option<f32>,
}

/// Per-kb outbound scrubbing config. Both knobs are independently
/// optional — a kb can drop just the kb-prompt without any regex
/// rules, or vice versa.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutboundSection {
    /// When `true`, the artifact serve middleware drops any
    /// `<template id="kb-prompt">` element (the prompt-leak guard
    /// from topic 02 §Decisions). Off by default — local development
    /// usually wants the prompt visible in DevTools.
    #[serde(default)]
    pub strip_kb_prompt: bool,

    /// Regex-based redactions applied in order. Each rule's pattern
    /// is searched and replaced (across the full body, not per line)
    /// before the response is sent. Compiled lazily + cached on
    /// first request per kb.
    #[serde(default)]
    pub redactions: Vec<RegexRule>,
}

/// A single redaction rule. The pattern is parsed by the `regex`
/// crate; an invalid pattern silently no-ops + logs at warn level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegexRule {
    pub pattern: String,
    pub replacement: String,
}

/// `kb share` publishing config. Both hosts are independently optional;
/// `live_origin` backs `--links absolute` permalink rewrites. The share
/// engine runs inside the daemon, so the secrets named here are read from
/// the *daemon's* environment (injected at launch via pass), not the CLI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShareSection {
    /// Cloudflare Pages + Access (gated shares). `None` → `kb share
    /// --host cloudflare-pages` errors with a setup hint.
    #[serde(default)]
    pub cloudflare: Option<CloudflareShareConfig>,

    /// GitHub Pages (public shares). `None` → `kb share --host
    /// github-pages` errors with a setup hint.
    #[serde(default)]
    pub github: Option<GithubShareConfig>,

    /// Live kb origin (e.g. `https://kb.example.com`) used to rewrite
    /// out-of-share cross-artifact links to permalinks under
    /// `--links absolute`. Required only when that mode is used.
    #[serde(default)]
    pub live_origin: Option<String>,
}

/// Cloudflare account + token-source for Pages Direct Upload + Access.
/// The API token is NOT stored here — `api_token_env` names the
/// environment variable the daemon reads it from at share time (never
/// inlined in kb.toml; kb has no `pass://` resolver, by design).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflareShareConfig {
    /// Cloudflare account id — every API path is `/accounts/{id}/…`.
    pub account_id: String,
    /// Zero-Trust team domain `<team>.cloudflareaccess.com` — the login
    /// origin for gated shares.
    pub team_domain: String,
    /// Pre-registered Google identity-provider UUID (`--gate google`).
    #[serde(default)]
    pub google_idp: Option<String>,
    /// Pre-registered GitHub identity-provider UUID (`--gate github`).
    #[serde(default)]
    pub github_idp: Option<String>,
    /// Env var holding the API token. Default `KB_CF_API_TOKEN`.
    #[serde(default = "default_cf_token_env")]
    pub api_token_env: String,
}

fn default_cf_token_env() -> String {
    "KB_CF_API_TOKEN".to_string()
}

impl CloudflareShareConfig {
    /// Read the Cloudflare API token from the configured env var.
    pub fn api_token(&self) -> Result<String> {
        read_secret_env(&self.api_token_env, "Cloudflare API token")
    }
}

/// GitHub account + token-source for the public Pages lane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubShareConfig {
    /// Owner (user or org) under which share repos are created.
    pub owner: String,
    /// Env var holding the token. Default `KB_GH_TOKEN`.
    #[serde(default = "default_gh_token_env")]
    pub token_env: String,
}

fn default_gh_token_env() -> String {
    "KB_GH_TOKEN".to_string()
}

impl GithubShareConfig {
    /// Read the GitHub token from the configured env var.
    pub fn token(&self) -> Result<String> {
        read_secret_env(&self.token_env, "GitHub token")
    }
}

/// Read a secret from the named environment variable. kb does NOT
/// resolve `pass://` references itself (its precedent is env-only): the
/// daemon's launcher injects the value (the compose `environment:` from
/// `~/deploy/.env`, or systemd `EnvironmentFile=`), itself populated
/// from Pass out-of-band. The error points the operator there.
pub fn read_secret_env(var: &str, what: &str) -> Result<String> {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(crate::Error::Config(format!(
            "{what} not found: set ${var} in the daemon's environment \
             (inject it at launch via pass — e.g. the compose `environment:` \
             from ~/deploy/.env, or systemd EnvironmentFile=)"
        ))),
    }
}

/// X1 — `[webhooks]` reactive event-bridge config. One daemon-wide task
/// subscribes to the event firehose and POSTs every envelope whose `type`
/// is in `types` to `url`. Deliberately a single endpoint + an explicit
/// allowlist (never "all events") so a misconfig can't fan the whole
/// firehose at an unsuspecting URL. The bridge is fire-and-forget and
/// eventually-consistent — a slow endpoint gets dropped events
/// (`broadcast` lag), never daemon backpressure (the bounded-bus
/// invariant). Outbound targets are SSRF-gated by
/// [`crate::webhook_url::prepare_webhook_dial`] (loopback + public OK;
/// RFC1918/ULA need `allow_private`; link-local/metadata always denied).
/// See `docs/extending.md`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebhooksSection {
    /// Destination URL. Must be `http://` or `https://`. Empty disables
    /// the bridge (no task spawned).
    pub url: String,

    /// Event-type allowlist — exact match on the envelope `type`
    /// (`artifact.indexed`, `comment.added`, `session.captured`, …).
    /// Empty forwards nothing (the bridge stays idle).
    #[serde(default)]
    pub types: Vec<String>,

    /// Per-request timeout in milliseconds. `None` → 5000. The await is
    /// bounded by this so one hung endpoint can't wedge the subscriber.
    #[serde(default)]
    pub timeout_ms: Option<u64>,

    /// When `false` (default), only loopback + public unicast. When
    /// `true`, also RFC1918 + ULA. Link-local / cloud metadata always
    /// refused. Set only for intentional LAN receivers.
    #[serde(default)]
    pub allow_private: bool,
}

/// Severity of a config validation issue. `Hard` blocks a write — the
/// config would fail to boot or behave dangerously (an unbindable addr,
/// an uncompilable redaction regex). `Warn` is advisory: the daemon
/// already tolerates it at boot by falling back (an unknown model name,
/// a not-yet-created kb dir), so the write proceeds but the editor
/// surfaces the note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Hard,
    Warn,
}

/// A single config validation finding. `pointer` is a JSON-pointer-style
/// path into the config (`/server/addr`, `/kb/canon/atlas/layout`) so the
/// web editor can map the issue back to the offending field; the
/// `PUT /api/config` handler echoes these in its problem+json body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub pointer: String,
    pub message: String,
    pub severity: Severity,
}

impl ValidationIssue {
    fn hard(pointer: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            pointer: pointer.into(),
            message: message.into(),
            severity: Severity::Hard,
        }
    }
    fn warn(pointer: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            pointer: pointer.into(),
            message: message.into(),
            severity: Severity::Warn,
        }
    }
    pub fn is_hard(&self) -> bool {
        self.severity == Severity::Hard
    }
}

impl KbConfig {
    /// Read and parse a `kb.toml` from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_toml_str(&raw)
    }

    /// Parse from a TOML string. (Named `from_toml_str` to sidestep the
    /// `FromStr` trait method-name collision flagged by clippy.)
    pub fn from_toml_str(s: &str) -> Result<Self> {
        let mut cfg: Self = toml::from_str(s)?;
        // MI-W5.R — deprecated `[memory] scoring_v2` alias, applied once at
        // parse time so every caller (boot, `save_preserving`'s reload, the
        // web config editor) sees it without re-checking.
        cfg.memory.apply_deprecated_alias();
        Ok(cfg)
    }

    /// Serialise to TOML.
    pub fn to_string_pretty(&self) -> Result<String> {
        toml::to_string_pretty(self)
            .map_err(|e| crate::Error::Config(format!("toml serialize: {e}")))
    }

    /// D1 — resolve the embedding-model name for a kb section, walking
    /// the three-layer precedence chain:
    ///
    /// 1. **Per-kb** — `kb_section.embedding_model` wins if set to a name
    ///    that exists in `embed::SUPPORTED_MODELS`.
    /// 2. **Daemon defaults** — `[defaults] embedding_model` wins next if
    ///    set to a known name.
    /// 3. **Registry default** — `embed::default_model()` (currently
    ///    `bge-small-en-v1.5`) is the final fallback so a freshly
    ///    `kb add`-ed kb gets semantic + hybrid search out of the box.
    ///
    /// Unknown names at any layer are skipped (they don't pre-empt later
    /// layers); the daemon also logs a `warn` for an unknown name in
    /// `[defaults]` at boot. Returns `None` only if a future refactor
    /// removes the `default: true` flag from every entry in
    /// `SUPPORTED_MODELS` — defensive, so the daemon falls back to
    /// today's "no embedder" behaviour rather than panicking.
    ///
    /// Tests that need the legacy raw-lookup behaviour (no fallback)
    /// should read `kb_section.embedding_model` directly.
    pub fn resolved_embedding_model<'a>(&'a self, kb_section: &'a KbSection) -> Option<&'a str> {
        if let Some(name) = kb_section.embedding_model.as_deref() {
            if crate::embed::model_info(name).is_some() {
                return Some(name);
            }
        }
        if let Some(name) = self.defaults.embedding_model.as_deref() {
            if crate::embed::model_info(name).is_some() {
                return Some(name);
            }
        }
        if self.defaults.disable_embedder_fallback {
            return None;
        }
        crate::embed::SUPPORTED_MODELS
            .iter()
            .find(|m| m.default)
            .map(|m| m.name)
    }

    /// X1 — resolve the indexable-extension map for a kb section, walking the
    /// same three-layer precedence as [`Self::resolved_embedding_model`]:
    ///
    /// 1. **Per-kb** — `kb_section.indexable_extensions`, if set and every
    ///    entry is valid, wins outright (it REPLACES, not merges).
    /// 2. **Daemon default** — `[indexer] indexable_extensions`, if set and
    ///    valid, is next.
    /// 3. **Built-in default** — [`crate::extmap::ExtensionMap::default`]
    ///    (today's `html`/`htm` → HTML, `md`/`markdown` → Markdown).
    ///
    /// Defensive like the embedding resolver: an INVALID layer (a hand-edited
    /// file that skipped `validate`) is logged + skipped, falling through to
    /// the next — the daemon boots on the built-in default rather than
    /// crashing. `validate` is the write-time gate that turns the same
    /// badness into a hard rejection.
    pub fn resolved_extension_map(&self, kb_section: &KbSection) -> crate::extmap::ExtensionMap {
        for (raw, layer) in [
            (&kb_section.indexable_extensions, "kb"),
            (&self.indexer.indexable_extensions, "[indexer]"),
        ] {
            if let Some(raw) = raw {
                // A present-but-EMPTY table (`[indexer.indexable_extensions]`
                // with every row commented out, or a bare header) parses to
                // `Some(empty)` — distinct from `None`. It must NOT win as an
                // empty map: an empty map is indexable-of-nothing, which the
                // reconcile delete pass reads as "every existing file is
                // de-mapped" and mass-deletes the whole kb's index. Treat it
                // like an invalid layer — warn + fall through to the next
                // (ultimately the built-in default), never returning it.
                // `validate` HARD-rejects the same shape at write time.
                if raw.is_empty() {
                    tracing::warn!(
                        layer,
                        "ignoring empty indexable_extensions table; falling back to the next layer",
                    );
                    continue;
                }
                match crate::extmap::ExtensionMap::from_config(raw) {
                    Ok(m) => return m,
                    Err(e) => tracing::warn!(
                        layer,
                        error = %e,
                        "ignoring invalid indexable_extensions; falling back to the next layer",
                    ),
                }
            }
        }
        crate::extmap::ExtensionMap::default()
    }

    /// Atomically write to disk via tmpfile + rename. Re-renders the
    /// whole file from the struct, so any comments/formatting in an
    /// existing file are lost — `kb add` uses this. The web config
    /// editor uses `save_preserving` instead.
    pub fn save(&self, path: &Path) -> Result<()> {
        let body = self.to_string_pretty()?;
        crate::fsx::write_atomic(path, body.as_bytes())
    }

    /// Like `save`, but preserves comments + key ordering in an existing
    /// file by splicing changed values via `toml_edit` rather than
    /// re-rendering from scratch (root CLAUDE.md invariant #12 — kb
    /// values byte-preserving edits). Falls back to a full
    /// `to_string_pretty` render when the target doesn't exist yet.
    /// Atomic (tmpfile + rename) like `save`.
    pub fn save_preserving(&self, path: &Path) -> Result<()> {
        let body = match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut doc = text.parse::<toml_edit::DocumentMut>().map_err(|e| {
                    crate::Error::Config(format!("parse existing kb.toml for edit: {e}"))
                })?;
                // Diff against the file's CURRENT config so we splice only
                // the fields that actually changed — unchanged lines (and
                // their comments, inline ones included) stay byte-identical,
                // and defaults the operator omitted aren't materialised
                // (no `ui = {}` / `parent_origin = …` cruft on first save).
                let old = Self::from_toml_str(&text).unwrap_or_default();
                let old_doc = toml_edit::ser::to_document(&old)
                    .map_err(|e| crate::Error::Config(format!("toml serialize: {e}")))?;
                let new_doc = toml_edit::ser::to_document(self)
                    .map_err(|e| crate::Error::Config(format!("toml serialize: {e}")))?;
                merge_changes(
                    doc.as_item_mut(),
                    Some(old_doc.as_item()),
                    new_doc.as_item(),
                );
                doc.to_string()
            }
            // Missing (or unreadable) file: nothing to preserve.
            Err(_) => self.to_string_pretty()?,
        };
        crate::fsx::write_atomic(path, body.as_bytes())
    }

    /// Validate a config before it's written + booted. Returns every
    /// issue found — both `Hard` and `Warn`; the caller sets policy. The
    /// `PUT /api/config` handler rejects when any `Hard` issue is present
    /// and echoes the `Warn`s in its 200 body. The only I/O is a
    /// best-effort `is_dir()` existence probe per kb; otherwise pure, so
    /// it's cheap to call from both the HTTP handler and a CLI verb.
    ///
    /// Note: a changed `[daemon] name` is NOT checked here (this fn only
    /// sees the candidate config, not the running daemon's name) — the
    /// HTTP handler enforces "name unchanged vs running" since paths are
    /// pinned for the daemon's lifetime.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();

        // [server] addr must be bindable.
        if !addr_looks_bindable(&self.server.addr) {
            issues.push(ValidationIssue::hard(
                "/server/addr",
                format!(
                    "`{}` is not a bindable address (expected host:port, e.g. 127.0.0.1:4000)",
                    self.server.addr
                ),
            ));
        }

        // [server] trusted_proxies must each parse as an IP (boot drops
        // bad entries with a warn; a write API rejects them outright).
        for (i, p) in self.server.trusted_proxies.iter().enumerate() {
            if p.trim().parse::<std::net::IpAddr>().is_err() {
                issues.push(ValidationIssue::hard(
                    format!("/server/trusted_proxies/{i}"),
                    format!("`{p}` is not a valid IP address"),
                ));
            }
        }

        // [identity] — operator + users[].name must already be
        // lowercase-valid (no silent fold); header must be an HTTP token.
        if !crate::identity::username_is_valid(&self.identity.operator) {
            issues.push(ValidationIssue::hard(
                "/identity/operator",
                format!(
                    "`{}` is not a valid username (expected ^[a-z0-9._@-]{{1,64}}$, \
                     already lowercase — config does not fold)",
                    self.identity.operator
                ),
            ));
        }
        if !crate::identity::header_name_is_valid(&self.identity.header) {
            issues.push(ValidationIssue::hard(
                "/identity/header",
                format!(
                    "`{}` is not a valid HTTP header token",
                    self.identity.header
                ),
            ));
        }
        for (i, u) in self.identity.users.iter().enumerate() {
            if !crate::identity::username_is_valid(&u.name) {
                issues.push(ValidationIssue::hard(
                    format!("/identity/users/{i}/name"),
                    format!(
                        "`{}` is not a valid username (expected ^[a-z0-9._@-]{{1,64}}$, \
                         already lowercase — config does not fold)",
                        u.name
                    ),
                ));
            }
        }

        // [server.rate_limit] caps must be > 0 — a 0/min cap would 429
        // every request to that route family.
        if let Some(rl) = &self.server.rate_limit {
            for (field, val) in [
                ("search", rl.search),
                ("atlas_recompute", rl.atlas_recompute),
                ("review_post", rl.review_post),
                ("history_post", rl.history_post),
            ] {
                if val == Some(0) {
                    issues.push(ValidationIssue::hard(
                        format!("/server/rate_limit/{field}"),
                        "rate-limit cap must be greater than 0 (0 rejects every request)"
                            .to_string(),
                    ));
                }
            }
        }

        // [server.attachments] — a 0 file cap rejects every upload; a 0
        // per-comment cap rejects every adopt; a 0 grace reaps a staged
        // blob before the composer can adopt it.
        if let Some(at) = &self.server.attachments {
            if at.max_file_bytes == Some(0) {
                issues.push(ValidationIssue::hard(
                    "/server/attachments/max_file_bytes",
                    "max_file_bytes must be greater than 0 (0 rejects every upload)".to_string(),
                ));
            }
            if at.max_per_comment == Some(0) {
                issues.push(ValidationIssue::hard(
                    "/server/attachments/max_per_comment",
                    "max_per_comment must be greater than 0 (0 rejects every attachment)"
                        .to_string(),
                ));
            }
            if at.gc_grace_hours == Some(0) {
                issues.push(ValidationIssue::warn(
                    "/server/attachments/gc_grace_hours",
                    "gc_grace_hours = 0 reaps a staged upload before the composer can adopt it"
                        .to_string(),
                ));
            }
        }

        // U1 — [server.capture]: a 0 file cap rejects every upload (hard,
        // same rationale as attachments above); a `default_kb` naming no
        // configured kb would silently 404 every Web Share Target POST
        // (warn — the daemon still boots, since kbs can be added later).
        // U2 follow-up: a 0 request cap rejects every request the same way
        // (hard); a request cap smaller than the file cap can never fit
        // even ONE file plus multipart overhead (warn — surfaces the
        // misconfiguration without refusing to boot).
        if let Some(cap) = &self.server.capture {
            if cap.max_file_bytes == Some(0) {
                issues.push(ValidationIssue::hard(
                    "/server/capture/max_file_bytes",
                    "max_file_bytes must be greater than 0 (0 rejects every upload)".to_string(),
                ));
            }
            if cap.max_request_bytes == Some(0) {
                issues.push(ValidationIssue::hard(
                    "/server/capture/max_request_bytes",
                    "max_request_bytes must be greater than 0 (0 rejects every upload)".to_string(),
                ));
            }
            if cap.max_request_bytes() < cap.max_file_bytes() {
                issues.push(ValidationIssue::warn(
                    "/server/capture/max_request_bytes",
                    format!(
                        "max_request_bytes ({}) is smaller than max_file_bytes ({}) — even a \
                         single file at the per-file cap can never fit in one request",
                        cap.max_request_bytes(),
                        cap.max_file_bytes()
                    ),
                ));
            }
            if let Some(name) = &cap.default_kb {
                let known = KbName::new(name)
                    .map(|n| self.kb.contains_key(&n))
                    .unwrap_or(false);
                if !known {
                    issues.push(ValidationIssue::warn(
                        "/server/capture/default_kb",
                        format!(
                            "`{name}` is not a configured kb; the Web Share Target route falls \
                             back to the first configured kb until this is fixed"
                        ),
                    ));
                }
            }
        }

        // [webhooks] — a malformed / SSRF-blocked url is a hard error (the
        // bridge could never POST it safely); an empty url or empty
        // type-list is a warn (the bridge simply doesn't start / forwards
        // nothing).
        if let Some(wh) = &self.webhooks {
            let url = wh.url.trim();
            if url.is_empty() {
                issues.push(ValidationIssue::warn(
                    "/webhooks/url",
                    "webhook url is empty; the event→webhook bridge will not start".to_string(),
                ));
            } else if let Err(msg) = crate::webhook_url::validate_webhook_url(url, wh.allow_private)
            {
                issues.push(ValidationIssue::hard("/webhooks/url", msg));
            }
            if wh.types.is_empty() {
                issues.push(ValidationIssue::warn(
                    "/webhooks/types",
                    "no event types selected; the webhook bridge will forward nothing".to_string(),
                ));
            }
            if wh.timeout_ms == Some(0) {
                issues.push(ValidationIssue::warn(
                    "/webhooks/timeout_ms",
                    "timeout_ms = 0 makes every webhook POST time out immediately".to_string(),
                ));
            }
        }

        // [retention] — a 0-day window sets the cutoff to `now`, so the very
        // next prune tick deletes EVERY row in that table. Retention is
        // opt-in data loss; a 0 is almost certainly a typo for "off" (drop
        // the key) — reject it outright rather than silently wiping history.
        if self.retention.history_days == Some(0) {
            issues.push(ValidationIssue::hard(
                "/retention/history_days",
                "history_days must be greater than 0 (0 deletes ALL history on the next prune; \
                 remove the key to keep history forever)"
                    .to_string(),
            ));
        }
        if self.retention.reading_sections_days == Some(0) {
            issues.push(ValidationIssue::hard(
                "/retention/reading_sections_days",
                "reading_sections_days must be greater than 0 (0 deletes ALL reading sections on \
                 the next prune; remove the key to keep them forever)"
                    .to_string(),
            ));
        }

        // [server] log_retention_days — L2. A 0-day window sets the cutoff
        // to `now`, so the next sweep deletes EVERY log file including
        // today's active one. Logs have no keep-forever mode (bounded on
        // purpose — they are daemon diagnostics, not user data), so a 0 is
        // a typo, not an off switch: reject it.
        if self.server.log_retention_days == 0 {
            issues.push(ValidationIssue::hard(
                "/server/log_retention_days",
                "log_retention_days must be greater than 0 (0 deletes ALL daemon log files, \
                 including today's, on the next sweep; raise the window instead — log pruning \
                 has no keep-forever mode)"
                    .to_string(),
            ));
        }

        // [backup] — GC-B4 off-host copy step. A partially-configured
        // section (only one of the two knobs set) never runs the remote
        // copy — warn so the operator notices instead of silently getting
        // no off-host backup. An explicitly-empty argv is a hard error
        // (there is nothing to execute).
        if let Some(cmd) = &self.backup.remote_cmd {
            if cmd.is_empty() {
                issues.push(ValidationIssue::hard(
                    "/backup/remote_cmd",
                    "remote_cmd is an empty argv; there is nothing to execute".to_string(),
                ));
            } else if self.backup.remote_dest.is_none() {
                issues.push(ValidationIssue::warn(
                    "/backup/remote_dest",
                    "remote_cmd is set but remote_dest is not; the off-host copy never runs"
                        .to_string(),
                ));
            }
        } else if self.backup.remote_dest.is_some() {
            issues.push(ValidationIssue::warn(
                "/backup/remote_cmd",
                "remote_dest is set but remote_cmd is not; the off-host copy never runs"
                    .to_string(),
            ));
        }

        // [sessions] — W7/LF-1. `live_window_secs = 0` doesn't disable
        // Tier-1 (the dir stays configured, the route stays live) — it
        // makes EVERY transcript instantly read "not live" regardless of
        // how recently it was written, which is surprising rather than
        // useful (an operator wanting Tier-1 off should unset
        // `live_transcripts_dir` instead). Warn, don't hard-fail: the
        // accessor already floors it back to the default (120s), so the
        // daemon boots and behaves sanely either way.
        if self.sessions.live_window_secs == Some(0) {
            issues.push(ValidationIssue::warn(
                "/sessions/live_window_secs",
                "live_window_secs = 0 makes every live transcript read as not-live regardless of \
                 recency; unset `live_transcripts_dir` to disable Tier-1 instead — this falls \
                 back to the default (120s)"
                    .to_string(),
            ));
        }

        // [defaults] embedding_model — warn if unknown (boot falls back).
        if let Some(m) = self.defaults.embedding_model.as_deref() {
            if crate::embed::model_info(m).is_none() {
                issues.push(ValidationIssue::warn(
                    "/defaults/embedding_model",
                    format!(
                        "`{m}` is not a known embedding model; the daemon falls back to the registry default"
                    ),
                ));
            }
        }

        // X1 — [indexer] indexable_extensions (daemon-wide default map). Unlike
        // an unknown embedding model (a WARN that falls back), a bad extension
        // entry is a HARD reject: silently indexing nothing (empty/dotted key)
        // or a phantom pipeline (unknown name) is data loss, not a graceful
        // fallback. D1: only `html`/`markdown` are valid pipelines.
        if let Some(exts) = &self.indexer.indexable_extensions {
            // A present-but-empty table maps NO extensions → the kb indexes
            // nothing AND the reconcile delete pass mass-deletes every existing
            // row (each file reads as de-mapped). That is data loss, never an
            // intentional config — HARD-reject it (`resolved_extension_map`
            // additionally falls through to the default at boot, defence in
            // depth). Empty is a distinct failure from a bad entry (the
            // per-entry loop below sees nothing to reject).
            if exts.is_empty() {
                issues.push(ValidationIssue::hard(
                    "/indexer/indexable_extensions",
                    "indexable_extensions is present but empty: a kb that maps no \
                     extensions indexes nothing and deletes every existing row on the \
                     next reconcile. Remove the table to inherit the default set.",
                ));
            }
            for (ext, pipeline) in exts {
                if let Err(e) = crate::extmap::validate_entry(ext, pipeline) {
                    issues.push(ValidationIssue::hard(
                        format!("/indexer/indexable_extensions/{ext}"),
                        e,
                    ));
                }
            }
        }

        // Per-kb checks.
        for (name, kb) in &self.kb {
            if !kb.path.is_dir() {
                issues.push(ValidationIssue::warn(
                    format!("/kb/{name}/path"),
                    format!("`{}` is not an existing directory", kb.path.display()),
                ));
            }
            if let Some(m) = kb.embedding_model.as_deref() {
                if crate::embed::model_info(m).is_none() {
                    issues.push(ValidationIssue::warn(
                        format!("/kb/{name}/embedding_model"),
                        format!(
                            "`{m}` is not a known embedding model; this kb falls back to [defaults] / registry default"
                        ),
                    ));
                }
            }
            if let Some(layout) = kb.atlas.as_ref().and_then(|a| a.layout.as_deref()) {
                let l = layout.to_ascii_lowercase();
                if l != "umap" && l != "pca" {
                    issues.push(ValidationIssue::warn(
                        format!("/kb/{name}/atlas/layout"),
                        format!(
                            "`{layout}` is not a known atlas layout (expected umap|pca); boot falls back to umap"
                        ),
                    ));
                }
            }
            if let Some(scope) = kb.memory_scope.as_deref() {
                if scope != "global" && scope != "project" {
                    issues.push(ValidationIssue::warn(
                        format!("/kb/{name}/memory_scope"),
                        format!("`{scope}` is not a known memory scope (expected global|project)"),
                    ));
                }
            }
            if let Some(w) = kb.graph_boost {
                if !w.is_finite() || w <= 0.0 || w > 4.0 {
                    issues.push(ValidationIssue::warn(
                        format!("/kb/{name}/graph_boost"),
                        format!(
                            "`{w}` is outside the sane weight range (0, 4]; 1.0 grants the most-linked artifact one full top-rank RRF arm — the boost still applies verbatim"
                        ),
                    ));
                }
            }
            if let Some(dp) = kb.decay_policy.as_deref() {
                if crate::memory::DecayPolicy::parse(dp).is_none() {
                    issues.push(ValidationIssue::warn(
                        format!("/kb/{name}/decay_policy"),
                        format!(
                            "`{dp}` is not a known decay policy (expected strict|balanced|loose)"
                        ),
                    ));
                }
            }
            if let Some(v) = kb.versions.as_deref() {
                if crate::vcs::VersionsMode::parse(v).is_none() {
                    issues.push(ValidationIssue::warn(
                        format!("/kb/{name}/versions"),
                        format!(
                            "`{v}` is not a known versions mode (expected auto|git|index|both|off); boot falls back to auto"
                        ),
                    ));
                }
            }
            if let Some(ob) = &kb.outbound {
                for (i, rule) in ob.redactions.iter().enumerate() {
                    if let Err(e) = regex::Regex::new(&rule.pattern) {
                        issues.push(ValidationIssue::hard(
                            format!("/kb/{name}/outbound/redactions/{i}/pattern"),
                            format!("invalid redaction regex `{}`: {e}", rule.pattern),
                        ));
                    }
                }
            }
            // W2.9 — per-kb resurface weight overrides. Same warn-and-fall-
            // back-verbatim shape as `graph_boost` above: a nonsensical
            // value never fails boot, it just applies the shipped default
            // for that ONE field (see `ResurfaceWeights::from_section`,
            // which performs the identical check at resolve time).
            if let Some(rs) = &kb.resurface {
                if let Some(w) = rs.comment_weight {
                    if !w.is_finite() || w <= 0.0 {
                        issues.push(ValidationIssue::warn(
                            format!("/kb/{name}/resurface/comment_weight"),
                            format!(
                                "`{w}` is not a positive finite weight; falling back to the default 0.6"
                            ),
                        ));
                    }
                }
                if let Some(w) = rs.read_weight {
                    if !w.is_finite() || w <= 0.0 {
                        issues.push(ValidationIssue::warn(
                            format!("/kb/{name}/resurface/read_weight"),
                            format!(
                                "`{w}` is not a positive finite weight; falling back to the default 0.4"
                            ),
                        ));
                    }
                }
                if rs.comment_saturation == Some(0) {
                    issues.push(ValidationIssue::warn(
                        format!("/kb/{name}/resurface/comment_saturation"),
                        "comment_saturation must be greater than 0 (it divides the comment \
                         component); falling back to the default 4"
                            .to_string(),
                    ));
                }
                if let Some(h) = rs.read_halflife_days {
                    if !h.is_finite() || h <= 0.0 {
                        issues.push(ValidationIssue::warn(
                            format!("/kb/{name}/resurface/read_halflife_days"),
                            format!(
                                "`{h}` is not a positive finite half-life; falling back to the default 45.0"
                            ),
                        ));
                    }
                }
                if let Some(f) = rs.score_floor {
                    if !f.is_finite() || f < 0.0 {
                        issues.push(ValidationIssue::warn(
                            format!("/kb/{name}/resurface/score_floor"),
                            format!(
                                "`{f}` is not a non-negative finite floor; falling back to the default 0.05"
                            ),
                        ));
                    }
                }
            }
            // X1 — per-kb indexable_extensions override. Same HARD-reject rules
            // as the daemon-wide table above (unknown pipeline / empty|dotted
            // key). D1: only `html`/`markdown` pipelines.
            if let Some(exts) = &kb.indexable_extensions {
                // Same empty-table trap as the daemon-wide map above: an empty
                // per-kb override indexes nothing and cascade-deletes this kb's
                // existing rows on the next reconcile. HARD-reject.
                if exts.is_empty() {
                    issues.push(ValidationIssue::hard(
                        format!("/kb/{name}/indexable_extensions"),
                        "indexable_extensions is present but empty: this kb would map no \
                         extensions, indexing nothing and deleting every existing row on \
                         the next reconcile. Remove the table to inherit the default set.",
                    ));
                }
                for (ext, pipeline) in exts {
                    if let Err(e) = crate::extmap::validate_entry(ext, pipeline) {
                        issues.push(ValidationIssue::hard(
                            format!("/kb/{name}/indexable_extensions/{ext}"),
                            e,
                        ));
                    }
                }
            }
            // CT-F5 — per-kb SLO targets. Warn-and-apply-VERBATIM (the
            // `graph_boost` precedent), never a hard reject and never a
            // silent clamp: a target is the operator's own claim about their
            // corpus, and an out-of-range one pins its indicator to a
            // constant status — visible on the very next read, which is a
            // faster correction loop than a boot failure. Nothing downstream
            // acts on any of these values, so a bad one can't break anything
            // beyond its own row (surfaced, never enforced).
            if let Some(s) = &kb.slo {
                for (field, v, is_pct) in [
                    ("coderef_resolution_pct", s.coderef_resolution_pct, true),
                    ("orphan_kb_sessions", s.orphan_kb_sessions, false),
                    ("ledger_parse_failure_pct", s.ledger_parse_failure_pct, true),
                    ("capture_freshness_hours", s.capture_freshness_hours, false),
                ] {
                    let Some(v) = v else { continue };
                    if !v.is_finite() || v < 0.0 {
                        issues.push(ValidationIssue::warn(
                            format!("/kb/{name}/slo/{field}"),
                            format!(
                                "`{v}` is not a non-negative finite target; it still applies \
                                 verbatim, so this indicator will read a constant status"
                            ),
                        ));
                    } else if is_pct && v > 100.0 {
                        issues.push(ValidationIssue::warn(
                            format!("/kb/{name}/slo/{field}"),
                            format!(
                                "`{v}` is above 100 for a percentage target; it still applies \
                                 verbatim, so this indicator can never leave `warn`"
                            ),
                        ));
                    }
                }
            }
        }

        // W3.A — [projects.*]: a root must be an absolute path (a relative
        // root can never prefix-match the absolute `cwd`/`repo_root` values
        // sessions carry, so it would silently match nothing — HARD, the
        // same "would do nothing" bar as the empty-extension-table checks
        // above); an entry with zero roots matches no session (WARN — legal
        // but useless, likely a forgotten `roots =`).
        for (id, proj) in &self.projects {
            if proj.roots.is_empty() {
                issues.push(ValidationIssue::warn(
                    format!("/projects/{id}/roots"),
                    "no roots declared: this project will never match a session (it stays \
                     reachable only if a session's auto-derived key happens to equal the id)"
                        .to_string(),
                ));
            }
            for (i, root) in proj.roots.iter().enumerate() {
                if !root.starts_with('/') {
                    issues.push(ValidationIssue::hard(
                        format!("/projects/{id}/roots/{i}"),
                        format!(
                            "`{root}` is not an absolute path — it can never prefix-match a \
                             session's cwd/repo_root"
                        ),
                    ));
                }
            }
        }

        issues
    }
}

/// Does `s` look like an address `TcpListener::bind` would accept? Accepts
/// numeric `IP:port` (incl. bracketed IPv6) via `SocketAddr`, plus the
/// `hostname:port` form (e.g. `localhost:4000`) that `SocketAddr` rejects
/// but bind resolves. Deliberately lenient — the restart loop's
/// last-good rollback is the real backstop for a bind that still fails.
fn addr_looks_bindable(s: &str) -> bool {
    if s.parse::<std::net::SocketAddr>().is_ok() {
        return true;
    }
    match s.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p > 0),
        None => false,
    }
}

/// Compare two freshly-serialised items for value equality, ignoring
/// decor. Both come from `toml_edit::ser::to_document` (no comments,
/// consistent formatting), so equal values render identically.
fn item_value_differs(a: &toml_edit::Item, b: &toml_edit::Item) -> bool {
    a.to_string().trim() != b.to_string().trim()
}

/// Splice the CHANGES between `old` and `new` into `dst` (the parsed
/// on-disk document), leaving unchanged keys — and their comments /
/// formatting — byte-identical. `old`/`new` are decor-free serialisations
/// of the previous + next config; `dst` is the real file. A key is:
///   - recursed into when it exists in `dst` (so unchanged leaves keep
///     their decor, and only genuinely-changed leaves get overwritten);
///   - inserted when `new` has it but `dst` doesn't AND it differs from
///     `old` (a real edit, not a default serde emitted — this is what
///     stops `ui = {}` / `parent_origin = …` cruft on the first save);
///   - removed when `new` dropped it.
///
/// Backs `save_preserving`.
fn merge_changes(dst: &mut toml_edit::Item, old: Option<&toml_edit::Item>, new: &toml_edit::Item) {
    if let (Some(dst_tbl), Some(new_tbl)) = (dst.as_table_like_mut(), new.as_table_like()) {
        let old_tbl = old.and_then(|o| o.as_table_like());
        let removed: Vec<String> = dst_tbl
            .iter()
            .map(|(k, _)| k.to_string())
            .filter(|k| !new_tbl.contains_key(k))
            .collect();
        for k in removed {
            dst_tbl.remove(&k);
        }
        for (k, nv) in new_tbl.iter() {
            let ov = old_tbl.and_then(|o| o.get(k));
            if dst_tbl.contains_key(k) {
                if let Some(dv) = dst_tbl.get_mut(k) {
                    merge_changes(dv, ov, nv);
                }
            } else {
                let changed = ov.map(|o| item_value_differs(o, nv)).unwrap_or(true);
                if changed {
                    dst_tbl.insert(k, nv.clone());
                }
            }
        }
    } else {
        let changed = old.map(|o| item_value_differs(o, new)).unwrap_or(true);
        if changed {
            *dst = new.clone();
        }
    }
}

/// Multi-daemon list — `~/.config/kb/daemons.toml` (cluster-4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonsConfig {
    #[serde(default)]
    pub daemon: BTreeMap<String, DaemonEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonEntry {
    pub endpoint: String,
}

impl DaemonsConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        toml::from_str(&raw).map_err(Into::into)
    }

    /// Load from `path` if present, else return a single-entry default
    /// pointing at `http://127.0.0.1:4000` (the local daemon). Used by
    /// `kb fleet` so first-run users don't need to author a daemons.toml.
    pub fn load_or_default(path: &Path) -> Self {
        Self::load(path).unwrap_or_else(|_| Self::default_local())
    }

    pub fn default_local() -> Self {
        let mut daemon = BTreeMap::new();
        daemon.insert(
            "local".to_string(),
            DaemonEntry {
                endpoint: "http://127.0.0.1:4000".to_string(),
            },
        );
        Self { daemon }
    }
}

impl DaemonEntry {
    /// Endpoint with trailing slashes trimmed. Use as the base URL when
    /// constructing API requests (`format!("{base}/api/identity")`).
    pub fn base_url(&self) -> &str {
        self.endpoint.trim_end_matches('/')
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn empty_config_parses_with_defaults() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert!(c.daemon.name.is_none());
        assert_eq!(c.server.addr, "127.0.0.1:4000");
        assert!(c.kb.is_empty());
    }

    #[test]
    fn server_addr_overrideable() {
        let toml_str = r#"
            [server]
            addr = "0.0.0.0:5000"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(c.server.addr, "0.0.0.0:5000");
    }

    // ---- R3 (v0.24) — opt-in retention windows ----

    /// An absent `[retention]` section leaves every window unset → the
    /// feature is OFF (keep forever) and no prune task is spawned.
    #[test]
    fn retention_absent_is_all_none() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert_eq!(c.retention.history_days, None);
        assert_eq!(c.retention.reading_sections_days, None);
        assert_eq!(c.retention.history_max_age_secs(), None);
        assert_eq!(c.retention.reading_sections_max_age_secs(), None);
        assert!(
            !c.retention.any_window_set(),
            "no window set → the prune task must not spawn"
        );
    }

    /// A `[retention]` section parses its day windows and resolves them to
    /// seconds; `any_window_set` flips true.
    #[test]
    fn retention_parses_windows_and_resolves_seconds() {
        let toml_str = r#"
            [retention]
            history_days = 30
            reading_sections_days = 7
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(c.retention.history_days, Some(30));
        assert_eq!(c.retention.reading_sections_days, Some(7));
        assert_eq!(c.retention.history_max_age_secs(), Some(30 * 86_400));
        assert_eq!(
            c.retention.reading_sections_max_age_secs(),
            Some(7 * 86_400)
        );
        assert!(c.retention.any_window_set());
    }

    /// Only one window need be set — the other stays "keep forever".
    #[test]
    fn retention_history_only_is_valid() {
        let toml_str = r#"
            [retention]
            history_days = 90
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(c.retention.history_max_age_secs(), Some(90 * 86_400));
        assert_eq!(c.retention.reading_sections_max_age_secs(), None);
        assert!(c.retention.any_window_set());
        assert!(
            c.validate()
                .iter()
                .all(|i| !i.pointer.starts_with("/retention")),
            "a positive history window is valid"
        );
    }

    /// A 0-day window is rejected (Hard) — it would delete every row on the
    /// next tick. Both fields are checked independently.
    #[test]
    fn retention_zero_window_rejected() {
        let c = KbConfig::from_toml_str("[retention]\nhistory_days = 0\n").unwrap();
        let issues = c.validate();
        let hist = issues
            .iter()
            .find(|i| i.pointer == "/retention/history_days")
            .expect("history_days = 0 is a hard error");
        assert!(hist.is_hard());

        let c = KbConfig::from_toml_str("[retention]\nreading_sections_days = 0\n").unwrap();
        let issues = c.validate();
        let rs = issues
            .iter()
            .find(|i| i.pointer == "/retention/reading_sections_days")
            .expect("reading_sections_days = 0 is a hard error");
        assert!(rs.is_hard());
    }

    // ---- L2 (v0.24) — [server] log_retention_days ----

    /// Absent key → the 14-day default (log pruning is always on, unlike
    /// the opt-in `[retention]` windows); an explicit key parses; and the
    /// default passes validation.
    #[test]
    fn log_retention_days_defaults_to_14_and_parses_override() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert_eq!(c.server.log_retention_days, 14);
        assert!(
            c.validate()
                .iter()
                .all(|i| i.pointer != "/server/log_retention_days"),
            "the default window is valid"
        );

        let c = KbConfig::from_toml_str("[server]\nlog_retention_days = 3\n").unwrap();
        assert_eq!(c.server.log_retention_days, 3);
    }

    /// A 0-day window is rejected (Hard) — it would reap every log file,
    /// including today's active one, on the next sweep.
    #[test]
    fn log_retention_zero_window_rejected() {
        let c = KbConfig::from_toml_str("[server]\nlog_retention_days = 0\n").unwrap();
        let issue = c
            .validate()
            .into_iter()
            .find(|i| i.pointer == "/server/log_retention_days")
            .expect("log_retention_days = 0 is a hard error");
        assert!(issue.is_hard());
    }

    // ---- PF-R1 (v0.40) — [server] fanout_cap ----

    /// Absent key → the byte-identical default of 8 (kb-server's
    /// pre-existing hardcoded `routes::FANOUT_CAP`); an explicit key parses.
    #[test]
    fn fanout_cap_defaults_to_eight_and_parses_override() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert_eq!(c.server.fanout_cap, 8);

        let c = KbConfig::from_toml_str("[server]\nfanout_cap = 16\n").unwrap();
        assert_eq!(c.server.fanout_cap, 16);
    }

    #[test]
    fn server_trusted_proxies_parses_and_defaults_empty() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert!(
            c.server.trusted_proxies.is_empty(),
            "trusted_proxies defaults to empty (local-only deployment)"
        );

        let toml_str = r#"
            [server]
            trusted_proxies = ["10.0.0.1", "172.17.0.1"]
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(c.server.trusted_proxies, vec!["10.0.0.1", "172.17.0.1"]);
    }

    // ---- v0.34 X1 — [identity] multi-user attribution ----

    /// Absent `[identity]` → operator/header defaults; empty users.
    #[test]
    fn identity_section_defaults() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert_eq!(c.identity.operator, "operator");
        assert_eq!(c.identity.header, "Remote-User");
        assert!(c.identity.users.is_empty());
        assert!(
            c.validate()
                .iter()
                .all(|i| !i.pointer.starts_with("/identity")),
            "default identity is valid"
        );
    }

    /// Full round-trip: operator + header + `[[identity.users]]` entries.
    #[test]
    fn identity_section_parses_operator_header_and_users() {
        let toml_str = r#"
            [identity]
            operator = "carol"
            header = "X-Forwarded-User"

            [[identity.users]]
            name = "carol"
            display = "Carol"

            [[identity.users]]
            name = "alice"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(c.identity.operator, "carol");
        assert_eq!(c.identity.header, "X-Forwarded-User");
        assert_eq!(c.identity.users.len(), 2);
        assert_eq!(c.identity.users[0].name, "carol");
        assert_eq!(c.identity.users[0].display.as_deref(), Some("Carol"));
        assert_eq!(c.identity.users[1].name, "alice");
        assert!(c.identity.users[1].display.is_none());
        assert!(
            c.validate()
                .iter()
                .all(|i| !i.pointer.starts_with("/identity")),
            "valid identity must pass validate"
        );
    }

    /// Uppercase operator is a hard config error (no silent fold).
    #[test]
    fn identity_uppercase_operator_rejected() {
        let c = KbConfig::from_toml_str(
            r#"
            [identity]
            operator = "Operator"
        "#,
        )
        .unwrap();
        let issue = c
            .validate()
            .into_iter()
            .find(|i| i.pointer == "/identity/operator")
            .expect("uppercase operator is a hard error");
        assert!(issue.is_hard());
    }

    /// Invalid header token is a hard config error.
    #[test]
    fn identity_bad_header_rejected() {
        let c = KbConfig::from_toml_str(
            r#"
            [identity]
            header = "Remote User"
        "#,
        )
        .unwrap();
        let issue = c
            .validate()
            .into_iter()
            .find(|i| i.pointer == "/identity/header")
            .expect("space in header is a hard error");
        assert!(issue.is_hard());
    }

    /// Uppercase users[].name is a hard config error.
    #[test]
    fn identity_uppercase_user_name_rejected() {
        let c = KbConfig::from_toml_str(
            r#"
            [identity]
            [[identity.users]]
            name = "Alice"
        "#,
        )
        .unwrap();
        let issue = c
            .validate()
            .into_iter()
            .find(|i| i.pointer == "/identity/users/0/name")
            .expect("uppercase user name is a hard error");
        assert!(issue.is_hard());
    }

    #[test]
    fn kb_atlas_section_parses_k_and_layout() {
        let toml_str = r#"
            [kb.canon]
            path = "/tmp/kb-canon"

            [kb.canon.atlas]
            k = 8
            layout = "pca"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let canon = KbName::new("canon").unwrap();
        let kb = c.kb.get(&canon).unwrap();
        let atlas = kb.atlas.as_ref().unwrap();
        assert_eq!(atlas.k, Some(8));
        assert_eq!(atlas.layout.as_deref(), Some("pca"));
    }

    #[test]
    fn kb_atlas_section_is_optional() {
        let toml_str = r#"
            [kb.canon]
            path = "/tmp/kb-canon"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let canon = KbName::new("canon").unwrap();
        let kb = c.kb.get(&canon).unwrap();
        assert!(kb.atlas.is_none());
    }

    // GC-D1 — `[kb.<name>.search] typo_tolerance` parses to `true`, and an
    // absent section (or kb) defaults to `false` — the flag is off unless
    // an operator opts a kb in explicitly.
    #[test]
    fn kb_search_section_parses_typo_tolerance() {
        let toml_str = r#"
            [kb.canon]
            path = "/tmp/kb-canon"

            [kb.canon.search]
            typo_tolerance = true
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let canon = KbName::new("canon").unwrap();
        let kb = c.kb.get(&canon).unwrap();
        assert!(kb.search.typo_tolerance);
    }

    #[test]
    fn kb_search_section_defaults_off() {
        let toml_str = r#"
            [kb.canon]
            path = "/tmp/kb-canon"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let canon = KbName::new("canon").unwrap();
        let kb = c.kb.get(&canon).unwrap();
        assert!(!kb.search.typo_tolerance, "typo_tolerance must default off");
    }

    // W2.9 — `[kb.<name>.resurface]` round-trips all five fields; an
    // absent section resolves to `None` (the atlas-section precedent).
    #[test]
    fn kb_resurface_section_parses_all_fields() {
        let toml_str = r#"
            [kb.canon]
            path = "/tmp/kb-canon"

            [kb.canon.resurface]
            comment_weight = 0.8
            read_weight = 0.2
            comment_saturation = 6
            read_halflife_days = 20.0
            score_floor = 0.1
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let canon = KbName::new("canon").unwrap();
        let kb = c.kb.get(&canon).unwrap();
        let rs = kb.resurface.as_ref().unwrap();
        assert_eq!(rs.comment_weight, Some(0.8));
        assert_eq!(rs.read_weight, Some(0.2));
        assert_eq!(rs.comment_saturation, Some(6));
        assert_eq!(rs.read_halflife_days, Some(20.0));
        assert_eq!(rs.score_floor, Some(0.1));
    }

    #[test]
    fn kb_resurface_section_is_optional() {
        let toml_str = r#"
            [kb.canon]
            path = "/tmp/kb-canon"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let canon = KbName::new("canon").unwrap();
        let kb = c.kb.get(&canon).unwrap();
        assert!(kb.resurface.is_none());
    }

    // A partial override (one field set) leaves the rest `None` on the
    // parsed section — `ResurfaceWeights::from_section` (kb-core
    // resurface.rs) is what falls the unset fields back to the shipped
    // defaults; this test only pins the TOML→struct parse.
    #[test]
    fn kb_resurface_section_partial_override_leaves_rest_none() {
        let toml_str = r#"
            [kb.canon]
            path = "/tmp/kb-canon"

            [kb.canon.resurface]
            comment_weight = 1.2
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let canon = KbName::new("canon").unwrap();
        let kb = c.kb.get(&canon).unwrap();
        let rs = kb.resurface.as_ref().unwrap();
        assert_eq!(rs.comment_weight, Some(1.2));
        assert!(rs.read_weight.is_none());
        assert!(rs.comment_saturation.is_none());
        assert!(rs.read_halflife_days.is_none());
        assert!(rs.score_floor.is_none());
    }

    // Nonsense values warn (never hard-reject — tuning knobs, not
    // architecture) at EVERY field's own pointer, mirroring the
    // `graph_boost` out-of-range precedent.
    #[test]
    fn validate_resurface_nonsense_values_warn_not_hard() {
        let mut c = KbConfig::default();
        let mut sec = kb_section_with_model(None);
        sec.path = PathBuf::from(".");
        sec.resurface = Some(ResurfaceSection {
            comment_weight: Some(-1.0),
            read_weight: Some(f32::NAN),
            comment_saturation: Some(0),
            read_halflife_days: Some(0.0),
            score_floor: Some(f32::NEG_INFINITY),
        });
        c.kb.insert(KbName::new("k").unwrap(), sec);
        let issues = c.validate();
        for pointer in [
            "/kb/k/resurface/comment_weight",
            "/kb/k/resurface/read_weight",
            "/kb/k/resurface/comment_saturation",
            "/kb/k/resurface/read_halflife_days",
            "/kb/k/resurface/score_floor",
        ] {
            assert!(
                issues.iter().any(|i| i.pointer == pointer && !i.is_hard()),
                "expected a warn at {pointer}: {issues:?}"
            );
        }
    }

    #[test]
    fn validate_resurface_absent_or_sane_values_produce_no_issues() {
        let mut c = KbConfig::default();
        let mut sec = kb_section_with_model(None);
        sec.path = PathBuf::from(".");
        sec.resurface = Some(ResurfaceSection {
            comment_weight: Some(0.8),
            read_weight: Some(0.2),
            comment_saturation: Some(6),
            read_halflife_days: Some(20.0),
            score_floor: Some(0.1),
        });
        c.kb.insert(KbName::new("k").unwrap(), sec);
        let issues = c.validate();
        assert!(
            issues
                .iter()
                .all(|i| !i.pointer.starts_with("/kb/k/resurface")),
            "sane resurface weights must not raise any issue: {issues:?}"
        );
    }

    #[test]
    fn kb_section_with_path() {
        let toml_str = r#"
            [daemon]
            name = "smoke"

            [kb.canon]
            path = "/tmp/kb-canon"
            skip_patterns = ["*.tmp", ".git"]

            [kb.canon.ui]
            theme = "ink"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(c.daemon.name.as_deref(), Some("smoke"));

        let canon = KbName::new("canon").unwrap();
        let kb = c.kb.get(&canon).expect("canon section present");
        assert_eq!(kb.path, PathBuf::from("/tmp/kb-canon"));
        assert_eq!(kb.skip_patterns, vec!["*.tmp", ".git"]);
        assert_eq!(kb.ui.theme.as_deref(), Some("ink"));
    }

    #[test]
    fn invalid_kb_name_rejected() {
        let toml_str = r#"
            [kb."Bad Name"]
            path = "/tmp/x"
        "#;
        let result = KbConfig::from_toml_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("kb.toml");

        let mut c = KbConfig::default();
        c.daemon.name = Some("smoke".into());
        c.kb.insert(
            KbName::new("canon").unwrap(),
            KbSection {
                path: PathBuf::from("/tmp/canon"),
                skip_patterns: vec![".git".into()],
                ui: UiSection::default(),
                embedding_model: None,
                reranker_model: None,
                chunked_embeddings: false,
                graph_boost: None,
                outbound: None,
                atlas: None,
                templates: std::collections::BTreeMap::new(),
                memory_scope: Some("global".into()),
                default_search_category: Some("memory-session".into()),
                code_url: None,
                decay_policy: None,
                versions: None,
                reading_progress: None,
                search: Default::default(),
                indexable_extensions: None,
                reconcile_secs: None,
                capture_dir: None,
                resurface: None,
                slo: None,
            },
        );
        c.save(&path).unwrap();

        let back = KbConfig::load(&path).unwrap();
        assert_eq!(back.daemon.name, c.daemon.name);
        assert_eq!(back.kb.len(), 1);
        // M2 — memory_scope survives the TOML round-trip.
        let kb = back.kb.get(&KbName::new("canon").unwrap()).unwrap();
        assert_eq!(kb.memory_scope.as_deref(), Some("global"));
        // R0-opt-in — default_search_category survives the TOML round-trip.
        assert_eq!(
            kb.default_search_category.as_deref(),
            Some("memory-session")
        );
    }

    #[test]
    fn memory_scope_parses_from_toml() {
        let toml_str = r#"
            [kb.mem]
            path = "/tmp/mem"
            memory_scope = "project"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let kb = c.kb.get(&KbName::new("mem").unwrap()).unwrap();
        assert_eq!(kb.memory_scope.as_deref(), Some("project"));
    }

    #[test]
    fn absent_memory_scope_defaults_none() {
        let toml_str = r#"
            [kb.plain]
            path = "/tmp/plain"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let kb = c.kb.get(&KbName::new("plain").unwrap()).unwrap();
        assert!(kb.memory_scope.is_none());
    }

    #[test]
    fn default_search_category_parses_from_toml() {
        let toml_str = r#"
            [kb.sessions]
            path = "/tmp/sessions"
            default_search_category = "memory-session"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let kb = c.kb.get(&KbName::new("sessions").unwrap()).unwrap();
        assert_eq!(
            kb.default_search_category.as_deref(),
            Some("memory-session")
        );
    }

    #[test]
    fn absent_default_search_category_defaults_none() {
        let toml_str = r#"
            [kb.plain]
            path = "/tmp/plain"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let kb = c.kb.get(&KbName::new("plain").unwrap()).unwrap();
        assert!(kb.default_search_category.is_none());
    }

    // ---- DCB W1.B — `[kb.*] code_url` ----

    #[test]
    fn code_url_parses_from_toml() {
        let toml_str = r#"
            [kb.platform]
            path = "/tmp/platform"
            code_url = "https://kbc.example.com"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let kb = c.kb.get(&KbName::new("platform").unwrap()).unwrap();
        assert_eq!(kb.code_url.as_deref(), Some("https://kbc.example.com"));
    }

    // --- CT-F5 [kb.*.slo] -----------------------------------------------

    #[test]
    fn slo_section_parses_from_toml() {
        let toml_str = r#"
            [kb.notes]
            path = "/tmp/notes"

            [kb.notes.slo]
            coderef_resolution_pct = 80.0
            orphan_kb_sessions = 0
            ledger_parse_failure_pct = 1.5
            capture_freshness_hours = 48
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let kb = c.kb.get(&KbName::new("notes").unwrap()).unwrap();
        let t = kb.slo.expect("section present").targets();
        assert_eq!(t.coderef_resolution_pct, Some(80.0));
        assert_eq!(t.orphan_kb_sessions, Some(0.0));
        assert_eq!(t.ledger_parse_failure_pct, Some(1.5));
        assert_eq!(t.capture_freshness_hours, Some(48.0));
        // Filtered to `/slo/` pointers: the fixture's `path` doesn't exist on
        // disk, which validate() warns about independently of this section.
        assert!(
            slo_issues(&c).is_empty(),
            "a sane target set warns nothing: {:?}",
            slo_issues(&c)
        );
    }

    /// Only the `[kb.*.slo]` validation issues — every fixture here uses a
    /// non-existent `path`, whose own warn is unrelated noise.
    fn slo_issues(c: &KbConfig) -> Vec<ValidationIssue> {
        c.validate()
            .into_iter()
            .filter(|i| i.pointer.contains("/slo/"))
            .collect()
    }

    /// Every key is independently optional: a section naming ONE target still
    /// yields a full four-indicator report, three of them measured-but-unjudged.
    #[test]
    fn a_partial_slo_section_leaves_the_other_targets_none() {
        let toml_str = r#"
            [kb.notes]
            path = "/tmp/notes"

            [kb.notes.slo]
            capture_freshness_hours = 6
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let t =
            c.kb.get(&KbName::new("notes").unwrap())
                .unwrap()
                .slo
                .expect("section present")
                .targets();
        assert_eq!(t.capture_freshness_hours, Some(6.0));
        assert_eq!(t.coderef_resolution_pct, None);
        assert_eq!(t.orphan_kb_sessions, None);
        assert_eq!(t.ledger_parse_failure_pct, None);
    }

    #[test]
    fn absent_slo_section_is_none_and_yields_empty_targets() {
        let toml_str = r#"
            [kb.plain]
            path = "/tmp/plain"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let kb = c.kb.get(&KbName::new("plain").unwrap()).unwrap();
        assert!(kb.slo.is_none());
        assert_eq!(
            kb.slo.unwrap_or_default().targets(),
            crate::slo::SloTargets::default()
        );
    }

    /// Warn-and-apply-VERBATIM (the `graph_boost` precedent) — never a hard
    /// reject, never a silent clamp.
    #[test]
    fn nonsensical_slo_targets_warn_but_still_parse() {
        let toml_str = r#"
            [kb.notes]
            path = "/tmp/notes"

            [kb.notes.slo]
            coderef_resolution_pct = 140.0
            orphan_kb_sessions = -3
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let issues = slo_issues(&c);
        assert_eq!(issues.len(), 2, "{issues:?}");
        assert!(
            issues.iter().all(|i| !i.is_hard()),
            "an out-of-range target must never fail boot: {issues:?}"
        );
        assert!(issues
            .iter()
            .any(|i| i.pointer == "/kb/notes/slo/coderef_resolution_pct"));
        assert!(issues
            .iter()
            .any(|i| i.pointer == "/kb/notes/slo/orphan_kb_sessions"));
        // Applied verbatim: the parse keeps exactly what was written.
        let t =
            c.kb.get(&KbName::new("notes").unwrap())
                .unwrap()
                .slo
                .unwrap()
                .targets();
        assert_eq!(t.coderef_resolution_pct, Some(140.0));
        assert_eq!(t.orphan_kb_sessions, Some(-3.0));
    }

    #[test]
    fn absent_code_url_defaults_none() {
        let toml_str = r#"
            [kb.plain]
            path = "/tmp/plain"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let kb = c.kb.get(&KbName::new("plain").unwrap()).unwrap();
        assert!(kb.code_url.is_none());
    }

    #[test]
    fn code_url_survives_toml_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("kb.toml");

        let mut c = KbConfig::default();
        c.daemon.name = Some("smoke".into());
        c.kb.insert(
            KbName::new("platform").unwrap(),
            KbSection {
                path: PathBuf::from("/tmp/platform"),
                skip_patterns: Vec::new(),
                ui: UiSection::default(),
                embedding_model: None,
                reranker_model: None,
                chunked_embeddings: false,
                graph_boost: None,
                outbound: None,
                atlas: None,
                templates: std::collections::BTreeMap::new(),
                memory_scope: None,
                default_search_category: None,
                code_url: Some("https://kbc.example.com".into()),
                decay_policy: None,
                versions: None,
                reading_progress: None,
                search: Default::default(),
                indexable_extensions: None,
                reconcile_secs: None,
                capture_dir: None,
                resurface: None,
                slo: None,
            },
        );
        c.save(&path).unwrap();

        let back = KbConfig::load(&path).unwrap();
        let kb = back.kb.get(&KbName::new("platform").unwrap()).unwrap();
        assert_eq!(kb.code_url.as_deref(), Some("https://kbc.example.com"));
    }

    #[test]
    fn reading_progress_parses_and_defaults_on() {
        let toml_str = r#"
            [kb.off]
            path = "/tmp/off"
            reading_progress = false

            [kb.plain]
            path = "/tmp/plain"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let off = c.kb.get(&KbName::new("off").unwrap()).unwrap();
        assert_eq!(off.reading_progress, Some(false));
        assert!(!off.reading_progress_enabled());
        let plain = c.kb.get(&KbName::new("plain").unwrap()).unwrap();
        assert!(plain.reading_progress.is_none());
        assert!(plain.reading_progress_enabled(), "default ON when unset");
    }

    #[test]
    fn daemons_config_parses_entries() {
        let toml_str = r#"
            [daemon.work]
            endpoint = "http://127.0.0.1:4000"

            [daemon.research]
            endpoint = "http://research.local:4000"
        "#;
        let d: DaemonsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(d.daemon.len(), 2);
        assert_eq!(d.daemon["work"].endpoint, "http://127.0.0.1:4000");
    }

    #[test]
    fn daemons_default_local_is_localhost_4000() {
        let d = DaemonsConfig::default_local();
        assert_eq!(d.daemon.len(), 1);
        assert_eq!(d.daemon["local"].endpoint, "http://127.0.0.1:4000");
    }

    #[test]
    fn daemons_load_or_default_falls_back_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.toml");
        let d = DaemonsConfig::load_or_default(&missing);
        assert_eq!(d.daemon.len(), 1);
    }

    #[test]
    fn indexer_section_defaults_when_absent() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert!(c.indexer.debounce_ms.is_none());
        assert!(c.indexer.reconcile_secs.is_none());
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        std::env::remove_var("KB_DEBOUNCE_MS");
        std::env::remove_var("KB_RECONCILE_SECS");
        assert_eq!(
            c.indexer.resolved_debounce_ms(),
            crate::watcher::DEFAULT_DEBOUNCE_MS,
        );
        assert_eq!(
            c.indexer.resolved_reconcile_secs(),
            IndexerSection::DEFAULT_RECONCILE_SECS,
        );
    }

    #[test]
    fn indexer_section_parses_config_values() {
        let toml_str = r#"
            [indexer]
            debounce_ms = 800
            reconcile_secs = 30
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        std::env::remove_var("KB_DEBOUNCE_MS");
        std::env::remove_var("KB_RECONCILE_SECS");
        assert_eq!(c.indexer.resolved_debounce_ms(), 800);
        assert_eq!(c.indexer.resolved_reconcile_secs(), 30);
    }

    #[test]
    fn indexer_section_env_overrides_config() {
        let toml_str = r#"
            [indexer]
            debounce_ms = 800
            reconcile_secs = 30
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var("KB_DEBOUNCE_MS", "150");
        std::env::set_var("KB_RECONCILE_SECS", "0");
        let dbms = c.indexer.resolved_debounce_ms();
        let rsecs = c.indexer.resolved_reconcile_secs();
        std::env::remove_var("KB_DEBOUNCE_MS");
        std::env::remove_var("KB_RECONCILE_SECS");
        assert_eq!(dbms, 150);
        assert_eq!(rsecs, 0, "reconcile_secs=0 disables the pass");
    }

    // ---- PF-I1 — resolved_reconcile_secs_for precedence chain ----
    //
    // Config-only ladder (per-kb override vs. `[indexer]` vs. default) is
    // covered here without touching env; env-trumps-everything is covered
    // separately using the same `ENV_TEST_LOCK` pattern as
    // `indexer_section_env_overrides_config` above.

    #[test]
    fn resolved_reconcile_secs_for_defaults_to_daemon_value_when_kb_unset() {
        let c = KbConfig::from_toml_str("[indexer]\nreconcile_secs = 45\n").unwrap();
        let kb = kb_section_with_model(None);
        assert!(kb.reconcile_secs.is_none());
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("KB_RECONCILE_SECS");
        assert_eq!(c.indexer.resolved_reconcile_secs_for(&kb), 45);
    }

    #[test]
    fn resolved_reconcile_secs_for_per_kb_override_wins_over_indexer_section() {
        let c = KbConfig::from_toml_str("[indexer]\nreconcile_secs = 45\n").unwrap();
        let mut kb = kb_section_with_model(None);
        kb.reconcile_secs = Some(10);
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("KB_RECONCILE_SECS");
        assert_eq!(c.indexer.resolved_reconcile_secs_for(&kb), 10);
    }

    #[test]
    fn resolved_reconcile_secs_for_falls_back_to_default_when_unset_everywhere() {
        let c = KbConfig::from_toml_str("").unwrap();
        let kb = kb_section_with_model(None);
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("KB_RECONCILE_SECS");
        assert_eq!(
            c.indexer.resolved_reconcile_secs_for(&kb),
            IndexerSection::DEFAULT_RECONCILE_SECS,
        );
    }

    #[test]
    fn resolved_reconcile_secs_for_env_trumps_per_kb_override() {
        let c = KbConfig::from_toml_str("[indexer]\nreconcile_secs = 45\n").unwrap();
        let mut kb = kb_section_with_model(None);
        kb.reconcile_secs = Some(10);
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("KB_RECONCILE_SECS", "0");
        let resolved = c.indexer.resolved_reconcile_secs_for(&kb);
        std::env::remove_var("KB_RECONCILE_SECS");
        assert_eq!(
            resolved, 0,
            "KB_RECONCILE_SECS env still trumps the per-kb override (the escape hatch)",
        );
    }

    #[test]
    fn storage_section_absent_resolves_capped_shipped_defaults() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert_eq!(
            c.storage.resolved_lance_index_cache_mb(),
            StorageSection::DEFAULT_LANCE_INDEX_CACHE_MB
        );
        assert_eq!(
            c.storage.resolved_lance_metadata_cache_mb(),
            StorageSection::DEFAULT_LANCE_METADATA_CACHE_MB
        );
        assert_eq!(
            c.storage.resolved_index_rebuild_min_secs(),
            StorageSection::DEFAULT_INDEX_REBUILD_MIN_SECS
        );
    }

    #[test]
    fn storage_section_parses_config_values_and_zero_opts_out() {
        let toml_str = r#"
            [storage]
            lance_index_cache_mb = 512
            lance_metadata_cache_mb = 128
            index_rebuild_min_secs = 60
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(c.storage.resolved_lance_index_cache_mb(), 512);
        assert_eq!(c.storage.resolved_lance_metadata_cache_mb(), 128);
        assert_eq!(c.storage.resolved_index_rebuild_min_secs(), 60);

        // Explicit 0 = opt out to pre-knob lance behavior (uncapped caches,
        // rebuild on every dirty) — NOT the shipped capped defaults.
        let c = KbConfig::from_toml_str(
            "[storage]\nlance_index_cache_mb = 0\nindex_rebuild_min_secs = 0\n",
        )
        .unwrap();
        assert_eq!(c.storage.resolved_lance_index_cache_mb(), 0);
        assert_eq!(c.storage.resolved_index_rebuild_min_secs(), 0);
        // Unset knobs still resolve to their shipped defaults.
        assert_eq!(
            c.storage.resolved_lance_metadata_cache_mb(),
            StorageSection::DEFAULT_LANCE_METADATA_CACHE_MB
        );
    }

    #[test]
    fn resolved_watch_mode_and_poll_interval_precedence() {
        use crate::watcher::{WatchMode, DEFAULT_POLL_INTERVAL_MS};
        let c = KbConfig::from_toml_str("[indexer]\nwatch_mode = \"poll\"\n").unwrap();
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("KB_WATCH_MODE");
        std::env::remove_var("KB_POLL_INTERVAL_MS");
        // config value honoured when no env
        assert_eq!(c.indexer.resolved_watch_mode(), WatchMode::Poll);
        // empty env must NOT mask the configured value
        std::env::set_var("KB_WATCH_MODE", "");
        assert_eq!(c.indexer.resolved_watch_mode(), WatchMode::Poll);
        // non-empty env wins
        std::env::set_var("KB_WATCH_MODE", "native");
        assert_eq!(c.indexer.resolved_watch_mode(), WatchMode::Native);
        std::env::remove_var("KB_WATCH_MODE");
        // poll interval: default, env override, and invalid/zero → default
        assert_eq!(
            c.indexer.resolved_poll_interval_ms(),
            DEFAULT_POLL_INTERVAL_MS
        );
        std::env::set_var("KB_POLL_INTERVAL_MS", "500");
        assert_eq!(c.indexer.resolved_poll_interval_ms(), 500);
        std::env::set_var("KB_POLL_INTERVAL_MS", "0");
        assert_eq!(
            c.indexer.resolved_poll_interval_ms(),
            DEFAULT_POLL_INTERVAL_MS
        );
        std::env::remove_var("KB_POLL_INTERVAL_MS");
    }

    /// Serialises env-touching tests so they don't trample each
    /// other's `KB_*` vars when run with `--test-threads > 1`.
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn daemon_entry_base_url_trims_trailing_slash() {
        let e = DaemonEntry {
            endpoint: "http://x:4000/".to_string(),
        };
        assert_eq!(e.base_url(), "http://x:4000");
        let e2 = DaemonEntry {
            endpoint: "http://x:4000".to_string(),
        };
        assert_eq!(e2.base_url(), "http://x:4000");
    }

    #[test]
    fn share_section_absent_is_empty() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert!(c.share.cloudflare.is_none());
        assert!(c.share.github.is_none());
        assert!(c.share.live_origin.is_none());
    }

    #[test]
    fn share_section_parses_cloudflare_and_github() {
        let toml_str = r#"
            [share]
            live_origin = "https://kb.example.com"

            [share.cloudflare]
            account_id = "acc123"
            team_domain = "myteam.cloudflareaccess.com"
            google_idp = "uuid-g"

            [share.github]
            owner = "octocat"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        let cf = c.share.cloudflare.as_ref().unwrap();
        assert_eq!(cf.account_id, "acc123");
        assert_eq!(cf.team_domain, "myteam.cloudflareaccess.com");
        assert_eq!(cf.google_idp.as_deref(), Some("uuid-g"));
        assert!(cf.github_idp.is_none());
        assert_eq!(cf.api_token_env, "KB_CF_API_TOKEN"); // default
        let gh = c.share.github.as_ref().unwrap();
        assert_eq!(gh.owner, "octocat");
        assert_eq!(gh.token_env, "KB_GH_TOKEN"); // default
        assert_eq!(
            c.share.live_origin.as_deref(),
            Some("https://kb.example.com")
        );
    }

    #[test]
    fn share_custom_token_env_name() {
        let toml_str = r#"
            [share.cloudflare]
            account_id = "a"
            team_domain = "t.cloudflareaccess.com"
            api_token_env = "MY_CF_TOKEN"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(c.share.cloudflare.unwrap().api_token_env, "MY_CF_TOKEN");
    }

    #[test]
    fn read_secret_env_errors_when_unset_with_hint() {
        // Uniquely-named var that won't be set; read-only, no env mutation.
        let res = read_secret_env("KB_TEST_DEFINITELY_UNSET_XYZ", "Test secret");
        let err = res.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("KB_TEST_DEFINITELY_UNSET_XYZ"));
        assert!(msg.contains("Test secret"));
    }

    // ---- D1 — resolved_embedding_model precedence chain ----

    fn kb_section_with_model(model: Option<&str>) -> KbSection {
        KbSection {
            path: PathBuf::from("/tmp/x"),
            skip_patterns: Vec::new(),
            ui: Default::default(),
            embedding_model: model.map(str::to_string),
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: Default::default(),
            memory_scope: None,
            default_search_category: None,
            code_url: None,
            decay_policy: None,
            versions: None,
            reading_progress: None,
            search: Default::default(),
            indexable_extensions: None,
            reconcile_secs: None,
            capture_dir: None,
            resurface: None,
            slo: None,
        }
    }

    #[test]
    fn defaults_section_round_trips_through_toml() {
        let toml_str = r#"
            [defaults]
            embedding_model = "bge-large-en-v1.5"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(
            c.defaults.embedding_model.as_deref(),
            Some("bge-large-en-v1.5")
        );
        // Re-serialise + re-parse: same value.
        let body = c.to_string_pretty().unwrap();
        let c2 = KbConfig::from_toml_str(&body).unwrap();
        assert_eq!(c.defaults.embedding_model, c2.defaults.embedding_model);
    }

    #[test]
    fn defaults_section_optional_and_defaults_to_none() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert!(c.defaults.embedding_model.is_none());
    }

    #[test]
    fn resolved_embedding_model_per_kb_wins() {
        let mut c = KbConfig::default();
        c.defaults.embedding_model = Some("bge-base-en-v1.5".into());
        let kb = kb_section_with_model(Some("bge-large-en-v1.5"));
        assert_eq!(
            c.resolved_embedding_model(&kb),
            Some("bge-large-en-v1.5"),
            "per-kb must beat [defaults]"
        );
    }

    #[test]
    fn resolved_embedding_model_defaults_when_kb_omits() {
        let mut c = KbConfig::default();
        c.defaults.embedding_model = Some("bge-large-en-v1.5".into());
        let kb = kb_section_with_model(None);
        assert_eq!(
            c.resolved_embedding_model(&kb),
            Some("bge-large-en-v1.5"),
            "[defaults] fires when kb omits embedding_model"
        );
    }

    #[test]
    fn resolved_embedding_model_falls_back_to_registry_default() {
        let c = KbConfig::default();
        let kb = kb_section_with_model(None);
        let registry = crate::embed::default_model().name;
        assert_eq!(
            c.resolved_embedding_model(&kb),
            Some(registry),
            "with neither per-kb nor [defaults], the registry default fires"
        );
        assert_eq!(registry, "bge-small-en-v1.5", "today's registry default");
    }

    #[test]
    fn resolved_embedding_model_unknown_name_skips_layer() {
        // Unknown name in [defaults] doesn't pre-empt the registry fallback,
        // and unknown name on the kb doesn't pre-empt [defaults].
        let mut c = KbConfig::default();
        c.defaults.embedding_model = Some("bge-base-en-v1.5".into());
        let kb = kb_section_with_model(Some("not-a-real-model"));
        assert_eq!(
            c.resolved_embedding_model(&kb),
            Some("bge-base-en-v1.5"),
            "unknown per-kb name falls through to [defaults]"
        );

        c.defaults.embedding_model = Some("also-not-real".into());
        let kb = kb_section_with_model(Some("still-not-real"));
        assert_eq!(
            c.resolved_embedding_model(&kb),
            Some("bge-small-en-v1.5"),
            "unknown at every configured layer falls through to registry default"
        );
    }

    #[test]
    fn resolved_embedding_model_respects_disable_fallback() {
        let mut c = KbConfig::default();
        c.defaults.disable_embedder_fallback = true;
        let kb = kb_section_with_model(None);
        assert_eq!(
            c.resolved_embedding_model(&kb),
            None,
            "disable_embedder_fallback suppresses the registry default"
        );

        // Per-kb still wins.
        let kb = kb_section_with_model(Some("bge-large-en-v1.5"));
        assert_eq!(
            c.resolved_embedding_model(&kb),
            Some("bge-large-en-v1.5"),
            "disable_embedder_fallback does not override an explicit per-kb model"
        );

        // [defaults] still fires.
        c.defaults.embedding_model = Some("bge-base-en-v1.5".into());
        let kb = kb_section_with_model(None);
        assert_eq!(
            c.resolved_embedding_model(&kb),
            Some("bge-base-en-v1.5"),
            "disable_embedder_fallback does not override an explicit [defaults] model"
        );
    }

    #[test]
    fn resolved_embedding_model_validates_all_three_layers() {
        // Sanity: every name returned must be in the registry, regardless
        // of which layer fired.
        let mut c = KbConfig::default();
        let kb = kb_section_with_model(None);
        let resolved = c.resolved_embedding_model(&kb).unwrap();
        assert!(crate::embed::model_info(resolved).is_some());

        c.defaults.embedding_model = Some("bge-base-en-v1.5".into());
        let kb = kb_section_with_model(Some("bge-large-en-v1.5"));
        let resolved = c.resolved_embedding_model(&kb).unwrap();
        assert!(crate::embed::model_info(resolved).is_some());
    }

    // ---- X1 (v0.24) — indexable-extension map: config parse + precedence +
    // validate ----

    fn ext_table(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn indexable_extensions_round_trip_through_toml() {
        let toml_str = r#"
            [indexer.indexable_extensions]
            txt = "markdown"
            html = "html"

            [kb.canon]
            path = "/tmp/canon"
            [kb.canon.indexable_extensions]
            rst = "markdown"
        "#;
        let c = KbConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(
            c.indexer
                .indexable_extensions
                .as_ref()
                .unwrap()
                .get("txt")
                .map(String::as_str),
            Some("markdown")
        );
        let kb = &c.kb[&KbName::new("canon").unwrap()];
        assert_eq!(
            kb.indexable_extensions
                .as_ref()
                .unwrap()
                .get("rst")
                .map(String::as_str),
            Some("markdown")
        );
        // Re-serialise + re-parse round-trips the tables.
        let body = c.to_string_pretty().unwrap();
        let c2 = KbConfig::from_toml_str(&body).unwrap();
        assert_eq!(
            c.indexer.indexable_extensions,
            c2.indexer.indexable_extensions
        );
    }

    #[test]
    fn resolved_extension_map_defaults_to_builtin_when_unset() {
        let c = KbConfig::default();
        let kb = kb_section_with_model(None);
        let m = c.resolved_extension_map(&kb);
        assert_eq!(m, crate::extmap::ExtensionMap::default());
        assert!(m.is_indexable(std::path::Path::new("a.html")));
        assert!(!m.is_indexable(std::path::Path::new("a.txt")));
    }

    #[test]
    fn resolved_extension_map_daemon_layer_fires_when_kb_omits() {
        let mut c = KbConfig::default();
        c.indexer.indexable_extensions = Some(ext_table(&[("txt", "markdown")]));
        let kb = kb_section_with_model(None);
        let m = c.resolved_extension_map(&kb);
        assert_eq!(
            m.pipeline(std::path::Path::new("a.txt")),
            Some(crate::extmap::Pipeline::Markdown)
        );
        // The layer REPLACES (not merges) the built-in default: html is gone.
        assert!(!m.is_indexable(std::path::Path::new("a.html")));
    }

    #[test]
    fn resolved_extension_map_per_kb_wins_over_daemon() {
        let mut c = KbConfig::default();
        c.indexer.indexable_extensions = Some(ext_table(&[("txt", "markdown")]));
        let mut kb = kb_section_with_model(None);
        kb.indexable_extensions = Some(ext_table(&[("adoc", "html")]));
        let m = c.resolved_extension_map(&kb);
        assert_eq!(
            m.pipeline(std::path::Path::new("a.adoc")),
            Some(crate::extmap::Pipeline::Html)
        );
        // per-kb replaces the daemon layer entirely: txt is no longer mapped.
        assert!(!m.is_indexable(std::path::Path::new("a.txt")));
    }

    #[test]
    fn resolved_extension_map_skips_invalid_layer() {
        // A hand-edited invalid per-kb layer (one that skipped `validate`)
        // falls through to the daemon layer rather than crashing the boot.
        let mut c = KbConfig::default();
        c.indexer.indexable_extensions = Some(ext_table(&[("txt", "markdown")]));
        let mut kb = kb_section_with_model(None);
        kb.indexable_extensions = Some(ext_table(&[("pdf", "pdf")])); // unknown pipeline
        let m = c.resolved_extension_map(&kb);
        assert!(m.is_indexable(std::path::Path::new("a.txt")));
        assert!(!m.is_indexable(std::path::Path::new("a.pdf")));
    }

    #[test]
    fn validate_hard_rejects_unknown_daemon_pipeline() {
        let mut c = KbConfig::default();
        c.indexer.indexable_extensions = Some(ext_table(&[("pdf", "pdf")]));
        assert!(c
            .validate()
            .iter()
            .any(|i| i.pointer == "/indexer/indexable_extensions/pdf" && i.is_hard()));
    }

    #[test]
    fn validate_hard_rejects_bad_per_kb_extension_keys() {
        let mut c = KbConfig::default();
        let mut kb = kb_section_with_model(None);
        // A dotted key AND an unknown pipeline — both HARD.
        kb.indexable_extensions = Some(ext_table(&[(".txt", "markdown"), ("md", "nope")]));
        c.kb.insert(KbName::new("canon").unwrap(), kb);
        let issues = c.validate();
        assert!(issues
            .iter()
            .any(|i| i.pointer == "/kb/canon/indexable_extensions/.txt" && i.is_hard()));
        assert!(issues
            .iter()
            .any(|i| i.pointer == "/kb/canon/indexable_extensions/md" && i.is_hard()));
    }

    #[test]
    fn validate_accepts_valid_extension_maps() {
        let mut c = KbConfig::default();
        c.indexer.indexable_extensions = Some(ext_table(&[("txt", "markdown"), ("html", "html")]));
        let mut kb = kb_section_with_model(None);
        kb.indexable_extensions = Some(ext_table(&[("md", "markdown")]));
        c.kb.insert(KbName::new("canon").unwrap(), kb);
        assert!(
            c.validate().iter().all(|i| !i.is_hard()),
            "valid extension maps must produce no hard issues"
        );
    }

    #[test]
    fn validate_hard_rejects_empty_extension_tables() {
        // A present-but-empty table (distinct from `None`) maps nothing → the
        // reconcile delete pass would mass-delete the kb. Both layers must
        // HARD-reject it so the empty-map wipe is impossible at write time.
        let mut c = KbConfig::default();
        c.indexer.indexable_extensions = Some(ext_table(&[]));
        let mut kb = kb_section_with_model(None);
        kb.indexable_extensions = Some(ext_table(&[]));
        c.kb.insert(KbName::new("canon").unwrap(), kb);
        let issues = c.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.pointer == "/indexer/indexable_extensions" && i.is_hard()),
            "an empty daemon-wide table is a HARD issue"
        );
        assert!(
            issues
                .iter()
                .any(|i| i.pointer == "/kb/canon/indexable_extensions" && i.is_hard()),
            "an empty per-kb table is a HARD issue"
        );
    }

    #[test]
    fn resolved_extension_map_skips_empty_layer() {
        // A hand-edited empty table (one that bypassed `validate`) must NOT
        // resolve to an empty map (which would make the reconcile delete pass
        // wipe the kb). It falls through: empty per-kb → daemon layer; empty
        // daemon + unset per-kb → the built-in default set.
        let mut c = KbConfig::default();
        c.indexer.indexable_extensions = Some(ext_table(&[("txt", "markdown")]));
        let mut kb = kb_section_with_model(None);
        kb.indexable_extensions = Some(ext_table(&[])); // empty per-kb → skip to daemon
        let m = c.resolved_extension_map(&kb);
        assert!(
            !m.is_empty(),
            "an empty layer must never resolve to an empty map"
        );
        assert!(m.is_indexable(std::path::Path::new("a.txt")));

        // Empty daemon layer AND unset per-kb → built-in default (never empty).
        let mut c2 = KbConfig::default();
        c2.indexer.indexable_extensions = Some(ext_table(&[]));
        let kb2 = kb_section_with_model(None);
        let m2 = c2.resolved_extension_map(&kb2);
        assert_eq!(m2, crate::extmap::ExtensionMap::default());
        assert!(m2.is_indexable(std::path::Path::new("a.html")));
    }

    // ---- CE1 — validate + save_preserving ----

    #[test]
    fn validate_default_config_has_no_hard_issues() {
        let issues = KbConfig::default().validate();
        assert!(
            issues.iter().all(|i| !i.is_hard()),
            "default config must have no hard issues: {issues:?}"
        );
    }

    #[test]
    fn validate_webhook_loopback_ok_private_hard_fails() {
        let mut c = KbConfig {
            webhooks: Some(WebhooksSection {
                url: "http://127.0.0.1:9000/kb-hook".into(),
                types: vec!["artifact.indexed".into()],
                timeout_ms: None,
                allow_private: false,
            }),
            ..Default::default()
        };
        assert!(
            !c.validate()
                .iter()
                .any(|i| i.pointer == "/webhooks/url" && i.is_hard()),
            "loopback webhook must validate: {:?}",
            c.validate()
        );

        c.webhooks = Some(WebhooksSection {
            url: "http://10.0.0.1/hook".into(),
            types: vec!["artifact.indexed".into()],
            timeout_ms: None,
            allow_private: false,
        });
        assert!(
            c.validate()
                .iter()
                .any(|i| i.pointer == "/webhooks/url" && i.is_hard()),
            "RFC1918 without allow_private must hard-fail"
        );

        c.webhooks = Some(WebhooksSection {
            url: "http://10.0.0.1/hook".into(),
            types: vec!["artifact.indexed".into()],
            timeout_ms: None,
            allow_private: true,
        });
        assert!(
            !c.validate()
                .iter()
                .any(|i| i.pointer == "/webhooks/url" && i.is_hard()),
            "allow_private unlocks LAN"
        );
    }

    #[test]
    fn webhooks_allow_private_defaults_false_when_key_absent() {
        let toml = r#"
            [webhooks]
            url = "http://127.0.0.1:9/h"
            types = ["artifact.indexed"]
        "#;
        let c: KbConfig = toml::from_str(toml).expect("deserialize");
        let wh = c.webhooks.expect("webhooks present");
        assert!(
            !wh.allow_private,
            "missing allow_private must default false"
        );
    }

    #[test]
    fn validate_flags_unbindable_addr_but_accepts_host_port() {
        let mut c = KbConfig::default();
        c.server.addr = "not-an-addr".into();
        assert!(c
            .validate()
            .iter()
            .any(|i| i.pointer == "/server/addr" && i.is_hard()));
        for ok in [
            "127.0.0.1:4000",
            "0.0.0.0:5000",
            "[::1]:4001",
            "localhost:4000",
        ] {
            c.server.addr = ok.into();
            assert!(
                !c.validate().iter().any(|i| i.pointer == "/server/addr"),
                "{ok} should be accepted as bindable"
            );
        }
    }

    #[test]
    fn validate_flags_bad_proxy_and_zero_rate_limit() {
        let mut c = KbConfig::default();
        c.server.trusted_proxies = vec!["10.0.0.1".into(), "nope".into()];
        c.server.rate_limit = Some(RateLimitSection {
            search: Some(0),
            ..Default::default()
        });
        let issues = c.validate();
        assert!(issues
            .iter()
            .any(|i| i.pointer == "/server/trusted_proxies/1" && i.is_hard()));
        assert!(
            !issues
                .iter()
                .any(|i| i.pointer == "/server/trusted_proxies/0"),
            "the valid proxy must not be flagged"
        );
        assert!(issues
            .iter()
            .any(|i| i.pointer == "/server/rate_limit/search" && i.is_hard()));
    }

    // ---- U1 — [server.capture] validation ----

    #[test]
    fn validate_capture_zero_max_file_bytes_is_hard() {
        let mut c = KbConfig::default();
        c.server.capture = Some(CaptureSection {
            max_file_bytes: Some(0),
            max_request_bytes: None,
            default_kb: None,
        });
        assert!(c
            .validate()
            .iter()
            .any(|i| i.pointer == "/server/capture/max_file_bytes" && i.is_hard()));
    }

    #[test]
    fn validate_capture_zero_max_request_bytes_is_hard() {
        let mut c = KbConfig::default();
        c.server.capture = Some(CaptureSection {
            max_file_bytes: None,
            max_request_bytes: Some(0),
            default_kb: None,
        });
        assert!(c
            .validate()
            .iter()
            .any(|i| i.pointer == "/server/capture/max_request_bytes" && i.is_hard()));
    }

    #[test]
    fn validate_capture_max_request_bytes_below_max_file_bytes_is_warn_not_hard() {
        let mut c = KbConfig::default();
        c.server.capture = Some(CaptureSection {
            max_file_bytes: Some(10 * 1024 * 1024),
            max_request_bytes: Some(1024),
            default_kb: None,
        });
        let issues = c.validate();
        assert!(issues
            .iter()
            .any(|i| i.pointer == "/server/capture/max_request_bytes" && !i.is_hard()));
        // A request cap at or above the file cap is clean.
        c.server.capture = Some(CaptureSection {
            max_file_bytes: Some(10 * 1024 * 1024),
            max_request_bytes: Some(20 * 1024 * 1024),
            default_kb: None,
        });
        assert!(!c
            .validate()
            .iter()
            .any(|i| i.pointer == "/server/capture/max_request_bytes"));
    }

    #[test]
    fn validate_capture_unknown_default_kb_is_warn_not_hard() {
        let mut c = KbConfig::default();
        c.server.capture = Some(CaptureSection {
            max_file_bytes: None,
            max_request_bytes: None,
            default_kb: Some("nope".into()),
        });
        let issues = c.validate();
        assert!(issues
            .iter()
            .any(|i| i.pointer == "/server/capture/default_kb" && !i.is_hard()));
        // A known kb name is clean.
        c.kb.insert(KbName::new("canon").unwrap(), kb_section_with_model(None));
        c.server.capture = Some(CaptureSection {
            max_file_bytes: None,
            max_request_bytes: None,
            default_kb: Some("canon".into()),
        });
        assert!(!c
            .validate()
            .iter()
            .any(|i| i.pointer == "/server/capture/default_kb"));
    }

    #[test]
    fn resolved_capture_dir_defaults_and_overrides() {
        let mut sec = kb_section_with_model(None);
        assert_eq!(sec.resolved_capture_dir(), "capture");
        sec.capture_dir = Some("inbox/staged".into());
        assert_eq!(sec.resolved_capture_dir(), "inbox/staged");
    }

    #[test]
    fn capture_section_max_file_bytes_defaults_to_10mib() {
        let sec = CaptureSection::default();
        assert_eq!(sec.max_file_bytes(), crate::capture::DEFAULT_MAX_FILE_BYTES);
        let sec = CaptureSection {
            max_file_bytes: Some(42),
            max_request_bytes: None,
            default_kb: None,
        };
        assert_eq!(sec.max_file_bytes(), 42);
    }

    #[test]
    fn capture_section_max_request_bytes_defaults_to_64mib() {
        let sec = CaptureSection::default();
        assert_eq!(
            sec.max_request_bytes(),
            crate::capture::DEFAULT_MAX_REQUEST_BYTES
        );
        let sec = CaptureSection {
            max_file_bytes: None,
            max_request_bytes: Some(99),
            default_kb: None,
        };
        assert_eq!(sec.max_request_bytes(), 99);
    }

    #[test]
    fn validate_bad_redaction_regex_is_hard() {
        let mut c = KbConfig::default();
        let mut sec = kb_section_with_model(None);
        sec.outbound = Some(OutboundSection {
            strip_kb_prompt: false,
            redactions: vec![RegexRule {
                pattern: "(".into(), // unbalanced — won't compile
                replacement: String::new(),
            }],
        });
        c.kb.insert(KbName::new("k").unwrap(), sec);
        assert!(c
            .validate()
            .iter()
            .any(|i| i.pointer == "/kb/k/outbound/redactions/0/pattern" && i.is_hard()));
    }

    #[test]
    fn validate_unknown_model_and_missing_dir_are_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = KbConfig::default();
        let mut sec = kb_section_with_model(Some("no-such-model"));
        sec.path = tmp.path().join("does-not-exist");
        c.kb.insert(KbName::new("k").unwrap(), sec);
        let issues = c.validate();
        assert!(issues
            .iter()
            .any(|i| i.pointer == "/kb/k/embedding_model" && !i.is_hard()));
        assert!(issues
            .iter()
            .any(|i| i.pointer == "/kb/k/path" && !i.is_hard()));
        assert!(
            issues.iter().all(|i| !i.is_hard()),
            "unknown model + missing dir are warnings, not hard: {issues:?}"
        );
    }

    #[test]
    fn validate_backup_partial_config_warns_both_directions() {
        let mut c = KbConfig::default();
        c.backup.remote_cmd = Some(vec!["rclone".into(), "copyto".into()]);
        // remote_dest absent → warn.
        assert!(c
            .validate()
            .iter()
            .any(|i| i.pointer == "/backup/remote_dest" && !i.is_hard()));

        let mut c2 = KbConfig::default();
        c2.backup.remote_dest = Some("remote:bucket/path".into());
        // remote_cmd absent → warn.
        assert!(c2
            .validate()
            .iter()
            .any(|i| i.pointer == "/backup/remote_cmd" && !i.is_hard()));
    }

    #[test]
    fn validate_backup_empty_argv_is_hard() {
        let mut c = KbConfig::default();
        c.backup.remote_cmd = Some(vec![]);
        c.backup.remote_dest = Some("remote:bucket/path".into());
        assert!(c
            .validate()
            .iter()
            .any(|i| i.pointer == "/backup/remote_cmd" && i.is_hard()));
    }

    #[test]
    fn validate_backup_fully_configured_is_clean() {
        let mut c = KbConfig::default();
        c.backup.remote_cmd = Some(vec![
            "rclone".into(),
            "copyto".into(),
            "{src}".into(),
            "{dest}".into(),
        ]);
        c.backup.remote_dest = Some("remote:bucket/path".into());
        assert!(c
            .validate()
            .iter()
            .all(|i| !i.pointer.starts_with("/backup")));
    }

    #[test]
    fn backup_section_build_argv_substitutes_src_and_dest() {
        let mut sec = BackupSection {
            remote_cmd: Some(vec![
                "rclone".into(),
                "copyto".into(),
                "{src}".into(),
                "{dest}".into(),
            ]),
            remote_dest: Some("remote:bucket/path".into()),
        };
        let argv = sec
            .build_argv(Path::new("/tmp/x/kb-20260710.tar.gz"))
            .expect("fully configured");
        assert_eq!(
            argv,
            vec![
                "rclone",
                "copyto",
                "/tmp/x/kb-20260710.tar.gz",
                "remote:bucket/path",
            ]
        );

        // Not configured (missing remote_dest) → None.
        sec.remote_dest = None;
        assert!(sec.build_argv(Path::new("/tmp/x/kb.tar.gz")).is_none());
    }

    // ---- MI-W2.1 / MI-W5.R — [memory] scoring_v2_relevance/_stability ----

    /// An absent `[memory]` section resolves to the MI-W5.R shipped default:
    /// relevance ON (measured, wins), stability OFF (unmeasured, pending a
    /// bench that loads the sessions corpus).
    #[test]
    fn memory_section_absent_defaults_to_relevance_on_stability_off() {
        let c = KbConfig::from_toml_str("").unwrap();
        assert!(c.memory.scoring_v2_relevance);
        assert!(!c.memory.scoring_v2_stability);
        assert_eq!(c.memory.scoring_v2, None);
    }

    #[test]
    fn memory_section_parses_the_two_flags_independently() {
        let c = KbConfig::from_toml_str(
            "[memory]\nscoring_v2_relevance = false\nscoring_v2_stability = true\n",
        )
        .unwrap();
        assert!(!c.memory.scoring_v2_relevance);
        assert!(c.memory.scoring_v2_stability);
    }

    /// A `[memory]` section that sets ONLY `scoring_v2_stability` must not
    /// disturb `scoring_v2_relevance`'s own default (`true`) — the two keys
    /// are independent, not a package deal.
    #[test]
    fn memory_section_partial_stability_only_keeps_relevance_default() {
        let c = KbConfig::from_toml_str("[memory]\nscoring_v2_stability = true\n").unwrap();
        assert!(
            c.memory.scoring_v2_relevance,
            "relevance keeps its own default"
        );
        assert!(c.memory.scoring_v2_stability);
    }

    /// MI-W5.R — the deprecated `scoring_v2` alias sets BOTH new flags to
    /// its value, reproducing the pre-split "one flag gates both" behavior,
    /// so an existing `kb.toml` doesn't silently change meaning.
    #[test]
    fn memory_section_deprecated_scoring_v2_alias_sets_both_flags_true() {
        let c = KbConfig::from_toml_str("[memory]\nscoring_v2 = true\n").unwrap();
        assert!(c.memory.scoring_v2_relevance);
        assert!(c.memory.scoring_v2_stability);
        assert_eq!(c.memory.scoring_v2, Some(true));
    }

    #[test]
    fn memory_section_deprecated_scoring_v2_alias_sets_both_flags_false() {
        let c = KbConfig::from_toml_str("[memory]\nscoring_v2 = false\n").unwrap();
        assert!(!c.memory.scoring_v2_relevance);
        assert!(!c.memory.scoring_v2_stability);
    }

    // invariant:13 minimal-diff
    #[test]
    fn save_preserving_keeps_comments_on_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("kb.toml");
        std::fs::write(
            &path,
            "# top-of-file note\n\
             [server]\n\
             # the listen address\n\
             addr = \"127.0.0.1:4000\"\n\
             \n\
             [indexer]\n\
             debounce_ms = 400  # inline note\n",
        )
        .unwrap();

        let mut c = KbConfig::load(&path).unwrap();
        c.server.addr = "127.0.0.1:4100".into();
        c.save_preserving(&path).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("# top-of-file note"),
            "top comment survived:\n{after}"
        );
        assert!(
            after.contains("# the listen address"),
            "addr comment survived:\n{after}"
        );
        // Inline comment on an UNCHANGED line survives — the diff-merge
        // never touches debounce_ms, so its trailing comment is intact.
        assert!(
            after.contains("# inline note"),
            "inline comment on the unchanged line survived:\n{after}"
        );
        assert!(
            after.contains("127.0.0.1:4100"),
            "addr was updated:\n{after}"
        );
        assert!(
            !after.contains("127.0.0.1:4000"),
            "old addr is gone:\n{after}"
        );
        // Minimal edit: no default fields materialised (the cruft live
        // verification caught — empty tables prepended above the header).
        assert!(!after.contains("ui = {}"), "no empty-table cruft:\n{after}");
        assert!(
            after.trim_start().starts_with("# top-of-file note"),
            "leading comment stays first (no cruft prepended):\n{after}"
        );

        let back = KbConfig::load(&path).unwrap();
        assert_eq!(back.server.addr, "127.0.0.1:4100");
        assert_eq!(
            back.indexer.debounce_ms,
            Some(400),
            "untouched field intact"
        );
    }

    #[test]
    fn save_preserving_adds_changed_field_in_place() {
        // A field absent from the file is inserted into the right section
        // (not at the top), comments preserved.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("kb.toml");
        std::fs::write(&path, "# header\n[server]\naddr = \"127.0.0.1:4000\"\n").unwrap();
        let mut c = KbConfig::load(&path).unwrap();
        c.server.mdns = true; // absent in the file (default false)
        c.save_preserving(&path).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("mdns = true"), "mdns added:\n{after}");
        assert!(
            after.trim_start().starts_with("# header"),
            "header stays first:\n{after}"
        );
        let server_idx = after.find("[server]").unwrap();
        assert!(
            after.find("mdns = true").unwrap() > server_idx,
            "mdns landed under [server], not above the header:\n{after}"
        );
        assert!(KbConfig::load(&path).unwrap().server.mdns);
    }

    #[test]
    fn save_preserving_falls_back_when_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("new.toml");
        let mut c = KbConfig::default();
        c.daemon.name = Some("fresh".into());
        c.save_preserving(&path).unwrap();
        assert!(path.exists());
        assert_eq!(
            KbConfig::load(&path).unwrap().daemon.name.as_deref(),
            Some("fresh")
        );
    }

    // ---- W7/LF-1 — [sessions] ----

    #[test]
    fn sessions_section_absent_from_toml_keeps_the_real_default_window() {
        // The derive(Default)-bypasses-serde-defaults trap this struct's doc
        // comment warns about: an absent `[sessions]` table must NOT resolve
        // `live_window_secs()` to 0.
        let c = KbConfig::default();
        assert_eq!(c.sessions.live_transcripts_dir, None);
        assert_eq!(
            c.sessions.live_window_secs(),
            SessionsSection::DEFAULT_LIVE_WINDOW_SECS
        );
    }

    #[test]
    fn sessions_section_parses_from_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("kb.toml");
        std::fs::write(
            &path,
            "[sessions]\nlive_transcripts_dir = \"/tmp/live\"\nlive_window_secs = 30\n",
        )
        .unwrap();
        let c = KbConfig::load(&path).unwrap();
        assert_eq!(
            c.sessions.live_transcripts_dir,
            Some(PathBuf::from("/tmp/live"))
        );
        assert_eq!(c.sessions.live_window_secs(), 30);
    }

    #[test]
    fn live_window_secs_zero_floors_to_the_default() {
        let s = SessionsSection {
            live_transcripts_dir: None,
            live_window_secs: Some(0),
        };
        assert_eq!(
            s.live_window_secs(),
            SessionsSection::DEFAULT_LIVE_WINDOW_SECS
        );
    }

    #[test]
    fn validate_sessions_zero_window_is_warn_not_hard() {
        let mut c = KbConfig::default();
        c.sessions.live_window_secs = Some(0);
        let issues = c.validate();
        let issue = issues
            .iter()
            .find(|i| i.pointer == "/sessions/live_window_secs")
            .expect("zero window must be flagged");
        assert!(!issue.is_hard(), "zero window should warn, not hard-fail");
    }

    // `expand_tilde` reads the process-global `$HOME` — serialise the two
    // tests that override it (embed_ipc.rs's `ENV_LOCK` precedent) and
    // always restore the original value, since other tests elsewhere in
    // this binary (e.g. anything touching `KbPaths::new`/`directories`)
    // read the real `$HOME` and must not observe the fake one.
    static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn expand_tilde_bare_expands_to_home() {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let orig = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/tildetest");
        let got = expand_tilde(Path::new("~"));
        match orig {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(got, PathBuf::from("/home/tildetest"));
    }

    #[test]
    fn expand_tilde_prefix_expands_and_joins_rest() {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let orig = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/tildetest");
        let got = expand_tilde(Path::new("~/.claude/projects"));
        match orig {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(got, PathBuf::from("/home/tildetest/.claude/projects"));
    }

    #[test]
    fn expand_tilde_leaves_absolute_and_relative_paths_alone() {
        assert_eq!(
            expand_tilde(Path::new("/already/absolute")),
            PathBuf::from("/already/absolute")
        );
        assert_eq!(
            expand_tilde(Path::new("relative/dir")),
            PathBuf::from("relative/dir")
        );
        // `~otheruser/...` is deliberately NOT expanded — kb has no portable
        // way to resolve another user's home.
        assert_eq!(
            expand_tilde(Path::new("~otheruser/projects")),
            PathBuf::from("~otheruser/projects")
        );
    }

    #[test]
    fn resolved_live_dir_none_when_unset() {
        let s = SessionsSection::default();
        assert_eq!(s.resolved_live_dir(), None);
    }
}
