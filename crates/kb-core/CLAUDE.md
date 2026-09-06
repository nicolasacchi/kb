# kb-core — internals

Sibling of the workspace-root [`/CLAUDE.md`](../../CLAUDE.md), which **indexes**
the cross-cutting invariants (security, comments, share, history, memory,
sessions) — full text in
[`/docs/architecture-invariants.md`](../../docs/architecture-invariants.md). This
file holds invariants that live entirely inside `kb-core` — storage schema, the
actor model, the atlas algorithm. Break them and the workspace compiles but fails
subtly at runtime.

## Architecture invariants

1. **lance embedding column nullable on the OUTER**: per-row writes
   without an embedding require this. See
   `kb-core/src/storage/schema.rs` (the spike-lance lesson).

2. **Storage actor is single-writer-per-kb**: `mpsc::channel` with
   `CHANNEL_CAPACITY = 1024`. Don't introduce parallel writers; embed
   work serialises through the actor on purpose. See
   `storage/actor.rs`. This guarantee is **per-process only** — there is
   no flock/advisory lock on the sqlite file or the `lance/` dir, so two
   daemon processes (or two machines over a synced/network-mounted state
   dir) sharing one `KB_HOME` will race and corrupt state; see
   `docs/self-host.md`.

   **SC4 read-priority lanes**: inbound messages split into TWO
   `CHANNEL_CAPACITY` mpsc lanes — a **read lane** (searches/gets/lists/
   counts + the search-support `Ensure*Index` trio) and a **write lane**
   (everything else). The actor loop still processes exactly ONE message
   at a time (single-writer holds), but `next_from_lanes` drains the read
   lane with priority so a foreground search never head-of-line-blocks
   behind a bulk-ingest backlog on the same actor. Fairness is bounded:
   after `READ_STARVATION_BOUND` (16) consecutive reads the biased select
   flips to force one write, so a relentless read stream can never starve
   ingest (the counter is per-actor-lifetime and only a served write
   resets it). Classification is `is_read_lane` — **conservative: the
   default is WRITE**, because a write misclassified as read could be
   pulled ahead of earlier-queued writes and break write-FIFO (a read
   misclassified as write only loses the fast lane). It is deliberately
   NOT `StorageKind::from_msg` (that classifier's `_ => Read` fallthrough
   is the wrong default here). **Read-your-writes semantics**: a caller
   that awaits its own write still sees it — the write's `oneshot`
   resolves only after the actor processes it, and any read the caller
   issues afterward is enqueued later. Only an UNRELATED concurrent read
   (issued by another task while a write backlog exists) may jump ahead
   and observe pre-backlog state; that read is never stale for its own
   issuer, and the `Ensure*Index` dirty-flag path self-heals a
   momentarily-behind search index. Shutdown + `CompactAll*` ride the
   write lane to keep FIFO ordering with the writes they follow.

3. **Atlas determinism**: `compute_layout(embeddings, k, seed)` must
   return bit-identical output across machines for the same inputs.
   CI verifies via `deterministic_under_same_seed`. Don't introduce
   `std::collections::HashMap` iteration ordering or system clock
   into the layout path.

4. **Embedding dim is per-kb, captured at `Storage::open` time**: the
   lance schema's `embedding` column width is not a const.
   `Storage::open(path, config_dim)` reads the on-disk
   `FixedSizeList<_, N>` width on re-open (source of truth for existing
   kbs) and uses `config_dim` for fresh kbs. Disk-dim ≠ config-dim →
   `Error::Config` at startup; the per-kb-section
   `embedding_model = "bge-base-en-v1.5"` resolves to 768 via
   `kb_core::embed::model_info`. Don't reintroduce a hardcoded dim
   anywhere — three-way bake-offs spin up `<corpus>-{small,base,large}`
   kbs in one daemon. `docs_to_batches(_, dim)` returns `Result<_>` on
   a wrong-width embedding (used to panic + kill the storage actor
   task) so the bake-off can survive a single bad row.

5. **Post-upsert enrichment is an ordered `EnrichmentHook` registry**
   (`crate::enrich`, RFC phase X2): after the batched `upsert_docs` commit
   (GC-B7: `flush_prepared_batch`), `indexer::finish_indexed_doc` runs the
    hooks from `enrich::default_hooks()` in registration
    order — `session-capture` → `memory-recall-ledger` (MI-W1.1, the V0035
    `memory_recalls` ledger — a sibling of `session-capture`, same
    `interested` gate, running right after it because it needs its own
    Turn/Item IR parse) → `memory-commit-ledger` (CT-F1, the V0038
    `memory_commits` exact-id join — same gate again, parsing the
    `Kb-Memory:` trailers back out of the envelope's already-resolved
    commits block; no transcript IR, no git call) →
    `memory-link-seed` → `edge-record` → `code-refs`
    (DCB W1.A — the other doc-graph writer over `ctx.html`; disjoint tables
    from `edge-record`, so the position is grouping, not coupling) →
    `snapshot-capture` (the V-track index-snapshot writer, gated on
    `versions_mode.uses_index()` + non-`memory-session`) → `list-anchor`
    (kb-list/1 anchor materialization). Each is
    **best-effort** (an `Err` is logged + skipped by the registry loop,
   never failing the index) and **self-handles its own errors** (tracing /
   `record_failure`) returning `Ok(())`, so behaviour + emitted SSE events
   stay byte-identical to the pre-X2 inline blocks. **Order IS the run
   order** — reordering changes the order events fire; the
   `default_hooks_registration_order` test pins it. Hooks write THROUGH the
   `StorageHandle` (never lance/sqlite directly — invariant #2) and are
   otherwise read-only on their `EnrichCtx` (all-borrows; the indexer
   snapshots `kb_category` + the seed inputs *before* `doc` moves into
   `upsert_doc`). Adding a sessions-shaped enricher = one new
   `impl EnrichmentHook` + one `default_hooks()` line; do NOT reintroduce
   an inline `if is_memory_*` branch in the `prepare_doc`/
   `finish_indexed_doc` pipeline. The trait method is a
   boxed future (`BoxFuture`), dodging async-fn-in-trait the same way the
   share host's enum does (see root invariant on `ShareBackend`).

## When to update this file

Add an invariant here when it lives entirely inside `kb-core` and a
contributor could break it without touching any other crate. Anything
that surfaces in HTTP, SPA, or CLI behavior belongs in the root
file's invariants list. If you're unsure, default to root — duplication
across both files is the failure mode to avoid.
