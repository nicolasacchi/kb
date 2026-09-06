-- W2.1 — frecency signal for the files search lane. An append-only EVENT
-- log (one row per `GET /api/file` read), not an aggregated counter: the
-- files lane derives both "most recent open" (the empty-query fallback) and
-- a per-path recency boost (the non-empty-query blend) straight from this
-- log via MAX(opened_at)/GROUP BY, so there is no separate increment path
-- to keep in sync with the raw event stream. Small and disposable — nothing
-- else in the schema references it, and it carries no derived data of its
-- own (see `store.rs`'s `bump_file_open`/`last_opened_map`/
-- `recent_file_opens`).
CREATE TABLE file_opens (
    repo_id    INTEGER NOT NULL REFERENCES repos(id),
    path       TEXT NOT NULL,
    -- Unix milliseconds (matches `chrono::DateTime::timestamp_millis`,
    -- what `routes::file` passes at the call site).
    opened_at  INTEGER NOT NULL
);
CREATE INDEX idx_file_opens_repo_path ON file_opens(repo_id, path);
CREATE INDEX idx_file_opens_opened_at ON file_opens(opened_at);
