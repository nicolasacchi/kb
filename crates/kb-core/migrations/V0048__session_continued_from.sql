-- Cross-harness session handoff lineage: which session a handed-off
-- session continues (omp -> Claude Code or Claude Code -> omp).
--
-- continued_from_harness / continued_from_session_id are NULL=not a
-- handoff continuation (every pre-V0048 capture and every ordinary
-- session). They are read from the `[kb-handoff from <harness> session
-- <id>]` marker on the session's FIRST real user prompt and are SURFACED,
-- never scored: no reader sorts, filters or ranks on them, and they never
-- enter the R1 digest or the `code` lane. The columns ride the `sessions`
-- row so the artifact-id lifecycle registries (cascade, sweep, relocate)
-- already move and delete them.
--
-- Additive and nullable; the id index is non-unique (one session can be
-- continued many times, and the lookup `continued_in` runs on it).
-- Rollback caveat: this bumps the refinery epoch, so an older binary
-- refuses to boot on a migrated volume (`refuse_if_volume_ahead`); see
-- docs/self-host.md "Rolling back across V0048".
ALTER TABLE sessions ADD COLUMN continued_from_harness TEXT;
ALTER TABLE sessions ADD COLUMN continued_from_session_id TEXT;
CREATE INDEX IF NOT EXISTS idx_sessions_continued_from ON sessions(continued_from_session_id);
