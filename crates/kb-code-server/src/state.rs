//! Shared daemon state threaded through route handlers.

use crate::blame::BlameCache;
use crate::config::{
    BehavioralSection, DoclensSection, KbDaemonSection, RepoEntry, ReviewSection, ScipSection,
    ScopesSection, SemanticSection,
};
use crate::git_status::StatusIndex;
use crate::join::kb_client::KbClient;
use crate::mirror::MirrorWatcher;
use crate::search::{FileIndex, SymbolIndex};
use crate::semantic::{ChunkStore, SemanticIndexer};
use crate::store::Store;
use crate::transcripts::indexer::TranscriptWatcher;
use kb_core::embed::Embedder;
use kb_core::events::EventBus;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Immutable per-boot state. W1.2 has no live-reload; a `PUT /api/config`
/// companion (if kb-code ever grows one) would mirror kb-server's CE
/// restart-loop (invariant #13) rather than mutating this in place.
///
/// `store`/`repo_ids` are W1.5 additions: `store` is opened once at boot
/// (`bind_and_spawn`) and shared read-only from here on (its own internal
/// `Mutex` serializes writes — see `store.rs`'s module doc); `repo_ids`
/// maps each configured repo's `name` (from `repos`) to the `repos.id` row
/// `bind_and_spawn` registered for it at boot, so route handlers never
/// re-derive or re-query it per request.
///
/// `bus`/`watch_mode`/`watcher` are W1.6 additions — see `bind_and_spawn`'s
/// doc for how they're wired together (the real `sink::IndexSink` +
/// `mirror::MirrorWatcher` + `GET /api/events`).
#[derive(Clone)]
pub struct AppState {
    pub version: &'static str,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub repos: Vec<RepoEntry>,
    pub store: Arc<Store>,
    pub repo_ids: HashMap<String, i64>,
    /// The daemon-wide SSE event bus (`GET /api/events`) — mirrors
    /// `kb_server::state::KbHandles::bus`'s "one bus, one monotonic id
    /// space" shape, scaled down to kb-code's single-daemon-many-repos
    /// world (there's no per-kb split to unify here, so this is just the
    /// one bus). The `sink` worker task emits `mirror.updated`/
    /// `repo.head_moved` onto it; nothing else publishes to it in Wave 1.
    pub bus: Arc<EventBus>,
    /// `"watching"` or `"polling"` — the live-mirror backend this boot
    /// actually armed (`config.watcher.mode`, resolved via
    /// `mirror::parse_watch_mode`). Constant for the whole daemon: W1.4's
    /// `MirrorConfig` has no per-repo override, and `Auto` resolves to the
    /// same `notify` backend as `Native` in `mirror::arm` (see that fn) —
    /// so this is a simple two-way split, computed once at boot rather than
    /// re-derived per `/api/repos` request. A repo mid-git-operation
    /// (rebase/merge marker present) reports `"gated"` instead — see
    /// `routes::watcher_state`, which checks that FRESH per request (marker
    /// presence isn't tracked here; the drain thread's live `RepoGate`
    /// state isn't shared cross-thread by `mirror`'s design).
    pub watch_mode: &'static str,
    /// Held purely for RAII — dropping a `MirrorWatcher` stops its
    /// background thread (see that type's doc). Never read directly after
    /// construction; mirrors `mirror::HeldDebouncer`'s own
    /// `#[allow(dead_code)]` convention for the identical reason. `Arc`-
    /// wrapped (not a bare field) so `AppState` stays cheaply `Clone`
    /// without requiring `MirrorWatcher: Clone`.
    #[allow(dead_code)]
    pub watcher: Arc<MirrorWatcher>,
    /// W2.1 — the files/symbols search lanes' per-boot in-memory caches
    /// (`search`'s module doc). `Arc`-wrapped for the same reason `store`
    /// is: cheap to clone into `AppState`'s own `Clone` impl, shared
    /// read-through by every request handler.
    pub file_index: Arc<FileIndex>,
    pub symbol_index: Arc<SymbolIndex>,
    /// V71-D1 — the per-signal search ranking flags (`[search]` in
    /// `kb-code.toml`, `config::SearchSection`), resolved ONCE at boot so
    /// every lane in one request agrees about which factors are on. `Copy`,
    /// not `Arc`: three booleans.
    pub search_factors: crate::search::Factors,
    /// V72-H4a — `aug-lane/1`'s enablement gate (`[lanes]` in
    /// kb-code.toml, `config::LanesSection`), resolved ONCE at boot for the
    /// same reason `search_factors` is: every surface in one request must
    /// agree about which lanes exist, and there is no live reload (see this
    /// struct's own doc). A lane is enabled ONLY from here — never by a
    /// route, never by a request, never by a file inside a repo.
    pub lanes: crate::config::LanesSection,
    /// V70-A3X — `GET /api/status`'s per-repo cache, same generation-gated
    /// shape (and same per-boot-singleton convention) as `file_index`/
    /// `symbol_index` above — see `git_status`'s module doc.
    pub status_index: Arc<StatusIndex>,
    /// W2.3 — the semantic search lane's config, kept on `AppState` so
    /// route handlers can gate on `repo_enabled` without re-parsing
    /// `kb-code.toml` (there is no live-reload — see this struct's own
    /// doc — so a clone taken once at boot never goes stale mid-process).
    pub semantic: SemanticSection,
    /// `Some` only when `semantic.enabled` — kb-code's own lance
    /// chunk-vector store (`<state>/kb-code/lance/`). `None` means the
    /// daemon never opened the semantic lance dataset at all (a fresh
    /// install with the feature off pays zero extra cost — see
    /// `SemanticSection`'s doc).
    pub semantic_chunk_store: Option<Arc<ChunkStore>>,
    /// `Some` only when `semantic.enabled` — the shared
    /// `jina-embeddings-v2-base-code` embedder subprocess handle (one per
    /// daemon, reused across every enabled repo's search + indexing).
    pub semantic_embedder: Option<Arc<Mutex<Embedder>>>,
    /// Held purely for RAII, same convention as `watcher` above — dropping
    /// it does not stop the background worker (see `SemanticIndexer`'s
    /// doc). `None` when the semantic lane is off.
    #[allow(dead_code)]
    pub semantic_indexer: Option<Arc<SemanticIndexer>>,
    /// W2.5 — the raw-transcripts lane's resolved `[transcripts] root`
    /// (`config::TranscriptsSection::resolved_root`, tilde-expanded).
    /// Always set regardless of `transcripts_watcher`'s state — the search
    /// route's snippet re-read and `GET /api/transcripts/status`'s
    /// reported `root` both need it even when the lane is disabled/failed
    /// to start (search then just returns zero hits from an empty
    /// index, rather than the route itself needing a special case).
    pub transcripts_root: PathBuf,
    /// Held for RAII (dropping stops the watcher's thread — same
    /// convention as `watcher` above) AND read by
    /// `transcripts::search::transcripts_status` to report whether the
    /// lane is actually live. `None` when `[transcripts] enabled = false`
    /// or the watcher failed to start (`bind_and_spawn` logs why; not
    /// fatal to daemon boot, unlike the mirror watcher — see
    /// `transcripts`' module doc).
    pub transcripts_watcher: Option<Arc<TranscriptWatcher>>,
    /// W2.4 — the unified Search-Everywhere box's SESSIONS lane federation
    /// target (`[kb_daemon]`, `search::sessions`). Cloned once at boot, same
    /// no-live-reload posture as `semantic` above.
    pub kb_daemon: KbDaemonSection,
    /// W2.4 — the SAME `AuthConfig` `router::build_router` layers
    /// `auth_bearer` with, ALSO reachable from inside a plain route handler
    /// (`routes::search_unified`) so it can re-derive loopback-ness itself
    /// (`kb_server::middleware::is_loopback_origin`) for the box's
    /// transcripts section — see that route's doc for why a handler needs
    /// this rather than only a middleware layer.
    pub auth: Arc<kb_server::state::AuthConfig>,
    /// W3.1 — the BLAME service's per-daemon `(repo_id, commit_sha, path)`
    /// region cache (`blame::cache`'s module doc — "Gitiles' shape"). One
    /// shared cache across every repo/request, same per-boot-singleton
    /// convention as `file_index`/`symbol_index` above.
    pub blame_cache: Arc<BlameCache>,
    /// W3.2 — the join ladder's federation handle (`join::kb_client::
    /// KbClient`), same `[kb_daemon]` target as `kb_daemon` above but a
    /// PERSISTENT per-boot object (not a stateless per-call helper like
    /// `search::sessions::search`) because it also owns the cached,
    /// TTL'd commit-map snapshot the fuzzy/squash arms share across calls
    /// — see that type's doc.
    pub kb_client: Arc<KbClient>,
    /// W3.6 — the join ladder's PRECOMPUTE lookback window, resolved ONCE
    /// at boot (`config::BackfillSection::resolved_depth`) — `None` walks a
    /// repo's whole history, `Some(d)` bounds it to commits authored
    /// within `d` of "now." Read by `routes::backfill_route`
    /// (`POST /api/backfill?repo=`); the SAME resolved value also governs
    /// the optional `[backfill] on_boot` background run in
    /// `lib.rs::bind_and_spawn`, so the two paths can never disagree on
    /// what "the configured depth" means.
    pub backfill_depth: Option<Duration>,
    /// W4.1 — the resolved `web-code/dist` directory (`spa::
    /// resolve_spa_dist`), read once at boot. `None` means the daemon was
    /// started without a built SPA (e.g. a bare `cargo test` run, or a
    /// dev environment that only runs `npm run dev` against the API) —
    /// `spa::serve` (the router's top-level fallback) degrades to a
    /// friendly 404 JSON body rather than panicking or 500ing.
    pub spa_dist: Option<PathBuf>,
    /// Phase G-server — the GitHub read overlay's federation handle
    /// (`GET /api/prs`, `GET /api/prs/{n}/comments`), same per-boot-
    /// singleton convention as `kb_client` above.
    pub github: Arc<crate::github::GithubClient>,
    /// V72-J1 — the effective `comments/1` annotation keyword set
    /// (`[comments] keywords`), resolved ONCE at boot (same no-live-reload
    /// posture as `scopes`/`semantic` below) and shared by the extraction
    /// pass and `GET /api/comments/keywords`, so what the index CONTAINS
    /// and what the route REPORTS can never disagree.
    pub comment_keywords: Arc<crate::comments::KeywordSet>,
    /// Phase N — named path-set globs (`[scopes]`), cloned once at boot
    /// (no live-reload — same posture as `semantic`/`kb_daemon`).
    pub scopes: ScopesSection,
    /// V3.R1 — local review sessions config (`[review]`), cloned once at
    /// boot. `max_patchsets` is read by every capture path; the auto-
    /// capture worker is armed from `patchset_capture` at boot only.
    pub review: ReviewSection,
    /// V3.2-B1 — behavioral counters config (`[behavioral]`), cloned once
    /// at boot. Window/max_commit_files drive backfill + incremental;
    /// `enabled` gates the head-moved worker only.
    pub behavioral: BehavioralSection,
    /// DCB W1.C — the doc↔code bridge's config (`[doclens]`), cloned once at
    /// boot (same no-live-reload posture as `semantic`/`scopes`/`review`).
    /// `kbs` is the ONLY scope on which corpora this daemon will pull prose
    /// from; the gate is enforced INSIDE `doclens::resolve::resolve_lens`
    /// (R8) so W3.A's background sync inherits it rather than re-deriving it.
    pub doclens: DoclensSection,
    /// DCB-W3.A.R fix 5 — the daemon-wide doc-lens sync re-entrancy guard.
    /// `false → true` CAS'd by `doclens::sync::try_run_doclens_sync` before a
    /// pass starts and reset after: the periodic worker and
    /// `POST /api/doc-lens/sync` both drive the same engine, and while any
    /// ONE pass is internally idempotent, a `force` cursor reset mid-pass by
    /// one caller can be partially undone by a concurrent non-force pass that
    /// already read the old cursor. The route turns a busy guard into `409`;
    /// the worker turns it into a skipped tick — see `doclens::sync`'s module
    /// doc.
    pub doclens_sync_running: Arc<std::sync::atomic::AtomicBool>,
    /// PRR-N12 (N1) — the per-repo SCIP-indexer config (`[scip]`), cloned
    /// once at boot (same no-live-reload posture as `scopes`/`review`/
    /// `behavioral`). `routes::repos`'s `ScipStatus` computation is the
    /// ONLY reader; `crate::scip`'s ingest route never consults it (it
    /// trusts whatever the CLI already POSTed).
    pub scip: ScipSection,
    /// PRR-L2 — the lip/1 provider registry (`[[intel.providers]]`,
    /// `crate::lip::LipRegistry`), built once at boot from `config.intel`
    /// (no live-reload — same posture as `scip`/`scopes`/`review` above).
    /// Every `resolve`/`hover`/`usages`/`diagnostics` overlay entry point
    /// in `crate::lip` reads through this; each `LipClient` inside it owns
    /// its OWN lazy, once-per-process handshake state (see that module's
    /// doc) — nothing here is mutated after construction.
    pub lip: Arc<crate::lip::LipRegistry>,
    /// V70-A2 (SEC-02) — the resolved Origin/Host allowlist
    /// (`security::origin::HostPolicy`), built once at boot from
    /// `[server] hostnames` + `[doclens] origins` + `[security]`. Read by
    /// the two guard middlewares layered on the whole `/api` nest. No live
    /// reload — same posture as every other config clone above.
    pub host_policy: crate::security::origin::HostPolicy,
    /// V70-A2 (SEC-13) — the compiled secret denylist
    /// (`security::secrets::SecretPolicy`): the built-in floor plus
    /// `[security] secret_globs`, consulted by every content-returning
    /// read site.
    pub secret_policy: crate::security::secrets::SecretPolicy,
    /// V70-A2 (SEC-15) — the daemon-wide git-subprocess fan-out semaphore
    /// (`[server] git_fanout` permits). Acquired by the merge-check route
    /// and the branches ahead/behind loop before each `git` child, so a
    /// burst of conflict checks cannot put N processes on a documented
    /// IO-bound host at once. A `tokio::sync::Semaphore` (not a std one):
    /// the wait must park a FUTURE, never a blocking-pool thread.
    pub git_fanout: Arc<tokio::sync::Semaphore>,
    /// V70-A2 (SEC-15) — `<state>/kb-code/scratch`, the root every
    /// per-request `merge-tree --write-tree` object directory is created
    /// under (`history::scratch`). Never inside a browsed repo; swept for
    /// orphans once at boot.
    pub scratch_root: PathBuf,
}

pub type SharedState = Arc<AppState>;
