-- v0.10 M2 — per-kb pinned memories. Same shape as the corkboard
-- (V0005) but scoped to the per-kb memory corpus. The recall route
-- fans out across each kb's table and decorates the matching hits
-- with `pinned: true` so they bypass the DecayPolicy floor.
--
-- Naming: kb-server side uses `/api/kb/{kb}/memories/{id}/pin` so the
-- HTTP shape mirrors the corkboard anchor; the table is
-- `pinned_memories` to keep the two concepts distinct internally.

CREATE TABLE pinned_memories (
    artifact_id  TEXT PRIMARY KEY,
    pinned_at    INTEGER NOT NULL  -- unix epoch seconds
);
CREATE INDEX idx_pinned_memories_at ON pinned_memories(pinned_at DESC);
