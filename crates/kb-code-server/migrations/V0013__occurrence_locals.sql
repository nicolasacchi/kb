-- V3.G1 — same-file locals binding column on `occurrences`.
--
-- Additive only. `local_def_ordinal` links a REFERENCE occurrence row to
-- the ordinal of the same-file DEFINITION occurrence it binds to under the
-- lexical scope graph (`crate::locals`). NULL means "not locally bound"
-- (unbound identifier, a def/import row, a language without a locals.scm
-- yet, or a scip-sourced row). The linkage is ordinal-within-(blob_hash,
-- salt), matching the existing PRIMARY KEY — not a byte offset — so a
-- re-derive of the tree-sitter pass rewrites both sides together under
-- the same (blob_hash, salt) key and never leaves a dangling pointer.
--
-- Pre-existing rows get NULL via the bare ADD COLUMN (SQLite default).
-- Cache invalidation for the four proof languages (rust/typescript/tsx/
-- python) is the per-language salt bump in lang.rs (+q1 → +q2), not this
-- migration: derived (blob_hash, salt) rows change shape, so the old salt
-- must stop hitting.

ALTER TABLE occurrences ADD COLUMN local_def_ordinal INTEGER;
