-- PRR-N3 — the Rails lens (`crate::frameworks::rails`): deterministic,
-- LLM-free, convention-based Rails DSL edges (rails-lens/1 grammar,
-- `crate::frameworks::RAILS_LENS_GRAMMAR_VERSION`).
--
-- Content-addressed like `import_specs`/`call_sites`/`type_relations`
-- (V0015/V0016): keyed `(blob_hash, salt, ordinal)`, replaced WHOLESALE on
-- re-extract of the producing blob (`Store::replace_rails_edges`). Unlike
-- those three, `repo_id` is stored directly on the row — routes/views
-- resolution needs the live sibling-file set to disambiguate partials/
-- views (mirrors `import_graph`'s two-phase spec-then-resolve design, not a
-- pure per-blob CST walk), so a row is meaningfully "this repo's edge" in a
-- way `call_sites` (a pure blob→blob mapping) never needed to be.
--
-- `kind` is the CLOSED rails-lens/1 enum (see `frameworks::mod`'s doc for
-- the full table + the `route_file` PRR-N3 addition for the `draw()`
-- file-split convention). `trust` is capped at likely/candidate BY
-- CONSTRUCTION (CHECK constraint) — this lane structurally cannot emit
-- `exact` (invariant #2/DCB precedent: a wrong `exact` is a release
-- blocker, so the trust tier that could produce one is simply absent from
-- the CHECK).
--
-- Numbering note: the design doc (docs/research design-nav.md §2) named
-- this V0025; per milestone arbitration this repo's actual next free slot
-- is V0026 (siblings in the same v0.39 milestone own V0024/V0025).

CREATE TABLE rails_edges (
    id         INTEGER PRIMARY KEY,
    repo_id    INTEGER NOT NULL,
    kind       TEXT NOT NULL,             -- closed rails-lens/1 enum
    src_path   TEXT NOT NULL,
    src_line   INTEGER,                   -- nullable: some producers are file-level (e.g. draw())
    src_symbol TEXT,
    dst_kind   TEXT,                      -- route | controller_action | view | partial | dom_id | routes_file | ...
    dst_path   TEXT,
    dst_symbol TEXT,
    trust      TEXT NOT NULL CHECK (trust IN ('likely', 'candidate')),
    blob_hash  TEXT NOT NULL,             -- the producing file's blob (content-addressed replace)
    salt       TEXT NOT NULL,
    ordinal    INTEGER NOT NULL,
    extra_json TEXT,                      -- small structured metadata (http verb, turbo_stream verb, assoc name…)
    UNIQUE(blob_hash, salt, ordinal)
);
CREATE INDEX rails_edges_dst ON rails_edges(repo_id, dst_path);
CREATE INDEX rails_edges_src ON rails_edges(repo_id, src_path);
CREATE INDEX rails_edges_kind ON rails_edges(repo_id, kind);
