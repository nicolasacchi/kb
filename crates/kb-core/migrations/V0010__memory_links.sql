-- L1 — memory ↔ kb links. Many-to-many edges between a memory
-- artifact (lives in a memory-scoped kb) and the normal kbs that
-- should see it during recall fan-out. Mirrors the per-kb shape of
-- corkboard (V0005) / pinned_memories (V0006) / bookmarks (V0009):
-- one table per memory-scoped kb, owned by the memory's home kb.
--
-- The sentinel `linked_kb = '*'` means "global" — recallable from
-- every kb. A real kb name cannot collide because `KbName::new`
-- (kb-core/src/types.rs) restricts kb names to `[a-z0-9_-]+`.
--
-- Source of truth: this table. HTML metas (`kb-global`,
-- `kb-linked-kbs`) only SEED the table on first index, gated by
-- the V0011 `memory_links_seeded` tombstone — subsequent re-indexes
-- never re-seed, so UI-driven mutations stick across file Modified
-- events.
--
-- Orthogonal to:
--   - pinned_memories (V0006) — decay-floor bypass for recall.
--   - <meta name="kb-supersedes"> — tombstone for stale memories.

CREATE TABLE memory_links (
    artifact_id  TEXT NOT NULL,        -- the memory's path-based ArtifactId
    linked_kb    TEXT NOT NULL,        -- target kb name; '*' = global
    created_at   INTEGER NOT NULL,     -- unix epoch seconds
    PRIMARY KEY (artifact_id, linked_kb)
);
CREATE INDEX idx_memory_links_kb ON memory_links(linked_kb);
