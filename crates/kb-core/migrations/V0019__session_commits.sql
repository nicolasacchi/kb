-- v0.x sessions "follow-the-work" P5 — persist the git commits/pushes/tags a
-- session produced (detected from Bash tool-calls + their output), so the
-- "what shipped from this work" link is a cheap indexed read and the resume
-- card can show what was committed.
--
-- Additive: a new child table keyed on artifact_id_session (the sessions PK,
-- handling multi-capture per invariant #11). Removed with the parent session
-- row in sessions_delete (the indexer unlink pass). SHAs are best-effort,
-- detected, never ground truth (invariant #10 ethos).
CREATE TABLE session_commits (
    artifact_id_session  TEXT NOT NULL,   -- FK-by-convention to sessions.artifact_id
    session_id           TEXT NOT NULL,   -- denormalized for cross-session feeds
    seq                  INTEGER NOT NULL,  -- 0-based order within the session
    kind                 TEXT NOT NULL CHECK (kind IN ('commit', 'push', 'tag')),
    sha                  TEXT,            -- short SHA parsed from the output; NULL if not found
    subject              TEXT,            -- commit message / label
    PRIMARY KEY (artifact_id_session, seq)
);
CREATE INDEX idx_session_commits_session ON session_commits(session_id);
