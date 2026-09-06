-- V72-H2b (D7) — the per-FAMILY "this blob was derived" marker.
--
-- NUMBERING: this file took main's CURRENT MAX + 1 at merge time, not a
-- reserved slot. The milestone ledger originally pre-assigned gaps and
-- expected them to be filled later; that is unsafe and V72-H2b is where it
-- was caught. refinery selects only migrations with `version > current`
-- (`refinery-core`'s `traits::get_unapplied_migrations`), and with
-- `abort_missing` at its default `true` an embedded migration BELOW the
-- applied maximum is a hard `MissingVersion` error — so a volume already
-- migrated past a reserved slot REFUSES TO BOOT once the gap-fill lands.
-- `store.rs`'s `embedded_migration_versions_are_contiguous` now makes that
-- structurally impossible to reintroduce.
--
-- Every cache-hit gate in `ingest::index_file` used to ask a ROW-COUNT
-- question ("does this blob have symbol rows under this salt?"), and a
-- row count cannot distinguish "not derived yet" from "derived, and the
-- honest answer was zero rows". Every zero-symbol file therefore re-parsed
-- on EVERY visit, forever: an ERB template (tier `none`), an SCSS file
-- (tier `highlight_only`), a comment-only Rust file, a Markdown file with
-- no headings. V72-H1 reported the ERB case; this table fixes the class.
--
-- One row per (blob_hash, family, salt). `family` is
-- `lang::SaltFamily::as_str()` — `symbols` or `highlights` — and `salt` is
-- that family's OWN salt (V72-H2b split `symbol_salt` from
-- `highlight_salt`), so a highlight-query bump invalidates the highlight
-- marker and leaves the symbol one untouched. `rows` is bookkeeping for
-- the re-extract bill, never a gate: the gate is the row's EXISTENCE.
--
-- Same lifecycle rules as the derived tables this marks:
--   * written in the same transaction as the derivation it marks
--     (`Store::mark_derived`, called from `replace_symbols`/
--     `put_highlights`), purging every OTHER salt of the same language +
--     family for the blob (invariant 11);
--   * swept by `Store::sweep_stale_salt_page` against its OWN family's
--     current salt set;
--   * deliberately NOT deleted when a `files` row goes away — ADR-2's
--     content-addressed "an orphaned blob's derived rows are never pruned
--     on file delete" applies here exactly as it does to `symbols`.
CREATE TABLE derived_status (
    blob_hash  TEXT    NOT NULL,
    family     TEXT    NOT NULL,
    salt       TEXT    NOT NULL,
    rows       INTEGER NOT NULL,
    PRIMARY KEY (blob_hash, family, salt)
);
