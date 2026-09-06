-- W2.3 semantic lane — a cheap "already chunked+embedded" cache-hit check
-- for the background semantic indexer (ADR-2's "never re-embed a seen
-- blob" win, applied to the embed pass, which is far more expensive than
-- the tree-sitter parse `symbols`/`highlights` already cache this way). The
-- real chunk TEXT + VECTOR data lives in kb-code's OWN `chunk_vectors` lance
-- table (`semantic::store::ChunkStore`, `<state>/kb-code/lance/`), not
-- here — this table is bookkeeping only, so the indexer can decide "skip
-- this blob" with a single indexed sqlite lookup instead of an async lance
-- existence query on every candidate blob.
--
-- Kept in lockstep with the lance side by the indexer: every
-- `chunk_vectors` write for a blob is paired with an insert/update here
-- (`Store::mark_chunked`), and every `chunk_vectors` delete for an orphaned
-- blob is paired with a delete here (`Store::clear_chunk_status_for_blob`)
-- — if this table ever says "chunked" for a blob the lance table has since
-- dropped, the next repo scan would wrongly skip re-embedding it, so the
-- two writes must always travel together (see `semantic::indexer`'s doc).
CREATE TABLE chunk_status (
    blob_hash    TEXT NOT NULL,
    salt         TEXT NOT NULL,
    chunk_count  INTEGER NOT NULL,
    PRIMARY KEY (blob_hash, salt)
);
