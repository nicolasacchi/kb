-- kb-code's own per-daemon SQLite store (ADR-2: derived data is keyed by
-- blob hash + a grammar-version salt, never by path, so unchanged content
-- is never re-parsed and a branch switch re-derives nothing).

CREATE TABLE repos (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE,
    root  TEXT NOT NULL
);

-- The CURRENT working-tree state mirror: one row per (repo, path), pointing
-- at whatever blob_hash is at that path right now. This table is cheap to
-- overwrite/rescan wholesale (`ingest::index_repo_working_tree` upserts
-- every tracked path on every run) — it carries no derived data itself,
-- only a pointer into it.
CREATE TABLE files (
    repo_id    INTEGER NOT NULL REFERENCES repos(id),
    path       TEXT NOT NULL,
    blob_hash  TEXT NOT NULL,
    -- Language id ("rust"/"python"/"ruby", the W1.5 tier) for a parsed
    -- file, or a tier marker for a file `ingest::index_file` deliberately
    -- did not parse: "unknown" (no grammar for this extension), "binary"
    -- (non-UTF8 content), "too-large" (over the 5 MiB parse cap), or "lfs"
    -- (a Git LFS pointer file).
    lang       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    PRIMARY KEY (repo_id, path)
);
CREATE INDEX idx_files_blob_hash ON files(blob_hash);

-- Derived data — ADR-2 core table. Keyed by (blob_hash, salt), NEVER by
-- path: identical content indexed under two different paths (or reachable
-- from two different branches) shares exactly one set of symbol rows, and
-- switching branches back to content already seen re-derives nothing.
-- `ordinal` is the extractor's own deterministic emission order (byte
-- position ascending — see `extract.rs`), not a meaningful id on its own;
-- it exists only to make the primary key unique per (blob_hash, salt).
CREATE TABLE symbols (
    blob_hash    TEXT NOT NULL,
    salt         TEXT NOT NULL,
    ordinal      INTEGER NOT NULL,
    name         TEXT NOT NULL,
    kind         TEXT NOT NULL,
    line_start   INTEGER NOT NULL,
    line_end     INTEGER NOT NULL,
    col_start    INTEGER NOT NULL,
    col_end      INTEGER NOT NULL,
    container    TEXT,
    signature    TEXT,
    PRIMARY KEY (blob_hash, salt, ordinal)
);

-- Derived data — same ADR-2 keying as `symbols`. `spans` is a JSON-encoded
-- `Vec<highlight::Span>` (see `highlight.rs` module doc for the exact
-- encoding + the capture-name-to-class mapping); stored as an opaque BLOB
-- so the encoding can change without a schema migration.
CREATE TABLE highlights (
    blob_hash  TEXT NOT NULL,
    salt       TEXT NOT NULL,
    spans      BLOB NOT NULL,
    PRIMARY KEY (blob_hash, salt)
);
