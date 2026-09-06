-- sessions-rethink W4 (memo R8/ADD-2) — widen session_research.kind's CHECK
-- constraint to accept 'grok_job'. The driver-side sniffer in
-- `parse_session_activity` detects a Bash tool_use invoking `grokclaude
-- {research,session,build,panel,fleet}` whose paired tool_result carries a
-- job ulid (or, when none is confidently recoverable, the invoking command
-- line as a degraded fallback) and records it here — the join key
-- `GET /api/sessions/by-job/{ulid}` (`kb sessions by-job`) queries from day
-- one, before W5's child-side (grokclaude-side) capture exists.
--
-- SQLite cannot ALTER a CHECK constraint in place, so this is the standard
-- rebuild: a new table with the widened constraint, copy every row over,
-- swap it in. Columns, PK and FK-by-convention semantics are otherwise
-- UNCHANGED from V0020 — this migration only widens the enum. Dropping the
-- table also drops its bound index; both are recreated identically.
CREATE TABLE session_research_new (
    artifact_id_session  TEXT NOT NULL,
    session_id           TEXT NOT NULL,
    seq                  INTEGER NOT NULL,
    kind                 TEXT NOT NULL CHECK (kind IN
                            ('kb_search', 'web', 'skill', 'subagent', 'plan_span', 'artifact_open', 'grok_job')),
    query                TEXT NOT NULL,
    PRIMARY KEY (artifact_id_session, seq)
);
INSERT INTO session_research_new (artifact_id_session, session_id, seq, kind, query)
    SELECT artifact_id_session, session_id, seq, kind, query FROM session_research;
DROP TABLE session_research;
ALTER TABLE session_research_new RENAME TO session_research;
CREATE INDEX idx_session_research_session ON session_research(session_id);
