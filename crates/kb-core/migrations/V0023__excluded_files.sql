-- X2 (v0.24) — per-file exclusion store. One row per operator-excluded file,
-- keyed by the SOURCE-RELATIVE path (forward-slash form — the same string
-- `paths::doc_rel_path` derives and the artifact id is hashed from). Follows
-- the `sources.paused` schema shape (durable operator intent in sqlite), NOT
-- the quarantine pattern (which piggybacks on the transient `errors` table and
-- is failure-driven).
--
-- Enforcement reads this via the storage actor at daemon bring-up into the
-- in-memory `exclusions::IngestGate`; the three ingest seams (walk_core,
-- watcher emit paths, top of `prepare_doc`) consult the gate, never sqlite
-- directly.
CREATE TABLE excluded_files (
    path        TEXT PRIMARY KEY,       -- source-relative, forward-slash
    excluded_at INTEGER NOT NULL,       -- unix seconds
    note        TEXT                    -- optional operator note
);
