-- V71-G0 ("kb-code v7.1 'Understanding' — entity index", design of record
-- docs/research/kb-code-v7-continuum-2026-09.html §The spine P9 / §Decisions
-- D6, evidence /tmp/kbc7/research/module-class-explorer.md §4 #1) — the
-- entity index (`entities/1`, `crate::entities`) plus the kbc-seq/1
-- workspace binding on `reading_sets` (D26).
--
-- schema-epoch: rides the SAME refinery epoch sequence as every other
-- kb-code-server migration (kb #2's `kb_core::sibling::
-- refuse_if_volume_ahead` boot guard) — no bump of its own, V0029 IS the
-- next epoch. Every statement below is O(1) DDL (three `CREATE TABLE`/
-- `CREATE INDEX` on an EMPTY new table, two `ALTER TABLE ADD COLUMN`,
-- one partial index over a column that is NULL on every existing row):
-- nothing scans, nothing backfills. That matters because the daemon binds
-- its listener only AFTER migrations finish (the V0027/V0028 lesson) — a
-- migration that walked `reading_sets` or the working tree would show up
-- as boot latency on every restart, forever.
--
-- ## One table, not two: the reopening IS the row
--
-- The design sketch names `entities` (keyed `(repo, fqn, kind)`) plus a
-- child `entity_definitions`. This migration ships ONE table whose row is
-- a DEFINITION SITE, and treats "the entity" as the GROUP BY over it —
-- deliberately, and for the same reason invariant 9 makes a workspace a
-- `reading_sets` row rather than a new entity: the parent row carried no
-- field the group-by cannot derive (kind, definition count, first/last
-- seen are all aggregates), and a stored aggregate maintained across N
-- independent per-path replaces is a second copy of the truth that WILL
-- drift the first time a delete-pass misses a decrement. A Ruby entity is
-- definitionally scattered across many files (the evidence report's whole
-- thesis); making the scattered site the atom, and the entity a query, is
-- the shape that cannot disagree with itself.
--
-- `entity_edges` (superclass / include / prepend / extend) is NOT created
-- here: nothing in this unit writes it, and an empty table with no writer
-- is exactly the v7.0 "silently dead surface" defect class. The plan
-- assigns "hierarchy incl. include/prepend/extend" to unit E1, which needs
-- its own CST walk (Ruby is not one of `hierarchy::supports_hierarchy`'s
-- four proof languages); the edges land there, with a writer.
--
-- ## Why `worktree` is in the key (the worktree thin slice)
--
-- Every derived table in this schema is keyed either by `blob_hash` (ADR-2
-- content addressing) or by `(repo_id, path)`. An entity's FQN is neither:
-- it depends on the CHECKOUT — the same blob at the same relative path
-- resolves to a different constant under a different Zeitwerk config, and
-- two worktrees of one repository legitimately hold different configs on
-- different branches. `worktree` is the empty string for a main worktree
-- and the linked-worktree NAME otherwise (`crate::entities::
-- worktree_key_for`, derived from gix's `git_dir` vs `common_dir`). Today
-- a second checkout is always a separately-registered `[[repos]]` entry
-- with its own `repo_id`, so this column cannot yet be anything but a
-- constant per repo — that is the point: the key is structurally unable to
-- alias BEFORE the deferred "union search over worktrees" feature (design
-- §Refusals of scope, defer → N) makes two checkouts share one repo id,
-- rather than being re-keyed under it afterwards. It also makes a row
-- self-describing about which checkout derived it.
CREATE TABLE entity_defs (
    repo_id        INTEGER NOT NULL REFERENCES repos(id),
    -- '' for a main worktree; the linked-worktree name otherwise.
    worktree       TEXT NOT NULL,
    -- Repo-relative, forward-slashed — the same shape `files.path` and
    -- `rails_edges.src_path` carry.
    path           TEXT NOT NULL,
    -- The extractor's own emission order within this file (outermost
    -- first, then by start line) — not meaningful on its own, it exists to
    -- make the primary key unique per (repo, worktree, path). Rewritten
    -- wholesale on every re-index of the path (`Store::
    -- replace_entity_defs`), the same delete-then-reinsert discipline
    -- `replace_rails_edges` uses and for the same reason: there is no
    -- stable per-row identity to diff against.
    ordinal        INTEGER NOT NULL,
    -- The fully-qualified constant path PROVED BY THE TREE: the literal
    -- `class`/`module` nesting in this file's own source, as reconstructed
    -- from the `symbols` rows' range containment. Never a convention.
    fqn            TEXT NOT NULL,
    -- 'class' | 'module' (the Ruby `map_kind` subset that names a
    -- constant). Rust-validated, never a SQL CHECK — this crate's house
    -- convention for a small stringly-typed vocabulary.
    kind           TEXT NOT NULL,
    -- How completely the TREE determines `fqn` ('lexical' | 'ambiguous',
    -- `entities::NESTING_*`). tree-sitter-ruby's tags query captures only
    -- the LAST constant of a compact path, so `class Reseller::Order`
    -- yields the name `Order`; the extractor recovers the dropped scope
    -- from the definition's own source line. When such a compact path
    -- appears INSIDE an enclosing module, Ruby's constant lookup decides
    -- between `M::A::B` and `::A::B` at RUNTIME — a thing no tree can
    -- settle — so the row records the lexically-nearest reading and is
    -- marked 'ambiguous', which `entities::class_for` structurally cannot
    -- turn into `exact`.
    nesting        TEXT NOT NULL,
    line_start     INTEGER NOT NULL,
    line_end       INTEGER NOT NULL,
    -- The constant the app's Zeitwerk configuration says this PATH must
    -- define, attached to the definition whose own last segment matches it
    -- (`entities::defs_for_file`). NULL when the path is under no autoload
    -- root, or when no definition in the file answers to that name — the
    -- honest "the convention expected something this file does not
    -- contain" state, never a fabricated row.
    zeitwerk_fqn   TEXT,
    -- What the Zeitwerk read itself was worth at index time ('read' |
    -- 'degraded' — `entities::zeitwerk::ZeitwerkState`). Stored per row so
    -- a read can classify honestly without re-reading (and re-guessing) a
    -- config that may since have changed: this records what was true when
    -- the claim was made.
    zeitwerk_state TEXT NOT NULL,
    -- The blob this claim was derived FROM. Compared at read time against
    -- the live `files.blob_hash` for the same path: a mismatch demotes the
    -- trust class rather than silently serving a claim about bytes that
    -- are no longer there (the aug-lane/1 freshness rule, design §P8).
    blob_hash      TEXT NOT NULL,
    PRIMARY KEY (repo_id, worktree, path, ordinal)
);
-- The `?ent=` lookup's two arms (`Store::entity_defs_for_fqn`): the
-- tree-proved name, and the convention-derived one.
CREATE INDEX idx_entity_defs_fqn ON entity_defs(repo_id, worktree, fqn);
CREATE INDEX idx_entity_defs_zeitwerk
    ON entity_defs(repo_id, worktree, zeitwerk_fqn) WHERE zeitwerk_fqn IS NOT NULL;
-- The per-path purge on `Store::delete_file` (worktree-agnostic: a deleted
-- path is gone from every checkout this repo id covers).
CREATE INDEX idx_entity_defs_path ON entity_defs(repo_id, path);

-- kbc-seq/1 (D26, design §P6) — the ONE key by which any other projection
-- (a plain set, a tour, a trail) declares the workspace it belongs to.
-- TEXT, matching `reading_sets.id`'s own TEXT primary key (`set_` + 12
-- hex) — the same shape/reasoning V0028's `annotations.set_id` records.
-- Self-referential within one table and deliberately WITHOUT a SQL FK
-- (same `review_id`/`parent_id`/`set_id` precedent — cascades in this
-- schema live in Rust, inside one transaction). NULL on every existing
-- row and on every row created without one, so `GET /api/sets` is
-- byte-identical for a caller that has never heard of it.
--
-- Boards (`canvas_sets`) deliberately do NOT gain this column in this
-- unit: nothing would write it. `GET /api/seq` surfaces them as
-- projections and says so in its own `notes` when a `?workspace=` filter
-- excludes them, rather than shipping a column with no writer.
ALTER TABLE reading_sets ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_reading_sets_workspace
    ON reading_sets(workspace_id) WHERE workspace_id IS NOT NULL;
