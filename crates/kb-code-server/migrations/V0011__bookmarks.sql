-- Phase N ("kb-code v3 — Navigate") — bookmarks: durable, per-repo places
-- (path + line), not conversations (annotations already cover those). A
-- bookmark is a mark you can re-open later — optionally named with a
-- single-char mnemonic (vim-style: `m a` / `'a`), with delete-then-set
-- move semantics when the same mnemonic is reassigned within a repo.
--
-- `repo` is the configured repo NAME (a plain TEXT column, not a FK to
-- `repos.id`) — same "string name as the wire/CLI surface" convention
-- `annotations` uses for its own identity keys, and deliberately NOT the
-- `repo_id INTEGER REFERENCES repos(id)` shape `reading_sets` picked
-- (V0009). Bookmarks are operator-owned places; they survive a repo
-- re-register with the same name, and the store never needs to join
-- through `repos` for the list/get path.
--
-- `mnemonic` is OPTIONAL: NULL for an anonymous bookmark, or a single
-- `[0-9a-z]` character when set. The UNIQUE partial index below enforces
-- "at most one live owner of a given mnemonic per repo" at the SQLite
-- layer — the route layer's "move semantics" (DELETE the previous owner,
-- then INSERT/UPDATE the new one) is what *transfers* the mark; the
-- index is the safety net that refuses a half-applied double-assign.

CREATE TABLE bookmarks (
    -- AUTOINCREMENT so a delete-then-set mnemonic move never recycles the
    -- just-freed id (plain INTEGER PRIMARY KEY reuses free rowids; the
    -- SPA invalidates by id, so a recycled id would be a false collision).
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    repo        TEXT NOT NULL,
    path        TEXT NOT NULL,
    line        INTEGER NOT NULL,
    mnemonic    TEXT,
    note        TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- `store::Store::list_bookmarks`'s by-repo lookup (`GET /api/bookmarks?repo=`).
CREATE INDEX idx_bookmarks_repo ON bookmarks(repo);

-- At most one bookmark per (repo, mnemonic) when a mnemonic is set —
-- anonymous bookmarks (mnemonic IS NULL) are unrestricted. SQLite's
-- partial UNIQUE index is the exact tool for this (NULL is not unique
-- against NULL under a full UNIQUE constraint, but we want MANY nulls
-- AND at most one of each non-null mnemonic).
CREATE UNIQUE INDEX idx_bookmarks_repo_mnemonic
    ON bookmarks(repo, mnemonic)
    WHERE mnemonic IS NOT NULL;
