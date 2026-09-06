-- kb share registry. One row per active static share (Cloudflare Pages +
-- Access, or GitHub Pages). Ground truth behind `kb share list`,
-- `kb share --update` (find-by-target → re-deploy to the SAME project so
-- the already-sent link stays stable), and `kb share revoke` (undo exactly
-- what was created). Insert-on-create, upsert-on-redeploy, delete-on-revoke.
--
-- Host-specific columns are nullable: a GitHub Pages share leaves the
-- cf_* / pages_project / access_* columns NULL and sets github_repo, and
-- vice-versa. `gate` is NULL for a public (ungated) share.

CREATE TABLE shares (
    name             TEXT PRIMARY KEY,        -- kb-share-<slug>-<6hex>; also the pages project / repo basename
    target           TEXT NOT NULL,           -- source-relative path / folder / artifact id that was shared
    host             TEXT NOT NULL CHECK (host IN ('cloudflare-pages','github-pages')),
    deployed_url     TEXT NOT NULL,
    gate             TEXT,                     -- gate spec (email:DOMAIN | email:a,b | google | github); NULL = public
    cf_account_id    TEXT,                     -- cloudflare: account the project + Access app live in
    pages_project    TEXT,                     -- cloudflare: <project>.pages.dev project name
    cf_deployment_id TEXT,                     -- cloudflare: last deployment id (for --update reconciliation)
    access_app_id    TEXT,                     -- cloudflare: self-hosted Access app id (revoke deletes it)
    access_policy_id TEXT,                     -- cloudflare: allow-policy id
    github_repo      TEXT,                     -- github: owner/repo (revoke deletes the repo)
    created_at       INTEGER NOT NULL,         -- unix epoch seconds
    updated_at       INTEGER NOT NULL          -- unix epoch seconds (= created_at unless --update bumped)
);
CREATE INDEX idx_shares_target ON shares(target);
CREATE INDEX idx_shares_created ON shares(created_at DESC);
