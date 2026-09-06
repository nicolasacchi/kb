-- MI-W1.1 — the memory-recall ledger: one row per (memory) hit a
-- `kb-recall` UserPromptSubmit hook actually injected into a captured
-- session, recovered from the transcript's `Item::MemoryInjection` items
-- (session-view/1 IR, invariant #11).
--
-- WHY here, not on the memory's own kb: derivation is local to session
-- indexing (the enrichment hook parses ONE just-captured transcript and
-- knows nothing about the memory corpora it references), so the ledger
-- lives in the SESSIONS corpus's sqlite, keyed loosely by `memory_kb` +
-- `memory_id` rather than a real FK. Reads that want "how often was memory
-- X recalled" fan out across every kb's `memory_recalls` table (most will
-- be empty — only a kb that ever captures `memory-session` transcripts
-- populates this), mirroring the existing `session_files.target_*` reverse-
-- lookup shape (invariant #28).
--
-- Lifecycle: REPLACE-on-recapture, scoped to the CAPTURE, not the session
-- (revised — the original comment here claimed captures for one session
-- always arrive in monotonic, non-interleaved order and so a delete-by-
-- `session_id` was safe with no read-time filtering; that isn't guaranteed
-- by anything in the codebase, so it doesn't hold). A `memory-session`
-- transcript is re-parsed in full on every Stop-hook capture (invariant #11
-- — one long session accrues many `sessions` rows, each with its OWN
-- `artifact_id`), so the enrichment hook deletes every existing row for
-- THIS capture's `artifact_id` and inserts the freshly-derived set in the
-- same transaction — exactly like `session_files`, which is keyed to one
-- specific capture's `artifact_id_session`. Every READ (`memory_recalls_for_session`,
-- `memory_recalls_counts_for_ids`) filters at query time via
-- `newest_capture_pred` (joining back to `sessions` on `started_at DESC`) so
-- a stale, superseded capture's rows — which are NOT deleted until that
-- capture's own `sessions` row is unlinked — never leak into a count.
--
-- `memory_id`/`memory_kb` are parsed out of the injected hit's free text
-- (`plugins/kb-memory/hooks/kb-recall.sh`'s jq filter is the only place a
-- memory's structured id survives into the transcript) — best-effort, no FK,
-- no cascade: a memory that's later forgotten/deleted just leaves an
-- orphaned ledger row (harmless; the census route simply won't find a
-- matching artifact to attach it to).
CREATE TABLE memory_recalls (
    memory_kb   TEXT NOT NULL,   -- the kb the recalled memory lives in
    memory_id   TEXT NOT NULL,   -- the recalled memory's 12-hex artifact id
    session_id  TEXT NOT NULL,   -- Claude Code session id (the recalling session)
    turn_id     TEXT,            -- the session-view/1 Turn id the injection landed on
    recalled_at INTEGER,         -- unix secs, from the enclosing Turn's ts (proxy — see enrich.rs)
    artifact_id TEXT NOT NULL    -- this capture's sessions.artifact_id (provenance)
);
CREATE INDEX idx_memory_recalls_memory_id  ON memory_recalls(memory_id);
CREATE INDEX idx_memory_recalls_session_id ON memory_recalls(session_id);
