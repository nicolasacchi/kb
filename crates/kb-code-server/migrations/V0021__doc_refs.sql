-- DCB W3.A ("kb-code v2 — the doc-code bridge") — the reverse "cited by"
-- index: one row per code-doc reference whose sync-time resolution against a
-- PINNED repo was a unique path match (`path_state == "present"` on
-- `codelens/1` — see `doclens/sync.rs`'s module doc for why ambiguous/absent/
-- external refs never get a row here).
--
-- A row is a CLAIM, not a cached verdict: `resolved_path` is re-validated
-- against the LIVE `files` table on every read (`doclens::sync::doc_refs_route`),
-- never trusted as current truth on its own — see kb's own CLAUDE.md invariant
-- #2 ("doc→code: extraction is pure + corpus-local in kb… classification is
-- delegated to kb-code and never persisted [in kb]"). This table is the one
-- deliberate, kb-code-SIDE exception: kb-code, not kb, may cache a claim about
-- ITS OWN repo, because kb-code owns read-time revalidation against that same
-- repo.
--
-- Denormalized (`doc_title`/`group_label`/`doc_path`) so `GET /api/doc-refs`
-- never needs a second round trip to kb at read time — all three are
-- cosmetic/display-only, never used to decide liveness.
--
-- Sync discipline: one DELETE-then-INSERT per (kb, doc_id) PER SYNC PASS,
-- scoped by (kb, doc_id) ACROSS every repo_id (not scoped to one repo) — so a
-- doc re-pinned to a DIFFERENT repo doesn't leave stale rows under its old
-- repo_id. Mirrors `reading_sets::create_reading_set`'s one-transaction insert
-- loop (`store.rs`), this crate's own precedent, rather than kb-core's
-- `record_edges`.
CREATE TABLE doc_refs (
    repo_id       INTEGER NOT NULL REFERENCES repos(id),
    kb            TEXT NOT NULL,
    doc_id        TEXT NOT NULL,
    -- The ref's own ordinal on `coderef/1` — NEVER reassigned, so a claim can
    -- always be traced back to the exact extracted reference it came from.
    ordinal       INTEGER NOT NULL,
    -- `coderef/1` `kind`, mirrored verbatim. The full 8-value set is
    -- path|path_line|path_range|path_list|symbol_method|symbol_const|issue|
    -- external, but only the first six can EVER resolve to
    -- path_state == "present", so this column's OBSERVED value set is a strict
    -- subset of the wire's kind set.
    kind          TEXT NOT NULL,
    -- `coderef/1` `raw` — the literal extracted text ("carts_controller.rb:284").
    raw_hint      TEXT NOT NULL,
    -- The repo-relative path this ref resolved to AT SYNC TIME (present/unique
    -- only). Re-validated on every read; never a live link on its own.
    resolved_path TEXT NOT NULL,
    -- The RESOLVED line (what the lens verified/remapped) when it produced one,
    -- else the document's own hint. `line_state` says which kind of evidence
    -- the pair carries.
    line_start    INTEGER,
    line_end      INTEGER,
    -- `codelens/1` line_state AT SYNC TIME — a display caveat only, never
    -- re-derived from this column.
    line_state    TEXT,
    group_key     TEXT,                -- coderef/1 group key, NULL = ungrouped
    group_label   TEXT,                -- denormalized (doc-level display only)
    doc_title     TEXT NOT NULL,       -- denormalized
    -- The CITING doc's own source-relative path (denormalized — builds the
    -- /a/{kb}/{doc_path} link-out via `doclens::doc_href` without a second kb
    -- round trip; NOT the cited code path).
    doc_path      TEXT NOT NULL,
    -- `coderef/1` doc_hash AT SYNC TIME — lets a consumer detect "the doc
    -- changed since this claim was captured" without re-fetching. NULL is
    -- "unknown", never "changed" (R14).
    doc_hash      TEXT,
    -- The resolving repo's head_sha at sync time (NULL only if the repo has
    -- zero commits) + whether that tree was dirty — the worktree label a claim
    -- was computed against.
    head_sha      TEXT,
    dirty         INTEGER NOT NULL DEFAULT 0,
    seen_at       INTEGER NOT NULL,    -- this sync pass's timestamp (unix seconds)
    PRIMARY KEY (repo_id, kb, doc_id, ordinal)
);

-- `doclens::sync::doc_refs_route`'s own lookup: "which docs cite path X in
-- repo Y".
CREATE INDEX idx_doc_refs_resolved_path ON doc_refs(repo_id, resolved_path);
-- Sync's own per-(kb, doc_id) DELETE, across every repo_id (see the note
-- above) — without this index that DELETE is an unindexed scan of the whole
-- table on every synced doc.
CREATE INDEX idx_doc_refs_kb_doc ON doc_refs(kb, doc_id);

-- Per-kb sync progress — the corpus-feed CURSOR (monotonic `extracted_at` +
-- artifact_id tiebreak, per W1.B's `coderef/1` feed contract). The cursor
-- value is an OPAQUE string this table never parses: sync persists and echoes
-- it back to `KbClient::code_refs_feed` verbatim, and the ONE place it is ever
-- decomposed is `doclens::sync::rewind_cursor`'s per-PASS same-second-gap
-- rewind (client-side only, never for feed-internal paging).
--
-- `cursor = NULL` means "never synced, start from the beginning". `last_error`
-- is the most recent pass's failure for this kb, if any; cleared to NULL on
-- the next successful pass for that kb.
CREATE TABLE doclens_sync_cursors (
    kb           TEXT PRIMARY KEY,
    cursor       TEXT,
    last_run_at  INTEGER NOT NULL,
    last_error   TEXT
);
