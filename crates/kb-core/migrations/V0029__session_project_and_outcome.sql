-- sessions-rethink W2 — the frozen V0029 column bundle (synthesis memo R5):
-- projects.md P2's 9-column manifest, renamed where the memo overrides
-- (`last_assistant_text`, not `outcome`), PLUS `substance` (memo R4). All
-- additive + nullable-or-defaulted, so every pre-existing `sessions` row
-- keeps working unread; `kb reindex` backfills every column via the SAME
-- `SessionCaptureHook` that populates a fresh capture (no bespoke backfill
-- path — see the enrich.rs wiring).
--
-- REJECTED (memo C-18): a persisted `trivial` bool (subsumed by `substance`),
-- and `outcome`/`outcome_reason` (R4 — deterministic outcome CLASSIFICATION
-- from closing text reads as in-daemon intelligence; badges are derived
-- display-time from facts instead. `/kb-distill` owns judgment, per the
-- no-in-daemon-LLM non-goal).

-- project_key / repo_root — P1's derived-key ladder (rungs 2+3 this wave;
-- rung 1, a forward-capture tail block, is deferred — see
-- `kb_core::sessions::derive_project`'s doc comment). `project_key` is
-- `claude_project_slug(root)`; `repo_root` is populated ONLY when the ladder
-- resolved a CONFIRMED git root (rung 2, from resolved `session_commits`
-- rows) — a rung-3 cwd-only key leaves it NULL (honest: we don't know it).
ALTER TABLE sessions ADD COLUMN project_key TEXT;
ALTER TABLE sessions ADD COLUMN repo_root   TEXT;
CREATE INDEX idx_sessions_project_key ON sessions(project_key);

-- harness — R5's closed set (`kb_core::sessions::HARNESSES`), extraction
-- ladder: adapter-meta JSONL line -> `<meta name="kb-harness">` -> the
-- `HARNESS_DEFAULT` fallback ('claude': a transcript that parsed as a
-- session at all, with no adapter fingerprint, came from Claude Code).
-- NOT NULL DEFAULT so un-backfilled history reads as honestly Claude, never
-- a NULL hole.
ALTER TABLE sessions ADD COLUMN harness TEXT NOT NULL DEFAULT 'claude';

-- cc_version — first non-empty top-level `version` field across the JSONL
-- (the `first_transcript_field(jsonl, "version")` rule).
ALTER TABLE sessions ADD COLUMN cc_version TEXT;

-- last_assistant_text — R3's closure quintuple: the last real (non-wrapper,
-- non-synthetic, main-thread) assistant prose, head-capped at
-- LAST_ASSISTANT_TEXT_MAX_CHARS (700) chars. The wire preview (`outcome`,
-- <=240 chars) is derived server-side from this column, never persisted
-- separately.
ALTER TABLE sessions ADD COLUMN last_assistant_text TEXT;

-- all_cwds — JSON array of every distinct `cwd` the session visited
-- (first-seen order). NULL when the session only ever touched ONE cwd (the
-- existing `cwd` column already covers that case).
ALTER TABLE sessions ADD COLUMN all_cwds TEXT;

-- commit_count — count of kind='commit' rows this capture wrote to
-- `session_commits` (push/tag excluded: the badge means commits shipped).
ALTER TABLE sessions ADD COLUMN commit_count INTEGER NOT NULL DEFAULT 0;

-- user_turns — count of role:"user" records classifying as
-- UserTextClass::Real (typed, or clearing the wrapper-skip fallback) and not
-- sidechain: the honest "real prompts" number, vs. the raw JSONL user-record
-- count `message_count` mixes in.
ALTER TABLE sessions ADD COLUMN user_turns INTEGER NOT NULL DEFAULT 0;

-- active_secs — the honest ACTIVE duration (R6/D4): sum of per-event-delta
-- gaps each clamped to ACTIVE_DELTA_CLAMP_SECS (300s), so a multi-day
-- multi-Stop capture reports real work time, not wall-clock span.
ALTER TABLE sessions ADD COLUMN active_secs INTEGER NOT NULL DEFAULT 0;

-- substance — surfaces.md S1's deterministic triage enum:
-- 'trivial' | 'routine' | 'substantive'. NULL is a valid, DELIBERATE value —
-- an un-backfilled row (pre-reindex) — and every reader must treat NULL as
-- 'substantive' (un-backfilled history is never hidden by a husk filter).
-- No CHECK constraint: forward-compatible with a future ladder value without
-- a migration, exactly like `sessions.model`/`sessions.harness` today.
ALTER TABLE sessions ADD COLUMN substance TEXT;
