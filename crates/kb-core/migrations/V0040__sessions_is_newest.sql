-- PF-R1 — materialize invariant #11's "newest capture per session_id" flag
-- instead of re-deriving it on every read.
--
-- `newest_capture_pred` (crates/kb-core/src/storage/sqlite.rs) has been the
-- ONE sanctioned answer to "collapse to the newest capture" since invariant
-- #11 shipped: a correlated `(SELECT artifact_id FROM sessions s2 WHERE
-- s2.session_id = <expr> ORDER BY started_at DESC, artifact_id ASC LIMIT 1)`
-- repeated at ~20 call sites — every sessions list/aggregate/child-table
-- read. Correct, but each call re-sorts every capture of the session (up to
-- 20+ on a busy day) at read time; it's the top storage-layer hotspot on the
-- sessions corpus.
--
-- `is_newest` is the SAME predicate, precomputed at WRITE time: exactly one
-- row per `session_id` carries `is_newest = 1` — the row `ORDER BY
-- started_at DESC, artifact_id ASC LIMIT 1` would pick, the IDENTICAL
-- tie-break `newest_capture_pred` already used (started_at DESC first, so a
-- clock-identical race between two captures still resolves deterministically
-- on artifact_id). `newest_capture_pred` itself now emits `... AND
-- s2.is_newest = 1` instead of re-sorting the whole group, so its ~15
-- remaining (cross-table) call sites are otherwise unchanged; the handful of
-- call sites that were comparing `sessions` against itself now read the flag
-- directly with no subquery at all.
--
-- MAINTENANCE is transactional, in the SAME tx as the mutation that can
-- change which capture is newest (never a background job, so the flag can
-- never be observed stale by a query issued afterward — invariant #2's
-- single-writer-per-kb guarantee):
--   * `Db::sessions_upsert` — re-derives the row's session_id group after
--     the INSERT/UPDATE (and the row's PRIOR session_id group too, on the
--     rare reindex that repairs a truncated-meta session_id — see
--     invariant #11's canonical-join corollary).
--   * `Db::sessions_delete` and the `sessions` step inside
--     `Db::cascade_delete_doc`'s `CASCADE_STEPS` loop — both re-derive the
--     group after a delete, promoting the next-newest capture.
--   * `Db::cascade_relocate_doc` needs NO change: it only ever UPDATEs the
--     `sessions` row's `artifact_id` (and `source_relative`) column, so
--     `is_newest`/`session_id` ride along untouched — pinned by
--     `cascade_relocate_doc_preserves_is_newest_flag`.
--
-- BACKFILL replicates `newest_capture_pred`'s EXACT correlated-subquery
-- ordering (not a window function, to stay byte-for-byte the same tie-break
-- as every read already used) so a pre-migration corpus lands in identical
-- agreement with what every read would have returned a moment before this
-- migration ran.
ALTER TABLE sessions ADD COLUMN is_newest INTEGER NOT NULL DEFAULT 0;

UPDATE sessions
SET is_newest = 1
WHERE artifact_id = (
    SELECT s2.artifact_id FROM sessions s2
    WHERE s2.session_id = sessions.session_id
    ORDER BY s2.started_at DESC, s2.artifact_id ASC
    LIMIT 1
);

-- The read shape every `newest_capture_pred` call (and every self-referential
-- call site that now reads the column bare) hits: an exact `session_id`
-- lookup constrained to the flagged row. Partial + UNIQUE: besides serving
-- the lookup as an index-only seek, the UNIQUE constraint is a live
-- assertion that maintenance never double-flags a session_id — a bug there
-- fails the very next write with a constraint violation instead of silently
-- reintroducing the double-count invariant #11 exists to prevent.
CREATE UNIQUE INDEX idx_sessions_newest_by_session ON sessions(session_id) WHERE is_newest = 1;
