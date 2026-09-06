-- perf review — targeted indexes for the hottest sqlite lookups. All
-- additive and order-preserving: they only let SQLite seek instead of
-- scanning, never changing a query's result set or ordering.

-- history_record_search runs
--   WHERE kind='search' AND query=?1 ORDER BY started_at DESC LIMIT 1.
-- The existing idx_history_started(started_at DESC) can't serve the
-- kind+query filter, so it falls back to a full table scan. A partial
-- index over (query, started_at DESC) restricted to search rows lets it
-- seek straight to the most-recent matching query.
CREATE INDEX idx_history_search
    ON history(query, started_at DESC)
    WHERE kind = 'search';

-- sessions_get runs
--   WHERE session_id=?1 ORDER BY started_at DESC, artifact_id ASC LIMIT 1.
-- The V0008 idx_sessions_session_id(session_id) serves the filter but
-- leaves SQLite to sort by (started_at, artifact_id) afterwards. A
-- covering index over all three keys makes the lookup an index-only seek
-- with a deterministic tiebreak (and supersedes the single-column one).
DROP INDEX IF EXISTS idx_sessions_session_id;
CREATE INDEX idx_sessions_session_id_started
    ON sessions(session_id, started_at DESC, artifact_id ASC);
