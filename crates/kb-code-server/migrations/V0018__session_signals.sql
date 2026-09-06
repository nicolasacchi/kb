-- V3.2-B2 — session pain signals + author_stats first-seen for agent_first_touch.
--
-- Populated when a commit→session join resolves and the kb-daemon session
-- detail is reachable (`GET /api/sessions/{id}`). ATTENTION signals only —
-- never quality verdicts. Missing row ⇒ pain is null (unknown ≠ none).
--
-- fail_count: reserved. kb-daemon SessionOut has no distinct test-failure
-- counter; only error_count (tool-result errors) is on the wire. We store
-- fail_count = 0 until a real fail signal exists — never invent one.

CREATE TABLE session_signals (
    repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    fail_count INTEGER NOT NULL DEFAULT 0,
    error_count INTEGER NOT NULL DEFAULT 0,
    duration_secs INTEGER NOT NULL DEFAULT 0,
    captured_at INTEGER NOT NULL,
    PRIMARY KEY (repo_id, session_id)
);
CREATE INDEX idx_session_signals_repo ON session_signals(repo_id);

-- Chronological first touch per (path, author) — enables agent_first_touch.
-- NULL on pre-V0018 rows until the next behavioral rebuild re-applies.
ALTER TABLE author_stats ADD COLUMN first_seen_unix INTEGER;
