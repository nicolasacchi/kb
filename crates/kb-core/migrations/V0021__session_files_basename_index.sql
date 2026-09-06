-- perf review — index the session_files.basename lookup that backs `kb why`
-- (session_files_for_basename). V0017 created idx_session_files_session,
-- _artifact and _target, but the `WHERE basename = ?1` equality lookup had no
-- index and fell back to a full table scan of a table that grows with every
-- file every session ever touched (thousands of rows after a claude-history
-- backfill). Additive + order-preserving — same pattern as V0012: it only
-- lets SQLite seek instead of scan, never changing the result set or order.
CREATE INDEX idx_session_files_basename ON session_files(basename);
