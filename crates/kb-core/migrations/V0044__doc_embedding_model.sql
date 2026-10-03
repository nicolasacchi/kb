-- v0.44 I1 (A3-7) — the producer model of each artifact's stored vector.
--
-- The embed-reuse decision (`try_preserve_embeddings`) must refuse to copy a
-- stored vector that a DIFFERENT model produced. Lance carries no model
-- column, so the name used to live in a per-kb JSON sidecar that was
-- rewritten whole (write + rename + dir fsync) once per newly recorded doc,
-- under a process-global mutex: O(N^2) I/O on the first population of a
-- large corpus, and the map was never pruned on delete or rekeyed on
-- relocate.
--
-- One row per artifact, written by the same storage actor that owns every
-- other sqlite table. Artifact-id keyed, so it is registered in all three
-- lifecycle registries (CASCADE_STEPS + SWEEP_TABLES + the
-- cascade_relocate_doc rekey; invariant #2). The legacy sidecar is imported
-- once (INSERT OR IGNORE) and renamed aside.

CREATE TABLE doc_embedding_model (
    artifact_id TEXT PRIMARY KEY,
    model       TEXT NOT NULL
);
