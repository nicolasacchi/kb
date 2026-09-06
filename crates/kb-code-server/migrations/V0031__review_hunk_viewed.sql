-- V73-K2a — per-HUNK viewed state for the review diff (design §D9:
-- "content-addressed per-hunk reviewed state that survives rebases").
--
-- `review_viewed` (V0014) answers "has this FILE been read at this blob",
-- keyed `(review_id, path)` and staled by a blob_sha comparison. That is
-- the right question for a file list and the wrong one for a 900-line
-- diff, where a reviewer finishes six of nine hunks and comes back
-- tomorrow. This table answers the hunk-sized question beside it; neither
-- replaces the other, and `GET /reviews/{id}/files` reports both.
--
-- `hunk_id` is an OPAQUE content address minted by the client
-- (`web-code/src/lib/diffHunks.ts`, schema `kbc-hunkid/1`: a 64-bit FNV-1a
-- over the file path plus the hunk's own `+`/`-` lines, deliberately
-- excluding line numbers and context rows so a rebase does not rename it).
-- The daemon does not parse diffs and therefore does not mint these — it
-- stores a set. That split is the point: the addressing scheme can change
-- (a `kbc-hunkid/2`) without a migration, and an id minted under an older
-- scheme simply stops matching — a viewed hunk reads UNVIEWED, never the
-- reverse, which is the safe direction for a "have I read this" signal.
--
-- The PK is `(review_id, hunk_id)`, not `(review_id, path, hunk_id)`:
-- the path is already inside the hash, so a second key column would be
-- redundant and could disagree with it. `path` is kept as a plain column
-- so a row can be attributed to a file in a query (and so a future
-- per-file purge is one `DELETE ... WHERE path = ?`) without becoming
-- part of the identity.

CREATE TABLE review_hunk_viewed (
    review_id  INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    hunk_id    TEXT NOT NULL,
    path       TEXT NOT NULL,
    viewed_at  INTEGER NOT NULL,
    PRIMARY KEY (review_id, hunk_id)
);

CREATE INDEX idx_review_hunk_viewed_path ON review_hunk_viewed(review_id, path);
