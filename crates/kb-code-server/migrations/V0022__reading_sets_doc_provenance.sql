-- DCB W3.C ("kb-code v2 — the doc-code bridge") — nullable doc-materialization
-- provenance on `reading_sets`. NULL on every set created via any OTHER path
-- (`create_set`, `from_session_route`) — populated only by
-- `POST /api/sets/from-doc` (`reading_sets::from_doc_route`).
--
-- `source_doc_hash` is the ONLY field `SetDetail.tsx`'s "doc changed since"
-- banner compares against a freshly re-fetched `codelens/1`'s own `doc_hash`
-- (`hooks/useDocLens.ts`'s `useDocLens` — reused as-is, no new route) —
-- repo-side drift (a rename with no doc edit) is a DIFFERENT staleness this
-- column deliberately does not track (same "kb-side change is the only
-- signal this milestone tracks" decision `doclens/sync.rs`'s module doc
-- already applies to `doc_refs`).
ALTER TABLE reading_sets ADD COLUMN source_kb TEXT;
ALTER TABLE reading_sets ADD COLUMN source_doc_id TEXT;
ALTER TABLE reading_sets ADD COLUMN source_doc_path TEXT;
ALTER TABLE reading_sets ADD COLUMN source_doc_hash TEXT;
