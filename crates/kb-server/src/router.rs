//! Top-level router. Two trees nested under one Router: the `/api/*` tree
//! for the parent origin's REST + SSE surface, and the artifact-subdomain
//! fallback that serves `<id>.artifacts.localhost:4000` requests.
//!
//! Topic 11 §G "Implementation pointers" suggests a small extractor that
//! branches on `Host:`. v0.0.1 implements that as a `fallback` handler:
//! if `/api/*` matches, the API tree handles it; otherwise the fallback
//! checks the Host header and either serves the artifact (subdomain
//! request) or returns 404 (parent origin request — no SPA in v0.0.1).

use crate::middleware::{
    auth_bearer, count_requests, is_loopback_web_origin, origin_allowlist, rate_limit, RateLimiter,
};
use crate::routes;
use crate::state::KbHandles;
use axum::{
    extract::DefaultBodyLimit,
    http::Method,
    middleware::from_fn_with_state,
    routing::{delete, get, patch, post, put},
    Router,
};
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

pub fn build_router(state: Arc<KbHandles>) -> Router {
    let auth_state = state.auth.clone();
    let origin_state = state.origin.clone();
    // v0.4 B1 + v0.5 Q1 — three per-token rate limiters, one per
    // endpoint family. Default 60 req/min/token; operators can dial
    // each independently via kb.toml `[server.rate_limit]`. Loopback
    // bypass is handled inside the middleware.
    let rate_limits = state.rate_limits;
    // v0.7.1 C1 — the limiters share the daemon's trusted-proxy Arc so
    // their loopback bypass evaluates the same set as `auth_bearer`.
    let trusted_proxies = state.origin.trusted_proxies.clone();
    let search_limiter = Arc::new(RateLimiter::new(
        rate_limits.search_per_min,
        std::time::Duration::from_secs(60),
        trusted_proxies.clone(),
    ));
    let atlas_limiter = Arc::new(RateLimiter::new(
        rate_limits.atlas_per_min,
        std::time::Duration::from_secs(60),
        trusted_proxies.clone(),
    ));
    let review_limiter = Arc::new(RateLimiter::new(
        rate_limits.review_per_min,
        std::time::Duration::from_secs(60),
        trusted_proxies.clone(),
    ));
    // M7: separate limiter for the history POST family
    // (open/scroll/search). Pre-M7 these routes were unrate-limited and
    // a runaway tab / buggy iframe runtime could fan out hundreds of
    // QPS into sqlite writes, monopolising the write lock. Default
    // 600/min/token (10/s) — well above legitimate use, low enough to
    // bound an adversarial client. Loopback bypass still applies.
    let history_limiter = Arc::new(RateLimiter::new(
        rate_limits.history_per_min,
        std::time::Duration::from_secs(60),
        trusted_proxies,
    ));
    // Y-track — the attachment-upload body cap = the configured per-file
    // limit + multipart slack. `try_read` (uncontended at boot/restart)
    // falls back to the default if the lock is momentarily held. The
    // handler enforces the precise per-file size via a streaming abort;
    // this layer is the coarse outer backstop.
    let attachment_body_limit = state
        .config
        .try_read()
        .ok()
        .and_then(|c| c.server.attachments.as_ref().map(|a| a.max_file_bytes()))
        .unwrap_or(kb_core::attachments::DEFAULT_MAX_FILE_BYTES)
        .saturating_add(routes::attachments::BODY_LIMIT_SLACK)
        as usize;
    // U2 (v0.25 quick capture) — capture upload body cap. UNLIKE the
    // Y-track attachment_body_limit above, the capture endpoint accepts a
    // MULTI-FILE batch (`routes::capture::MAX_CAPTURE_FILES`), so a
    // single-file-sized limit (`max_file_bytes` + slack) starves a legal
    // multi-file request even when every individual file is under the
    // per-file cap (U2 follow-up finding: two 6 MiB files, 10 MiB per-file
    // cap, dies as generic multipart noise before per-file logic runs).
    // The outer `DefaultBodyLimit` on both capture routes below is instead
    // `max(max_request_bytes, max_file_bytes + slack)` — the combined-
    // request budget widens automatically if an operator raises
    // `max_file_bytes` past the default `max_request_bytes`, and never
    // shrinks below one file's own worst case. Own sub-router below so the
    // limit applies only to the two capture routes, not the whole tree.
    let capture_cfg = state
        .config
        .try_read()
        .ok()
        .and_then(|c| c.server.capture.clone())
        .unwrap_or_default();
    let capture_body_limit = capture_cfg.max_request_bytes().max(
        capture_cfg
            .max_file_bytes()
            .saturating_add(routes::attachments::BODY_LIMIT_SLACK),
    ) as usize;
    let api = Router::new()
        // v0.0.1 routes
        .route("/identity", get(routes::identity::get))
        // v0.34 Y1 — configured ∪ observed users (attribution directory).
        .route("/users", get(routes::users::list))
        .route("/kbs", get(routes::kbs::list))
        // TM-track — queryable metrics snapshot (coarse always; detailed when
        // `[server] metrics = true`). A Read route, auth + loopback-bypass
        // like /identity + /stats; no rate-limit bucket.
        .route("/metrics", get(routes::metrics::get))
        // /search lives in `rate_limited` below.
        .route("/kb/{kb}/docs", get(routes::docs::list))
        // R5 — list/query comments across a kb (folder/status/author/stale
        // filters). A read, so it sits in the un-rate-limited api tree
        // alongside `docs` (the fine-grained mutations live in
        // `review_routes` below under the review rate-limit bucket).
        .route("/kb/{kb}/reviews", get(routes::comments::list_reviews))
        // Track U — resolve by source-relative path (static `by-path`
        // segment wins over the `{id}` route below in matchit).
        .route(
            "/kb/{kb}/docs/by-path/{*path}",
            get(routes::docs::get_by_path),
        )
        .route("/kb/{kb}/docs/{id}", get(routes::docs::get))
        .route("/kb/{kb}/artifact/{id}", get(routes::docs::artifact_bytes))
        // Track V — artifact version timeline + text/raw diff (read-only).
        .route(
            "/kb/{kb}/artifacts/{id}/versions",
            get(routes::versions::list),
        )
        .route("/kb/{kb}/artifacts/{id}/diff", get(routes::versions::diff))
        // RP-track — per-artifact reading summary (read; `?lite=true` =
        // whole-page numbers only). Shares the api tree's auth, no rate-limit.
        .route(
            "/kb/{kb}/artifacts/{id}/reading",
            get(routes::history::get_reading),
        )
        // DCB W1.B — deterministic code references extracted from the doc's
        // own bytes (`coderef/1`). Pure reads; no fan-out; kb never resolves
        // them (invariant #2/#4 — kb-code owns classification).
        .route(
            "/kb/{kb}/docs/{id}/code-refs",
            get(routes::coderefs::for_doc),
        )
        // CT-A1 (U3 parse-back) — every memory highlighted FROM this
        // artifact (the reverse of `MemoryProvenance`); fans out across
        // every memory-scoped corpus on the daemon (invariant #28).
        .route(
            "/kb/{kb}/docs/{id}/memories-from",
            get(routes::memory::memories_from),
        )
        // Corpus cursor feed — what kb-code's doc-lens sync pages over.
        // ASCENDING keyset on (extracted_at, artifact_id), wire-encoded as
        // ONE opaque `?cursor=<extracted_at>:<artifact_id>` string (R3 — not
        // `?cursor_id=`, and NOT `?since=`, which means a recency window
        // everywhere else in kb). No `/docs/` segment (matchit collision).
        .route("/kb/{kb}/code-refs", get(routes::coderefs::feed))
        // W2.11 — the generation prompt as first-class, scrub-gated content
        // (read-only; LOCAL-RENDER ONLY on strip-configured corpora).
        .route("/kb/{kb}/artifacts/{id}/prompt", get(routes::prompt::get))
        // Folder (or whole-kb) download → .zip of source artifacts. A
        // read, like `docs`; descendant-inclusive `folder=` filter.
        .route("/kb/{kb}/download", get(routes::download::get))
        // kb share — publish an artifact/folder to a gated/public static
        // host. POST runs the engine (deploy + gate); the reads + DELETE
        // (revoke) are cheap. Auth-gated with the rest of /api/*.
        .route("/kb/{kb}/share", post(routes::share::create))
        // Local self-contained bundle → .zip (scrubbed + in-share links
        // relativized). No host/registry; the CLI (`kb share --local`) writes it.
        .route("/kb/{kb}/share/export", post(routes::share::export))
        // Single uncompressed page → native format (scrubbed `.html`, or raw
        // `.md` source). No zip; the CLI (`kb share --page`) / SPA save it.
        .route(
            "/kb/{kb}/share/export/page",
            post(routes::share::export_page),
        )
        .route("/kb/{kb}/shares", get(routes::share::list))
        .route("/kb/{kb}/shares/{name}", delete(routes::share::delete))
        .route("/kb/{kb}/sources", get(routes::sources::list))
        .route(
            "/kb/{kb}/sources/{src}/reindex",
            post(routes::reindex::post),
        )
        .route("/kb/{kb}/reindex", post(routes::reindex::kb_post))
        // Lance dataset maintenance — compact small fragments, rebuild
        // indices, prune old manifest versions. Sibling of reindex but
        // doesn't touch source files. Synchronous: the actor serialises
        // writes anyway, so we just await the optimize pass.
        .route("/kb/{kb}/compact", post(routes::compact::post))
        // v0.9 M4 — memory ingest: render + write into the corpus dir;
        // the watcher indexes it (write-only). State-changing, so the
        // origin allowlist applies (CLI/loopback bypass).
        .route("/kb/{kb}/artifacts", post(routes::artifacts::create))
        // v0.9 M5 — `kb forget`: delete the artifact file (watcher drops
        // the row; explicit delete_by_path makes it immediate).
        .route("/kb/{kb}/artifacts/{id}", delete(routes::artifacts::delete))
        // Edit kb-tags / kb-category in the source file, then re-index
        // (write-only, like create). Same auth/origin guards as the
        // artifact mutations above.
        .route(
            "/kb/{kb}/artifacts/{id}/meta",
            patch(routes::artifacts::patch_meta),
        )
        .route("/events", get(routes::events::get))
        .route("/events.schema.json", get(routes::schema::enum_get))
        .route(
            "/events/schema/{kind}/{version}",
            get(routes::schema::per_type),
        )
        // v0.1 additions (topic 11 §B)
        .route(
            "/settings",
            get(routes::settings::get).patch(routes::settings::patch),
        )
        // CE — read/edit the daemon's kb.toml; PUT persists + restarts.
        .route("/config", get(routes::config::get).put(routes::config::put))
        // L2 — runtime FILE log level (reload handle; live, no restart).
        .route(
            "/log-level",
            get(routes::log_level::get).put(routes::log_level::put),
        )
        .route("/stats", get(routes::stats::cross))
        .route("/kb/{kb}/stats", get(routes::stats::per_kb))
        .route("/kb/{kb}/sources/{src}/pause", post(routes::sources::pause))
        .route(
            "/kb/{kb}/sources/{src}/resume",
            post(routes::sources::resume),
        )
        .route("/kb/{kb}/errors", get(routes::errors::list))
        .route(
            "/kb/{kb}/errors/{err}/dismiss",
            post(routes::errors::dismiss),
        )
        .route(
            "/kb/{kb}/errors/{err}/apply-fix",
            post(routes::errors::apply_fix),
        )
        // X3 (v0.24) — per-file exclusion. POST removes the doc via the
        // KeepUserData cascade (comments + history survive) and gates every
        // ingest seam; DELETE re-includes + force-reindexes. The {path}
        // segment is the percent-encoded source-relative path (`%2F` for
        // `/` — axum decodes a single segment, matchit splits first).
        .route(
            "/kb/{kb}/exclusions",
            get(routes::exclusions::list).post(routes::exclusions::create),
        )
        .route(
            "/kb/{kb}/exclusions/{path}",
            delete(routes::exclusions::remove),
        )
        // Quarantine inspection + un-quarantine. List filters open
        // errors by retry_count >= QUARANTINE_THRESHOLD-1 to find
        // artifacts the indexer gave up on; restore clears the rows,
        // removes the sidecar, and re-emits watch.modify (force=true)
        // so the indexer retries with a clean retry-counter.
        .route("/kb/{kb}/quarantine", get(routes::quarantine::list))
        .route(
            "/kb/{kb}/quarantine/restore",
            post(routes::quarantine::restore),
        )
        .route(
            "/kb/{kb}/quarantine/restore-all",
            post(routes::quarantine::restore_all),
        )
        // GS-track — the static `report` segment wins over `{id}` (matchit
        // static-priority); artifact ids are 12-hex so no real id collides.
        .route("/kb/{kb}/graph/report", get(routes::graph::report))
        .route("/kb/{kb}/graph/{id}", get(routes::graph::get))
        // CT-F5 — corpus-health SLOs. Read + an append-only snapshot write.
        // Ordinary `auth_bearer` surface, deliberately NOT loopback-only:
        // nothing here is a tree/ref mutation or unscrubbed transcript text,
        // and the write appends a row of four numbers to a log that changes
        // no behaviour anywhere (surfaced, never enforced).
        .route("/kb/{kb}/slo", get(routes::slo::get))
        .route("/kb/{kb}/slo/snapshot", post(routes::slo::snapshot))
        .route("/kb/{kb}/slo/snapshots", get(routes::slo::snapshots))
        // v0.6 R1 — per-kb history snapshots, cheap GETs.
        .route("/kb/{kb}/runs", get(routes::sources::runs_list))
        .route("/kb/{kb}/queries", get(routes::search::queries_list))
        // GC-B3 — cross-kb zero-hit corpus-gap report (scope=all fan-out,
        // invariant #28), the queries-ring sibling to `/stats`.
        .route(
            "/queries/zero-hit",
            get(routes::search::queries_zero_hit_all),
        )
        // v0.6 T1 — frequency-sorted tag aggregator for the SPA left rail.
        .route("/kb/{kb}/tags", get(routes::tags::list))
        // Q-track — category/status/severity facet buckets for the search rail.
        .route("/kb/{kb}/facets", get(routes::facets::list))
        // Resurface — deterministic pull-only queue (open comments + unfinished
        // reads); design in docs/research/kb-resurface-queue-2026-07.html.
        .route("/kb/{kb}/resurface", get(routes::resurface::list))
        // W2.2 — on-this-day + session echoes: a deterministic date-join
        // (created/read/worked-on vs 1-3yr + 6mo anniversaries), sibling to
        // resurface above — separate endpoint so its own scoring never rides
        // through resurface's golden-pinned comment/read term contract.
        .route("/kb/{kb}/echoes", get(routes::echoes::list))
        // Unit 2 — the desk radiator: a deterministic, per-(kb, UTC day)
        // HTML+inline-SVG "day at a glance" document (an e-ink panel's ONE
        // fixed URL) or its JSON data twin (SPA `/ambient`, `kb daycard`);
        // content-negotiated in `routes::daycard::wants_json`. Reuses
        // resurface/history/list_docs exactly like echoes/timeline above —
        // no rate-limit bucket needed (same read-class shape).
        .route("/kb/{kb}/daycard", get(routes::daycard::get))
        // v0.8 G1 — folder tree (descendant-inclusive counts) for the
        // gallery left rail's expandable folder filter.
        .route("/kb/{kb}/folders", get(routes::folders::list))
        // F3b — batch folder rename (prefix rewrite via relocate engine).
        .route(
            "/kb/{kb}/folders/rename",
            post(routes::relocate::rename_folder),
        )
        // F3b — single-artifact move/rename (path-derived id remaps).
        .route("/kb/{kb}/docs/{id}/move", post(routes::relocate::move_doc))
        // L1 — resolve a 12-hex id, source-relative path, or unique
        // filename/suffix to a single artifact (drives `kb find` and
        // the `--path` flag on `kb comments`).
        .route("/kb/{kb}/lookup", get(routes::lookup::lookup))
        // Cross-artifact edges (kind = 'link'); backs the SPA atlas view.
        .route("/kb/{kb}/edges", get(routes::atlas::list_edges))
        // W1.B — deterministic c-TF-IDF cluster labels, refreshed on every
        // atlas recompute/recluster. Plain read (one small sqlite scan, no
        // memoization needed), so it rides the base api tree rather than
        // `atlas_routes`'s recompute/recluster rate limiter below.
        .route("/kb/{kb}/atlas/labels", get(routes::atlas::labels))
        // W2.3a — true (embedding-space) nearest neighbors for one doc.
        // Sibling of atlas/labels above: plain read, no rate-limit bucket.
        .route("/kb/{kb}/atlas/similar/{id}", get(routes::atlas::similar))
        // M-a — the FULL-corpus atlas point set in one memoised scan (fixes
        // the paged `?projection=atlas` gallery route silently truncating
        // the map past its 200-doc default limit). Sibling of atlas/labels
        // + atlas/similar above: plain read, no rate-limit bucket.
        .route("/kb/{kb}/atlas/points", get(routes::atlas::points))
        // W3 T-b — the corpus time-lapse: the frame list, and one frame's
        // points server-side Procrustes-aligned against another. Same
        // posture as atlas/labels + atlas/points above: small bounded
        // sqlite scans, no rate-limit bucket.
        .route("/kb/{kb}/atlas/history", get(routes::atlas::history_list))
        .route(
            "/kb/{kb}/atlas/history/{id}",
            get(routes::atlas::history_show),
        )
        // Explicit operator retention (the insert path already self-prunes
        // to DEFAULT_ATLAS_FRAMES_KEEP=24) — a plain-enough sqlite DELETE
        // that it doesn't need `atlas_routes`'s recompute/recluster bucket.
        .route(
            "/kb/{kb}/atlas/history/prune",
            post(routes::atlas::history_prune),
        )
        // W3 F-b — the dual-field atlas: an operator-hand-placed JSON
        // Canvas overlay (fixed per-kb sidecar, same "store what parses"
        // posture as boards) plus the machine-vs-operator displacement
        // score. Same read/write posture as boards below: no rate-limit
        // bucket, no index interaction whatsoever. See `routes::atlas_field`
        // module doc for the storage ruling.
        .route(
            "/kb/{kb}/atlas/field",
            get(routes::atlas_field::get_field).put(routes::atlas_field::put_field),
        )
        .route(
            "/kb/{kb}/atlas/field/disagreement",
            get(routes::atlas_field::disagreement),
        )
        // v0.6+ H2 — per-user activity log: artifact opens (with
        // scroll-resume), search queries, comments authored.
        // The GET shares the api tree's auth + loopback bypass; the
        // POST family is rate-limited via `history_routes` below
        // (deep-review M7).
        .route("/kb/{kb}/history", get(routes::history::get_list))
        // W2.10 — per-UTC-day event density for the gallery's activity
        // calendar (bounded GROUP BY, no rate-limit bucket needed — same
        // read-class shape as history/echoes/facets above).
        .route(
            "/kb/{kb}/history/calendar",
            get(routes::history::get_calendar),
        )
        // C-a — four day-bucketed lanes (created/read/session/comment)
        // sharing one UTC-day axis: the reflection canvas's foundation.
        // Same read-class shape as history/calendar/echoes/facets above —
        // no rate-limit bucket needed.
        .route("/kb/{kb}/timeline", get(routes::timeline::list))
        // Q1 — fleet-wide cold load for the SPA stale-anchors dashboard;
        // walks every kb's persisted `.anchors-stale.json` sidecar.
        .route("/anchors/stale", get(routes::anchors::list_stale))
        // v0.10 K2 — anchor corkboard. Cross-kb list + per-kb pin/unpin.
        // SPA-facing name "anchors"; internally the table is `corkboard`
        // so the existing stale-anchor sidecar (above) keeps its name.
        .route("/anchors", get(routes::anchors::list))
        // Z4 — fleet-wide open-comments inbox. Cross-kb read: every OPEN
        // kb-comments/1 comment across all corpora, newest activity first,
        // fanned out (#28). Sibling of /anchors + /sessions (plain api tree
        // auth + loopback bypass; no rate-limit bucket).
        .route("/inbox", get(routes::inbox::list))
        // Desk v2 — federated handoff aggregate. Cross-kb read of indexed
        // `handoff/` docs (invariant #28 fan-out). Same auth posture as
        // `/inbox` (plain api tree + loopback bypass, no rate-limit bucket).
        .route("/desk", get(routes::desk::list))
        // CT-D1 — the ONE deterministic, budgeted context pack: recall +
        // recollect POINTERS + open comments on matching artifacts + the
        // kb-local code_refs summary, composed in-process (no lane is a new
        // read). Cross-kb, fanned out (#28); same auth posture as /inbox
        // above (plain api tree + loopback bypass, no rate-limit bucket) —
        // it composes reads the caller could already make one at a time.
        .route("/context", get(routes::context::get))
        // W2.15b — the tribal-knowledge proposal inbox: post-session memory
        // CANDIDATES (agent-authored) land in a per-kb `.proposals/` queue;
        // approve fires the same write path `POST .../artifacts` uses.
        // Submit is a write (sibling of `/kb/{kb}/artifacts` above — same
        // auth/origin posture, no rate-limit bucket); the fleet GET is a
        // plain fanned-out read, sibling of `/inbox` above.
        .route("/kb/{kb}/proposals", post(routes::proposals::submit))
        .route("/proposals", get(routes::proposals::list))
        .route(
            "/kb/{kb}/proposals/{id}/approve",
            post(routes::proposals::approve),
        )
        .route(
            "/kb/{kb}/proposals/{id}/reject",
            post(routes::proposals::reject),
        )
        .route(
            "/kb/{kb}/anchors/{artifact_id}",
            post(routes::anchors::pin).delete(routes::anchors::unpin),
        )
        // v0.14 S3 — sessions feature. Cross-kb list of memory-session
        // artifacts (`kb-category=memory-session`), per-session detail,
        // and per-session memory rows. SSE: session.captured /
        // session.deleted carry `{kb, artifact_id, session_id, ...}`.
        .route("/sessions", get(routes::sessions::list))
        // A1 — folder facet. Registered before `/{session_id}` so the static
        // segment wins over the dynamic capture.
        .route("/sessions/folders", get(routes::sessions::folders))
        // W7 (R15/LF-1) — the Tier-1 stat-only presence probe. Static,
        // registered before `/{session_id}` for the same reason as every
        // other static segment on this route family; loopback-only HARD
        // (LF-5, checked INSIDE the handler — see `routes::sessions::presence`).
        .route("/sessions/presence", get(routes::sessions::presence))
        // LSC-2 (`docs/research/kb-live-sessions-cockpit-2026-08.html` §6)
        // — the live-sessions cockpit's daemon side. Daemon-wide (NOT
        // `{kb}`-scoped, unlike almost everything else on this route
        // family — a beat arrives before any capture exists to attribute
        // it to). `auth_bearer` applies normally (this whole `api` tree's
        // layer, below) — deliberately NOT loopback-only, unlike
        // `/sessions/presence`/`/sessions/{id}/live` above: see
        // `routes::sessions::live_status`'s doc comment for why serving
        // metadata (not raw transcript bytes) under the ordinary auth
        // posture is safe here. Static, registered before `/{session_id}`.
        .route("/sessions/beat", post(routes::sessions::beat))
        .route("/sessions/live-status", get(routes::sessions::live_status))
        // SL2 (`docs/research/kb-slate-design-2026-09.html` §9) — the
        // per-project blackboard. Daemon-wide (NO `{kb}` segment: a slate
        // keys on a PROJECT, and a project maps to several kbs or none —
        // D2), mounted here beside `/sessions/beat` because it shares that
        // family's posture exactly: `auth_bearer` from this tree's layer
        // below, deliberately NOT loopback-only, so the SPA behind
        // Authelia works (§8). The one exception is `DELETE …?purge=true`,
        // which checks loopback INSIDE the handler like
        // `/sessions/presence`. Static segments (`/posts`, `/delta`, …)
        // are registered after the `/{slug}` root; axum's matcher prefers
        // the longer literal path, so no ordering trap here — but the
        // `{id-or-seq}` capture MUST stay under `/posts/` for the same
        // reason `/sessions/{session_id}` sits after its static siblings.
        .route("/slates", get(routes::slates::list))
        .route(
            "/slates/{slug}",
            get(routes::slates::get).delete(routes::slates::purge),
        )
        .route(
            "/slates/{slug}/posts",
            get(routes::slates::posts).post(routes::slates::append),
        )
        .route(
            "/slates/{slug}/posts/{key}",
            get(routes::slates::post_detail),
        )
        .route("/slates/{slug}/delta", get(routes::slates::delta))
        .route("/slates/{slug}/history", get(routes::slates::history))
        // D27 (v0.42) — the REPORTED cursor. A mutation, deliberately not
        // folded into any GET: nothing here is written by a read.
        .route("/slates/{slug}/cursor", post(routes::slates::cursor))
        .route("/slates/{slug}/close", post(routes::slates::close))
        .route("/slates/{slug}/reopen", post(routes::slates::reopen))
        .route("/slates/{slug}/rotate", post(routes::slates::rotate))
        // W3.A/P4 — the projects facet (registry-merged rollup cards).
        // Static, before `/{session_id}`.
        .route("/sessions/projects", get(routes::sessions::projects))
        // P7 — narrative threads (sessions clustered into continued efforts).
        .route("/sessions/threads", get(routes::sessions::threads))
        // P8 — materialise a thread into an editable kb-list/1 list.
        .route(
            "/sessions/threads/save",
            post(routes::sessions::save_thread),
        )
        // R3 — `kb recollect`: semantic "has this been done?" over session
        // digests. Static segment, registered before `/{session_id}`.
        .route("/sessions/recollect", get(routes::sessions::recollect))
        // R9 — research rollups + the activity funnel (static, before dynamic).
        .route(
            "/sessions/research-rollup",
            get(routes::sessions::research_rollup),
        )
        .route("/sessions/funnel", get(routes::sessions::funnel))
        // W6 — the project ledger (moonshots M4): a view over existing
        // primitives grouped by UTC day. Static, before `/{session_id}`.
        .route("/sessions/ledger", get(routes::sessions::ledger))
        // kb-code Wave 0 (W0.6) — the sha→session reverse lookup + the flat
        // bulk commit-map feed. Static segments, registered before
        // `/{session_id}` so they win over the dynamic capture.
        .route("/sessions/by-commit", get(routes::sessions::by_commit))
        .route("/sessions/commit-map", get(routes::sessions::commit_map))
        // W4/R8/ADD-2 — the grokclaude job join: which session(s) invoked (or,
        // from W5, ARE) this job ulid. Static prefix, registered before
        // `/{session_id}` for the same reason as `by-commit`/`commit-map`.
        .route("/sessions/by-job/{ulid}", get(routes::sessions::by_job))
        // W2/R11/C-16 — the by-artifact join: which session (if any) is
        // BEHIND this capture artifact, and whether it's the newest capture.
        // Single-kb (kb is in the path, no fan-out); static prefix registered
        // before `/{session_id}` (though `by-artifact` can never collide with
        // a session id positionally — it's under its own `{kb}/{artifact_id}`
        // 2-segment tail, not `/sessions/{session_id}`).
        .route(
            "/sessions/by-artifact/{kb}/{artifact_id}",
            get(routes::sessions::by_artifact),
        )
        .route("/sessions/{session_id}", get(routes::sessions::get))
        .route(
            "/sessions/{session_id}/memories",
            get(routes::sessions::memories),
        )
        // MI-W4.2c — the PULL side of `/memories` above: what this session's
        // `kb-recall` hook actually injected.
        .route(
            "/sessions/{session_id}/recalls",
            get(routes::sessions::recalls),
        )
        .route(
            "/sessions/{session_id}/touches",
            get(routes::sessions::touches),
        )
        // A4/A6 — the session's file-activity manifest (read/edit/write,
        // in-corpus links + out-of-corpus plain paths).
        .route("/sessions/{session_id}/files", get(routes::sessions::files))
        // S9 — the per-session decisions log (steering moments).
        .route(
            "/sessions/{session_id}/decisions",
            get(routes::sessions::decisions),
        )
        // P5 — the git actions the session produced.
        .route(
            "/sessions/{session_id}/commits",
            get(routes::sessions::commits),
        )
        // MI-W4.6 — provenance thread 3rd hop: per-commit touched-file
        // staleness (on-demand git read, no new storage).
        .route(
            "/sessions/{session_id}/commits/{sha}/files",
            get(routes::sessions::commit_files),
        )
        // R4 — the research / tool-usage signals the session produced.
        .route(
            "/sessions/{session_id}/research",
            get(routes::sessions::research),
        )
        // SP3 — stream a portable `.kbsession.zip` (manifest + raw transcript)
        // for cross-machine `claude -r` resume via `kb sessions pull`. Forces
        // the redaction floor on a non-loopback client (invariant #4/#11).
        .route(
            "/sessions/{session_id}/export",
            get(routes::sessions::export),
        )
        // W3.R-b — the session-replay/1 timeline: prompts, tool calls, edits,
        // commits and decisions on the transcript's own clock, each beat
        // resolved to (kb, artifact, heading). Forces the redaction floor on a
        // non-loopback client exactly as `/export` does (invariant #4) — the
        // wire carries verbatim prompt text and edit snippets.
        .route(
            "/sessions/{session_id}/replay",
            get(routes::sessions::replay),
        )
        // W2/R1 — the `session-view/1` interpreted IR (`fields=`/`turns=`
        // projection) and the decoded-JSONL raw twin. Same redaction-floor
        // posture as `/replay`/`/export` (invariant #4).
        .route("/sessions/{session_id}/view", get(routes::sessions::view))
        .route("/sessions/{session_id}/raw", get(routes::sessions::raw))
        // W7 (R15/LF-3b) — the live-tail delta route: loopback-only HARD
        // (checked inside the handler, LF-5 — a stricter posture than
        // `/view`/`/raw`'s scrub-floor-and-serve, since a live transcript is
        // unscrubbed mid-flight content). Polled by the SPA follow-mode
        // panel + `kb sessions read --follow`, never a standing stream (#24).
        .route("/sessions/{session_id}/live", get(routes::sessions::live))
        // R5 — the session<->comments link: open review comments on the
        // in-corpus artifacts this session touched.
        .route(
            "/sessions/{session_id}/comments",
            get(routes::sessions::session_comments),
        )
        // A7 — the reverse link: which sessions touched this artifact.
        .route(
            "/artifacts/{kb}/{artifact_id}/sessions",
            get(routes::sessions::artifact_sessions),
        )
        // R2 — `kb why <file>`: the sessions that touched a file + the
        // reasoning (prompt/decisions/commits) that produced it.
        .route("/why", get(routes::sessions::why))
        // RP-track — what the human READ in the SPA during the session window
        // (the read-counterpart to /touches).
        .route(
            "/sessions/{session_id}/readings",
            get(routes::sessions::readings),
        )
        // RL-track (v0.18) — reading lists. Multiple named, ordered lists
        // per kb; entries target a whole artifact or an anchored section;
        // read state derived from reading progress. Cross-kb GET fan-out;
        // per-kb CRUD. SSE: list.created/.updated/.deleted +
        // list.entry.added/.updated/.removed (+ the indexer hook's
        // list.entry.anchor_stale/_resolved).
        .route("/lists", get(routes::lists::index))
        .route("/kb/{kb}/lists", post(routes::lists::create))
        .route(
            "/kb/{kb}/lists/{id}",
            get(routes::lists::detail)
                .patch(routes::lists::update)
                .delete(routes::lists::delete),
        )
        .route(
            "/kb/{kb}/lists/{id}/entries",
            post(routes::lists::add_entry),
        )
        .route(
            "/kb/{kb}/lists/{id}/entries/{eid}",
            patch(routes::lists::update_entry).delete(routes::lists::remove_entry),
        )
        .route("/kb/{kb}/lists/{id}/export", get(routes::lists::export))
        .route("/kb/{kb}/lists/{id}/import", post(routes::lists::import))
        .route("/kb/{kb}/lists/{id}/prune", post(routes::lists::prune))
        // List → self-contained offline .zip (ordered entries + generated TOC).
        // Same engine as /share/export; skipped tombstones via x-kb-share-skipped.
        .route(
            "/kb/{kb}/lists/{id}/share/export",
            post(routes::share::export_list),
        )
        // W2.4 — Boards v1: a JSON Canvas `.canvas` geometry sidecar per
        // reading list, corpus-resident but index-INERT (`.canvas` is
        // unmapped in every kb's ExtensionMap — no lance row, no reindex,
        // no index_generation bump). GET serves the raw doc (or the empty
        // default); PUT replaces it wholesale. SSE: board.updated {kb,
        // list_id}. See `routes::boards` module doc.
        .route(
            "/kb/{kb}/boards/{list_id}/canvas",
            get(routes::boards::get_canvas).put(routes::boards::put_canvas),
        )
        // N-track — notes / todo-lists. A note is a Markdown artifact
        // (`kb-category=note`); these routes add body editing (toggle a
        // checkbox, rewrite text) on top of the artifact machinery.
        // Cross-kb GET fan-out; per-kb CRUD + toggle/append. SSE:
        // note.created / .updated / .deleted.
        .route("/notes", get(routes::notes::list_all))
        .route(
            "/kb/{kb}/notes",
            get(routes::notes::list).post(routes::notes::create),
        )
        .route(
            "/kb/{kb}/notes/{id}",
            get(routes::notes::get_one)
                .patch(routes::notes::update)
                .delete(routes::notes::delete),
        )
        .route("/kb/{kb}/notes/{id}/toggle", post(routes::notes::toggle))
        .route(
            "/kb/{kb}/notes/{id}/tasks",
            post(routes::notes::append_task),
        )
        // Wikilinks / backlinks — notes as connective tissue. `[[…]]` in a
        // note resolves to a corpus artifact; edges ride the existing graph.
        // outgoing+backlinks for one note · backlinks for any artifact ·
        // `[[` autocomplete. All pure reads (intra-kb edges; no fan-out).
        .route("/kb/{kb}/notes/{id}/links", get(routes::notes::note_links))
        .route("/kb/{kb}/backlinks/{id}", get(routes::links::backlinks))
        .route("/kb/{kb}/wikilinks/suggest", get(routes::links::suggest))
        // CT-F3 — unlinked mentions: the DERIVED queue of docs whose prose
        // names another artifact with no edge to show for it, and the one
        // explicit verb that authors a suggested `[[wikilink]]` into a
        // Markdown source. The report never persists anything; apply is the
        // only mutation, and it re-derives the mention before splicing.
        .route(
            "/kb/{kb}/links/suggest",
            get(routes::links::suggest_unlinked),
        )
        .route("/kb/{kb}/links/apply", post(routes::links::apply_unlinked))
        // v0.9 M3 — agent-memory recall: ranked fan-out across the
        // in-scope memory corpora. A cross-kb read; sits in the api tree
        // (inherits auth + loopback bypass; no rate-limit bucket).
        .route("/memory/recall", get(routes::memory::recall))
        // MI-W1.2 — memory census: paginated per-corpus artifact scan +
        // recall-usage stats. Single-kb (`?kb=`), unlike `/memory/recall`'s
        // cross-corpus fan-out.
        .route("/memory/census", get(routes::memory::census))
        // MI-W3.1 — on-demand cross-corpus duplicate report. Read-only;
        // never mutates.
        .route("/memory/dupes", get(routes::memory::dupes))
        // MI-W4.4 — the bounded, DERIVED hygiene triage queue. Read-only;
        // never mutates.
        .route("/memory/triage", get(routes::memory::triage))
        // v0.10 M2 — memory pinning + daemon-wide decay policy.
        .route(
            "/kb/{kb}/memories/{artifact_id}/pin",
            post(routes::memory::pin_memory).delete(routes::memory::unpin_memory),
        )
        .route(
            "/kb/{kb}/memories/{artifact_id}/promote",
            post(routes::memory::promote),
        )
        // MI-W3.2b — the ONE mutable memory meta via the API (everything
        // else stays immutable; see `patch_meta`'s doc comment).
        .route(
            "/kb/{kb}/memories/{artifact_id}/salience",
            patch(routes::memory::patch_salience),
        )
        // MI-W2.4a — `kb memory log`'s data source: one supersede chain,
        // walked both directions.
        .route(
            "/kb/{kb}/memories/{artifact_id}/lineage",
            get(routes::memory::lineage),
        )
        // CT-B2 — the memory-side reverse of `memory_recalls`: every
        // session that recalled this memory, fanned out across the
        // daemon (the ledger lives with the RECALLING session's kb).
        .route(
            "/kb/{kb}/memories/{artifact_id}/recalled-by",
            get(routes::memory::recalled_by),
        )
        // CT-F1 — the memory↔commit EXACT-ID join: every commit that cited
        // this memory via a `Kb-Memory:` trailer. Same fan-out shape and
        // same placement rationale as `recalled-by` above (the rows live
        // with the RECORDING session's kb, not the memory's own).
        .route(
            "/kb/{kb}/memories/{artifact_id}/commits",
            get(routes::memory::committed_in),
        )
        // L6 — memory ↔ kb link mutations. PUT replaces the whole set;
        // POST/DELETE on /links/{target_kb} toggle a single edge. Read
        // via GET (used by the SPA popover; the recall response also
        // carries the link set inline so this is a fallback).
        .route(
            "/kb/{kb}/memories/{artifact_id}/links",
            get(routes::memory_links::get).put(routes::memory_links::replace),
        )
        .route(
            "/kb/{kb}/memories/{artifact_id}/links/{target_kb}",
            post(routes::memory_links::add).delete(routes::memory_links::remove),
        )
        .route(
            "/memory/policy",
            get(routes::memory::get_policy).put(routes::memory::put_policy),
        )
        // MI-W2.4c — EPOCH HONESTY marker for temporal-query caveats.
        .route("/memory/tombstone-era", get(routes::memory::tombstone_era))
        // v0.13 Q4 — daemon-wide saved-queries store.
        .route(
            "/saved-queries",
            get(routes::saved_queries::list).post(routes::saved_queries::upsert),
        )
        .route(
            "/saved-queries/{name}",
            delete(routes::saved_queries::delete),
        )
        // S5 admin — destructive operations gated by the SPA Settings
        // tab's type-the-name ConfirmModal. All mutate via the storage
        // actor (single-writer invariant); each emits a distinguishable
        // SSE event so subscribers can react without polling.
        //   DELETE /api/kb/{kb}                 — wipe lance + sqlite
        //                                         history/errors/edges
        //   POST   /api/kb/{kb}/history/purge   — wipe history only
        //   POST   /api/shutdown                — signal graceful drain
        .route("/kb/{kb}", delete(routes::drop::delete))
        .route("/kb/{kb}/history/purge", post(routes::history::purge))
        .route("/shutdown", post(routes::shutdown::post));

    // v0.4 B1 + v0.5 Q1 — rate-limited subset, three sub-routers so
    // each endpoint family gets its own per-token bucket. The /export
    // endpoint shares the review limiter (same .review file = same
    // /min budget). All three merged INTO the api tree below so they
    // inherit the auth layer.
    let search_routes = Router::new()
        .route("/search", get(routes::search::get))
        .layer(from_fn_with_state(search_limiter, rate_limit));
    let atlas_routes = Router::new()
        .route("/kb/{kb}/atlas/recompute", post(routes::atlas::recompute))
        .route("/kb/{kb}/atlas/recluster", post(routes::atlas::recluster))
        // W3 T-d — the RECONSTRUCTED backfill: N back-dated frames laid out
        // from today's embeddings over each mtime cut point's doc subset.
        // Lives in THIS bucket (not beside the plain history reads) because
        // it is the same class of expensive whole-corpus write as recompute
        // — N layouts instead of one — and shares its single-flight slot.
        .route(
            "/kb/{kb}/atlas/history/backfill",
            post(routes::atlas::history_backfill),
        )
        .layer(from_fn_with_state(atlas_limiter, rate_limit));
    // Y-track — comment attachment upload + serve. Shares the review
    // rate-limit bucket (same .review file = same /min budget). Carries the
    // ONLY router body-size limit (every other route is small JSON); the
    // stage handler does the precise per-file streaming enforcement.
    let attachment_routes = Router::new()
        .route(
            "/kb/{kb}/review/{id}/attachments",
            post(routes::attachments::stage),
        )
        .route(
            "/kb/{kb}/review/{id}/attachments/{aid}",
            get(routes::attachments::serve),
        )
        // Y4 — upload-and-adopt to an existing comment/reply (the CLI
        // one-shot + the SPA "add to a posted comment"), and detach.
        .route(
            "/kb/{kb}/review/{id}/comments/{cid}/attachments",
            post(routes::attachments::upload_to_comment),
        )
        .route(
            "/kb/{kb}/review/{id}/comments/{cid}/attachments/{aid}",
            delete(routes::attachments::detach_comment),
        )
        .route(
            "/kb/{kb}/review/{id}/comments/{cid}/replies/{rid}/attachments",
            post(routes::attachments::upload_to_reply),
        )
        .route(
            "/kb/{kb}/review/{id}/comments/{cid}/replies/{rid}/attachments/{aid}",
            delete(routes::attachments::detach_reply),
        )
        .layer(DefaultBodyLimit::max(attachment_body_limit))
        .layer(from_fn_with_state(review_limiter.clone(), rate_limit));
    // U2 (v0.25 quick capture) — the per-kb capture upload. Own sub-router
    // (same reason as attachment_routes above) purely for the DefaultBodyLimit;
    // merged into the api tree below so it inherits auth + the loopback-CORS
    // outer layer like every other /api/* route. Not rate-limited (mirrors
    // the memory-ingest `POST /kb/{kb}/artifacts`, also an occasional write).
    let capture_routes = Router::new()
        .route("/kb/{kb}/capture", post(routes::capture::create))
        .route("/kb/{kb}/desk", post(routes::desk::create))
        .route(
            "/kb/{kb}/artifacts/{id}/content",
            put(routes::artifacts::put_content),
        )
        .layer(DefaultBodyLimit::max(capture_body_limit));
    let review_routes = Router::new()
        // R8 — the whole-document write POST is retired; the CLI (R6) and
        // SPA (R7) both mutate through the fine-grained endpoints below.
        // GET stays (initial load, export, `kb comments show`, SSE refetch).
        .route("/kb/{kb}/review/{id}", get(routes::review::get))
        .route(
            "/kb/{kb}/review/{id}/export",
            post(routes::review::post_export),
        )
        // R5 — fine-grained comment mutations. Each runs the load →
        // typed-mutation → save sequence under `review_lock`; clients send
        // a delta instead of the whole document + If-Match.
        .route(
            "/kb/{kb}/review/{id}/comments",
            post(routes::comments::add_comment),
        )
        .route(
            "/kb/{kb}/review/{id}/comments/{cid}",
            patch(routes::comments::edit_comment).delete(routes::comments::delete_comment),
        )
        .route(
            "/kb/{kb}/review/{id}/comments/{cid}/replies",
            post(routes::comments::add_reply),
        )
        .route(
            "/kb/{kb}/review/{id}/comments/{cid}/replies/{rid}",
            patch(routes::comments::edit_reply).delete(routes::comments::delete_reply),
        )
        .route(
            "/kb/{kb}/review/{id}/comments/{cid}/anchor",
            patch(routes::comments::set_anchor),
        )
        .route(
            "/kb/{kb}/review/{id}/comments/{cid}/resolve",
            post(routes::comments::resolve),
        )
        .route(
            "/kb/{kb}/review/{id}/comments/{cid}/unresolve",
            post(routes::comments::unresolve),
        )
        .route(
            "/kb/{kb}/review/{id}/resolve-all",
            post(routes::comments::resolve_all),
        )
        .route(
            "/kb/{kb}/review/{id}/unresolve-all",
            post(routes::comments::unresolve_all),
        )
        // W2.15a — three-state review verdict (comment/approve/request-
        // changes), distinct from any individual comment's status. POST sets
        // (creates the review file on absence, mirroring `add_comment`);
        // DELETE clears it (404s on absence, mirroring `delete_comment`).
        .route(
            "/kb/{kb}/review/{id}/verdict",
            post(routes::comments::set_verdict).delete(routes::comments::clear_verdict),
        )
        .route(
            "/kb/{kb}/review/{id}/apply",
            post(routes::comments::apply_batch),
        )
        .route(
            "/kb/{kb}/review/{id}/import",
            post(routes::comments::import),
        )
        .layer(from_fn_with_state(review_limiter, rate_limit));
    // M7: rate-limited history POSTs. Sub-router so the 600/min/token
    // cap stacks only on these endpoints (the GET `/kb/{kb}/history`
    // lives above and shares the api tree's auth, no rate-limit).
    // RP-track: the reading beacon rides the same limiter (comparable
    // volume to scroll — throttled one-in-flight on the parent).
    let history_routes = Router::new()
        .route("/kb/{kb}/history/open", post(routes::history::post_open))
        .route(
            "/kb/{kb}/history/scroll",
            post(routes::history::post_scroll),
        )
        .route(
            "/kb/{kb}/history/search",
            post(routes::history::post_search),
        )
        .route(
            "/kb/{kb}/history/reading",
            post(routes::history::post_reading),
        )
        .layer(from_fn_with_state(history_limiter, rate_limit));

    let api = api
        .merge(search_routes)
        .merge(atlas_routes)
        .merge(review_routes)
        .merge(attachment_routes)
        .merge(capture_routes)
        .merge(history_routes)
        // v0.4 A2 — bearer-token auth on the whole /api/* tree.
        // Loopback bypass keeps local CLI/TUI/SPA flows working without
        // a token; non-loopback requests need `Authorization: Bearer <t>`
        // when the daemon's token file is populated.
        .layer(from_fn_with_state(auth_state, auth_bearer))
        // N7 — count every request reaching /api/*. Cheap atomic add;
        // the ticker in lib.rs converts to a per-second rate and emits
        // a `metrics.tick` SSE event.
        .layer(from_fn_with_state(state.metrics.clone(), count_requests))
        // SW3: outermost — loopback-only CORS on reads, so a browser page
        // served by ONE local daemon can read GETs (incl. the
        // `/api/events` stream) from OTHER local daemons: the
        // multi-daemon fleet. GET-only (writes stay same-origin +
        // origin_allowlist-gated); no credentials; predicate keeps
        // non-loopback websites blind to the API. Outermost so the
        // headers also land on auth/limiter error responses — a
        // cross-origin fetch must be able to READ its 401/429.
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::predicate(|origin, _| {
                    origin.to_str().map(is_loopback_web_origin).unwrap_or(false)
                }))
                .allow_methods([Method::GET]),
        );

    // U2 (v0.25 quick capture) TRAP — the Web Share Target action posts to
    // bare `/capture`, OUTSIDE the `/api` nest (that's the manifest's
    // `action` field — a browser share sheet can't be told to hit
    // `/api/capture`), so it does NOT inherit the api tree's auth_bearer
    // layer above. `route_layer` only wraps routes registered BEFORE the
    // call (axum's contract — see `docs/routing/route_layer.md`), so this
    // route + its layers must be built as their own tiny router and merged
    // in below BEFORE `/healthz`/`.fallback` are added, or root invariant
    // #4's fail-closed guarantee breaks for this one route on a public
    // bind. A plain top-level `.layer()` here would also re-wrap the
    // already-nested /api tree (double body-limit), which is why this is a
    // `.merge()`, not a `.layer()` chained onto the router below.
    let capture_share = Router::new()
        .route("/capture", post(routes::capture::share_target))
        .layer(DefaultBodyLimit::max(capture_body_limit))
        .route_layer(from_fn_with_state(state.auth.clone(), auth_bearer));

    Router::new()
        .nest("/api", api)
        .merge(capture_share)
        // P0 ops — unauthenticated liveness probe. Mounted on the TOP-LEVEL
        // Router (NOT inside the /api nest), so it bypasses auth_bearer + the
        // rate limiters + count_requests + the loopback-CORS layer (all
        // /api-scoped). An explicit route wins over the Host-dispatching
        // fallback below, so it answers on EVERY origin (parent + artifact
        // subdomains). origin_allowlist (below) only gates state-changing
        // methods, so this GET passes untouched. (Mirror any change in the
        // `/healthz` skip in tests/api_docs.rs.)
        .route("/healthz", get(routes::health::get))
        // Anything not matched by /api/* lands at the dispatcher, which
        // forwards to artifact-subdomain serving OR the SPA shell based
        // on the Host header. Lets one daemon serve both /api/* +
        // <id>.artifacts.localhost + parent-origin SPA on one port.
        .fallback(routes::dispatch::fallback)
        .layer(from_fn_with_state(origin_state, origin_allowlist))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
