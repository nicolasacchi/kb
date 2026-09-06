-- v0.14 S2 — per-kb session enrichment table. One row per
-- memory-session artifact (kb-category="memory-session") in this
-- kb's corpus. Populated by the indexer when the doc lands;
-- removed by `delete_by_path` when the underlying file is unlinked.
--
-- Holds the parse-expensive metadata that drives the SPA's
-- /sessions list (message_count, first_user_prompt) so each request
-- doesn't re-scrape the embedded <pre> JSONL. The lance row stays
-- the source of truth for the artifact itself; this is enrichment.
--
-- session_id is the Claude Code conversation id parsed from the
-- artifact's <meta name="kb-session"> or, for backfill of
-- pre-v0.14 transcripts, from the source filename
-- `session-<ts>-<sid>.html`. Cross-kb listing happens at the HTTP
-- layer (`GET /api/sessions`) via the same BTreeMap fan-out the
-- corkboard / anchors route uses.

CREATE TABLE sessions (
    artifact_id        TEXT PRIMARY KEY,
    session_id         TEXT NOT NULL,
    started_at         INTEGER NOT NULL,  -- unix epoch seconds
    ended_at           INTEGER NOT NULL,  -- unix epoch seconds (mtime at index time)
    message_count      INTEGER NOT NULL,
    first_user_prompt  TEXT,
    source_relative    TEXT NOT NULL
);
CREATE INDEX idx_sessions_started_at ON sessions(started_at DESC);
CREATE INDEX idx_sessions_session_id ON sessions(session_id);
