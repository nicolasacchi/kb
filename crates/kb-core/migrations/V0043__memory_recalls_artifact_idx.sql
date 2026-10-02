-- memory_recalls is filtered by artifact_id on every hot write path:
-- memory_recalls_replace (delete-then-insert per capture, i.e. every
-- Stop-hook capture), the cascade delete, the relocate UPDATE and the
-- reconcile sweep. Live-serve rows (V0042) are appended on every recall and
-- only age out under the opt-in [retention] window, so the table grows
-- without bound by default and each of those statements full-scanned it.
-- Index-only; no data change.
CREATE INDEX IF NOT EXISTS idx_memory_recalls_artifact_id
    ON memory_recalls(artifact_id);
