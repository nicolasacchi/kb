-- CT-F5 — corpus-health SLOs: the measurement instrument for the whole
-- connective-tissue program. Two additive things, one feature:
--
--   1. `slo_snapshots` — the append-only log `kb slo snapshot` writes.
--   2. three NULLABLE census columns on `sessions` — the ONE new write the
--      ledger marker-parse-failure indicator needs (see below).
--
-- SURFACED, NEVER ENFORCED. Nothing in this file changes any behaviour on a
-- missed target. There is no alerting, no gate, no retry, no auto-repair and
-- no scoring consumer: an SLO miss is a number a human reads. The whole point
-- of an SLO here is to make corpus health FALSIFIABLE — a target you can miss
-- is a claim you can check — not to give the daemon a new reason to act.

-- ---------------------------------------------------------------------------
-- 1. The append-only snapshot log.
-- ---------------------------------------------------------------------------
--
-- NOT ARTIFACT-ID-KEYED — stated explicitly because the omission is otherwise
-- indistinguishable from the invariant-#2 mistake it looks like. Every other
-- recent side table (`code_refs`, `memory_commits`, `artifact_snapshots`) is
-- keyed to an artifact and therefore MUST appear in all three id-lifecycle
-- registries (`CASCADE_STEPS` + `SWEEP_TABLES` + the `cascade_relocate_doc`
-- rekey tx), because #27/F3 relocate never re-indexes and a missing entry
-- strands rows under a dead id forever. `slo_snapshots` carries no
-- `artifact_id` and no artifact-derived key at all: a row is a
-- WHOLE-CORPUS reading at a wall-clock instant (the kb is implied by which
-- per-kb sqlite file the row lives in). Deleting, moving, or reindexing any
-- document must NOT rewrite or reclaim a past reading — that would falsify
-- the history the log exists to keep. So its deliberate absence from those
-- three registries is the CORRECT registration, not an oversight.
--
-- APPEND-ONLY means append-only: the only statements this table ever sees are
-- INSERT and SELECT. There is no update path, no dedup-on-unchanged (unlike
-- `atlas_snapshots`, whose `coord_hash` skips an identical frame — a
-- flat-lining SLO is itself the signal, so every run must land), and no
-- retention prune. Rows are tiny (one per indicator per run, written only
-- when an operator or a cron runs `kb slo snapshot`), so unbounded growth is
-- an operator's choice, not a leak.
CREATE TABLE slo_snapshots (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Wall clock (unix seconds) at the run that produced this row. Rows from
    -- ONE `kb slo snapshot` invocation share it — that shared value is the
    -- run's identity; there is deliberately no separate run id, since a
    -- second-granularity instant plus the indicator key is already unique in
    -- practice and a run id would be a key nothing joins on.
    taken_at_unix INTEGER NOT NULL,
    -- Stable indicator key (`kb_core::slo::SloKey::as_str`): one of
    -- `coderef_resolution_pct` | `orphan_kb_sessions` |
    -- `ledger_parse_failure_pct` | `capture_freshness_hours`. A closed set in
    -- code, stored as TEXT so a future indicator is an append, not a
    -- migration — and so a row written by a newer binary still READS on an
    -- older one (it renders as an unknown key rather than failing a decode).
    indicator     TEXT NOT NULL,
    -- The measured value, or NULL when the indicator was `unknown` (the
    -- inputs genuinely weren't there — e.g. a corpus with no `code_refs`
    -- rows at all). NULL is the honest reading; 0 would be a lie that a
    -- later trend read could not tell apart from a real zero.
    value         REAL,
    -- The configured target from `[kb.<name>.slo]` at the time of the run, or
    -- NULL when none was configured. Stored ALONGSIDE the value rather than
    -- looked up at read time, because config changes: a row must stay
    -- interpretable against the target it was actually judged by.
    target        REAL,
    -- `ok` | `warn` | `unknown` — `kb_core::slo::SloStatus::as_str`. Derived
    -- from (value, target) at write time and stored for the same reason
    -- `target` is: so the log stays readable without re-deriving anything.
    -- There is no `fail`: the vocabulary is deliberately two-valued plus
    -- unknown, because nothing downstream is allowed to act on it.
    status        TEXT NOT NULL
);

