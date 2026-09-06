-- L1 — one-shot tombstone for memory link seeding. Paired with
-- V0010 `memory_links`. The indexer seeds links from HTML metas
-- (`kb-global`, `kb-linked-kbs`) only when this table has no row
-- for the artifact; after that first seed (or after the L4
-- backfill pass writes `*` for a pre-V0010 memory), the row here
-- pins the seeded decision and the indexer never re-imports the
-- metas — so a user who clears every link in the UI doesn't get
-- them rewritten on the next file Modified event.
--
-- The row is dropped from `process_delete` (paired with the lance
-- delete) so a removed-and-recreated memory file with the same
-- artifact id will re-seed cleanly.

CREATE TABLE memory_links_seeded (
    artifact_id  TEXT PRIMARY KEY,
    seeded_at    INTEGER NOT NULL  -- unix epoch seconds
);
