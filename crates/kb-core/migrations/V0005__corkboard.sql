-- v0.10 K1 — anchor corkboard. One row per artifact the user has
-- "anchored" (bookmarked) on this kb. Cross-kb listing happens at the
-- HTTP layer (`GET /api/anchors`) by fanning out to each kb's actor
-- and merging by kb name.
--
-- Schema named `corkboard` to avoid the naming collision with the
-- stale-comment-anchor sidecar (`kb_core::anchors`, .anchors-stale.json).
-- External naming (HTTP routes + SSE event keys + the SPA pill) stays
-- "anchor"; only the internal table + Rust module use `corkboard`.

CREATE TABLE corkboard (
    artifact_id  TEXT PRIMARY KEY,
    created_at   INTEGER NOT NULL  -- unix epoch seconds
);
CREATE INDEX idx_corkboard_created ON corkboard(created_at DESC);
