-- W3 T-a — atlas FRAMES: the corpus time-lapse (temporal cartographic
-- replay). One `atlas_snapshots` row per atlas recompute/recluster whose
-- coordinates actually CHANGED, plus one `atlas_snapshot_points` row per
-- artifact in that frame. Replaying the frames newest→oldest shows how the
-- corpus map drifted as documents were added, edited, and removed.
--
-- Lives in sqlite, NOT as lance columns — the same argument V0027 makes for
-- `atlas_labels`, only sharper here. The lance `atlas_x`/`atlas_y`/
-- `atlas_cluster` columns are ONE column set and therefore hold exactly ONE
-- layout: the current one. N retained frames as lance columns would mean N
-- column triples and a schema migration per frame, which is absurd; and a
-- frame is a derived, disposable, whole-corpus side artifact (like `edges`
-- and `atlas_labels`), not per-doc truth. Writing either table must NOT bump
-- the storage-actor index generation (root invariant #15) — neither touches
-- the lance row-set, exactly like `update_atlas` itself.
--
-- FRAMES START EMPTY. Nothing in kb retains a past layout or a past
-- embedding, so a historical backfill is IMPOSSIBLE: the time-lapse ships
-- blind and only becomes watchable as recomputes accumulate going forward.
--
-- CLUSTER IDS RENUMBER BETWEEN FRAMES. `atlas::kmeans_lloyd` seeds centroids
-- by array POSITION and reseeds empty clusters randomly, so "cluster 3" in
-- frame N and "cluster 3" in frame N+1 are unrelated labels. Any consumer
-- must remap colours per frame (e.g. via the frame's `atlas_labels`-style
-- terms or a centroid match), never trust cluster id continuity.

CREATE TABLE atlas_snapshots (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at_unix  INTEGER NOT NULL,  -- caller-supplied (no clock in the deterministic layout path)
    point_count      INTEGER NOT NULL,  -- rows in atlas_snapshot_points for this frame
    cluster_count    INTEGER NOT NULL,  -- distinct cluster ids in this frame
    layout           TEXT NOT NULL,     -- algorithm label: 'umap' | 'pca' | 'recluster'
    coord_hash       TEXT NOT NULL,     -- sha256 hex over the id-sorted (id, x bits, y bits, cluster) tuples; the dedup key
    provenance       TEXT NOT NULL      -- 'recorded' (written by the recompute that produced it) | 'reconstructed'
);
CREATE INDEX idx_atlas_snapshots_created
    ON atlas_snapshots(created_at_unix DESC, id DESC);

CREATE TABLE atlas_snapshot_points (
    snapshot_id  INTEGER NOT NULL REFERENCES atlas_snapshots(id) ON DELETE CASCADE,
    artifact_id  TEXT NOT NULL,
    x            REAL NOT NULL,     -- normalised [0,1] canvas coord (f32 widened)
    y            REAL NOT NULL,
    cluster      INTEGER NOT NULL,  -- matches the lance atlas_cluster i16 — NOT stable across frames
    PRIMARY KEY (snapshot_id, artifact_id)
);
