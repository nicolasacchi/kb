//! `GET /api/events.schema.json` — enum of v0.0.1 event types.
//! `GET /api/events/schema/{type}/v1.json` — per-type JSON schema.
//!
//! v0.0.1 keeps the schemas minimal — name + payload fields list. v0.1+
//! may produce full JSON Schema documents.

use axum::{
    body::Body,
    extract::Path,
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use serde_json::json;

/// Update protocol: every `EventBus::emit`/`bus.emit(...)` call site in
/// this crate or `kb-core` must have its first-argument string literal
/// listed here (and a matching arm in `per_type` below) in the SAME
/// commit that adds the call site — `cargo test -p kb-server
/// routes::schema::drift` (below) regex-scans both crates' `src/` trees
/// and fails on any emitted-but-undeclared kind.
const V0_0_1_TYPES: &[&str] = &[
    "index.start",
    "index.file",
    "index.complete",
    "artifact.indexed",
    "artifact.removed",
    "watch.create",
    "watch.modify",
    "watch.delete",
    "query",
    "error",
    "connection",
    "lag",
    "gap",
    // v0.1 additions (this plan §"SSE event taxonomy")
    "index.embedding",
    // Oversized-doc hardening (the embed-cap hotfix, ab6a05e9/8df0e707):
    // the two REFUSAL events the indexer emits instead of embedding
    // something it cannot safely handle — a doc already quarantined at
    // this content_hash after repeated retries, and a doc whose chunk
    // count hit `MAX_CHUNKS_PER_DOC`. Both are per-DOC (not per-run) and
    // both exist so a deliberate skip is visible rather than silent.
    "index.embed_skipped",
    "index.chunks_capped",
    "source.paused",
    "source.resumed",
    "error.dismissed",
    "error.fixed",
    "atlas.recompute.start",
    "atlas.recompute.complete",
    // v0.2 — comments + annotator. (`comment.created`/`resolved`/`exported`
    // were listed pre-D1 but never emitted by any code path — kb-cli's
    // push formatter mentioned them for parity with the proposed schema
    // but the server never fired one. Removed deep-review D1 — readers
    // can rely on `comments.updated` to react to any review edit.)
    "comment.anchor_stale",
    "comments.updated",
    // v0.5 P4 — anchor lifecycle counterpart
    "comment.anchor_resolved",
    // v0.6+ H2 — visit/scroll/search recorder. INSERT-only emit gate
    // documented in CLAUDE.md invariant 11; bumps/scroll updates do NOT
    // fire this event so SPA subscribers don't thrash on every reload
    // or scroll. Pre-D1 the type was emitted but missing from the
    // schema enum — clients couldn't introspect it via `/api/schema`.
    "history.recorded",
    // v0.7+ W1+ — periodic reconciler pass summary. Emitted at the end
    // of each scheduled walk; pairs with the `tracing::info` log so the
    // SPA / TUI activity feed shows reconcile activity inline with
    // live watcher events.
    "reconcile.complete",
    // R1 — emitted when the watcher detects an inotify queue overflow
    // (or platform-equivalent) error. The daemon also spawns an
    // immediate forced rescan so the dataset self-recovers; this event
    // is the subscriber-visible signal that the self-recovery fired.
    "watcher.lagged",
    // N7+N8 — 1Hz tick emitted by the daemon metrics_ticker; carries
    // request-rate + storage-channel depth snapshots for the TUI's
    // TRAFFIC tab.
    "metrics.tick",
    // v0.9 M6 — agent-memory lifecycle. `ingested` when a memory lands;
    // `stale` when a memory carrying kb-supersedes=X lands (X is now
    // shadowed and drops from recall); `forgotten` when `kb forget`
    // deletes one. `resolved` is reserved (no MVP trigger). The SPA
    // /memory view subscribes to these to refresh without a reload.
    "memory.ingested",
    "memory.stale",
    "memory.resolved",
    "memory.forgotten",
    // v0.13 D7 — memory promotion. Emitted by POST
    // /api/kb/{src}/memories/{id}/promote when a memory artifact is
    // copied into a non-memory kb (kb-* memory metas stripped).
    "memory.promoted",
    // v0.5 Q5 — recluster fast path (k-means only on existing coords);
    // emitted by routes/atlas.rs but missing from this enum until S5.
    // Without listing them the schema endpoint advertised
    // `atlas.recompute.*` but not the recluster variants, even though
    // the SPA atlas view + the new dashboard subscribe to both.
    "atlas.recluster.start",
    "atlas.recluster.complete",
    // S5 admin actions — dashboard's destructive operations. `kb.dropped`
    // fires after `DELETE /api/kb/{kb}` wipes lance + sqlite for a kb;
    // `history.purged` after `POST /api/kb/{kb}/history/purge`. Both
    // carry `{kb}` in the payload.
    "kb.dropped",
    "history.purged",
    // v0.10 K2 — anchor corkboard. `anchor.added` / `anchor.removed`
    // fire on pin/unpin; the SPA uses them for cross-tab sync of the
    // Header anchor pill + the /anchors view list. Payload carries
    // `{kb, artifact_id}`.
    "anchor.added",
    "anchor.removed",
    // v0.14 S3 — sessions feature. `session.captured` fires after the
    // indexer upserts a V0008 enrichment row for a memory-session
    // artifact (Stop-hook transcript). `session.deleted` fires when
    // the source file is unlinked. SPA /sessions view subscribes to
    // both for live updates without a refetch.
    "session.captured",
    "session.deleted",
    // Lance dataset maintenance — `compact_all` lifecycle. Emitted by
    // the startup auto-compact heuristic and by the explicit
    // `POST /api/kb/{kb}/compact` route. Payload carries `{kb, trigger,
    // ms, stats}` on `done`; `started` is fire-and-forget so the TUI
    // can render an "optimizing…" badge until the matching `done` lands.
    "maintenance.compact.started",
    "maintenance.compact.done",
    "maintenance.compact.failed",
    // RL-track (v0.18) — reading lists. List header lifecycle +
    // per-entry lifecycle from the routes; `anchor_stale` /
    // `anchor_resolved` from the indexer's ListAnchorHook when a reindex
    // changes an entry anchor's resolution. Every payload carries `kb` +
    // `list_id` (header events use `id`) so the SPA bridge can target
    // its `["list", kb, id]` invalidation. A bulk import emits ONE
    // `list.updated`, not per-entry events.
    "list.created",
    "list.updated",
    "list.deleted",
    "list.entry.added",
    "list.entry.updated",
    "list.entry.removed",
    "list.entry.anchor_stale",
    "list.entry.anchor_resolved",
    // v0.24 X3 — per-file exclusion lifecycle. `excluded` fires when the
    // operator excludes a path (the KeepUserData cascade also emits
    // `artifact.removed` when an index row existed); `included` on
    // re-include (the forced reindex follows with `artifact.indexed`).
    // Emitted only on an actual state change — an idempotent re-POST of
    // an already-excluded path fires nothing.
    "artifact.excluded",
    "artifact.included",
    // W3 T-b — corpus time-lapse (V0028). Fired from `routes/atlas.rs`'s
    // `spawn_recompute`/`spawn_recluster`, AFTER the matching
    // `atlas.{recompute,recluster}.complete`, only when that call actually
    // appended a new `atlas_snapshots` row (kb-core's `record_atlas_snapshot`
    // dedups bit-identical geometry and writes nothing — no event either).
    "atlas.snapshot.recorded",
    // W3 T-d — the RECONSTRUCTED backfill (`POST /atlas/history/backfill`).
    // `start` carries the planned frame count; `complete` the written/skipped
    // split (or an `error`). Each frame the run actually writes ALSO emits
    // the existing `atlas.snapshot.recorded` above, so the SPA's time-lapse
    // invalidation needs no new bridge entry. Both payloads carry
    // `provenance: "reconstructed"` — these frames are today's embeddings
    // laid over a past doc subset, never recorded history.
    "atlas.backfill.start",
    "atlas.backfill.complete",
    // W3 F-b — the dual-field atlas's operator overlay. Fired by
    // `routes/atlas_field.rs::put_field` on every whole-doc PUT of the
    // per-kb operator-placed JSON Canvas sidecar (mirrors `board.updated`'s
    // shape: the whole doc is replaced on every write, so there is no
    // per-field diff to report).
    "atlas.field.updated",
    // 2026-08-16 drift repair — a `schema::drift` audit of every
    // `EventBus::emit`/`IngestSink` call site in kb-core + kb-server found
    // these already shipping in production but never added here (the same
    // D1-shaped gap the top-of-file comments call out for pre-D1 events).
    // Grouped by the route/hook that fires them, not by date, to match this
    // list's existing convention.
    //
    // routes/notes.rs — the Markdown-note CRUD lifecycle (invariant #16:
    // notes are artifacts, not a new entity, so the SPA still needs
    // note-scoped SSE to refresh its dedicated /notes view without a
    // gallery-wide `artifact.*` refetch).
    "note.created",
    "note.updated",
    "note.deleted",
    // routes/memory_links.rs — per-memory cross-kb link set (PUT replaces
    // the whole set, POST/DELETE add or remove one target kb).
    "memory.linked",
    "memory.unlinked",
    // routes/boards.rs — the JSON-Canvas board sidecar; whole-doc replace
    // on every write (see the `atlas.field.updated` comment above, which
    // mirrors this shape).
    "board.updated",
    // routes/proposals.rs — the agent-authored memory proposal queue.
    "proposal.created",
    "proposal.resolved",
    // routes/quarantine.rs — restoring a repeatedly-failing path out of the
    // error quarantine (single restore + the bulk `restore_all` sweep both
    // fire one event per path).
    "quarantine.restored",
    // routes/config.rs — fired right before the daemon flips
    // `restart_requested` + trips shutdown, so subscribers see the
    // announcement before the SSE stream itself closes.
    "daemon.restarting",
    // lib.rs maintenance tickers — siblings of `maintenance.compact.*`
    // above. `retention.pruned` is per-kb (old history/reading rows);
    // `logs.pruned` is fleet-wide (the daemon's own ndjson log dir has no
    // kb scope).
    "maintenance.retention.pruned",
    "maintenance.logs.pruned",
    // enrich.rs MI-W1.1 — the `memory_recalls` ledger hook (CLAUDE.md
    // invariant #10). One event per session CAPTURE with the recall count,
    // never one per row, so a busy session recalling dozens of memories
    // across its turns doesn't event-storm subscribers.
    "session.memory_recalls",
    // enrich.rs CT-F1 — the `memory_commits` exact-id ledger hook, the
    // write sibling of `session.memory_recalls` above and subject to the
    // same one-event-per-CAPTURE rule. Fired ONLY when a capture actually
    // derived rows, which (the trailer being opt-in per repo, default off)
    // makes it rare by construction.
    "session.memory_commits",
    // LSC-2 (`docs/research/kb-live-sessions-cockpit-2026-08.html` §6
    // "Events") — `POST /api/sessions/beat` fires this ONLY when a
    // session's DERIVED state actually changes (never once per beat, or a
    // busy fleet would flood every browser, invariant #24). Daemon-wide
    // (no `{kb}` scope — see `routes::sessions::beat`).
    "session.state",
    // SL2 (`docs/research/kb-slate-design-2026-09.html` §9 "SSE") — the
    // per-project blackboard. `slate.updated` fires ONCE PER APPEND, never
    // per read (#24); `kind` is the post's own kind, or the lifecycle verb
    // (`close`/`reopen`/`rotate`). `hide` names the seq this post removed
    // from the shown set (a drop or a supersede) or is null. Daemon-wide:
    // the payload carries `slug`, never `kb`.
    "slate.updated",
    // The purge (`DELETE /api/slates/{slug}?purge=true`, loopback-only) —
    // the ONE deletion path a slate has.
    "slate.deleted",
];

#[derive(Debug, Serialize)]
pub struct SchemaEnum {
    pub envelope: serde_json::Value,
    pub types: Vec<&'static str>,
}

pub async fn enum_get() -> Json<SchemaEnum> {
    Json(SchemaEnum {
        envelope: json!({
            "fields": ["v", "id", "type", "ts", "payload"],
            "v": 1,
        }),
        types: V0_0_1_TYPES.to_vec(),
    })
}

pub async fn per_type(Path((kind, version)): Path<(String, String)>) -> Response<Body> {
    if version != "v1.json" && version != "v1" {
        return (StatusCode::NOT_FOUND, "only v1 schemas in v0.0.1").into_response();
    }
    if !V0_0_1_TYPES.contains(&kind.as_str()) {
        return (StatusCode::NOT_FOUND, format!("unknown event type: {kind}")).into_response();
    }
    let schema = match kind.as_str() {
        "index.start" => json!({"type": kind, "payload": ["run", "kb", "src", "total"]}),
        "index.file" => {
            json!({"type": kind, "payload": ["run", "kb", "path", "ms", "edges", "ok"]})
        }
        "index.complete" => {
            json!({"type": kind, "payload": ["run", "kb", "ok_count", "err_count", "duration_ms"]})
        }
        "artifact.indexed" => {
            json!({"type": kind, "payload": ["artifact_id", "kb", "path", "hash", "mtime", "change_kind"]})
        }
        "artifact.removed" => {
            json!({"type": kind, "payload": ["artifact_id", "kb", "path"]})
        }
        // v0.24 X3 — `path` is source-relative (the exclusion store's key);
        // `artifact_id` the deterministic id that path derives to.
        "artifact.excluded" | "artifact.included" => {
            json!({"type": kind, "payload": ["kb", "path", "artifact_id"]})
        }
        "watch.create" | "watch.modify" | "watch.delete" => {
            json!({"type": kind, "payload": ["kb", "path"]})
        }
        "query" => json!({"type": kind, "payload": ["kb", "q", "hits", "ms"]}),
        "error" => json!({"type": kind, "payload": ["id", "kind", "kb", "path", "msg"]}),
        "connection" => json!({"type": kind, "payload": ["state"]}),
        "lag" => json!({"type": kind, "payload": ["skipped"]}),
        "gap" => json!({"type": kind, "payload": ["requested_id", "oldest_available_id"]}),
        "index.embedding" => {
            json!({"type": kind, "payload": ["run", "kb", "path", "model", "bytes"]})
        }
        // Embed-cap refusals — payloads mirror the emit sites in
        // `kb_core::indexer` exactly; both name the doc by `path`
        // (per-doc events, unlike `index.complete`'s per-run shape).
        "index.embed_skipped" => {
            json!({"type": kind, "payload": ["run", "kb", "path", "retries"]})
        }
        "index.chunks_capped" => {
            json!({"type": kind, "payload": ["run", "kb", "path", "total", "kept"]})
        }
        // N7+N8+P3+P4: 1Hz tick from the daemon's metrics_ticker task.
        "metrics.tick" => {
            json!({"type": kind, "payload": [
                "requests_total", "requests_last_sec",
                "storage_channel_depth", "storage_channel_capacity",
                "routes",  // [{kind, count, p50_ms, p95_ms}]
                "embedder_degraded", "embedder_respawn_count"  // v0.16 Q-track
            ]})
        }
        "source.paused" | "source.resumed" => {
            json!({"type": kind, "payload": ["kb", "src"]})
        }
        "error.dismissed" => json!({"type": kind, "payload": ["kb", "id"]}),
        "error.fixed" => json!({"type": kind, "payload": ["kb", "id", "run"]}),
        "atlas.recompute.start" => {
            json!({"type": kind, "payload": ["run", "kb", "artifact_count"]})
        }
        "atlas.recompute.complete" => {
            json!({"type": kind, "payload": ["run", "kb", "duration_ms"]})
        }
        "comment.anchor_stale" => {
            json!({"type": kind, "payload": ["id", "kb", "artifact_id", "anchor_kind", "fuzzy_score"]})
        }
        "comment.anchor_resolved" => {
            json!({"type": kind, "payload": ["kb", "artifact_id", "comment_id", "score"]})
        }
        // W2.15a — `verdict` added: the current `ReviewFile.verdict` (or
        // `null`), piggybacked onto the same event (see `emit_updated` in
        // routes/comments.rs) rather than a new SSE type.
        "comments.updated" => {
            json!({"type": kind, "payload": ["kb", "artifact_id", "open_count", "total_count", "verdict"]})
        }
        "history.recorded" => {
            json!({"type": kind, "payload": ["kb", "kind", "artifact_id", "query"]})
        }
        "reconcile.complete" => {
            json!({"type": kind, "payload": ["kb", "files", "deletes", "duration_ms"]})
        }
        "watcher.lagged" => {
            json!({"type": kind, "payload": ["kb", "reason", "ts"]})
        }
        "memory.ingested" => json!({"type": kind, "payload": ["kb", "id", "path"]}),
        "memory.stale" => {
            json!({"type": kind, "payload": ["kb", "id", "superseded_by"]})
        }
        "memory.resolved" => json!({"type": kind, "payload": ["kb", "id"]}),
        "memory.forgotten" => json!({"type": kind, "payload": ["kb", "id"]}),
        "memory.promoted" => {
            json!({"type": kind, "payload": ["src_kb", "dest_kb", "src_id", "dest_id", "dest_path"]})
        }
        "anchor.added" | "anchor.removed" => {
            json!({"type": kind, "payload": ["kb", "artifact_id"]})
        }
        "session.captured" => {
            json!({"type": kind, "payload": [
                "kb", "artifact_id", "session_id", "started_at", "message_count"
            ]})
        }
        "session.deleted" => {
            json!({"type": kind, "payload": ["kb", "artifact_id"]})
        }
        "kb.dropped" => json!({"type": kind, "payload": ["kb"]}),
        "history.purged" => json!({"type": kind, "payload": ["kb"]}),
        // These four were in the enum but missing from this match since
        // S5 — the contains-check passed and the handler hit the
        // `unreachable!` (a latent panic on schema introspection).
        "atlas.recluster.start" => {
            json!({"type": kind, "payload": ["run", "kb", "artifact_count"]})
        }
        "atlas.recluster.complete" => {
            json!({"type": kind, "payload": ["run", "kb", "duration_ms"]})
        }
        "maintenance.compact.started" => json!({"type": kind, "payload": ["kb", "trigger"]}),
        "maintenance.compact.done" | "maintenance.compact.failed" => {
            json!({"type": kind, "payload": ["kb", "trigger", "ms", "stats"]})
        }
        "list.created" | "list.updated" => {
            json!({"type": kind, "payload": ["kb", "id", "title"]})
        }
        "list.deleted" => json!({"type": kind, "payload": ["kb", "id"]}),
        "list.entry.added" | "list.entry.updated" | "list.entry.removed" => {
            json!({"type": kind, "payload": ["kb", "list_id", "entry_id", "artifact_id"]})
        }
        "list.entry.anchor_stale" => {
            json!({"type": kind, "payload": [
                "kb", "list_id", "entry_id", "artifact_id", "anchor_kind", "source_relative"
            ]})
        }
        "list.entry.anchor_resolved" => {
            json!({"type": kind, "payload": ["kb", "list_id", "entry_id", "artifact_id"]})
        }
        "atlas.snapshot.recorded" => {
            json!({"type": kind, "payload": ["kb", "id", "points"]})
        }
        "atlas.backfill.start" => {
            json!({"type": kind, "payload": ["run", "kb", "frames", "provenance"]})
        }
        "atlas.backfill.complete" => {
            json!({"type": kind, "payload": [
                "run", "kb", "written", "skipped", "duration_ms", "provenance", "error?"
            ]})
        }
        "atlas.field.updated" => json!({"type": kind, "payload": ["kb"]}),
        // 2026-08-16 drift repair — see the matching enum comment block.
        "note.created" => {
            json!({"type": kind, "payload": ["kb", "id", "path", "folder", "is_notepad"]})
        }
        "note.updated" => json!({"type": kind, "payload": ["kb", "id", "folder"]}),
        "note.deleted" => json!({"type": kind, "payload": ["kb", "id"]}),
        // `memory.linked` payload varies by route: PUT (replace) carries
        // `global`+`linked_kbs` (the whole new set), POST (add-one) carries
        // `added`. Both share `kb`+`id`; listed as the union, matching how
        // union-shaped events elsewhere in this match (e.g.
        // `maintenance.compact.done`) already document mixed call sites.
        "memory.linked" => {
            json!({"type": kind, "payload": ["kb", "id", "global?", "linked_kbs?", "added?"]})
        }
        "memory.unlinked" => json!({"type": kind, "payload": ["kb", "id", "removed"]}),
        "board.updated" => json!({"type": kind, "payload": ["kb", "list_id"]}),
        "proposal.created" => json!({"type": kind, "payload": ["kb", "id", "title"]}),
        // `outcome` is "approved" (also carries `artifact_id`) or "rejected".
        "proposal.resolved" => {
            json!({"type": kind, "payload": ["kb", "id", "outcome", "artifact_id?"]})
        }
        "quarantine.restored" => json!({"type": kind, "payload": ["kb", "path"]}),
        "daemon.restarting" => json!({"type": kind, "payload": ["addr", "config_path"]}),
        "maintenance.retention.pruned" => json!({"type": kind, "payload": ["kb", "deleted"]}),
        "maintenance.logs.pruned" => json!({"type": kind, "payload": ["deleted"]}),
        // Identical payload shape to its recall sibling above — same
        // capture identity, same "count of rows this capture derived".
        "session.memory_recalls" | "session.memory_commits" => {
            json!({"type": kind, "payload": ["kb", "artifact_id", "session_id", "count"]})
        }
        "slate.updated" => {
            json!({"type": kind, "payload": [
                "slug", "seq", "kind", "id", "topic?", "re?", "hide?", "pin?"
            ]})
        }
        "slate.deleted" => json!({"type": kind, "payload": ["slug"]}),
        "session.state" => {
            json!({"type": kind, "payload": [
                "session_id", "harness", "state", "holder", "project", "since_unix"
            ]})
        }
        _ => unreachable!("contains-check above"),
    };
    Json(schema).into_response()
}

#[cfg(test)]
mod drift {
    //! Drift guard (2026-08-16): regex-scans BOTH this crate's own `src/`
    //! (`CARGO_MANIFEST_DIR`) and the sibling `kb-core/src`
    //! (`CARGO_MANIFEST_DIR/../kb-core/src` — `events.rs` owns the
    //! `EventBus`, but the real producers live all over `indexer.rs`/
    //! `enrich.rs`/`watcher.rs`) for every `EventBus::emit` call site, and
    //! asserts every literal it finds is declared in `V0_0_1_TYPES`.
    //! Anchored on the precise call shape (a `.emit(` method call whose
    //! FIRST argument is a string literal, single-line or split across
    //! lines — never a bare dotted string anywhere in a file) to avoid
    //! false positives from payload JSON keys (`"kb"`, `"run"`, …) or
    //! prose.
    //!
    //! Two literals are KNOWN, audited non-kinds and excluded explicitly
    //! rather than silently tolerated: `kb-core/src/events.rs`'s own unit
    //! tests call `bus.emit("tick", …)` ~17 times as an arbitrary
    //! placeholder payload (the tests exercise ring/broadcast mechanics,
    //! not any real kind); `lib.rs`'s `webhook_bridge_only_forwards_
    //! allowlisted_types` test calls `bus.emit("comment.added", …)` as a
    //! plausible-looking but FICTITIOUS example event (the top-of-file
    //! comment on `V0_0_1_TYPES` already documents that `comment.created`/
    //! `resolved`/`exported` were proposed pre-D1 and never actually wired
    //! to any emit site — this test literal is the same category of
    //! never-shipped example, not a missed production kind). A real
    //! producer using either string would be an actual coincidence-shaped
    //! bug this exception list can't hide (the drift test would need a
    //! THIRD, unexplainable literal to slip past it).
    //!
    //! **Update protocol**: when you add a new `bus.emit("some.kind", …)`
    //! call site anywhere in kb-server or kb-core, add `"some.kind"` to
    //! `V0_0_1_TYPES` above (and a matching arm in `per_type`) in the SAME
    //! commit — `cargo test -p kb-server routes::schema::drift` catches an
    //! omission.

    use super::V0_0_1_TYPES;
    use std::path::{Path, PathBuf};

    /// Literals that ARE production-shaped (would otherwise fail
    /// `looks_like_kind`'s conservative filter) but are audited test-only
    /// placeholders, not real emitted kinds — see the module doc above.
    const KNOWN_NON_KINDS: &[&str] = &["tick", "comment.added"];

    /// Every `.emit("literal.kind"` occurrence in `src`, allowing the
    /// call's opening paren and the string literal to be separated by
    /// whitespace/newlines (the common multi-line `bus.emit(\n    "kind",`
    /// shape `cargo fmt` produces once a call also has a payload argument).
    fn emit_literals_in(src: &str) -> Vec<String> {
        let bytes = src.as_bytes();
        let mut out = Vec::new();
        let needle = b".emit(";
        let mut i = 0;
        while let Some(pos) = find(bytes, needle, i) {
            let mut j = pos + needle.len();
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                let start = j + 1;
                if let Some(end_rel) = src[start..].find('"') {
                    out.push(src[start..start + end_rel].to_string());
                }
            }
            i = pos + needle.len();
        }
        out
    }

    fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
        if from >= haystack.len() || needle.is_empty() {
            return None;
        }
        haystack[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + from)
    }

    /// A literal is a plausible event kind (not a payload-key/prose false
    /// positive) if it is lowercase, dotted-or-bare, and free of spaces —
    /// every real kind in this crate is `word(.word)+` or a bare `word`.
    /// This is a filter on top of the precise `.emit(` anchor above, not a
    /// replacement for it.
    fn looks_like_kind(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
    }

    fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn every_emitted_kind_is_declared_in_v0_0_1_types() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let kb_server_src = manifest_dir.join("src");
        let kb_core_src = manifest_dir.join("../kb-core/src");
        assert!(
            kb_core_src.is_dir(),
            "expected a sibling kb-core crate at {}",
            kb_core_src.display()
        );

        let mut files = Vec::new();
        collect_rs_files(&kb_server_src, &mut files);
        collect_rs_files(&kb_core_src, &mut files);
        assert!(
            files.len() > 100,
            "sanity: expected to walk both crates' whole src/ trees, got {} files",
            files.len()
        );
        // This file's own module/fn doc comments (above) deliberately show
        // example `.emit("kind", …)` call shapes — excluded so the scan
        // doesn't flag its own documentation prose as an undeclared kind.
        // It has no real `EventBus::emit` call sites of its own (only
        // `per_type`'s `json!({"payload": [...]})` arms, which never match
        // the `.emit(` anchor).
        let self_path = manifest_dir.join("src/routes/schema.rs");

        let mut missing = Vec::new();
        for file in &files {
            if file == &self_path {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(file) else {
                continue;
            };
            for lit in emit_literals_in(&contents) {
                if !looks_like_kind(&lit) || KNOWN_NON_KINDS.contains(&lit.as_str()) {
                    continue; // not a plausible kind, or an audited placeholder
                }
                if !V0_0_1_TYPES.contains(&lit.as_str()) && !missing.contains(&lit) {
                    missing.push(format!("{lit} ({})", file.display()));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "event kind(s) emitted but missing from schema::V0_0_1_TYPES: {missing:#?}"
        );
    }
}
