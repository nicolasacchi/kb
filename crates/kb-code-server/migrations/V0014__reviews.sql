-- V3.R1 ("Review cockpit") — local Gerrit-lite review sessions.
-- A review is a named base..head pair with ordered patchset snapshots
-- under refs/kbc/review/<id>/ps<n>, viewed-file tracking (Reviewable-style
-- blob_sha staleness), and open-annotation counts on the change set.
-- Single operator, no approvals/merge/notifications. NOT a PR host.
--
-- (Authored as V0015 with V0014 reserved for a parallel import-graph lane;
-- renumbered to V0014 at harvest because this lane landed FIRST — no DB had
-- applied it yet, and refinery filename order must match landing order.)
-- Tables are additive; foreign keys cascade on review delete.

CREATE TABLE reviews (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    repo        TEXT NOT NULL,
    title       TEXT,
    base_ref    TEXT NOT NULL,
    head_ref    TEXT NOT NULL,
    session_id  TEXT,
    state       TEXT NOT NULL DEFAULT 'open',  -- open | closed
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX idx_reviews_repo_state ON reviews(repo, state);

CREATE TABLE review_patchsets (
    id           INTEGER PRIMARY KEY,
    review_id    INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    ps_number    INTEGER NOT NULL,
    tip_sha      TEXT NOT NULL,
    base_sha     TEXT NOT NULL,  -- merge-base(base_ref, tip) at capture
    captured_at  INTEGER NOT NULL,
    UNIQUE(review_id, ps_number)
);
CREATE INDEX idx_review_patchsets_review ON review_patchsets(review_id);

CREATE TABLE review_viewed (
    review_id   INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    blob_sha    TEXT NOT NULL,
    viewed_at   INTEGER NOT NULL,
    PRIMARY KEY (review_id, path)
);
