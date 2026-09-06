-- V0036 (V74-L1, design of record docs/research/
-- kb-code-v7-continuum-2026-09.html §Decisions D10 + D21, Track L) —
-- `kbc-canvas/1`: the BOARD store.
--
-- schema-epoch: rides the SAME refinery epoch sequence as every other
-- kb-code-server migration (kb invariant #2's `kb_core::sibling::
-- refuse_if_volume_ahead` boot guard) — no bump of its own, V0036 IS the
-- next epoch. Every statement is O(1) DDL on four brand-new EMPTY tables:
-- nothing scans, nothing backfills, so this adds no boot latency (the
-- V0027/V0028/V72-B0 lesson — no whole-corpus work between `Store::open`
-- and the `TcpListener::bind`).
--
-- ## Why NOT `canvas_sets`, and why not a kbc-seq/1 row
--
-- `canvas_sets` (V0019) is the v3.4-C1 SPA fragment canvas: ONE opaque
-- client JSON payload per row, and "the payload is never parsed
-- server-side" is a stated property of that table (`seq.rs`'s module doc
-- leans on it to explain why a board's `size` is `null`). A kbc-canvas/1
-- board is the exact opposite claim: every node is a REFERENCE the daemon
-- must re-resolve through the Ladder on every read, which it cannot do
-- without structured rows. Riding `canvas_sets` would either force the
-- server to parse the column it is invariantly forbidden to parse, or
-- silently redefine that column's meaning for rows the SPA already wrote.
-- So: new tables, and `canvas_sets` is FROZEN beside them (the
-- `/api/usages` → `/api/usages/2` treatment).
--
-- kbc-seq/1 (V71-G0, invariant 14) stays a projection LAYER: `seq.rs`
-- resolves boards out of BOTH tables after this migration and creates,
-- moves and merges nothing. A kbc-canvas/1 board reports a TRUE node
-- count where a `canvas_sets` row still reports `null`.
--
-- ## What a node IS
--
-- A CLAIM about something that lives somewhere else — a file range, a
-- review finding, an annotation, a session turn, a saved query. There is
-- deliberately NO resolution-state column: `pinned`/`carried`/`orphan` is
-- computed PER REQUEST by `boards::resolve`, exactly as `lanes::classing`
-- (invariant 21) and `entities::class_for` (invariant 13) do for their own
-- lanes, and root invariant #2's "kb-code mints classes, nothing is
-- cached" demands. A node whose target moved must never read back as
-- fresh.
--
-- `ref_json` is this daemon's OWN typed reference (serde of
-- `boards::NodeRef`), written only by `canvas apply` AFTER the lint has
-- validated it, and parsed on every read. That is the deliberate opposite
-- of `reading_sets.desk_json` (invariant 9), which is stored opaque and
-- verbatim precisely because nothing server-side interprets it. A board
-- whose references the daemon could not interpret could not be resolved,
-- and an unresolvable board is the thing this schema exists to prevent.
--
-- ## Coordinates
--
-- There are NO x/y columns on `canvas_nodes` except `pin_x`/`pin_y`, and
-- those are only ever written from an explicit `pins` map (D10: "boards
-- are stored coordinate-free plus pins, so the LLM never emits
-- coordinates"). Layout is the SPA's (TypeScript, one engine); the
-- server derives geometry in exactly one place, `boards::layout`, and
-- only for the JSON Canvas EXPORT, which is a snapshot rather than the
-- live board.
--
-- ## Cascades
--
-- `repo_id` cascades from `repos` the same way `canvas_sets` does. The
-- three child tables cascade from `canvas_boards` via SQL FKs (the
-- `review_findings` precedent, not the Rust-side `delete_reading_set`
-- one): a board's children are meaningless without it and no other table
-- references them.

CREATE TABLE canvas_boards (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id        INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    slug           TEXT    NOT NULL,
    title          TEXT    NOT NULL,
    description_md TEXT    NOT NULL DEFAULT '',
    -- pending | draft | accepted | archived. Rust-validated
    -- (`boards::is_valid_status`), never a SQL CHECK — this crate's house
    -- convention for a small stringly-typed vocabulary that must stay
    -- relaxable without a table rebuild (invariant 9; `annotations`'
    -- `anchor_kind`/`intent` do the same).
    status         TEXT    NOT NULL,
    -- The authoring git ref, CodeTour's `ref` field: what the author was
    -- looking at. Advisory context for a reader, NEVER a resolution input
    -- (every node re-resolves against the CURRENT working tree).
    authored_ref   TEXT,
    -- Canonical hash of the applied document. `apply` compares this to
    -- decide `unchanged: true` — the idempotency contract, in one column
    -- rather than a field-by-field diff that could disagree with itself.
    content_hash   TEXT    NOT NULL,
    -- Bumped on every CHANGING apply and on every status transition.
    revision       INTEGER NOT NULL DEFAULT 1,
    created_unix   INTEGER NOT NULL,
    updated_unix   INTEGER NOT NULL,
    UNIQUE (repo_id, slug)
);
CREATE INDEX idx_canvas_boards_repo ON canvas_boards(repo_id);

CREATE TABLE canvas_nodes (
    board_id  INTEGER NOT NULL REFERENCES canvas_boards(id) ON DELETE CASCADE,
    -- The AUTHOR's stable id (`boards::NODE_ID_RE`-shaped). Apply keys
    -- nodes on it, which is what makes a re-apply an upsert rather than a
    -- delete-and-recreate that would orphan every node thread.
    node_id   TEXT    NOT NULL,
    ordinal   INTEGER NOT NULL,
    kind      TEXT    NOT NULL,
    title     TEXT,
    body_md   TEXT,
    ref_json  TEXT    NOT NULL,
    group_id  TEXT,
    -- The `annotations.id` of this node's thread parent. Node threads
    -- REUSE the annotations store (D10) — there is no second comments
    -- table here, and this column is a soft reference with no FK, the
    -- same posture `canvas_sets.review_id` takes.
    thread_id TEXT,
    -- SERVER-written, never author-supplied: the text of the primary
    -- range's FIRST line, captured at apply time and ONLY when the file
    -- on disk was the blob the node claims (the `lane_facts.snippet`
    -- precedent, invariant 21 — a snippet captured against some other
    -- bytes would manufacture a match instead of admitting an orphan).
    -- This is what the carry-forward Ladder re-resolves against once the
    -- blob moves; without it a changed file can only ever be an orphan.
    anchor_snippet TEXT,
    pin_x     REAL,
    pin_y     REAL,
    PRIMARY KEY (board_id, node_id)
);

CREATE TABLE canvas_edges (
    board_id   INTEGER NOT NULL REFERENCES canvas_boards(id) ON DELETE CASCADE,
    ordinal    INTEGER NOT NULL,
    from_node  TEXT    NOT NULL,
    to_node    TEXT    NOT NULL,
    kind       TEXT    NOT NULL,
    label      TEXT,
    -- authored | derived. A DERIVED edge carries the trust class of the
    -- kb-code edge it came from; an AUTHORED edge carries none, and the
    -- lint refuses a `trust` on one — a human's arrow is not a claim the
    -- index can back (D10: "derived edges carry their class, authored
    -- edges one stroke").
    provenance TEXT    NOT NULL,
    trust      TEXT,
    PRIMARY KEY (board_id, ordinal)
);

CREATE TABLE canvas_steps (
    board_id INTEGER NOT NULL REFERENCES canvas_boards(id) ON DELETE CASCADE,
    ordinal  INTEGER NOT NULL,
    node_id  TEXT    NOT NULL,
    caption  TEXT,
    PRIMARY KEY (board_id, ordinal)
);
