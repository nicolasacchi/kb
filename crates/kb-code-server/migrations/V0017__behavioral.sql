-- V3.2-B1 — behavioral store (repo-addressed history counters).
--
-- These are HISTORY FACTS, not content-addressed derived data: keyed by
-- (repo_id, path) rather than blob_hash. Counters are rebuilt/windowed by
-- `crate::behavioral` (git log --numstat walk). ATTENTION signals only —
-- never quality verdicts (hotspot ≠ bad code).
--
-- Window semantics: full rebuilds are exact for the configured window;
-- incremental head-moved updates are additive within the current window
-- (no incremental subtract — wrong and unverifiable).

CREATE TABLE behavioral_meta (
    repo_id INTEGER PRIMARY KEY REFERENCES repos(id) ON DELETE CASCADE,
    last_commit_sha TEXT,
    updated_at INTEGER NOT NULL
);

CREATE TABLE path_stats (
    repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    revisions INTEGER NOT NULL DEFAULT 0,
    lines_added INTEGER NOT NULL DEFAULT 0,
    lines_deleted INTEGER NOT NULL DEFAULT 0,
    first_seen_unix INTEGER,
    last_touch_unix INTEGER,
    PRIMARY KEY (repo_id, path)
);
CREATE INDEX idx_path_stats_repo_revisions ON path_stats(repo_id, revisions DESC);
CREATE INDEX idx_path_stats_repo_last_touch ON path_stats(repo_id, last_touch_unix);

CREATE TABLE author_stats (
    repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    author TEXT NOT NULL,
    commits INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id, path, author)
);
CREATE INDEX idx_author_stats_repo_path ON author_stats(repo_id, path);

CREATE TABLE cochange_pairs (
    repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    path_a TEXT NOT NULL,
    path_b TEXT NOT NULL,
    co_commits INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id, path_a, path_b),
    CHECK (path_a < path_b)
);
CREATE INDEX idx_cochange_pairs_a ON cochange_pairs(repo_id, path_a, co_commits DESC);
CREATE INDEX idx_cochange_pairs_b ON cochange_pairs(repo_id, path_b, co_commits DESC);
