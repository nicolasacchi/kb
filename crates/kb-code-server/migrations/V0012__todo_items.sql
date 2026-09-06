-- Phase N ("kb-code v3 — Navigate") — TODO index: comment-marker hits
-- (TODO/FIXME/HACK/XXX/BUG) extracted during the per-file ingest pass
-- (`extract::extract_todos`, gated to the eight full-tier languages —
-- outline/text-tier files are skipped). Rows are replaced per file on
-- re-ingest (`store::Store::replace_todo_items`) so they never stale-
-- accumulate, the same "delete then re-insert, no stable per-row identity
-- to diff against" discipline `replace_symbols`/`replace_occurrences`
-- already use for their own derived data (ADR-2's spirit, even though
-- this table is keyed by `file_id` rather than `(blob_hash, salt)` —
-- todos are path-bound places the operator navigates to, not pure
-- content-addressed facts).
--
-- `files` originally used a composite PRIMARY KEY `(repo_id, path)` with
-- no free-standing integer id (V0001). SQLite still assigns each row a
-- `rowid`, but a FOREIGN KEY needs a named column — rebuild `files` with
-- an `id INTEGER PRIMARY KEY` and keep `UNIQUE (repo_id, path)` so every
-- existing `ON CONFLICT(repo_id, path)` upsert keeps working byte-for-
-- byte. No other table referenced `files` by FK before this migration
-- (annotations/file_opens key on path text), so the rebuild is free.

CREATE TABLE files_new (
    id         INTEGER PRIMARY KEY,
    repo_id    INTEGER NOT NULL REFERENCES repos(id),
    path       TEXT NOT NULL,
    blob_hash  TEXT NOT NULL,
    -- Language id ("rust"/"python"/…) for a parsed file, or a tier marker
    -- for a file `ingest::index_file` deliberately did not parse:
    -- "unknown" / "binary" / "too-large" / "lfs" — same semantics as V0001.
    lang       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    UNIQUE (repo_id, path)
);
INSERT INTO files_new (repo_id, path, blob_hash, lang, size)
    SELECT repo_id, path, blob_hash, lang, size FROM files;
DROP TABLE files;
ALTER TABLE files_new RENAME TO files;
CREATE INDEX idx_files_blob_hash ON files(blob_hash);

CREATE TABLE todo_items (
    id       INTEGER PRIMARY KEY,
    file_id  INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    line     INTEGER NOT NULL,
    marker   TEXT NOT NULL,
    text     TEXT NOT NULL
);
-- `store::Store::replace_todo_items`'s by-file_id delete + the list query's
-- join from a path filter.
CREATE INDEX idx_todo_items_file_id ON todo_items(file_id);
-- `GET /api/todos?marker=`'s equality filter.
CREATE INDEX idx_todo_items_marker ON todo_items(marker);
