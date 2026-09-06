-- V3.1-H1 — call-site + type-relation edges (content-addressed).
--
-- Mirrors `import_specs` (V0015): keyed (blob_hash, salt, ordinal), replaced
-- wholesale on re-extract under the same (blob_hash, salt). Salt bumps for
-- the four proof languages (rust/typescript/tsx/python, +q2 → +q3 in
-- lang.rs) invalidate the blob cache so these derived tables re-fill.
--
-- `call_sites` records every call_expression / method call / `new X(...)`
-- found while parsing a blob. `caller_ordinal` is the ordinal of the
-- innermost symbols-pass entry whose range contains the site (NULL at top
-- level). Cap 20_000 rows per file (loud truncate in the extractor).
--
-- `type_relations` records explicit type hierarchy edges:
--   rust:  impl Trait for Type (kind=impl), trait Sub: Super (kind=supertrait)
--   ts/tsx: class A extends B (kind=extends), implements I (kind=implements)
--   python: class A(B, C) (kind=bases, one row per base; skip object + kwargs)
-- Uncapped (tiny). Conservative: skip macro/cfg/computed bases.
--
-- Edge trust class is NOT stored — it is the class of the name resolution
-- that produced the endpoint at query time (never better; dynamic dispatch /
-- trait-object / duck-typed calls stay candidate).

CREATE TABLE call_sites (
    blob_hash         TEXT NOT NULL,
    salt              TEXT NOT NULL,
    ordinal           INTEGER NOT NULL,
    callee_name       TEXT NOT NULL,
    callee_qualifier  TEXT,
    line              INTEGER NOT NULL,
    col               INTEGER NOT NULL,
    arg_count         INTEGER,
    caller_ordinal    INTEGER,
    PRIMARY KEY (blob_hash, salt, ordinal)
);
CREATE INDEX idx_call_sites_callee ON call_sites(callee_name);
CREATE INDEX idx_call_sites_blob ON call_sites(blob_hash, salt);

CREATE TABLE type_relations (
    blob_hash  TEXT NOT NULL,
    salt       TEXT NOT NULL,
    ordinal    INTEGER NOT NULL,
    kind       TEXT NOT NULL,  -- 'impl' | 'supertrait' | 'extends' | 'implements' | 'bases'
    subject    TEXT NOT NULL,
    object     TEXT NOT NULL,
    line       INTEGER NOT NULL,
    PRIMARY KEY (blob_hash, salt, ordinal)
);
CREATE INDEX idx_type_relations_subject ON type_relations(subject);
CREATE INDEX idx_type_relations_object ON type_relations(object);
CREATE INDEX idx_type_relations_blob ON type_relations(blob_hash, salt);
