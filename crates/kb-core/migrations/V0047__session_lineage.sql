-- Session lineage (Track A): which sessions are children of which, and
-- which project a capture belongs to.
--
-- parent_session_id / root_session_id / project_slug are NULL=not stamped
-- (every pre-V0047 capture and every top-level session). They are
-- SURFACED, never scored: no reader sorts, filters or ranks on them, and
-- they never enter the R1 digest or the `code` lane. The lineage columns
-- ride the `sessions` row so the artifact-id lifecycle registries
-- (cascade, sweep, relocate) already move and delete them.
--
-- Additive and nullable; the root index is non-unique (a root has many
-- children). Rollback caveat: this bumps the refinery epoch, so an older
-- binary refuses to boot on a migrated volume
-- (`refuse_if_volume_ahead`); see docs/self-host.md "Rolling back across
-- V0047".
ALTER TABLE sessions ADD COLUMN parent_session_id TEXT;
ALTER TABLE sessions ADD COLUMN root_session_id TEXT;
ALTER TABLE sessions ADD COLUMN project_slug TEXT;
CREATE INDEX IF NOT EXISTS idx_sessions_root ON sessions(root_session_id);
