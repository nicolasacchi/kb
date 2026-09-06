-- v0.33 X2 — stable first-indexed timestamp per artifact.
--
-- fs btime (`created_unix`) is unreliable across copies; `mtime_unix` drifts
-- on edit; `indexed_at_unix` refreshes every reindex. This table records when
-- kb FIRST saw the doc (INSERT OR IGNORE), so a "created" sort has a durable
-- anchor that survives reindex + file copies. SQLite-only (lance non-nullable
-- adds are impossible on existing datasets; a nullable lance column would be
-- clobbered by merge_insert on reindex).

CREATE TABLE doc_first_seen (
    artifact_id        TEXT PRIMARY KEY,
    first_indexed_unix INTEGER NOT NULL
);
