-- W4.6 — code annotations: durable, path-scoped line comments. Entirely
-- separate from kb's own kb-comments/1 `.review/<id>.json` files
-- (invariant #6 in the root CLAUDE.md) — this table lives ONLY in
-- kb-code's own per-daemon sqlite (this file), keyed by (repo_id, path),
-- with no kb corpus/artifact involved at all.
--
-- `anchor` is a JSON-encoded `kb_core::review::Anchor` (the SAME tagged
-- enum + serde shape kb's own `.review/*.json` comments use — see that
-- type's doc in crates/kb-core/src/review.rs) — imported, never copied.
-- `crate::annotations` always constructs the `Selection` variant here:
-- `offset` holds the 1-based line number at creation time, `css_path` is
-- left empty (there is no CSS structural path for a plain source file —
-- kb-core's path-based duplicate-disambiguation tiebreak is inert for
-- this table; `crate::annotations::resolve` does its own
-- nearest-to-original-line tiebreak instead), `snippet` is that line's
-- trimmed text capped at `kb_core::review::DEFAULT_CONTEXT_CHARS` (the
-- same cap kb-core's own fuzzy resolver compares against, so a stored
-- snippet never carries more than what re-resolution can actually use).
-- See `crate::annotations`'s module doc for the full construction/
-- resolution contract, including why `fuzzy_resolve_anchor_with` (kb-core's
-- own Jaro-Winkler-backed resolver) is reused rather than reimplemented.
--
-- `resolved` (0/1, SQLite's own boolean convention — matches
-- `chunk_status`/every other flag column in this schema) is an operator
-- triage flag ("I've dealt with this annotation"), unrelated to
-- `crate::annotations::resolve`'s "did the anchor still find its line"
-- staleness check — the two are deliberately different concepts sharing
-- similar names (mirrors kb's own `CommentStatus::Resolved` vs a stale
-- anchor being two independent axes).
CREATE TABLE annotations (
    id         TEXT PRIMARY KEY,
    repo_id    INTEGER NOT NULL REFERENCES repos(id),
    path       TEXT NOT NULL,
    anchor     TEXT NOT NULL,
    body       TEXT NOT NULL,
    author     TEXT NOT NULL DEFAULT 'you',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    resolved   INTEGER NOT NULL DEFAULT 0
);

-- `GET /api/annotations?repo=&path=`'s data source — every annotation for
-- one (repo, path).
CREATE INDEX idx_annotations_repo_path ON annotations(repo_id, path);
