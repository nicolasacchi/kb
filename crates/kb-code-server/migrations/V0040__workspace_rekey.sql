-- V75-M1 ("kb-code v7.5 'Worktrees, branches, time' — the Workspace
-- re-key", design of record docs/research/kb-code-v7-continuum-2026-09.html
-- §Decisions D13) — every repo-keyed table declares WHICH identity owns its
-- rows: the WORKSPACE (the shared git object store) or the WORKTREE (one
-- checkout on disk).
--
-- NUMBERING: this file took main's CURRENT MAX + 1 at merge time, not a
-- reserved slot (invariant 11(iii); V0033 stays permanently empty). No
-- schema-epoch constant is bumped by hand — `store::schema_epoch()` reads
-- the highest EMBEDDED migration version, so landing this file IS the
-- epoch bump, and an older binary pointed at a volume that ran it refuses
-- to boot through `kb_core::sibling::refuse_if_volume_ahead` (kb invariant
-- #2, the 13.5 h kbc rollback lesson). That refusal is the whole reason
-- this unit ships a backup gate (`crate::backup`) and a rehearsal verb
-- (`kb-code rehearse-migration`): the epoch is a ONE-WAY DOOR for the
-- volume.
--
-- TWO WORDS, ONE SPELLING — read this before adding a column here. D26's
-- kbc-seq/1 already owns the identifier `reading_sets.workspace_id`
-- (V0029), where it names a *reading set* of kind `workspace` (the Desk).
-- That is a DIFFERENT concept from D13's Workspace (a shared git object
-- store). This migration therefore never adds a column called
-- `workspace_id` to a table that could be confused with a Desk: the four
-- authored, path-anchored projection tables (`reading_sets`,
-- `canvas_sets`, `canvas_boards`, `bookmarks`) are WORKTREE-class and gain
-- `worktree_id`, so invariant 14's "canvas_sets deliberately has no
-- workspace_id" stays literally true.
--
-- O(1) DDL ONLY (V0029's rule, and V72-B0(a)'s: no whole-corpus
-- maintenance pass may sit between `Store::open` and the bind). Every
-- statement below is an `ALTER TABLE ADD COLUMN` on a column that is NULL
-- for every existing row, a `CREATE TABLE` on an empty new table, a
-- `CREATE TRIGGER`, or a PARTIAL index whose predicate excludes every
-- existing row (V0029's own precedent). NOTHING is backfilled here: the
-- backfill is a PAGED BACKGROUND pass (`crate::rekey`, V72-B0's shape)
-- that starts after the listener binds and persists a resumable cursor.
--
-- WHY A TRIGGER RATHER THAN 30 EDITED INSERT STATEMENTS. The key is a pure
-- function of `repos` (`workspace_id`/`worktree_id` on the row this row
-- already points at), so the ONE home for it is here, next to the column.
-- An `AFTER INSERT ... WHEN NEW.<key> IS NULL` trigger cannot be forgotten
-- by a future INSERT the way a hand-edited column list can, and it keeps
-- every `Store` write signature unchanged. `store::rekey::REPO_KEYED_TABLES`
-- is the Rust mirror of the list below and
-- `store::tests::v75_m1::every_repo_keyed_table_is_classified` walks
-- `sqlite_master` against it, so a table added later with a `repo_id`/
-- `repo` column fails the build until it declares a class.
--
-- The trigger is a no-op once `repos.workspace_id` is NULL-free and the
-- row already carries a key; it costs one rowid-keyed UPDATE per INSERT on
-- the 22 tables below, inside the caller's own transaction. Measured
-- against the ingest passes it touches (`replace_comments`,
-- `replace_rails_edges`, `replace_entity_defs`, the lane ingest) that is a
-- fraction of the tree-sitter parse those inserts follow; it is NOT free
-- and is recorded here rather than assumed.

-- ── The identity columns on `repos` (the CANONICAL home) ──────────────
-- `repos` is `meta`-class: it does not GAIN a foreign key, it HOLDS the
-- two. Written by `crate::workspace::resolve_and_upsert` on the background
-- pass, never by a route.
ALTER TABLE repos ADD COLUMN workspace_id TEXT;
ALTER TABLE repos ADD COLUMN worktree_id  TEXT;

-- ── D13's Workspace: the shared git object store ──────────────────────
-- `id` = `ws_` + 12 hex of an FNV-1a fold over (canonical common-dir,
-- root commit) — D13's "id = canonical common-dir + root-commit", derived
-- by `crate::workspace::workspace_id`. `root_commit` is NULL when HEAD is
-- unborn or the root walk exceeded its budget; the id then derives from
-- the common dir alone and `crate::workspace` says so rather than
-- fabricating a sha.
CREATE TABLE workspaces (
    id           TEXT    PRIMARY KEY,
    common_dir   TEXT    NOT NULL UNIQUE,  -- canonical, invariant #27
    root_commit  TEXT,                     -- NULL = unborn HEAD or over budget
    created_at   INTEGER NOT NULL,
    seen_at      INTEGER NOT NULL
);

-- ── D13's Worktree: one checkout, whose PATH IS A MUTABLE ATTRIBUTE ───
-- `id` is the admin-dir name (`<common>/worktrees/<id>`) — D13's "id =
-- admin-dir name; path is a mutable attribute" — or the reserved sentinel
-- `crate::workspace::MAIN_WORKTREE_ID` for the main worktree, which has no
-- admin dir. Moving a worktree on disk changes `path`, never `id`.
--
-- `mounted` + `path_resolution` are D13's thin-slice honesty pair: a
-- worktree git knows about but whose canonicalised path is outside every
-- configured `[[repos]]` root is "known, not mounted" and is listed with
-- that stated, never hidden and never silently browsed.
CREATE TABLE worktrees (
    workspace_id    TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    id              TEXT    NOT NULL,      -- admin-dir name, or "(main)"
    path            TEXT,                  -- MUTABLE attribute; NULL if git reported none
    branch          TEXT,                  -- short name; NULL when detached/unborn
    head_sha        TEXT,                  -- NULL when unborn
    is_main         INTEGER NOT NULL DEFAULT 0,
    bare            INTEGER NOT NULL DEFAULT 0,
    detached        INTEGER NOT NULL DEFAULT 0,
    locked          INTEGER NOT NULL DEFAULT 0,
    lock_reason     TEXT,                  -- verbatim; the owner oracle parses it DEFENSIVELY
    prunable        INTEGER NOT NULL DEFAULT 0,
    prunable_reason TEXT,
    mounted         INTEGER NOT NULL DEFAULT 0,
    path_resolution TEXT    NOT NULL DEFAULT 'absent',  -- exact | ancestor | absent
    repo_id         INTEGER REFERENCES repos(id),        -- the [[repos]] entry, when mounted
    seen_at         INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, id)
);
CREATE INDEX idx_worktrees_repo ON worktrees(repo_id) WHERE repo_id IS NOT NULL;

-- ── The re-key backfill's resumable cursor ────────────────────────────
-- V72-B0's marker is a sidecar FILE because a table would have bumped the
-- refinery epoch. This unit bumps the epoch anyway, so the cursor lives
-- here instead, written in the SAME transaction as the page it describes —
-- a file and a page can disagree; a row and its own page cannot.
-- `fingerprint` is the FNV-1a fold over the RESOLVED repo identities plus
-- the plan version (`rekey::identity_fingerprint`). A repo added, removed
-- or re-pointed changes it, which invalidates the recorded cursor and
-- walks the table again — a rowid scan whose `WHERE <key> IS NULL`
-- predicate skips every row already keyed, so it costs reads, not writes.
CREATE TABLE rekey_progress (
    table_name  TEXT    PRIMARY KEY,
    cursor      INTEGER NOT NULL DEFAULT 0,  -- last rowid keyed
    done        INTEGER NOT NULL DEFAULT 0,
    rows_keyed  INTEGER NOT NULL DEFAULT 0,
    fingerprint TEXT,
    updated_at  INTEGER NOT NULL
);

-- ══ OBJECT class ═════════════════════════════════════════════════════
-- Rows that are a function of the OBJECT STORE — a blob, a commit, git
-- history — and therefore identical for every checkout of one workspace.

ALTER TABLE behavioral_meta  ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_behavioral_meta_ws ON behavioral_meta(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_behavioral_meta_ws AFTER INSERT ON behavioral_meta
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE behavioral_meta SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE path_stats ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_path_stats_ws ON path_stats(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_path_stats_ws AFTER INSERT ON path_stats
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE path_stats SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE author_stats ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_author_stats_ws ON author_stats(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_author_stats_ws AFTER INSERT ON author_stats
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE author_stats SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE cochange_pairs ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_cochange_pairs_ws ON cochange_pairs(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_cochange_pairs_ws AFTER INSERT ON cochange_pairs
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE cochange_pairs SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE session_signals ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_session_signals_ws ON session_signals(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_session_signals_ws AFTER INSERT ON session_signals
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE session_signals SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE commit_sessions ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_commit_sessions_ws ON commit_sessions(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_commit_sessions_ws AFTER INSERT ON commit_sessions
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE commit_sessions SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE scip_runs ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_scip_runs_ws ON scip_runs(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_scip_runs_ws AFTER INSERT ON scip_runs
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE scip_runs SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE doc_refs ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_doc_refs_ws ON doc_refs(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_doc_refs_ws AFTER INSERT ON doc_refs
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE doc_refs SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE rails_edges ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_rails_edges_ws ON rails_edges(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_rails_edges_ws AFTER INSERT ON rails_edges
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE rails_edges SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

-- `entity_defs` already carries a per-checkout discriminator INSIDE its
-- primary key (`worktree`, V0029/invariant 13: an entity's FQN depends on
-- the checkout). That column is a NAME, not an identity: it says WHICH
-- checkout, `workspace_id` says which OBJECT STORE. They compose.
ALTER TABLE entity_defs ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_entity_defs_ws ON entity_defs(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_entity_defs_ws AFTER INSERT ON entity_defs
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE entity_defs SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE lane_runs ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_lane_runs_ws ON lane_runs(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_lane_runs_ws AFTER INSERT ON lane_runs
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE lane_runs SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE lane_facts ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_lane_facts_ws ON lane_facts(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_lane_facts_ws AFTER INSERT ON lane_facts
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE lane_facts SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE comments ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_comments_ws ON comments(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_comments_ws AFTER INSERT ON comments
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE comments SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE claims ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_claims_ws ON claims(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_claims_ws AFTER INSERT ON claims
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE claims SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

-- V74-L3a's `recipe_trust` (V0038) landed while this unit was in flight.
-- An operator's acceptance of a repo-versioned recipe keys on the git BLOB
-- OID of the recipe file — a content address — so the approval is a fact
-- about the object store and survives a branch switch.
ALTER TABLE recipe_trust ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_recipe_trust_ws ON recipe_trust(workspace_id) WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_recipe_trust_ws AFTER INSERT ON recipe_trust
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE recipe_trust SET workspace_id = (SELECT workspace_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

-- ══ WORKTREE class ═══════════════════════════════════════════════════
-- Rows that are a property of a PATH ON DISK in one checkout: the mirror
-- index, its attention lane, working-tree annotations, review checkouts,
-- and the four authored path-anchored projections. Sharing one of these
-- across two checkouts of one workspace would be a wrong answer, not a
-- saving.

ALTER TABLE files ADD COLUMN worktree_id TEXT;
CREATE TRIGGER trg_files_wt AFTER INSERT ON files
WHEN NEW.worktree_id IS NULL
BEGIN
    UPDATE files SET worktree_id = (SELECT worktree_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE file_opens ADD COLUMN worktree_id TEXT;
CREATE TRIGGER trg_file_opens_wt AFTER INSERT ON file_opens
WHEN NEW.worktree_id IS NULL
BEGIN
    UPDATE file_opens SET worktree_id = (SELECT worktree_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE annotations ADD COLUMN worktree_id TEXT;
CREATE TRIGGER trg_annotations_wt AFTER INSERT ON annotations
WHEN NEW.worktree_id IS NULL
BEGIN
    UPDATE annotations SET worktree_id = (SELECT worktree_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE reading_sets ADD COLUMN worktree_id TEXT;
CREATE TRIGGER trg_reading_sets_wt AFTER INSERT ON reading_sets
WHEN NEW.worktree_id IS NULL
BEGIN
    UPDATE reading_sets SET worktree_id = (SELECT worktree_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE canvas_sets ADD COLUMN worktree_id TEXT;
CREATE TRIGGER trg_canvas_sets_wt AFTER INSERT ON canvas_sets
WHEN NEW.worktree_id IS NULL
BEGIN
    UPDATE canvas_sets SET worktree_id = (SELECT worktree_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE canvas_boards ADD COLUMN worktree_id TEXT;
CREATE TRIGGER trg_canvas_boards_wt AFTER INSERT ON canvas_boards
WHEN NEW.worktree_id IS NULL
BEGIN
    UPDATE canvas_boards SET worktree_id = (SELECT worktree_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

-- The two tables keyed by repo NAME rather than `repos.id` (V0011/V0014).
-- `repos.name` is UNIQUE, so the lookup is the same one-row read.
ALTER TABLE bookmarks ADD COLUMN worktree_id TEXT;
CREATE TRIGGER trg_bookmarks_wt AFTER INSERT ON bookmarks
WHEN NEW.worktree_id IS NULL
BEGIN
    UPDATE bookmarks SET worktree_id = (SELECT worktree_id FROM repos WHERE name = NEW.repo)
    WHERE rowid = NEW.rowid;
END;

-- V74-L3b's `trails` (V0039) landed while this unit was in flight. A
-- trail is an ordered list of path+line steps the operator actually
-- walked, each pinned to the blob they were looking at — a record of one
-- CHECKOUT being read, exactly like `reading_sets` and the tours that
-- share `canvas_boards`.
ALTER TABLE trails ADD COLUMN worktree_id TEXT;
CREATE TRIGGER trg_trails_wt AFTER INSERT ON trails
WHEN NEW.worktree_id IS NULL
BEGIN
    UPDATE trails SET worktree_id = (SELECT worktree_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

-- V74-L3a's `recipe_runs` (V0038). A materialised run is the answer a
-- recipe gave over ONE checkout's state — the mirror index is a
-- worktree-class table, and a run against one branch's tree is not a run
-- against another's.
ALTER TABLE recipe_runs ADD COLUMN worktree_id TEXT;
CREATE TRIGGER trg_recipe_runs_wt AFTER INSERT ON recipe_runs
WHEN NEW.worktree_id IS NULL
BEGIN
    UPDATE recipe_runs SET worktree_id = (SELECT worktree_id FROM repos WHERE id = NEW.repo_id)
    WHERE rowid = NEW.rowid;
END;

ALTER TABLE reviews ADD COLUMN worktree_id TEXT;
CREATE TRIGGER trg_reviews_wt AFTER INSERT ON reviews
WHEN NEW.worktree_id IS NULL
BEGIN
    UPDATE reviews SET worktree_id = (SELECT worktree_id FROM repos WHERE name = NEW.repo)
    WHERE rowid = NEW.rowid;
END;
