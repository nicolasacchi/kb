-- W2.5 — the RAW TRANSCRIPTS lane: a PULL-ONLY full-text index over local
-- Claude Code transcript JSONL (`~/.claude/projects/**/*.jsonl` by default).
-- Never fused into the code/doc search lanes above, never embedded, never
-- served over a non-loopback request (`routes.rs`'s `loopback_only`
-- middleware, `router.rs`) — this is a grounded operator-ruling scope: kb's
-- own R0 rationale (session transcripts are excluded from kb's *ranked*
-- search/recall to avoid pollution) is about RANKING, not a ban on raw
-- search; a human/agent grepping their own recent chat history for "what
-- did I say about X" is exactly the workflow this closes.

-- Tail state: one row per transcript JSONL file this daemon has ever seen.
-- `src_file` is ROOT-RELATIVE (forward-slash, includes the `<project-dir>/`
-- prefix) — e.g. "-home-user-project-kb/<session-uuid>.jsonl" or
-- "-home-user-project-kb/<session-uuid>/subagents/agent-<id>.jsonl" — unique
-- across the whole `[transcripts] root`, so two projects can never collide
-- even though session/agent ids are already globally-unique UUIDs in
-- practice. `project_dir` is split out as its own column (the first path
-- component of `src_file`) purely so it's cheap to report/filter without
-- re-parsing the path string on every read.
--
-- `inode`/`byte_offset`/`mtime` are `transcripts::indexer`'s tail-state: a
-- reparse only ever reads `[byte_offset, EOF)` UNLESS the file's `inode`
-- has changed since the last tail (rotation/truncation/rewrite), in which
-- case every `transcript_turns` row for this `file_id` is deleted and the
-- whole file is re-walked from byte 0. `inode` is unix-only in practice —
-- see `indexer.rs`'s `file_inode` (this fleet's targets are Linux/macOS/
-- WSL2; native Windows is out of scope per the project CLAUDE.md).
CREATE TABLE transcript_files (
    id           INTEGER PRIMARY KEY,
    project_dir  TEXT NOT NULL,
    src_file     TEXT NOT NULL UNIQUE,
    inode        INTEGER NOT NULL,
    byte_offset  INTEGER NOT NULL,
    -- Unix seconds (mtime granularity doesn't need millisecond precision —
    -- unlike `ts` below, which is a per-turn wall-clock timestamp used for
    -- the search route's newest-first ordering).
    mtime        INTEGER NOT NULL
);
CREATE INDEX idx_transcript_files_project_dir ON transcript_files(project_dir);

-- One row per indexable TURN — see `transcripts::parse`'s module doc for
-- the exact JSONL-line-to-turn(s) extraction rules. A single JSONL line can
-- produce MULTIPLE rows here (an `assistant` line with a thinking block, a
-- text block, AND a tool_use block yields three) — `byte_offset`/`byte_len`
-- are the OFFSET/LENGTH OF THE WHOLE JSONL LINE (not a sub-range), so every
-- turn extracted from one line shares the same byte range; the search
-- route's snippet builder re-reads that range and re-derives the specific
-- turn's own text via `parse::parse_line` (matched back by `uuid`+`kind`),
-- rather than storing a second byte sub-range per block.
--
-- `session_id`/`uuid`/`parent_uuid` are the envelope's own
-- `sessionId`/`uuid`/`parentUuid` (verbatim strings — kb-core's own
-- sessions surface, invariant #11, treats `sessionId` as the ONLY
-- canonical session identity; this table follows the same rule, never a
-- filename-derived id). `parent_uuid` is NULL for a session's root turn.
-- `tool_name` is set only for `kind = 'tool_use'`. `file_paths` is a
-- JSON-encoded `Vec<String>` (mirrors `highlights.spans`' "opaque
-- JSON-in-a-TEXT/BLOB column" convention elsewhere in this schema) of paths
-- a `tool_use` turn's input referenced (`file_path`/`path`/`notebook_path`
-- keys) — empty-array `"[]"` for every other kind.
CREATE TABLE transcript_turns (
    id            INTEGER PRIMARY KEY,
    file_id       INTEGER NOT NULL REFERENCES transcript_files(id),
    session_id    TEXT NOT NULL,
    uuid          TEXT NOT NULL,
    parent_uuid   TEXT,
    -- Unix MILLISECONDS (matches `store.rs`'s `file_opens.opened_at`
    -- convention) — parsed from the envelope's ISO-8601 `timestamp`.
    ts            INTEGER NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('user', 'assistant', 'thinking', 'tool_use', 'tool_result')),
    tool_name     TEXT,
    file_paths    TEXT NOT NULL DEFAULT '[]',
    is_sidechain  INTEGER NOT NULL DEFAULT 0,
    byte_offset   INTEGER NOT NULL,
    byte_len      INTEGER NOT NULL
);
CREATE INDEX idx_transcript_turns_file_id ON transcript_turns(file_id);
CREATE INDEX idx_transcript_turns_session_id ON transcript_turns(session_id);
CREATE INDEX idx_transcript_turns_ts ON transcript_turns(ts);

-- The full-text index itself — a CONTENTLESS FTS5 table (`content=''`): the
-- searchable text is tokenized and indexed but NEVER stored a second time
-- (the raw JSONL stays the sole source of truth; the search route re-reads
-- `[byte_offset, byte_offset+byte_len)` off disk for a snippet — see
-- `transcripts::search`'s module doc). `rowid` is set EXPLICITLY on every
-- insert to equal the `transcript_turns.id` it indexes (manual insert
-- discipline, documented below — NOT sqlite triggers), so a MATCH hit's
-- `rowid` is a direct, un-translated foreign key back to
-- `transcript_turns`.
--
-- `contentless_delete=1` (SQLite 3.43+; this workspace pins
-- `libsqlite3-sys` 0.30.1 / bundled SQLite 3.46.x, well past that) is what
-- makes `DELETE FROM transcript_fts WHERE rowid = ?` legal on a contentless
-- table — WITHOUT it, a contentless FTS5 table can only grow (SQLite has no
-- way to un-index a posting list it never stored the original text for);
-- WITH it, SQLite maintains an internal tombstone marking the rowid deleted
-- so `indexer.rs`'s inode-swap-triggers-full-reparse path (delete every
-- turn for a `file_id`, then re-derive from byte 0) actually removes the
-- stale postings rather than leaking duplicate/stale hits into every future
-- search.
--
-- Manual insert/delete discipline (not sqlite triggers): every
-- `transcript_turns` write happens through `store::Store::
-- insert_transcript_turns`/`delete_transcript_turns_for_file`, which pair
-- the `transcript_turns` mutation with the matching `transcript_fts`
-- mutation in the SAME transaction — there is no separate mutation API
-- `transcript_turns` could be written through that a trigger would need to
-- guard against, so a hand-paired transaction is simpler than an
-- AFTER INSERT/DELETE trigger pair for the same one-writer-owns-both
-- guarantee.
CREATE VIRTUAL TABLE transcript_fts USING fts5(
    text,
    content='',
    contentless_delete=1,
    tokenize='unicode61'
);
