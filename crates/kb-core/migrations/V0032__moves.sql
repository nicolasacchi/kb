-- F3a — artifact relocate intent log. Append-only audit of id migrations
-- when a file is moved/renamed inside a corpus. Artifact ids are path-
-- derived (invariant #27), so a rename is a new id; this table records the
-- old→new mapping BEFORE any mutation (intent) and stamps completed_at when
-- the storage-side rekey finishes. Incomplete or recent rows suppress the
-- watcher's delete-cascade for the old path (crash-safety net + race guard
-- against the Debounced Deleted+Created pair the FS rename synthesizes).

CREATE TABLE moves (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    old_id        TEXT NOT NULL,       -- 12-hex ArtifactId before the move
    new_id        TEXT NOT NULL,       -- 12-hex ArtifactId after the move
    old_rel       TEXT NOT NULL,       -- source-relative path before
    new_rel       TEXT NOT NULL,       -- source-relative path after
    moved_at      INTEGER NOT NULL,    -- unix epoch seconds (intent write)
    completed_at  INTEGER              -- NULL while in flight; set on success
);
CREATE INDEX idx_moves_old_id  ON moves(old_id);
CREATE INDEX idx_moves_old_rel ON moves(old_rel);
