-- DCB W1.A (invariant #2) — deterministic doc→code references.
--
-- WHY A NEW TABLE, NOT `edges`: the edges PK is artifact↔artifact and
-- `backlinks_of` / the graph BFS / the atlas link layer / the orphan-delete
-- cascade all assume `get_by_id` resolves the destination. A code ref's
-- destination is a path in a repo kb has never seen and cannot open, so it
-- would be a permanently dangling edge in every one of those consumers.
--
-- WHY TWO TABLES: `code_refs_docs` is written on EVERY extraction, including
-- one that finds nothing. Without it a zero-ref doc is indistinguishable from
-- an unscanned one, the `coderef/1` cursor feed has nothing monotonic to page
-- over, and a consumer that once saw refs here could never learn they are gone.
--
-- NOTHING IN HERE IS A VERDICT. Every column is a HINT recorded from the doc's
-- own bytes. Existence, ambiguity, line drift and symbol precision are decided
-- by kb-code per request and are never persisted.

CREATE TABLE code_refs_docs (
    artifact_id     TEXT PRIMARY KEY,
    -- raw-bytes content hash of the source AT THE LAST EXTRACTION (12-hex,
    -- identical to the lance Doc's `content_hash` at that moment) — the
    -- doc-changed key. NOT NULL here (a row only exists once something has
    -- been extracted), but NOT necessarily the doc's CURRENT bytes either: a
    -- doc excluded by the hook's gate, or never reindexed since an edit, keeps
    -- a STALE value that no longer matches the doc's live content_hash
    -- (coderef/1 doc header, R14). The wire's `doc_hash` is null only when
    -- there is no row at all (`never_scanned`) — consumers computing "doc
    -- changed since" treat THAT null as "unknown, no banner", never as
    -- "changed"; a present-but-stale value is a known, accepted limitation.
    doc_hash        TEXT NOT NULL,
    -- wall-clock unix-seconds at the write that most recently CHANGED this
    -- doc's extraction (R4) — injected via `EnrichCtx::now_unix`, never read
    -- live inside `coderefs.rs` itself (that module stays clock-free). Only
    -- written when the extraction result actually changed (the comparison in
    -- `record_code_refs` deliberately excludes this column), so it is
    -- monotonic per artifact and an unchanged doc never re-surfaces on the
    -- cursor feed. Unlike file mtime (rejected: not reliably monotonic — a
    -- checkout/rsync/restore can regress it below a consumer's watermark), a
    -- wall-clock write timestamp cannot regress this way.
    extracted_at    INTEGER NOT NULL,
    -- `<meta name="kb-code-rev">` verbatim (`<label>@<sha>[+dirty]`), or NULL.
    code_rev        TEXT,
    ref_count       INTEGER NOT NULL DEFAULT 0,
    group_count     INTEGER NOT NULL DEFAULT 0,
    ungrouped_count INTEGER NOT NULL DEFAULT 0,
    -- 1 when extraction hit `coderefs::MAX_REFS_PER_DOC` and stopped early.
    truncated       INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_code_refs_docs_cursor ON code_refs_docs(extracted_at, artifact_id);

CREATE TABLE code_refs (
    artifact_id      TEXT NOT NULL,
    -- 0-based, document order. Stable within one extraction; NOT stable across
    -- an edit (a ref inserted above shifts every later ordinal) — consumers key
    -- on (artifact_id, ordinal) only within one `doc_hash`.
    ordinal          INTEGER NOT NULL,
    -- path | path_line | path_range | path_list | symbol_method | symbol_const
    -- | issue | external   (kb_core::coderefs::CodeRefKind)
    kind             TEXT NOT NULL,
    -- the token/element text VERBATIM, as the doc wrote it.
    raw_text         TEXT NOT NULL,
    -- repo-relative-ish path AS WRITTEN. Never normalized, never guessed.
    -- For `issue`, "<owner>/<repo>".
    path_hint        TEXT,
    -- for `issue`, the issue NUMBER lives in line_start (no extra column).
    line_start       INTEGER,
    line_end         INTEGER,
    -- normalized full span list ("425,440") whenever the source's :LINES
    -- suffix was a comma list — set on kind=path_list, but ALSO on an
    -- external ref carrying a comma list (kind=external wins over the
    -- line-shape classification for a gem/node_modules path, e.g.
    -- "gem-1.0/lib/a.rb:5,9"); NULL for path_line/path_range, whose single
    -- span lives entirely in line_start/line_end.
    line_spans       TEXT,
    symbol_container TEXT,
    symbol_member    TEXT,
    -- ≤200 bytes of the enclosing block's prose. HUMAN-FACING ONLY: no
    -- resolution predicate may read it (see the W1.A spec §6).
    context          TEXT NOT NULL DEFAULT '',
    -- space-joined identifier-shaped tokens, ≤8 / ≤120 bytes. The ONLY
    -- context input `codelens/1`'s line_state predicate may read.
    context_tokens   TEXT NOT NULL DEFAULT '',
    -- the enclosing h2/h3, NULL = ungrouped (refs before the first heading).
    -- There is deliberately NO sentinel group row for "ungrouped" (R9): the
    -- null FK plus `code_refs_docs.ungrouped_count` is the whole
    -- representation.
    group_key        TEXT,
    group_label      TEXT,
    group_anchor     TEXT,
    -- 1 when the ref came from `<code data-kb-ref=…>`. A declared ref still
    -- gets resolved — verification is always on.
    declared         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (artifact_id, ordinal)
);

-- "which docs cite path X" (kb-side lint + the W1.B corpus report) without a
-- full scan.
CREATE INDEX idx_code_refs_path_hint ON code_refs(path_hint);
