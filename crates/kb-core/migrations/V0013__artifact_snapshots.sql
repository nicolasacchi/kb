-- Track V — per-artifact content snapshots for the Versions/Diff timeline.
-- One row per *distinct* indexed revision of an artifact's source: when the
-- indexer re-indexes a file whose raw-bytes content_hash differs from the
-- latest stored snapshot, it appends a row (SnapshotCaptureHook in
-- kb_core::enrich), then prunes to the newest DEFAULT_SNAPSHOT_KEEP per
-- artifact. This gives the version timeline history even when the corpus
-- isn't under git; git-backed corpora in `auto` mode prefer git history and
-- fall back to these only when a file is untracked.
--
-- raw_source is the file's verbatim text (HTML, or the .md for markdown) so
-- both the prose diff (re-extracted via parser::text_blocks at diff time,
-- matching the git path) and the raw-HTML toggle can be served from it.
-- Memory-session transcripts are deliberately NOT snapshotted (storing N
-- copies of a multi-MB transcript is the real growth risk — the capture hook
-- skips kb-category="memory-session").
--
-- artifact_id is the path-based id; renaming a file changes the id, so
-- pre-rename snapshots live under the old id (git --follow still crosses the
-- rename). Removed wholesale by the indexer's delete pass when the file is
-- unlinked. This is enrichment — the lance row stays the source of truth for
-- the artifact's current state.

CREATE TABLE artifact_snapshots (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id   TEXT NOT NULL,
    content_hash  TEXT NOT NULL,   -- 12-hex raw-bytes hash; the dedup key
    raw_source    TEXT NOT NULL,   -- verbatim source text at this revision
    captured_at   INTEGER NOT NULL -- unix epoch seconds (mtime at index time)
);
CREATE INDEX idx_snapshots_artifact
    ON artifact_snapshots(artifact_id, captured_at DESC, id DESC);
