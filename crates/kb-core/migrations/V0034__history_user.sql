-- v0.34 X1 — multi-user attribution (kb-users/1).
--
-- Users are plain username strings used for ATTRIBUTION only (not
-- authorization). History rows, list read-overrides, and (in review JSON)
-- comments need a stable per-user key so two people sharing one daemon
-- don't clobber each other's visit gaps, scroll high-water marks, or
-- list checkmarks.
--
-- What was rejected:
--   * A separate `users` table + integer FKs — attribution is a free-form
--     string (header / token / operator); unknown users still attribute
--     verbatim, so a closed registry would only add join cost.
--   * Case-preserving usernames — append-only history makes a case fork
--     permanent (`Alice` vs `alice` would never merge). The ladder folds
--     to lowercase before stamping; config must already be lowercase.
--   * Migrating the legacy `list_entries.read_override` column in SQL —
--     the operator name lives in config, which migrations cannot see.
--     An idempotent startup pass (`Db::identity_backfill`) rewrites
--     `history.user = ''` and copies legacy overrides once config is in
--     hand.
--
-- Lifecycle:
--   * New rows stamp `user` from the identity ladder (phase Y).
--   * Pre-multi-user history rows land as `user = ''`; backfill rewrites
--     them to the configured operator.
--   * `list_entries.read_override` is FROZEN after backfill (never read
--     again by rollup/derive; writers go to `list_entry_user_state`).
--   * The partial index on open visits gains `user` so the 30-min gap
--     dedup and rollup window stay O(log n) per (artifact, user).

ALTER TABLE history ADD COLUMN user TEXT NOT NULL DEFAULT '';
-- '' means "pre-multi-user row" — an idempotent startup pass rewrites it to
-- the configured operator once config is in hand (migrations cannot know it).
DROP INDEX idx_history_artifact_open;
CREATE INDEX idx_history_artifact_open
    ON history(artifact_id, user, started_at DESC)
    WHERE artifact_id IS NOT NULL AND kind = 'open';
CREATE TABLE list_entry_user_state (
    entry_id   TEXT NOT NULL REFERENCES list_entries(id) ON DELETE CASCADE,
    user       TEXT NOT NULL,
    read_override TEXT,          -- 'read' | 'unread' | NULL
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (entry_id, user)
);
-- Marker: the config-aware startup pass (`Db::identity_backfill`) runs once.
-- A marker row — not re-derivation — because cleared overrides must stay
-- cleared (re-running INSERT OR IGNORE from the frozen legacy column would
-- resurrect a user-cleared list_entry_user_state row).
CREATE TABLE identity_backfill_done (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    done_at INTEGER NOT NULL
);
