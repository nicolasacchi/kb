-- v0.0.1 baseline: per-kb sqlite tracking sources, errors, runs, edges.
-- Lance is the source of truth for indexed Doc rows; sqlite is the
-- side-channel for daemon state that doesn't fit columnar storage.

CREATE TABLE sources (
    slug       TEXT PRIMARY KEY,           -- SourceSlug::from_path
    path       TEXT NOT NULL UNIQUE,
    added_at   INTEGER NOT NULL,           -- unix epoch seconds
    paused     INTEGER NOT NULL DEFAULT 0  -- 0 = active, 1 = paused
);

CREATE TABLE index_runs (
    id           TEXT PRIMARY KEY,           -- r-<6base32>
    source_slug  TEXT NOT NULL REFERENCES sources(slug),
    started_at   INTEGER NOT NULL,
    finished_at  INTEGER,                    -- NULL while in flight
    ok_count     INTEGER NOT NULL DEFAULT 0,
    err_count    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_runs_source ON index_runs(source_slug);
CREATE INDEX idx_runs_started ON index_runs(started_at);

CREATE TABLE errors (
    id            TEXT PRIMARY KEY,           -- e-<6base32>
    kind          TEXT NOT NULL,              -- parse | io | sqlite | lance | embed
    source_slug   TEXT NOT NULL,
    path          TEXT NOT NULL,
    message       TEXT NOT NULL,
    content_hash  TEXT,                       -- when content changes, error clears
    retry_count   INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    dismissed     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_errors_source ON errors(source_slug);
CREATE INDEX idx_errors_path ON errors(path);
CREATE INDEX idx_errors_open ON errors(dismissed) WHERE dismissed = 0;

CREATE TABLE edges (
    src_artifact  TEXT NOT NULL,
    dst_artifact  TEXT NOT NULL,
    kind          TEXT NOT NULL,              -- link | embed
    PRIMARY KEY (src_artifact, dst_artifact, kind)
);
CREATE INDEX idx_edges_dst ON edges(dst_artifact);
