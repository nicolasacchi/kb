-- perf — indexes for the hottest reverse session lookups that previously
-- fell back to a full table scan. All additive and order-preserving: they
-- only let SQLite seek instead of scanning, never changing a query's
-- result set or ordering.
--
-- session_commits_by_sha_prefix runs
--   WHERE (sha LIKE ?1 OR sha_full LIKE ?1) AND <newest-capture-pred>.
-- Without dedicated indexes SQLite scans every commit row.
CREATE INDEX idx_session_commits_sha ON session_commits(sha);
CREATE INDEX idx_session_commits_sha_full ON session_commits(sha_full);

-- session_research_by_job runs
--   WHERE kind = 'grok_job' AND query = ?1 AND <newest-capture-pred>.
-- A composite (kind, query) index serves the equality pair directly.
CREATE INDEX idx_session_research_kind_query ON session_research(kind, query);
