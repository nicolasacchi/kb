-- v0.x sessions "episodic memory" R4 — persist the research / tool-usage
-- signals a session produced (kb-CLI searches, web searches/fetches, subagent
-- spawns, skills/MCP tools, plan presentations), detected deterministically
-- from the transcript's tool_use blocks. Powers "how kb was used / what was
-- researched", feeds the recollect digest, and backs the R5 research rollups.
--
-- Additive child table keyed on artifact_id_session (the sessions PK, handling
-- multi-capture per invariant #11). Removed with the parent session row in
-- sessions_delete (the indexer unlink pass). Heuristic-parsed and framed as
-- "detected, not ground truth" (invariant #10 ethos).
CREATE TABLE session_research (
    artifact_id_session  TEXT NOT NULL,   -- FK-by-convention to sessions.artifact_id
    session_id           TEXT NOT NULL,   -- denormalized for cross-session feeds
    seq                  INTEGER NOT NULL, -- 0-based order within the session
    kind                 TEXT NOT NULL CHECK (kind IN
                            ('kb_search', 'web', 'skill', 'subagent', 'plan_span', 'artifact_open')),
    query                TEXT NOT NULL,   -- the query / target / label (best-effort)
    PRIMARY KEY (artifact_id_session, seq)
);
CREATE INDEX idx_session_research_session ON session_research(session_id);
