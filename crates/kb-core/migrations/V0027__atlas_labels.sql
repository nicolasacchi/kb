-- W1.B — deterministic c-TF-IDF cluster labels, computed at atlas
-- recompute/recluster time (kb_core::atlas_labels, a pure BERTopic-style
-- module: score(t,c) = tf(t,c) * ln(1 + A/f(t))). One row per (cluster,
-- rank) — top 5 ranked terms per atlas cluster, replaced wholesale on every
-- recompute/recluster (see `Db::set_atlas_labels`, a DELETE-all + INSERT-all
-- transaction, mirroring `record_edges`'s replace-the-set shape).
--
-- Lives in sqlite (not the lance `atlas_cluster` column) because it's a
-- derived-from-corpus-text side table, not a per-doc row — same rationale
-- as `edges`. Writing it does NOT bump the storage-actor index generation
-- (invariant #15): it never touches the lance row-set, mirroring
-- `update_atlas` itself.
CREATE TABLE atlas_labels (
    cluster      INTEGER NOT NULL,  -- atlas_cluster id (matches lance i16)
    rank         INTEGER NOT NULL,  -- 1-based, score desc / term asc tiebreak
    term         TEXT NOT NULL,
    tf           REAL NOT NULL,     -- term count within this cluster
    ft           REAL NOT NULL,     -- term count across ALL clusters
    score        REAL NOT NULL,     -- tf * ln(1 + A/ft)
    computed_at  INTEGER NOT NULL,  -- unix seconds, caller-supplied (no clock in the deterministic compute path)
    PRIMARY KEY (cluster, rank)
);
