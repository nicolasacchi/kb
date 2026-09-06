-- V3.4-C1 — canvas-set persistence (SPA working-set canvas durability).
--
-- The SPA's Code Bubbles-style, read-only fragment canvas persists per
-- review/session. The server owns durability ONLY — layout/geometry is an
-- opaque client JSON payload (cap enforced at the route: 256 KiB, 413 with
-- the byte count on overflow — never silent truncation).
--
-- `review_id` associates with a local review by id only — NO FK cascade.
-- Review GC leaves canvas rows in place so a deleted review does not
-- destroy operator layout work; the id is a soft reference.
--
-- Unique (repo_id, name): one canvas name per repo (same posture as
-- reading_sets).

CREATE TABLE canvas_sets (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id      INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    review_id    INTEGER,
    payload      TEXT NOT NULL,
    created_unix INTEGER NOT NULL,
    updated_unix INTEGER NOT NULL,
    UNIQUE (repo_id, name)
);
CREATE INDEX idx_canvas_sets_repo ON canvas_sets(repo_id);
CREATE INDEX idx_canvas_sets_review ON canvas_sets(review_id);
