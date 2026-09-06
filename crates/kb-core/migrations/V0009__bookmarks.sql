-- v0.13 B1 — bookmarks. Per-kb workflow state attached to an artifact:
-- a fixed-enum status (wishlist | todo | working_on | done | to_remove),
-- an optional free-form note, and an optional comma-separated `labels`
-- list. One row per artifact per kb; cross-kb listing happens at the
-- HTTP layer (`GET /api/bookmarks`) by fanning out and merging by kb.
--
-- Orthogonal to:
--   - corkboard (V0005)        — binary "pinned" set surfaced as ⚓
--                                 anchors in the SPA chrome.
--   - pinned_memories (V0006)  — memory-recall decay-floor bypass.
--   - <meta name="kb-status">  — artifact-authored intent baked into
--                                 the HTML (read-only at indexing).
--
-- The status set is enforced at the kb-core app layer
-- (`BookmarkStatus` enum). The column is plain TEXT so the schema
-- doesn't change when a new variant is added.

CREATE TABLE bookmarks (
    artifact_id  TEXT PRIMARY KEY,
    status       TEXT NOT NULL,
    note         TEXT,
    labels       TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,  -- unix epoch seconds, frozen on insert
    updated_at   INTEGER NOT NULL   -- unix epoch seconds, bumped on every set
);
CREATE INDEX idx_bookmarks_status  ON bookmarks(status);
CREATE INDEX idx_bookmarks_updated ON bookmarks(updated_at DESC);
