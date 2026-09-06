-- V4.C1 ("Review comments") — review-scoped annotations + later-phase
-- schema (suggestions, verdicts). Additive: every existing annotation
-- reads back `review_id`/`ps_number`/`side` = NULL and resolves exactly
-- as it did pre-migration (pinned by `store::tests::legacy_v1_shaped_
-- annotation_rows_read_back_as_line_note_and_resolve_identically` and
-- the annotations_route harness).
--
-- `review_id` is `reviews.id` with NO SQL foreign key — same parent_id
-- precedent (V0008): cascade-delete lives in Rust
-- (`store::Store::delete_review` / `delete_annotation`) because a plain
-- FK cannot express "also drop annotation_suggestions for this review's
-- annotations and their replies" as one store-owned transaction.
-- `side` is route-validated (`old` | `new`), never CHECK-constrained
-- (V0008 convention for stringly-typed vocabularies).
--
-- `annotation_suggestions` and `reviews.verdict*` ship in this
-- immutable migration so V4.C2 can add the write routes without a
-- second schema change. This phase only READs suggestions (never
-- upserts verdicts or suggestions).
ALTER TABLE annotations ADD COLUMN review_id INTEGER;
ALTER TABLE annotations ADD COLUMN ps_number INTEGER;
ALTER TABLE annotations ADD COLUMN side TEXT;
CREATE INDEX idx_annotations_review ON annotations(review_id, resolved) WHERE review_id IS NOT NULL;
CREATE TABLE annotation_suggestions (
    annotation_id    TEXT PRIMARY KEY,
    replacement      TEXT NOT NULL,
    original         TEXT NOT NULL,
    base_blob_sha    TEXT NOT NULL,
    applied          INTEGER NOT NULL DEFAULT 0,
    applied_at       INTEGER,
    applied_head_sha TEXT,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
);
ALTER TABLE reviews ADD COLUMN verdict TEXT;
ALTER TABLE reviews ADD COLUMN verdict_note TEXT;
ALTER TABLE reviews ADD COLUMN verdict_at INTEGER;
ALTER TABLE reviews ADD COLUMN verdict_ps INTEGER;
