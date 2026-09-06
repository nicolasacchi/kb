-- B2 — token-level foundations: a `doc` column on `symbols` (signature was
-- already a column, populated for the first time this Wave — see
-- extract.rs's module doc) and a brand-new `occurrences` table.
--
-- Additive only, same ADR-2 keying discipline as `symbols`/`highlights`:
-- `occurrences` is keyed by (blob_hash, salt), never by path. Unlike
-- `symbols` (definitions only, one row per definition), `occurrences` is
-- EVERY identifier-like token in the four `lang::TOKEN_LEVEL_LANG_IDS`
-- languages (Rust/TypeScript/TSX/JavaScript this Wave) — def/ref/import —
-- see `occurrences.rs`'s module doc for the extraction + classification
-- rules. `ordinal` is the extractor's own deterministic emission order
-- (byte position ascending), same convention as `symbols.ordinal` — it
-- exists only to make the primary key unique per (blob_hash, salt).

ALTER TABLE symbols ADD COLUMN doc TEXT;

CREATE TABLE occurrences (
    blob_hash  TEXT NOT NULL,
    salt       TEXT NOT NULL,
    ordinal    INTEGER NOT NULL,
    name       TEXT NOT NULL,
    -- "def" | "ref" | "import" — see occurrences.rs's ROLE_* constants.
    role       TEXT NOT NULL,
    line       INTEGER NOT NULL,
    col_start  INTEGER NOT NULL,
    col_end    INTEGER NOT NULL,
    PRIMARY KEY (blob_hash, salt, ordinal)
);

-- `routes::resolve` (GET /api/resolve)'s name-lookup path: every occurrence
-- named `name` in one blob (further filtered to role='def' in Rust, not in
-- SQL — see that route's doc).
CREATE INDEX idx_occurrences_name ON occurrences(blob_hash, salt, name);
