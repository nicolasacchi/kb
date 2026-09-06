-- Phase E3 ("kb-code v2 — The Operable Reader") — reading sets: named,
-- server-persisted, ordered collections of file/span references. kb's own
-- reading-list design (kb's CLAUDE.md invariant #25 — ordered spans, notes,
-- import-as-one-transaction) is the spiritual ancestor, ported to code: a
-- shareable understanding-path ("the ingest path," "everything the auth
-- rework touched"), agent-feedable via `GET /api/pack?set=`
-- (`agentview::pack`), and auto-materializable from a session's touched
-- files (`POST /api/sets/from-session`, loopback-only — `reading_sets`
-- module doc).

CREATE TABLE reading_sets (
    id          TEXT PRIMARY KEY,
    repo_id     INTEGER NOT NULL REFERENCES repos(id),
    name        TEXT NOT NULL,
    description TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    UNIQUE (repo_id, name)
);

-- Spans are ORDERED: `ordinal` is the span's POSITION within its set
-- (0-based, contiguous). Every mutation that touches the span LIST
-- (create/replace) rewrites the whole ordinal sequence in one transaction
-- — the same "delete then re-insert, no stable per-row identity to diff
-- against" discipline `store::Store::replace_symbols`/`replace_occurrences`
-- already use for their own derived data (see those methods' doc). The one
-- exception is `append_reading_set_span` (`POST /api/sets/{id}/spans`),
-- which only ever adds after the current MAX(ordinal) — nothing before it
-- shifts, so no rewrite is needed there.
--
-- `path` is repo-relative (validated non-escaping at the route boundary,
-- `routes::safe_rel_path` — same gate `GET /api/tree`/`GET /api/file`
-- already apply). `line_start`/`line_end` are BOTH NULL for a whole-file
-- span, BOTH set for a line range (`line_start <= line_end`, also
-- route-validated) — there is no "start only" half-range shape. `ref` is
-- an optional caller-supplied revspec label (never resolved/validated
-- against git here — an opaque annotation, same posture as the
-- `annotations` table's own `sha`-bearing `anchor2` JSON for a `diff`
-- annotation, migration V0008). `note` is free text (e.g. a commit subject,
-- for a span materialized by `POST /api/sets/from-session`).
--
-- No SQL `ON DELETE CASCADE` on `set_id` (same convention as
-- `annotations.parent_id`, migration V0008's doc) — `store::Store::
-- delete_reading_set` deletes a set's spans in the SAME transaction as the
-- set row itself.
CREATE TABLE reading_set_spans (
    set_id      TEXT NOT NULL REFERENCES reading_sets(id),
    ordinal     INTEGER NOT NULL,
    path        TEXT NOT NULL,
    line_start  INTEGER,
    line_end    INTEGER,
    ref         TEXT,
    note        TEXT,
    PRIMARY KEY (set_id, ordinal)
);
-- `store::Store::{reading_set_spans,replace_reading_set_spans,
-- append_reading_set_span,delete_reading_set}`'s own by-`set_id` lookups
-- and deletes.
CREATE INDEX idx_reading_set_spans_set_id ON reading_set_spans(set_id);