-- The only read shape: newest-first, bounded (`kb slo log`). Matches
-- `idx_atlas_snapshots_created`'s (ts DESC, id DESC) tie-break so runs inside
-- the same second still page deterministically.
CREATE INDEX idx_slo_snapshots_taken ON slo_snapshots(taken_at_unix DESC, id DESC);

-- ---------------------------------------------------------------------------
-- 2. The recall-parse census — the ONE new write this milestone adds.
-- ---------------------------------------------------------------------------
--
-- CT-A3 introduced `DerivedRecalls`'s three-way parse census (`marker_parsed`
-- / `fallback_parsed` / `failed`, `kb_core::sessions::view`) and the
-- `memory-recall-ledger` enrichment hook already computes it on EVERY capture
-- — but it only ever ndjson-LOGGED it. A log line is greppable, not
-- queryable: there is no way to read "what fraction of injected recall hits
-- parsed via neither grammar, corpus-wide" without replaying logs. That makes
-- the indicator unmeasurable without a new write, so this migration persists
-- the per-capture totals additively.
--
-- WHY `sessions` AND NOT A NEW TABLE: a census row IS a per-capture fact, and
-- `sessions` is exactly the table with one row per capture (`artifact_id`
-- PRIMARY KEY, invariant #11's multi-capture shape). A sibling table would
-- duplicate that key, and — being artifact-id-keyed — would owe all three
-- id-lifecycle registrations; `sessions` already has them (`CASCADE_STEPS`
-- carries it, and the relocate tx rekeys it), so folding the columns in here
-- inherits a correct lifecycle instead of re-deriving one.
--
-- WHY NULLABLE, DEFAULT NULL: NULL means "this capture predates the census"
-- (or its ledger hook never ran); 0 means "this capture was measured and had
-- zero". Those must stay distinguishable or the failure RATE silently
-- averages a pre-census corpus toward zero and the indicator lies about a
-- healthy system. Every read filters on `marker_parsed IS NOT NULL`, and an
-- empty result is reported as `unknown`, never as 0%.
--
-- NO BACKFILL. Like `atlas_snapshots`, the census starts EMPTY and fills as
-- captures land — the counters are derived from a transcript walk the hook
-- does at capture time, and re-deriving them for the whole corpus would mean
-- a full reindex whose only product is this metric. Not worth a reindex; the
-- indicator honestly reads `unknown` until the first post-migration capture.
--
-- WRITTEN BY: `MemoryRecallLedgerHook` (`enrich.rs`), via
-- `sessions_set_recall_census` — a plain UPDATE, deliberately NOT folded into
-- `sessions_upsert`'s column list. The hook runs immediately AFTER
-- `SessionCaptureHook` in `default_hooks()`'s sequential order, so the row
-- exists by then; and because `sessions_upsert`'s ON CONFLICT branch never
-- names these columns, a later re-capture of the same artifact cannot clobber
-- them. An UPDATE that matches no row (a capture whose session row failed to
-- write) is a silent no-op that simply leaves the census NULL — the honest
-- reading.
--
-- NEVER SCORED. Provenance cannot reach the scorer (invariant #10's CT
-- amendment): no column below is wired onto `RecallHit`/`Scored` or read by
-- `kb_core::memory`'s ranking. It feeds one display surface and one CLI verb.
ALTER TABLE sessions ADD COLUMN recall_marker_parsed INTEGER;
ALTER TABLE sessions ADD COLUMN recall_fallback_parsed INTEGER;
ALTER TABLE sessions ADD COLUMN recall_failed INTEGER;
