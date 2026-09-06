-- RL-track — Reading Lists. Multiple named, ordered lists per kb whose
-- entries target a whole artifact OR a section of it (anchor = the
-- kb-comments review::Anchor JSON). Replaces the v0.13 bookmarks feature
-- (the DROP ships separately in V0016 so the removal phase is
-- independently green — refinery files are append-only/checksummed).
--
-- Lifecycle: lists are USER-CURATED state, like bookmarks/history before
-- them — the indexer's unlink pass and purge_kb_data do NOT touch these
-- tables. An entry whose artifact left lance renders as a tombstone at
-- read time (title/source_relative resolve to NULL); path-stable artifact
-- ids make delete-then-recreate self-heal.
--
-- position is a DENSE 0-based integer, renumbered inside one transaction
-- on every structural mutation (add/move/remove/import). Race-free by
-- construction: every mutation serialises through the per-kb storage
-- actor (kb-core invariant #2), so there are no concurrent writers to
-- interleave with.
--
-- anchor_stale is the PERSISTED resolution state (unlike the comments
-- in-process stale tracker): the ListAnchorHook re-resolves anchors on
-- every reindex and writes transitions here, so staleness survives a
-- daemon restart without a sidecar file.

CREATE TABLE lists (
    id          TEXT PRIMARY KEY,                 -- "l_" + 12 hex
    title       TEXT NOT NULL COLLATE NOCASE,
    description TEXT,
    pinned      INTEGER NOT NULL DEFAULT 0,
    archived    INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,                 -- unix epoch seconds
    updated_at  INTEGER NOT NULL                  -- bumped on list edits + structural entry mutations
);
CREATE UNIQUE INDEX idx_lists_title ON lists(title);

CREATE TABLE list_entries (
    id            TEXT PRIMARY KEY,               -- "le_" + 12 hex
    list_id       TEXT NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    kb            TEXT NOT NULL,                  -- owning kb today; carried so cross-kb can land later without a migration
    artifact_id   TEXT NOT NULL,
    anchor        TEXT,                           -- canonical serde_json of review::Anchor; NULL = whole artifact
    note          TEXT,
    position      INTEGER NOT NULL,               -- 0-based, dense within the list
    read_override TEXT,                           -- 'read' | 'unread' | NULL (derived state wins when NULL)
    words         INTEGER,                        -- per-SECTION word estimate; NULL = use the lance word_count
    anchor_stale  INTEGER NOT NULL DEFAULT 0,     -- ListAnchorHook resolution state, restart-safe
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL                -- bumped on user edits only, NEVER by the machine sync
);
CREATE INDEX idx_list_entries_list     ON list_entries(list_id, position);
CREATE INDEX idx_list_entries_artifact ON list_entries(artifact_id);
-- Same artifact twice in one list is allowed iff the anchors differ;
-- anchor JSON is canonical (always produced by lists::anchor_to_json).
CREATE UNIQUE INDEX idx_list_entries_dedupe
    ON list_entries(list_id, artifact_id, ifnull(anchor, ''));
