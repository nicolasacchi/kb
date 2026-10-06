-- v0.46 SEG-A — segmented capture of very long harness sessions (seg1).
--
-- A session whose translated transcript outgrows the segment target is
-- captured as an ORDERED CHAIN of ordinary sessions: part 1 keeps the bare
-- raw session id (so existing captures and artifact ids never change), part
-- k>=2 is its own session under `<raw id>-p<NN>`. Each part is a complete
-- `sessions` row with its own `is_newest` group and `*_for_session` rows;
-- the unit every per-session read aggregates stays `session_id`.
--
-- These two columns are the chain link, copied from the part's adapter-meta
-- line (`segmentOf`, `segmentIdx`):
--   segment_of  — the raw (part-1) session id; NULL for an ordinary session
--                 AND for part 1 itself (part 1 is never rewritten).
--   segment_idx — the 1-based part index; NULL exactly when segment_of is.
-- They are SURFACED, never scored, and never joined on by a per-session
-- read. `segment_count`, `prev` and `next` are derived at READ time from
-- `WHERE segment_of = ? AND is_newest = 1`; nothing about counts is stored.
--
-- Additive, nullable, NO backfill: every existing row reads as an ordinary
-- session. Rollback caveat: this bumps the refinery epoch, so an older
-- binary refuses to boot on a migrated volume (`refuse_if_volume_ahead`);
-- see docs/self-host.md "Rolling back across V0045".
ALTER TABLE sessions ADD COLUMN segment_of TEXT;
ALTER TABLE sessions ADD COLUMN segment_idx INTEGER;

-- The chain read shape: "the newest capture of every part of <raw id>".
CREATE INDEX idx_sessions_segment_of ON sessions(segment_of) WHERE segment_of IS NOT NULL;
