-- v0.x sessions "follow-the-work" P0/S2 — widen the per-kb session
-- enrichment row and add the session->file edge table.
--
-- WHY: session_render.rs already parses the working dir, the AI title, the
-- git branch and every file the session touched on EVERY serve, then throws
-- it away — the V0008 row kept only 7 thin columns. This migration persists
-- that parse once at index time (via the SessionCaptureHook), so the
-- readable index (title/cwd), folder grouping, and bidirectional
-- session<->file links become cheap indexed reads instead of multi-MB
-- transcript re-scans.
--
-- All columns are additive + nullable (or DEFAULT 0), so existing V0008 rows
-- survive untouched and backfill their new values on the next reindex of the
-- [kb.sessions] corpus (no on-disk rewrite). The ended_at column is NOT
-- altered here — it stays INTEGER NOT NULL; the enrich hook simply starts
-- writing the corrected last-event timestamp into it instead of the capture
-- mtime ("correct in place", operator decision 2026-06-25).

-- Display + grouping fields, all transcript-derived (no corpus knowledge):
ALTER TABLE sessions ADD COLUMN title              TEXT;     -- aiTitle (display name); NULL falls back to first_user_prompt
ALTER TABLE sessions ADD COLUMN cwd                TEXT;     -- modal working directory (the folder/project key)
ALTER TABLE sessions ADD COLUMN git_branch         TEXT;     -- first gitBranch seen
ALTER TABLE sessions ADD COLUMN files_read_count   INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN files_edited_count INTEGER NOT NULL DEFAULT 0;

-- Folder facet (A1) — the /api/sessions/folders route groups on cwd.
CREATE INDEX idx_sessions_cwd ON sessions(cwd);

-- session_files — one row per (session, path, action) the session touched.
-- path is verbatim from the transcript (absolute OR source-relative,
-- whatever the agent passed); the enrich hook canonicalises + resolves
-- corpus membership at index time (S3) and fills target_kb/target_artifact_id
-- for in-corpus paths. Out-of-corpus paths persist with in_corpus=0 and a
-- plain path (A6). The reverse "which sessions touched this artifact" lookup
-- (A7) seeks on (target_kb, target_artifact_id).
--
-- Keyed on artifact_id_session (the sessions PK), NOT session_id: a session
-- can be captured more than once (continued sessions create a second
-- artifact), and each capture owns its own file set (invariant #11). Rows are
-- removed together with their parent session row in sessions_delete (the
-- indexer unlink pass), mirroring the V0008 lifecycle.
CREATE TABLE session_files (
    artifact_id_session  TEXT NOT NULL,   -- FK-by-convention to sessions.artifact_id
    session_id           TEXT NOT NULL,   -- Claude Code session id (denormalized for /sessions/{sid}/files)
    path                 TEXT NOT NULL,   -- verbatim transcript path
    basename             TEXT NOT NULL,   -- final path component, for display + grouping
    action               TEXT NOT NULL CHECK (action IN ('read', 'write', 'edit')),
    in_corpus            INTEGER NOT NULL DEFAULT 0,  -- 1 when path resolved under a kb mount
    target_kb            TEXT,            -- resolved corpus name (S3); NULL when out-of-corpus
    target_artifact_id   TEXT,            -- resolved 12-hex artifact id (S3); NULL when out-of-corpus
    PRIMARY KEY (artifact_id_session, path, action)
);
CREATE INDEX idx_session_files_session  ON session_files(session_id);
CREATE INDEX idx_session_files_artifact ON session_files(artifact_id_session);
CREATE INDEX idx_session_files_target   ON session_files(target_kb, target_artifact_id);
