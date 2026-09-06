-- V72-J1 ("kb-code v7.2 — Comments") — `comments/1`: the comment index.
--
-- ONE scanner over every comment in a file, classified into the D8
-- taxonomy (doc | annotation | directive | section | licence | generated |
-- commented_code | prose). This table SUBSUMES the Phase-N TODO index:
-- `todo_items` is dropped at the bottom of this migration and
-- `GET /api/todos` becomes a filtered VIEW over these rows (kind =
-- 'annotation' AND keyword IN the legacy TODO family). Two scanners over
-- the same lines is exactly the drift this unit exists to remove.
--
-- ## Key
--
-- `(repo_id, path, blob_sha, comments_version, ordinal)`. The first two
-- are the READ key — every query in `crate::comments::routes` is
-- path-scoped, never blob-scoped, which is why the replace-time DELETE and
-- `Store::delete_file` are BOTH scoped to `(repo_id, path)` (the R1 lesson
-- kb-code-server invariant 12(a) records: a delete scoped to the blob
-- leaves the previous blob's rows live forever after an edit and skips
-- them entirely on a file delete). The last three are the FRESHNESS
-- stamp: `blob_sha` says which bytes produced these rows and
-- `comments_version` says which taxonomy + which tree-sitter grammar salt
-- did (`comments::comments_version_for`), so a bump to either re-extracts
-- instead of serving a stale kind forever.
--
-- `repo_id INTEGER` (not the repo NAME) matches `rails_edges`/
-- `entity_defs`, the two other path-keyed derived tables, rather than
-- `bookmarks`/`annotations`'s operator-owned TEXT name — these rows are
-- derived from bytes and die with the index, not with the operator's
-- workspace.
--
-- ## What is NOT here
--
-- `doc_state`. The drift oracle (`crate::comments::drift`) is arithmetic
-- over `git blame` computed PER REQUEST and persisted nowhere — a cached
-- freshness verdict is wrong the moment the next commit lands, and the
-- doc↔code bridge's "kb-code mints classes, nothing is cached" posture
-- (root CLAUDE.md invariant #2) applies verbatim to a lane whose whole job
-- is reporting how old something is.

CREATE TABLE comments (
    id                   INTEGER PRIMARY KEY,
    repo_id              INTEGER NOT NULL REFERENCES repos(id),
    path                 TEXT    NOT NULL,
    -- The blob these rows were derived from; a re-ingest of the same
    -- blob + version is a cache hit and skips the parse entirely.
    blob_sha             TEXT    NOT NULL,
    -- `comments@N+<lang salt>` — see the module doc above.
    comments_version     TEXT    NOT NULL,
    -- Stable emission order within the file (line ascending).
    ordinal              INTEGER NOT NULL,
    -- One of the eight `comments::CommentKind` names. Rust-validated, not
    -- a SQL CHECK — this crate's house convention for a small stringly-
    -- typed vocabulary that must stay relaxable without a table rebuild
    -- (`reading_sets.kind`, `annotations.anchor_kind`).
    kind                 TEXT    NOT NULL,
    -- Annotation only: the matched keyword, and its own trailing text
    -- (capped at `keywords::KEYWORD_TEXT_CAP` chars — this string IS
    -- `GET /api/todos`'s `text` field, so its cap is load-bearing for
    -- that route's byte-compatibility).
    keyword              TEXT,
    keyword_text         TEXT,
    -- Annotation only: the parsed smart_todo bag, as JSON
    -- (`keywords::SmartTodoFields`). Stored as text because the bag is
    -- OPEN — `by:` or any future key round-trips without a column.
    fields_json          TEXT,
    -- 1-based, inclusive.
    line_start           INTEGER NOT NULL,
    line_end             INTEGER NOT NULL,
    -- The block's own text, sigils stripped, bounded at
    -- `comments::COMMENT_TEXT_CAP_BYTES` (2 KiB) with the flag beside it
    -- — never a silent cut.
    text                 TEXT    NOT NULL,
    text_truncated       INTEGER NOT NULL DEFAULT 0,
    -- Doc only: the definition this block sits immediately above, and the
    -- body range the drift oracle blames against.
    symbol_name          TEXT,
    symbol_kind          TEXT,
    symbol_line_start    INTEGER,
    symbol_line_end      INTEGER,
    -- Directive only. `directive_has_reason` is NULL for a magic comment
    -- or build tag (which carries a value, not a justification) and only
    -- 0/1 for a SUPPRESSION directive — so the audit lane can never
    -- report `# frozen_string_literal: true` as "unreasoned".
    directive_tool       TEXT,
    directive_has_reason INTEGER,
    UNIQUE (repo_id, path, blob_sha, comments_version, ordinal)
);

-- `comments_for_file` / the replace-time delete / `delete_file`.
CREATE INDEX idx_comments_repo_path ON comments(repo_id, path);
-- `GET /api/comments?kind=` and the summary's per-kind GROUP BY.
CREATE INDEX idx_comments_repo_kind ON comments(repo_id, kind);
-- `GET /api/comments?keyword=` and `GET /api/todos`'s family filter.
CREATE INDEX idx_comments_repo_keyword ON comments(repo_id, keyword);

-- The Phase-N TODO index is subsumed (V0012). Its route, its CLI verb and
-- its two `tree::sources` decoration lanes all keep working — they now
-- read `comments` through the same `Store::list_todo_items` signature.
-- `files.id`, which V0012 introduced purely so this table could carry an
-- FK, stays: `import_edges` references it too.
DROP TABLE todo_items;
