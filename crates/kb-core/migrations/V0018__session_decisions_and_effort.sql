-- v0.x sessions "follow-the-work" P4/S9 — persist the steering decisions and
-- the per-session effort/error stats the extractor already produces, so the
-- decisions log, the effort columns on the index, and the resume card become
-- cheap indexed reads (no transcript re-scan).
--
-- All additive: the four sessions columns are DEFAULT 0 / nullable, and
-- session_decisions is a new child table keyed on artifact_id_session (the
-- sessions PK, handling multi-capture per invariant #11). Old rows backfill on
-- the next reindex of [kb.sessions]. Decisions/effort are extracted
-- deterministically (LLM-free, invariant #10 ethos) and framed as detected.

-- Effort + error stats on the sessions row.
ALTER TABLE sessions ADD COLUMN token_total INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN tool_calls  INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN model       TEXT;
ALTER TABLE sessions ADD COLUMN error_count INTEGER NOT NULL DEFAULT 0;

-- The decisions log — one row per steering moment (AskUserQuestion answer or
-- ExitPlanMode approval), in transcript order (seq). Removed together with the
-- parent session row in sessions_delete (the indexer unlink pass).
CREATE TABLE session_decisions (
    artifact_id_session  TEXT NOT NULL,   -- FK-by-convention to sessions.artifact_id
    session_id           TEXT NOT NULL,   -- denormalized for the cross-session decisions feed
    seq                  INTEGER NOT NULL,  -- 0-based order within the session
    kind                 TEXT NOT NULL CHECK (kind IN ('question', 'plan')),
    prompt               TEXT NOT NULL,   -- the question text (or 'plan approved')
    answer               TEXT,            -- the chosen answer; NULL for a bare plan approval
    PRIMARY KEY (artifact_id_session, seq)
);
CREATE INDEX idx_session_decisions_session ON session_decisions(session_id);
