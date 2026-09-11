-- V76-R3b (D13 M2) — worktree lifecycle: the daemon may DELETE a worktree
-- only when it recorded creating it. Additive column, default 0, so every
-- worktree git already knew about (operator-created, or listed before this
-- unit) stays undeletable through the daemon. `replace_worktrees` preserves
-- the flag across re-enumeration (the path is a mutable attribute; the
-- "we created this" bit is not).
--
-- O(1) DDL: ALTER TABLE ADD COLUMN with a constant default. No backfill.
ALTER TABLE worktrees ADD COLUMN created_by_daemon INTEGER NOT NULL DEFAULT 0;
