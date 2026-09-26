-- V0045 (RS-U1, kb-code review store — README.md / BUILD-BRIEF.md §2 U1) —
-- the data model for the kb-owned internal git store (D1/D2/D3) and the
-- base-tracking model (D14/D15) that replaces a single pinned
-- `reviews.base_ref` commit with a resolved POLICY. Ships together with
-- the base-semantics fix as ONE gated epoch (README §5.5, §14 Phase 1,
-- D6) — `backup::GATED_EPOCHS` now names 40 (the Workspace re-key) AND 45
-- (this migration), so a volume crossing EITHER takes the automatic
-- pre-migration `index.db.pre-V<epoch>.bak` snapshot before the runner
-- touches a row (`backup::ensure_for_epoch_crossing`, called from
-- `Store::open` BEFORE the refinery runner — see `backup.rs`). An older
-- binary (which still only knows epoch 44) refuses to boot against a
-- volume this migration touched, automatically, via the existing
-- `kb_core::sibling::refuse_if_volume_ahead` guard: `store::schema_epoch()`
-- reads the highest EMBEDDED migration version, so landing this file IS
-- the epoch bump — no separate constant to change anywhere else.
--
-- Everything below is additive: two brand-new, empty tables, and
-- `ALTER TABLE ... ADD COLUMN` on `reviews`/`review_patchsets` with either
-- no default (NULL for every existing row) or a constant DEFAULT — O(1)
-- DDL, no backfill, no whole-table scan, matching this crate's own house
-- rule (V0040's re-key epoch comment, V0044's `mtime` comment): a schema
-- epoch that needs a real data backfill runs it as a PAGED BACKGROUND pass
-- AFTER the bind, never inside `Store::open`. This migration needs none.
--
-- ── review_stores / repo_stores (README §5.1, §5.5; D2/D3) ────────────
--
-- One `review_stores` row per FORGE PROJECT (`store_key` = normalized
-- `host/owner/name`, or `local:<uuid>` for a repo with no remote), never
-- per clone — D2's "one shared store per forge project, from the start".
-- `repo_stores` is the membership join: `repo_id` is the PRIMARY KEY (a
-- repo belongs to exactly one store) but `store_id` is deliberately NOT
-- UNIQUE, so many `[[repos]]` entries (rails-01..05, five clones of one
-- GitHub project) can share one `review_stores` row — this is the
-- README §5.5 amendment over `design-internal-store.md`'s older 1:1
-- sketch (`store_id INTEGER NOT NULL UNIQUE`); per BUILDER-RULES, README
-- is authoritative and wins here.
--
-- `store_key` carries its own UNIQUE index (not present in the older
-- sketch, which predates the shared-store amendment): the registration
-- ladder (README §5.1) JOINS an existing store when a repo's normalized
-- remote URL already has one, and mints a new row only when none
-- matches — that "join or create" contract needs the DB itself to refuse
-- two rows for one project, or two boots could each mint a separate
-- store for the same project under a race.
--
-- `git_dir` is the concrete path used at creation (`<root>/<uuid>.git`),
-- stored rather than re-derived from `[review.store] root` at read time:
-- `root` is user config and can change later; the store's own directory
-- must not silently follow it.
--
-- `state` is the seeding lifecycle (README §5.2 step 5): absent (row
-- exists, nothing seeded yet) -> seeding (background job running,
-- mutations get 503 `store-seeding`) -> ready (every ref importable from
-- every member has been verified), or broken. CHECK-constrained: like
-- `mutations.admission` (V0027) and `rails_edges.trust` (V0026), this is
-- a closed set tied to exactly the states the seeding state machine (U3)
-- drives through, not a business-level label a later phase grows an arm
-- on.
--
-- `forge_verified` is D8's release-gate flag: GitHub is 'verified' (the
-- live forge check, README §14 Phase 1 gate (e)); every other forge ships
-- 'unverified' until the Phase-2 checklist runs. A structurally closed
-- pair, CHECK-constrained for the same reason as `state`.
--
-- `cred_kind`, `forge_kind`, `base_url_source` and `key_read_only` are
-- DELIBERATELY plain TEXT with NO CHECK, route/Rust-validated instead —
-- this crate's established convention for an enum a LATER phase grows an
-- arm on (see `commit_sessions.via`'s comment, V0005: "so a future arm
-- never needs a migration to add its own label"). `cred_kind` gains
-- `deploy-key`/`token_file` in Phase 2 (README §8; out of scope for U1
-- per BUILD-BRIEF §1); `forge_kind` already names GitLab/Gitea/Forgejo/
-- Bitbucket Server today but only GitHub is verified (D8); `base_url_source`
-- is an 8-rung ladder (README §5.1) of exactly the kind a later rung gets
-- added to.
--
-- ── reviews / review_patchsets (README §5.5; the base model) ──────────
--
-- `base_mode` (track|local|pin), `base_branch`, `base_member` (a
-- `repos.id`, for `local`), `base_set_by` (auto|user|legacy) and
-- `base_status` (JSON: state/source/last_fetch) together replace the
-- single pinned `base_ref` commit that caused kb-code review 65's
-- 40-vs-10-commit bug (README §1). `objects_state` is the per-review
-- connectivity verdict from store seeding (README §5.2 step 4: NULL=ok |
-- objects-missing | legacy-unverified).
--
-- NOT CHECK-constrained, matching `reviews`' OWN existing convention:
-- `verdict` (V0023) and `state` (V0014) are both small closed-looking
-- enums on this exact table and NEITHER carries a CHECK — a review is
-- route-validated end to end on this table, so these six follow the
-- table they are joining rather than the CHECK judgement made above for
-- the two brand-new tables.
--
-- `base_member`/`reviews.worktree_id` (V0040) both name a foreign id
-- without an inline SQL `REFERENCES` on the `ALTER TABLE` — V0040 set
-- that precedent (`ALTER TABLE reviews ADD COLUMN worktree_id TEXT;`,
-- no REFERENCES even though it names a `workspaces.id`) and this
-- migration follows it rather than being the first `ALTER TABLE ADD
-- COLUMN ... REFERENCES` in the crate.
--
-- LEGACY ROWS (every review that exists before this migration runs):
-- `base_set_by` defaults to the literal `'legacy'` via a constant
-- `DEFAULT` (O(1), no backfill UPDATE — V0044's own `mtime` precedent) —
-- 'legacy' is a REAL semantic value (README §3: "a row from before the
-- upgrade... keeps its old behaviour until someone runs retrack"), not a
-- placeholder, so a legacy row is honestly self-describing the moment
-- this migration lands, before any consumer code (U6+) ever reads it.
-- `base_mode`/`base_branch`/`base_member` stay NULL for these rows on
-- purpose: README §10 step 3 is explicit that legacy rows are
-- "classified on read and never rewritten silently" (a `refs/remotes/R/B`
-- shaped `base_ref` becomes `track(B)`, a bare `main` becomes
-- `track(main)`, a pinned sha stays pinned) — that classification is a
-- job for the consumer reading `reviews.base_ref` at request time (U6),
-- never something this migration should guess and freeze into a column.
-- `review_patchsets.base_tip_sha`/`kind` stay NULL for every existing
-- patchset for the same reason: no patchset captured before this
-- migration ever recorded the base branch's tip, and no reason code
-- (`kind`) was ever computed for it — NULL means "legacy patchset,
-- predates the base model", never a guess.
--
-- G2 (verify/store.md): the store is found via `repos.id`, but a
-- review's own `repo` column is a NAME (TEXT), not `repos.id` — chosen in
-- `store/reviews.rs` so "reviews outlive a store re-register". Resolving
-- a review to ITS store is therefore a two-hop join,
-- `reviews.repo -> repos.name -> repos.id -> repo_stores.store_id`,
-- exactly the shape V0040's own workspace-rekey triggers already had to
-- solve for `reviews.worktree_id` (`WHERE name = NEW.repo`). See
-- `Store::store_for_repo_name` in `store/review_stores.rs`, which does
-- this two-hop lookup as one query rather than leaving every future
-- caller to reinvent it.

CREATE TABLE review_stores (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid             TEXT    NOT NULL UNIQUE,       -- <root>/<uuid>.git dir name + manifest
    store_key        TEXT    NOT NULL,              -- normalized host/owner/name, or 'local:<uuid>'
    git_dir          TEXT    NOT NULL,
    base_url         TEXT,                          -- NULL for a local:-only store
    base_url_source  TEXT,                          -- explicit|config|pr-slug|gh-resolved|single|origin|upstream-verified|guessed
    forge_kind       TEXT,                          -- github|gitlab|gitea|forgejo|bitbucket-server|none
    forge_host       TEXT,
    forge_slug       TEXT,
    forge_verified   TEXT    NOT NULL DEFAULT 'unverified'
                             CHECK (forge_verified IN ('verified', 'unverified')),  -- D8
    cred_kind        TEXT    NOT NULL DEFAULT 'inherit',  -- gh-cli|deploy-key|token_file|anonymous|inherit|none
    cred_reason      TEXT,
    cred_account     TEXT,                          -- D12: the pinned gh_user this store's fetch answers as
    key_fingerprint  TEXT,
    key_read_only    TEXT,                          -- verified-api@ts|confirmed@ts|unverified|write-enabled
    state            TEXT    NOT NULL DEFAULT 'absent'
                             CHECK (state IN ('absent', 'seeding', 'ready', 'broken')),
    state_json       TEXT,                          -- {code,hint,last_base_fetch,last_work_fetch,last_maint,fallback_hits}
    created_at       INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_review_stores_key ON review_stores(store_key);

CREATE TABLE repo_stores (
    repo_id             INTEGER PRIMARY KEY REFERENCES repos(id),
    store_id            INTEGER NOT NULL REFERENCES review_stores(id),  -- NOT UNIQUE: many members, one store (D2)
    legacy_import_json  TEXT,                        -- {at, imported, conflicts[], missing_reviews[]}
    legacy_refs_state   TEXT NOT NULL DEFAULT 'present'
                             CHECK (legacy_refs_state IN ('present', 'cleaned', 'none'))
);
CREATE INDEX idx_repo_stores_store ON repo_stores(store_id);

ALTER TABLE reviews ADD COLUMN base_mode     TEXT;    -- track|local|pin; NULL = legacy (derive from base_ref)
ALTER TABLE reviews ADD COLUMN base_branch   TEXT;    -- branch name for track()/local(); NULL for pin() or legacy
ALTER TABLE reviews ADD COLUMN base_member   INTEGER; -- the repo id for local(); NULL otherwise (no inline REFERENCES — see the V0040 precedent note above)
ALTER TABLE reviews ADD COLUMN base_set_by   TEXT NOT NULL DEFAULT 'legacy';  -- auto|user|legacy
ALTER TABLE reviews ADD COLUMN base_status   TEXT;    -- JSON: {state, source, last_fetch}
ALTER TABLE reviews ADD COLUMN objects_state TEXT;    -- NULL=ok | objects-missing | legacy-unverified

ALTER TABLE review_patchsets ADD COLUMN base_tip_sha TEXT;  -- tip of the base branch at capture; NULL = legacy patchset
ALTER TABLE review_patchsets ADD COLUMN kind         TEXT;  -- push|rebase|base-moved|base-corrected|retarget; NULL = legacy
