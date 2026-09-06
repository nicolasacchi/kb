-- V3.G2 — import graph + arity ranges on symbols.
--
-- Two complementary tables for the import graph:
--
-- 1. `import_specs` — content-addressed (blob_hash + salt), mirroring
--    `symbols`/`occurrences`: every import statement found while parsing a
--    blob, regardless of whether the target is resolvable on disk. Evidence
--    for "this file says it imports X". Replaced wholesale on re-derive
--    under the same (blob_hash, salt) key.
--
-- 2. `import_edges` — repo-addressed (file_id → target_file_id): the
--    STATICALLY resolved subset of those specs. Rebuilt per file_id on every
--    ingest visit (delete+insert), because resolution depends on the live
--    files table (target must exist) and the current path's directory, not
--    just content. ON DELETE CASCADE from files(id) keeps edges in sync with
--    live-mirror removals. Unresolvable specs simply leave no edge row — the
--    spec still lives as evidence in `import_specs`.
--
-- Out of scope for v3.0 resolution (documented here so a later wave doesn't
-- silently invent it):
--   * TypeScript `tsconfig.json` path aliases (`paths` / `baseUrl`)
--   * Macro-generated / cfg-gated Rust modules (unresolved is fine; wrong is not)
--   * node_modules / Cargo.toml / pip package resolution
--
-- Also additive on `symbols`: `param_min` / `param_max` for call-site arity
-- scoring (scorer, never a gate). NULL = unknown / unbounded (varargs or
-- unparsed). Pre-existing rows get NULL via bare ADD COLUMN.

CREATE TABLE import_specs (
    blob_hash  TEXT NOT NULL,
    salt       TEXT NOT NULL,
    ordinal    INTEGER NOT NULL,
    -- Language-native / resolve_module_file-ready module path string
    -- (e.g. "crate::util::helper", "./widget", "pkg/sub").
    raw_spec   TEXT NOT NULL,
    -- Coarse kind: "use" | "mod" | "import" | "from" | "require" | "export_from".
    kind       TEXT NOT NULL,
    PRIMARY KEY (blob_hash, salt, ordinal)
);

CREATE TABLE import_edges (
    file_id         INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    target_file_id  INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    raw_spec        TEXT NOT NULL
);
CREATE INDEX idx_import_edges_file_id ON import_edges(file_id);
CREATE INDEX idx_import_edges_target ON import_edges(target_file_id);

ALTER TABLE symbols ADD COLUMN param_min INTEGER;
ALTER TABLE symbols ADD COLUMN param_max INTEGER;
