-- Phase D-server ("The Operable Reader") — annotations v2: anchor kinds
-- (line | range | symbol | diff), threads (parent_id), and intents. See
-- `crate::annotations`'s module doc for the full per-kind construction/
-- resolution contract and `routes.rs`'s annotations section for the wire
-- shape. Additive in EFFECT: every existing (V0006/W4.6) row reads back as
-- `anchor_kind = 'line'`, `intent = 'note'`, `anchor2`/`parent_id` both
-- NULL — i.e. resolves EXACTLY as it did pre-migration (pinned by
-- `store::tests::legacy_v1_shaped_row_...` and
-- `annotations_route.rs`'s legacy-row test).
--
-- SQLite has no `ALTER TABLE ... ALTER COLUMN` (there is no way to relax an
-- existing NOT NULL constraint via `ADD COLUMN` alone), and a THREAD REPLY
-- carries NO anchor of its own (`anchor`/`anchor2` both NULL for a reply
-- row — see `routes::create_annotation`'s reply branch) — so the
-- previously-`NOT NULL` `anchor` column must become nullable. That requires
-- the standard SQLite table-rebuild recipe (create the new shape, copy
-- every row across, drop the old table, rename) rather than a plain
-- `ALTER TABLE ADD COLUMN`. The other three new columns
-- (`anchor_kind`/`anchor2`/`parent_id`) and `intent` could each have used a
-- bare `ADD COLUMN` on their own, but doing all four in the SAME rebuild
-- keeps this migration one self-contained unit rather than splitting
-- `anchor`'s relaxation into a separate file for no benefit.
--
-- Vocabularies (stringly-typed, validated at the ROUTE boundary — see
-- `crate::annotations::{is_valid_anchor_kind,is_valid_intent}` — never
-- CHECK-constrained here, matching this table's existing convention of
-- leaving `resolved`'s 0/1 and every other flag/enum-ish column
-- unconstrained at the SQL layer):
--   anchor_kind: line | range | symbol | diff
--   intent:      note | question | todo | flag-for-agent | tour-stop
--
-- `parent_id` deliberately carries NO `REFERENCES annotations(id)` SQL
-- foreign key (unlike `repo_id`'s own `REFERENCES repos(id)`, kept as-is
-- below) — parent-exists validation, the "one level of nesting only" rule
-- (a reply's parent must not itself be a reply), and cascade-delete are ALL
-- enforced in Rust (`routes::create_annotation`'s reply branch,
-- `store::Store::delete_annotation`'s transaction) because a plain SQL FK
-- can express "the parent id exists" but not "the parent must not itself be
-- a reply," and `ON DELETE CASCADE` would still need that same code-side
-- check duplicated for the create path anyway.
CREATE TABLE annotations_new (
    id          TEXT PRIMARY KEY,
    repo_id     INTEGER NOT NULL REFERENCES repos(id),
    path        TEXT NOT NULL,
    -- NULL only for a reply (parent_id IS NOT NULL) — every top-level
    -- annotation always has one. Holds the "backup"/start Selection for
    -- symbol/range kinds too — see crate::annotations's module doc.
    anchor      TEXT,
    anchor_kind TEXT NOT NULL DEFAULT 'line',
    -- Nullable, kind-dependent JSON payload: the END Selection for `range`,
    -- a `{name,container,kind}` descriptor for `symbol`, `{sha_full}` for
    -- `diff`; NULL for `line` and for a reply.
    anchor2     TEXT,
    -- Set only on a reply — the annotation it replies to.
    parent_id   TEXT,
    intent      TEXT NOT NULL DEFAULT 'note',
    body        TEXT NOT NULL,
    author      TEXT NOT NULL DEFAULT 'you',
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    resolved    INTEGER NOT NULL DEFAULT 0
);

INSERT INTO annotations_new
    (id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
     body, author, created_at, updated_at, resolved)
SELECT
    id, repo_id, path, anchor, 'line', NULL, NULL, 'note',
    body, author, created_at, updated_at, resolved
FROM annotations;

DROP TABLE annotations;
ALTER TABLE annotations_new RENAME TO annotations;

-- Same query as V0006's own index — dropped along with the old table, so
-- it must be recreated against the rebuilt one.
CREATE INDEX idx_annotations_repo_path ON annotations(repo_id, path);
-- `store::Store::delete_annotation`'s cascade (`WHERE parent_id = ?`) and
-- `store::Store::list_open_annotations`'s per-row reply-count subquery.
CREATE INDEX idx_annotations_parent_id ON annotations(parent_id);
-- `GET /api/annotations/open`'s data source (`routes::list_open_annotations`
-- / `store::Store::list_open_annotations`): every unresolved, top-level row
-- for one repo, optionally filtered by intent.
CREATE INDEX idx_annotations_repo_resolved ON annotations(repo_id, resolved, parent_id);
