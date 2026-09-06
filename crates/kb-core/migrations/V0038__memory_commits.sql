-- CT-F1 — the memory↔commit EXACT-ID join: which commits a memory was
-- actually cited in, recovered from `Kb-Memory:` commit trailers.
--
-- WHY A NEW TABLE (the `code_refs` precedent, invariant #2): this is a
-- claim about a MEMORY and a COMMIT — neither is an artifact↔artifact
-- edge (the `edges` PK), and a commit sha is not a doc `get_by_id` can
-- ever resolve. `session_commits` is the wrong home too: its rows are
-- keyed to one capture's commit LIST and are replaced wholesale per
-- capture, while a (memory, commit) pair is a durable fact that outlives
-- any single capture of the session that produced it.
--
-- WHAT'S DERIVED, AND FROM WHERE: nothing here is read from git a second
-- time. The capture envelope's additive `<script id="kb-session-commits">`
-- block already carries each commit's capture-time `git show` resolution —
-- `sha_full`/`subject`/`repo_root` plus the verbatim unfolded `trailers`
-- lines (`kb_core::vcs::resolve_commit`, invariant #11's Wave-0 amendment)
-- — and `session_commits.trailers` has persisted them, unread, since
-- V0025. CT-F1 adds ONE parse arm over that same already-parsed block
-- (`sessions::memory_ids_from_trailers` → `MemoryCommitLedgerHook`), so
-- every column below is a projection of bytes the capture already had.
--
-- THE WRITE SIDE IS OPT-IN, PER REPO, DEFAULT OFF. The `Kb-Memory:`
-- trailer is stamped by `plugins/kb-memory/hooks/git-dispatch/
-- trailer-logic.sh` only when the repo sets `kb.memoryTrailers=true` in
-- its OWN `.git/config` (operator ruling, 2026-08-20: opaque memory ids
-- landing in git history is fine for a private repo, not as a default).
-- Consequence for every reader: an EMPTY result is a NON-SIGNAL — it
-- overwhelmingly means "this repo never opted in", never "this memory
-- influenced no commit". Label accordingly; never render an absence here
-- as evidence.
--
-- NEVER SCORED. This is provenance, and provenance cannot reach the
-- scorer (invariant #10's CT amendment): no column below is wired onto
-- `RecallHit`/`Scored` or read by `kb_core::memory`'s ranking. It is a
-- display + investigation surface only (`kb why-memory`, the SPA Memory
-- Dossier, `GET /api/kb/{kb}/memories/{id}/commits`).
CREATE TABLE memory_commits (
    -- The memory's 12-hex artifact id, verbatim from the trailer value.
    -- Best-effort and FK-less, exactly like `memory_recalls.memory_id`: a
    -- memory later forgotten/deleted just leaves a row nothing resolves.
    -- The trailer grammar carries no kb name (`Kb-Memory: <hex12>`), so
    -- there is deliberately NO `memory_kb` column — and because artifact
    -- ids can collide ACROSS corpora (invariant #7 v2 / #28 v2: the id is
    -- a hash of the source-relative path), a same-id memory in another kb
    -- would join to the same rows. Accepted: the reader is always already
    -- scoped to one `{kb}` and the row is labelled a citation, not a
    -- proof.
    memory_id   TEXT NOT NULL,
    -- The commit's FULL hash, from the capture's own `git show` (never a
    -- second git read). Only a RESOLVED `kind="commit"` row can land
    -- here: an unresolved detection has no `sha_full`, and a push/tag row
    -- isn't a commit.
    sha_full    TEXT NOT NULL,
    -- Short sha + subject as the capture resolved them — carried so the
    -- read renders "committed in <sha> <subject>" without a git call or a
    -- join back to `session_commits` (whose rows the NEXT capture of the
    -- same session replaces wholesale).
    sha         TEXT,
    subject     TEXT,
    -- `find_git_root` result at capture time: WHICH repo this sha lives
    -- in. Load-bearing for honesty — a bare sha is meaningless across
    -- repos — and never a path kb opens.
    repo_root   TEXT,
    -- Provenance of the CLAIM (not of the fact): the session whose
    -- capture carried the trailer, and that capture's own
    -- `sessions.artifact_id`. The latter is this row's ARTIFACT-ID
    -- LIFECYCLE KEY — `memory_commits` is registered under it in all
    -- three id registries (`CASCADE_STEPS` + `SWEEP_TABLES` + the
    -- `cascade_relocate_doc` rekey tx), because #27/F3 relocate never
    -- re-indexes, so a missing registration would strand these rows under
    -- a dead id forever (invariant #2's lesson). `memory_id` is NOT swept
    -- or cascaded: it names a memory that usually lives in a DIFFERENT
    -- kb, where this kb's `keep` set says nothing about it.
    session_id  TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    -- Unix seconds when this row was DERIVED (the capture's indexer
    -- clock, `EnrichCtx::now_unix`) — deliberately NOT a commit
    -- timestamp: the capture envelope carries none, and inventing one
    -- would mean a second git read this design refuses. Named
    -- `recorded_at` so no reader mistakes it for author/commit date.
    recorded_at INTEGER NOT NULL,
    -- (memory, commit) is the FACT; everything else is provenance of the
    -- claim. The PK makes the write idempotent across the multi-capture
    -- fan-out (invariant #11: one session accrues many captures, each
    -- re-deriving the same trailers) — the newest capture's INSERT OR
    -- REPLACE simply rewrites the same row, so unlike `memory_recalls`
    -- these reads need NO `newest_capture_pred` scope to avoid
    -- double-counting: duplicates are structurally impossible.
    PRIMARY KEY (memory_id, sha_full)
);

-- The forward read: "which commits cite memory X" (the `commits` route +
-- `kb why-memory`). The PK's leading column already serves it; this index
-- is the reverse — "which memories does commit Y cite" — for the future
-- commit-side lookup and for the sweep's DISTINCT scan on artifact_id.
CREATE INDEX idx_memory_commits_artifact ON memory_commits(artifact_id);
CREATE INDEX idx_memory_commits_sha ON memory_commits(sha_full);
