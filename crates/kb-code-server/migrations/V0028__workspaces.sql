-- V70-A10 ("kb-code v7 'The Continuum' — Workspaces v0", operator addition
-- 2026-09-03) — the first projection-specific columns on the EXISTING
-- kbc-seq/1 substrate (reading_sets / reading_set_spans, migration V0009 +
-- V0022): a "workspace" is a reading_set with kind='workspace' plus a
-- captured desk snapshot, an optional ref label, and a longer description.
-- The design of record (docs/research/kb-code-v7-continuum-2026-09.html)
-- §The spine P6 rules the full kbc-seq/1 physical unification (one table
-- for reading sets / tours / trails / boards) is a v7.1 unit; this
-- migration deliberately WIDENS the current table rather than adding a
-- new one, matching that unit's own stated plan ("one wire schema... a
-- resolver over the three existing tables").
--
-- schema-epoch: this migration rides the SAME refinery epoch sequence as
-- every other kb-code-server migration (kb #2's `kb_core::sibling::
-- refuse_if_volume_ahead` boot guard) — no bump of its own, V0028 IS the
-- next epoch.
--
-- `kind` distinguishes a plain reading set ('set', the pre-existing
-- default — every row created before this migration reads back 'set'
-- unchanged, so `GET /api/sets?repo=` with no `kind=` param stays
-- byte-identical) from a workspace ('workspace'). Validated in Rust
-- (`reading_sets::is_valid_set_kind`), never a SQL CHECK constraint — this
-- crate's house convention for a small stringly-typed vocabulary
-- (`annotations.rs`'s `anchor_kind`/`intent` do the same; a CHECK can't be
-- relaxed later without a table rebuild, a Rust allow-list can).
ALTER TABLE reading_sets ADD COLUMN kind TEXT NOT NULL DEFAULT 'set';

-- The captured `DeskState` snapshot (`web-code/src/desk/deskState.ts`),
-- JSON, size-capped at 64 KiB by the route
-- (`reading_sets::MAX_DESK_JSON_BYTES`) — stored VERBATIM, never parsed or
-- interpreted server-side (kb's own `<template id="kb-prompt">` / kb-share
-- precedent: the daemon persists an opaque client payload without becoming
-- a second copy of the client's own schema). NULL on a plain 'set' row and
-- on a 'workspace' row saved before a client sent one.
ALTER TABLE reading_sets ADD COLUMN desk_json TEXT;

-- An optional caller-supplied ref/branch label a workspace groups under
-- (`GET /api/sets?kind=workspace&group=ref`'s grouping key) — same "opaque
-- annotation, never resolved/validated against git beyond SHAPE" posture
-- as `reading_set_spans.ref` (V0009's own doc) and `annotations`'s review-
-- scope fields: validated with `reviews::reject_user_ref` (shape only,
-- injection-safety — never checked for existence).
ALTER TABLE reading_sets ADD COLUMN ref TEXT;

-- Free-text Markdown description (size-capped at 64 KiB by the route,
-- `reading_sets::MAX_DESCRIPTION_MD_BYTES`) — deliberately SEPARATE from
-- the pre-existing `description` column (Phase E3's short one-line list
-- label): a workspace's `description_md` is the longer "what/why" write-up
-- the save dialog's Description field captures. NULL on a plain 'set' row.
ALTER TABLE reading_sets ADD COLUMN description_md TEXT;

-- `annotations.set_id` — a workspace's notes (general path-less notes AND
-- code-anchored comments alike; see `annotations::ANCHOR_KIND_SET`). TEXT,
-- NOT INTEGER: `reading_sets.id` (V0009) is a TEXT primary key (`set_` +
-- 12 hex chars, `reading_sets::new_set_id`) — unlike `reviews.id`
-- (INTEGER autoincrement), which `annotations.review_id` references, so an
-- INTEGER column here could never hold a real set id.
--
-- (deviation, recorded) — the unit brief specified `set_id INTEGER` by
-- analogy with `review_id` without checking `reading_sets`'s own PK type;
-- TEXT is the only shape that can actually reference it. A reply inherits
-- its parent's `set_id` (`routes::inherit_scope_field`, the same ladder
-- `review_id`/`ps_number`/`side` already use), so every row belonging to a
-- workspace — parent AND reply alike — carries the SAME `set_id`, which is
-- what lets the cascade below stay a single `WHERE set_id = ?` delete
-- rather than a parent/reply two-step.
--
-- NO SQL FK, same `review_id` (V0023) / `parent_id` (V0008) precedent —
-- cascade-delete lives in Rust (`store::Store::delete_reading_set`,
-- widened this migration to also drop every annotation (and any
-- annotation_suggestions row) whose `set_id` matches, in the SAME
-- transaction as the set + its spans) because a plain FK cannot express
-- that multi-table cascade as one store-owned transaction.
ALTER TABLE annotations ADD COLUMN set_id TEXT;
CREATE INDEX idx_annotations_set ON annotations(set_id, resolved) WHERE set_id IS NOT NULL;
