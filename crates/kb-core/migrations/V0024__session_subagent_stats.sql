-- v0.x kb-code W0.2 — subagent aggregate stats on the per-kb sessions row.
--
-- WHY: subagent work is invisible today (invariant #11's SessionActivity only
-- tracks the MAIN agent's tokens/tool_calls/errors). Ground truth: a
-- SYNCHRONOUS Agent/Task delegation leaves a parent-transcript
-- `toolUseResult` carrying its own rollup (totalTokens/totalToolUseCount/
-- toolStats); an ASYNC/BACKGROUND delegation (the harness default) leaves
-- only a `status:"async_launched"` stub with NO stats — those numbers exist
-- only in the per-agent sidecar files (`<transcript-dir>/<session-id>/
-- subagents/agent-*.jsonl`) that W0.4/W0.5 will walk at capture time. This
-- migration is the sync fallback (retroactive on every existing capture, via
-- W0.2's parent-transcript extraction) plus the schema both paths write
-- through.
--
-- All additive + DEFAULT 0, so existing rows survive untouched and backfill
-- on the next reindex. `subagent_launched_unstatted` is deliberately a
-- SEPARATE counter from the stat columns — a session that launched agents
-- but got no stats back must never read identically to a session that
-- launched none (zeros never masquerade as "no subagents ran").
ALTER TABLE sessions ADD COLUMN subagent_count              INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN subagent_tokens             INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN subagent_tool_calls         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN subagent_files_edited       INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN subagent_launched_unstatted INTEGER NOT NULL DEFAULT 0;
