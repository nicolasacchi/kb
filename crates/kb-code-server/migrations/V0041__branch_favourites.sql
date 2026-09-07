-- V75-M3 (design D15) — starred branches for `branch-facts/1`'s `?fav=`
-- filter and the `favourite` reason chip.
--
-- Posture, stated because D16 rules that v7.0 preferences stay
-- BROWSER-LOCAL: this one is daemon-side because it is a REPO fact the CLI
-- must see too (`kb-code branch facts --fav` and the SPA's star are the
-- same star, and a browser-local star would be invisible to the agent half
-- of the loop). It is daemon-GLOBAL and never per-identity — kb-code has
-- ONE identity (the v0.34 ruling), so there is deliberately no user
-- column, exactly as `bookmarks` (V0011) and `doc_lens_pins` (V0020) have
-- none.
--
-- `repo` is the configured repo NAME as plain TEXT with no foreign key —
-- the bookmarks/doc-lens-pins precedent: an operator preference must
-- survive a repo being re-registered.
--
-- `ref_name` is the FULL ref (`refs/heads/x`, `refs/remotes/origin/x`), not
-- the short name: a local branch and a remote of the same short name are
-- two different things to star, and the facts route already keys rows by
-- their full ref.
CREATE TABLE branch_favourites (
    repo       TEXT NOT NULL,
    ref_name   TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (repo, ref_name)
);

-- `store::Store::list_branch_favourites`'s by-repo lookup. The PK above
-- already covers `(repo, ref_name)` prefix lookups, but SQLite will not use
-- a WITHOUT ROWID-less table's PK index for an ORDER BY on `created_at`;
-- this keeps the listing a single index scan.
CREATE INDEX idx_branch_favourites_repo ON branch_favourites(repo, created_at);

-- V75-M1's re-key classification, applied to this table at CREATE time
-- rather than by a later ALTER: `rekey::REPO_KEYED_TABLES` must classify
-- every repo-keyed table, and `rekey::tests::every_repo_keyed_table_is_
-- classified` fails the build by name if one does not.
--
-- OBJECT class. A branch is a ref, and refs live in the common dir that
-- every worktree of one workspace shares — two checkouts of the same
-- object store see the SAME `refs/heads/feature/x`, so starring it in one
-- and not the other would be a wrong answer, not a saving. (Contrast
-- `bookmarks`, which is worktree-class because a bookmark anchors a PATH
-- ON DISK at a line.)
--
-- The value comes from the TRIGGER, not from Rust — V0040's rule, and the
-- reason a future INSERT cannot leave the key NULL. `repo` here is the
-- repo NAME (the `bookmarks`/`reviews` shape), so the lookup is by name.
ALTER TABLE branch_favourites ADD COLUMN workspace_id TEXT;
CREATE INDEX idx_branch_favourites_ws ON branch_favourites(workspace_id)
    WHERE workspace_id IS NOT NULL;
CREATE TRIGGER trg_branch_favourites_ws AFTER INSERT ON branch_favourites
WHEN NEW.workspace_id IS NULL
BEGIN
    UPDATE branch_favourites SET workspace_id = (SELECT workspace_id FROM repos WHERE name = NEW.repo)
    WHERE rowid = NEW.rowid;
END;
