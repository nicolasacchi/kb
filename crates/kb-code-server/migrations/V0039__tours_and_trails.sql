-- V0039 (V74-L3b, design of record docs/research/
-- kb-code-v7-continuum-2026-09.html §Decisions D12 (tours and trails),
-- D17 (attention, not comprehension), D10 (one step model) and §P7,
-- Track L) — `kbc-tour/1` rides the canvas tables; `kbc-trail/1` gets its
-- own three.
--
-- schema-epoch: rides the SAME refinery epoch sequence as every other
-- kb-code-server migration (kb invariant #2's `kb_core::sibling::
-- refuse_if_volume_ahead` boot guard) — no bump of its own, V0039 IS the
-- next epoch. Every statement is O(1) DDL: two `ALTER TABLE … ADD COLUMN`
-- with constant defaults (sqlite rewrites no rows for those) and four
-- brand-new EMPTY tables. Nothing scans, nothing backfills, so this adds
-- no boot latency (the V72-B0 lesson — no whole-corpus work between
-- `Store::open` and the `TcpListener::bind`).
--
-- ============================================================
-- Part 1 — `kbc-tour/1`: a tour is a BOARD whose nodes are its steps
-- ============================================================
--
-- D10 rules that "a board's `steps` and a tour share the step model — do
-- not build two". Two tables of step-shaped rows would be two lints, two
-- resolvers, two walkthrough implementations and two chances to disagree
-- with the Ladder. So a tour is not a new entity: it is a `canvas_boards`
-- row with `kind = 'tour'`, whose `canvas_nodes` ARE its steps and whose
-- `canvas_steps` rows carry their order and per-step camera.
--
-- That is exactly invariant 9's ruling ("a `reading_sets` row with
-- `kind = 'workspace'` IS a workspace — not a new entity") and invariant
-- 14's restatement of it for kbc-seq/1, applied one table over. It is
-- also why `seq.rs` needs no new source table for its `tour` projection:
-- the projection layer already resolves `canvas_boards`, and this column
-- is what lets it tell the two apart.
--
-- `kind` is Rust-validated (`tours::is_valid_board_kind`), never a SQL
-- CHECK — this crate's house convention for a small stringly-typed
-- vocabulary that must stay relaxable without a table rebuild (invariant
-- 9; `annotations`' `anchor_kind`/`intent` and `canvas_boards.status` all
-- do the same).
--
-- The DEFAULT is 'board', so every row written by V74-L1 reads back as a
-- board unchanged and `GET /api/boards` stays byte-identical for a store
-- with no tours in it. Every board read now filters on `kind` explicitly
-- rather than relying on that default: a tour must never appear in the
-- board list, and a board must never appear in the tour list.
--
-- ONE consequence worth stating out loud: `canvas_boards`' existing
-- `UNIQUE (repo_id, slug)` now spans BOTH kinds, so a tour and a board in
-- one repo cannot share a slug. That is deliberate rather than
-- incidental — a slug is what `?step=` deep-links and what the CLI takes
-- positionally, and two different documents answering to one name in one
-- repo is the ambiguity this table's identity was already designed to
-- prevent.
ALTER TABLE canvas_boards ADD COLUMN kind TEXT NOT NULL DEFAULT 'board';

-- The per-step CAMERA (D12: "camera? (fold/ctx hints, no coordinates)").
-- JSON, written only by `tours::apply` AFTER the lint has validated it,
-- and parsed on every read — the `canvas_nodes.ref_json` posture, not the
-- opaque-and-verbatim `reading_sets.desk_json` one (invariant 9), because
-- a camera this daemon could not interpret could not be applied.
--
-- It carries NO COORDINATES, and the lint refuses a payload that smuggles
-- any in under any name (`boards::lint`'s `coordinates` rule, reused
-- verbatim): a camera says "fold this card" and "show ±3 lines of
-- context", never where a card sits. Layout stays the SPA's, in
-- TypeScript, with one engine (D10).
--
-- NULL on every board step (V74-L1 wrote none) and on a tour step whose
-- author declared no camera.
ALTER TABLE canvas_steps ADD COLUMN camera_json TEXT;

-- The board/tour list is now always kind-filtered, so the index the list
-- reads through carries the discriminator.
CREATE INDEX idx_canvas_boards_repo_kind ON canvas_boards(repo_id, kind);

-- ============================================================
-- Part 2 — `kbc-trail/1`: the navigation record, OFF by default
-- ============================================================
--
-- D17 is the whole design of this half, and it is a PRIVACY decision
-- before it is a schema: viewport dwell was client-local in v7.0 and may
-- go server-side "only in the milestone that ships pause, purge and
-- retention TOGETHER; an explicit opt-in that is OFF on first boot with a
-- visible indicator; the agent-facing surface is aggregate only …, never
-- per-line spans … never a gate, never a score term."
--
-- Four properties of this schema exist to make those sentences
-- structurally true rather than merely intended:
--
-- 1. **There is no per-line table.** The finest row here is ONE STEP —
--    a place the operator went, with ONE dwell number. There is nowhere
--    to put a viewport span, a caret position or a scroll offset, and
--    `trails::reject_sub_step_keys` refuses a payload that tries (by
--    name, before the typed parse, so the message teaches the rule
--    instead of saying "unknown field" — `boards::lint`'s `coordinates`
--    precedent).
-- 2. **Dwell is DERIVED and QUANTISED, never client-supplied.** The
--    route computes `left_at - entered_at` and floors it to
--    `[trails] step_granularity_secs`. A client cannot hand in a finer
--    number because there is no column for it to land in.
-- 3. **The aggregate's only time key is the DAY.** `day` is written per
--    step precisely so `GET /api/trails/aggregate` can group without ever
--    reading `entered_at` — "no ordering below the day", enforced by the
--    query rather than by a promise.
-- 4. **Nothing here is a score term.** No ranking module in this crate
--    may so much as import `crate::trails`; `trails::tests::
--    no_ranking_module_imports_the_trail_ledger` is the source scan that
--    fails BY FILE if one does — `claims.rs`'s own pin (invariant 23(a)),
--    which is itself root invariant #10's surfaced-never-scored law.
--
-- Purge and retention are both wholesale over THESE tables and nothing
-- else. In particular an `annotations` row carrying a `trail_id` (a
-- human's dissent on an agent-AUTHORED trail — see below) SURVIVES a
-- purge: that is `claims`' ruling (invariant 23(a) — authored content is
-- not derived data, and deleting a person's own written objection because
-- they cleared their movement record would destroy the record that
-- explains it). The movement record is the sensitive material; the note
-- is the human's own words.

-- One trail. A RECORDED trail is minted per repo per UTC day (or by an
-- explicit fork); an AUTHORED one is laid down whole by an agent
-- (`POST /api/trails` with `authored: true`) for the human to walk with
-- `]`/`[`.
CREATE TABLE trails (
    id             TEXT    PRIMARY KEY,       -- "trl_" + 12 hex
    repo_id        INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    -- recorded | authored. Rust-validated (`trails::is_valid_origin`),
    -- never a SQL CHECK (the house convention above).
    origin         TEXT    NOT NULL,
    title          TEXT,
    -- The UTC calendar day a recorded trail covers, `YYYY-MM-DD`. NULL on
    -- an authored trail and on a fork (both of which are minted by an
    -- explicit act rather than by the calendar), which is exactly the
    -- rows the partial unique index below excludes.
    day            TEXT,
    -- Fork provenance (D12's fork chips): the trail this one branched
    -- from and the step it branched at. A soft reference with no FK, the
    -- `canvas_sets.review_id` / `claims.review_id` precedent — a fork
    -- outlives the parent it came from, and a parent that was purged
    -- leaves a fork that says so rather than a row that vanishes.
    parent_id      TEXT,
    parent_ordinal INTEGER,
    -- The SPA's own opaque session label, stored verbatim and never
    -- parsed. Not `sessions.session_id`: this daemon makes no join
    -- between a trail and a transcript, because that join is exactly the
    -- surveillance shape D17 refuses.
    session_hint   TEXT,
    created_unix   INTEGER NOT NULL,
    updated_unix   INTEGER NOT NULL
);
CREATE INDEX idx_trails_repo ON trails(repo_id, created_unix DESC);
-- One recorded, unforked trail per repo per day — the "a new trail per
-- day or per explicit fork" rule, as an assertion rather than a comment.
CREATE UNIQUE INDEX idx_trails_day ON trails(repo_id, day)
    WHERE day IS NOT NULL AND parent_id IS NULL;

-- One step. The FINEST row this schema has, by construction (see (1)
-- above).
CREATE TABLE trail_steps (
    trail_id   TEXT    NOT NULL REFERENCES trails(id) ON DELETE CASCADE,
    ordinal    INTEGER NOT NULL,
    -- The typed hop (design §P7, verbatim): search | definition_of |
    -- usage_of | caller_of | blame | why | story | review | framework |
    -- manual | agent_suggested. Rust-validated (`trails::VIA_KINDS`).
    via        TEXT    NOT NULL,
    path       TEXT,
    line_start INTEGER,
    line_end   INTEGER,
    symbol     TEXT,
    -- The blob the operator was looking at — the Ladder's witness, so the
    -- human's own read of their own trail can say `pinned`/`carried`/
    -- `orphan` per step instead of pretending a line number still means
    -- what it meant (`canvas_nodes`' own posture, invariant 24(a)).
    blob_sha   TEXT,
    -- SECONDS, and already quantised to `[trails] step_granularity_secs`
    -- when it was written. There is deliberately no `left_at`: it is an
    -- input to `dwell_secs`, not a fact worth keeping.
    entered_at INTEGER NOT NULL,
    dwell_secs INTEGER NOT NULL,
    -- The UTC day `entered_at` fell on, denormalised so the aggregate can
    -- group by day WITHOUT reading `entered_at` at all (see (3) above).
    day        TEXT    NOT NULL,
    note       TEXT,
    PRIMARY KEY (trail_id, ordinal)
);
CREATE INDEX idx_trail_steps_day ON trail_steps(day);
CREATE INDEX idx_trail_steps_path ON trail_steps(trail_id, path);

-- The runtime opt-in state — ONE row, id 1. Separate from
-- `[trails] enabled` on purpose, and the two mean different things:
-- the config key is the OPERATOR's master switch (absent ⇒ the whole
-- feature is off and no route will write anything), this row is the
-- explicit, per-boot-persistent opt-in D17 requires ("OFF on first
-- boot"). A fresh volume has NO row here, which reads as `off` — the
-- default is the absence of a decision, not a decision to record.
--
-- `mode` is off | recording | paused (`trails::MODES`), Rust-validated.
-- `POST /api/trails/state` is its only writer and is loopback-only and
-- audited, so every transition lands in `mutations_audit` (V0027) for
-- free.
CREATE TABLE trails_state (
    id           INTEGER PRIMARY KEY,
    mode         TEXT    NOT NULL,
    changed_unix INTEGER NOT NULL
);

-- A human's inline DISSENT on an agent-AUTHORED trail (D12: "the human
-- walks `]`/`[` and dissents inline, `trail notes` drains"). Notes REUSE
-- the annotations store rather than growing a second comments table —
-- the `canvas_nodes.thread_id` ruling (invariant 24(a)) and invariant 9's
-- `annotations.set_id` precedent, whose TEXT shape this column copies
-- because `trails.id` is a TEXT primary key too.
--
-- No SQL FK, same `set_id` (V0028) / `review_id` (V0023) / `parent_id`
-- (V0008) precedent. A reply inherits its parent's `trail_id` through the
-- SAME `routes::inherit_scope_field` ladder, so a whole thread carries
-- one trail id.
ALTER TABLE annotations ADD COLUMN trail_id TEXT;
CREATE INDEX idx_annotations_trail ON annotations(trail_id, resolved)
    WHERE trail_id IS NOT NULL;
