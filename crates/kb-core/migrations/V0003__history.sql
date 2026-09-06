-- v0.6+ H1: per-user activity log (artifact opens + scroll, searches,
-- comments). One polymorphic table with a `kind` discriminator so the
-- SPA's chronological timeline view is a single ORDER BY started_at DESC
-- query. Nullable columns track the kind-specific fields.
--
-- Visit-gap model: an "open" row represents one continuous viewing
-- session. Re-opening the same artifact within 30 minutes bumps
-- updated_at on the existing row; opening after that window starts a
-- new row. Scroll updates from the iframe UPDATE scroll_y/scroll_max
-- on the latest open row for that artifact.

CREATE TABLE history (
    id          INTEGER PRIMARY KEY,            -- rowid; no AUTOINCREMENT (v1 doesn't delete rows)
    kind        TEXT NOT NULL CHECK (kind IN ('open','search','comment')),
    artifact_id TEXT,                            -- non-null for open & comment
    query       TEXT,                            -- non-null for search
    comment_id  TEXT,                            -- non-null for comment
    scroll_y    INTEGER NOT NULL DEFAULT 0,
    scroll_max  INTEGER NOT NULL DEFAULT 0,
    started_at  INTEGER NOT NULL,                -- unix epoch seconds (matches sources.added_at, errors.created_at convention)
    updated_at  INTEGER NOT NULL                 -- unix epoch seconds (= started_at unless scroll/visit-gap bumped)
);
CREATE INDEX idx_history_started ON history(started_at DESC);
CREATE INDEX idx_history_artifact_open
    ON history(artifact_id, started_at DESC)
    WHERE artifact_id IS NOT NULL AND kind = 'open';
