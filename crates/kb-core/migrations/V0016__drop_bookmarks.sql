-- RL-track (v0.18) — Reading Lists replace the v0.13 bookmarks feature.
-- Operator-locked decision: clean replacement, NO data migration — the
-- flat per-artifact status tracker is superseded by ordered, per-kb
-- lists (V0015) with derived read state. Separate from V0015 because
-- refinery migrations are append-only/checksummed: V0015 had shipped
-- before the removal phase landed, so the drop is its own step.
DROP TABLE IF EXISTS bookmarks;
