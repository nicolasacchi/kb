-- Sticky "fully read" indicator: track the furthest scroll_y reached
-- during a visit alongside the last-known position. scroll_y still
-- holds the current/resume position; scroll_y_max is a high-water mark
-- bumped server-side via SQLite's max() builtin on every scroll update.
-- The SPA's reading-progress chip computes pct from scroll_y_max /
-- scroll_max so re-reading the intro (scrolling back up) doesn't
-- flip a ✓ artifact back to a sub-95% percentage.
--
-- Backfill copies scroll_y into scroll_y_max for existing open rows:
-- best-effort, correct for any row last saved at-or-near its peak
-- (the common case). Undershoots only for the unlucky "scrolled back
-- up just before upgrade" case, which self-heals on the next scroll.

ALTER TABLE history ADD COLUMN scroll_y_max INTEGER NOT NULL DEFAULT 0;
UPDATE history SET scroll_y_max = scroll_y WHERE kind = 'open';
