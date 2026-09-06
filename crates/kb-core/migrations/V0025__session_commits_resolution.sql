-- kb-code W0.4 — capture-time git resolution of a session's detected
-- commits. Today `session_commits` (V0019) only carries what the
-- transcript's Bash tool-call output happened to print: a possibly-short
-- sha and a best-effort subject (unrecoverable for `-F <file>` messages).
-- `kb sessions capture` (the Rust envelope writer that replaces the bash
-- kb-capture.sh heredoc) resolves each detected sha with ONE
-- `git show -s` against the session's recorded cwd and rides the result on
-- the capture artifact itself (an additive `<script id="kb-session-commits">`
-- JSON block, kb_core::sessions::render_commits_block), so the daemon's
-- enrichment — which parses the ARTIFACT, never the live repo — can persist
-- the true sha/subject/author/parents/trailers.
--
-- All additive + backward compatible: existing rows keep their
-- transcript-detected sha/subject untouched, with `resolved` defaulting to
-- 0 (never re-resolved retroactively — the capturing session's cwd/repo may
-- no longer exist by the time a migration runs). An old/imported capture
-- with no commits block continues to insert `resolved = 0` rows forever;
-- there is no backfill pass.
--
-- `trailers` is newline-joined `"Key: value"` lines (NOT JSON) — one line
-- per git trailer, in commit-message order, verbatim from
-- `git show -s --format='%(trailers:only,unfold)'`. Chosen over a nested
-- JSON array so a plain `SELECT`/sqlite3 CLI session can eyeball it (trailer
-- values are single-line by construction — git folds continuations), and so
-- `session_commits_for_session` doesn't need a second JSON-decode pass on
-- every row. `NULL` when resolution found no trailers (or never ran).
ALTER TABLE session_commits ADD COLUMN sha_full  TEXT;
ALTER TABLE session_commits ADD COLUMN repo_root TEXT;
ALTER TABLE session_commits ADD COLUMN resolved  INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session_commits ADD COLUMN author    TEXT;
ALTER TABLE session_commits ADD COLUMN parents   INTEGER;
ALTER TABLE session_commits ADD COLUMN trailers  TEXT;
