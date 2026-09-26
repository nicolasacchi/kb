-- Live-serve ledger columns. A recall response that actually returned hits
-- has no capture artifact_id, so it cannot ride memory_recalls_replace
-- (that DELETE is scoped to one capture and would drop that capture's
-- rows). The append path INSERTs instead and stamps a synthetic
-- artifact_id (`served-{session_id}-{served_at}-{pos}`) so this column
-- stays NOT NULL — this migration does not relax it — and a later capture
-- replace cannot delete the serve rows.
--
-- `title` and `injected_chars` are serve-time facts the capture parser
-- does not have. Both are NULLABLE with no DEFAULT: every pre-V0042 row,
-- and every capture row memory_recalls_replace still writes without
-- naming them, stays valid and reads back as NULL. No backfill.
--
-- ALTER-only. V0035 created the table; V0037 and V0041 already added
-- columns. A merged migration is never restated.
ALTER TABLE memory_recalls ADD COLUMN title TEXT;
ALTER TABLE memory_recalls ADD COLUMN injected_chars INTEGER;
