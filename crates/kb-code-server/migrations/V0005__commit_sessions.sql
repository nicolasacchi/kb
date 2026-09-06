-- W3.2 — the join ladder's precompute cache. `join::ladder::resolve_commit`
-- reads THROUGH this table before running the trailer/exact/fuzzy/none
-- arms, and writes the result back after. Keyed by (repo_id, sha) where
-- `sha` is always the CANONICAL full hex sha (local `git`/gix resolution
-- disambiguates any short prefix before this table is ever touched — see
-- `join::local::resolve_local`), so two different prefixes of the same
-- commit share one row.
--
-- `confidence`/`via` are the join/1 shape's two labels: `confidence` is the
-- 4-valued enum (`trailer|exact|fuzzy|none`, golden-pinned in
-- `join::ladder`), `via` is the free-form taxonomy label naming which arm
-- actually fired (`commit-trailer`|`by-commit`|`subject`|`squash-trailer`|
-- `squash-subject`|`time-window`|`no-match`|`kb-unreachable`|`kb-disabled`).
-- Stored as plain TEXT (not a CHECK-constrained enum) so a future arm never
-- needs a migration to add its own `via` label.
--
-- `session_id`/`kb`/`display_name`/`started_at` are the best-effort
-- enrichment fields from kb's federated session data — any of them may be
-- NULL even on a `trailer`/`exact` hit (kb daemon unreachable at the moment
-- of resolution still lets the LOCAL trailer arm succeed with a bare
-- `session_id`, per the ladder's "trailer arm still works" contract) and
-- ALL of them are NULL on a `none` row.
--
-- `resolved_at` (unix seconds) drives freshness: `trailer`/`exact` rows are
-- PERMANENT (a commit's own trailer, or kb's own indexed sha match, never
-- goes stale for a fixed sha — `join::ladder::TTL_SECS` is never consulted
-- for these two), `fuzzy`/`none` rows expire after `TTL_SECS` (currently
-- 24h) so a LATER capture (the operator's session finishes and gets
-- indexed, or a trailer lands via a later amend under a different sha) can
-- upgrade a previously-approximate or absent resolution — see
-- `join::ladder::is_fresh`.
CREATE TABLE commit_sessions (
    repo_id      INTEGER NOT NULL REFERENCES repos(id),
    sha          TEXT NOT NULL,
    confidence   TEXT NOT NULL,
    via          TEXT NOT NULL,
    session_id   TEXT,
    kb           TEXT,
    display_name TEXT,
    started_at   INTEGER,
    resolved_at  INTEGER NOT NULL,
    PRIMARY KEY (repo_id, sha)
);
