//! The kb-owned internal git store (README.md §5, D1-D3) and the
//! base-tracking columns on `reviews`/`review_patchsets` (V0045 / RS-U1).
//!
//! Moved out of the store monolith. `Store`'s private connection and the
//! helpers it shares stay in the parent module; this child can call them.
//! Public paths stay `crate::store`. Row types (`ReviewStoreRow`,
//! `RepoStoreRow`, `ReviewBaseRow`, `PatchsetBaseFields`) live in
//! `store/mod.rs`, per this crate's "methods in the child, types in the
//! parent" convention (see `WorkspaceRow`/`store/workspace.rs`).
//!
//! This unit (RS-U1) only lays the data model and its CRUD surface. It does
//! NOT wire a store into review creation or patchset capture — that is a
//! later unit's job (README BUILD-BRIEF.md U3/U6). Every method here is
//! therefore additive: `reviews::create_review`/`Store::insert_patchset`
//! (the pre-existing, still-used entry points) are untouched, and
//! `insert_patchset_with_base` is a NEW function beside `insert_patchset`,
//! not a replacement — legacy callers keep compiling unchanged.
use super::*;

impl Store {
    // --- review_stores (README §5.1/§5.5; D1-D3) --------------------------

    /// Insert a new `review_stores` row and return its id. `cred_kind`,
    /// `forge_verified` and `state` are left to their column DEFAULTs
    /// (`'inherit'`, `'unverified'`, `'absent'`) — a fresh store starts
    /// with nothing verified and nothing seeded; later units set those
    /// through [`Self::set_review_store_state`]/
    /// [`Self::set_review_store_forge`]/[`Self::set_review_store_credential`]
    /// once the seeding job and the credential ladder actually run.
    pub fn create_review_store(
        &self,
        uuid: &str,
        store_key: &str,
        git_dir: &str,
        base_url: Option<&str>,
        base_url_source: Option<&str>,
        now: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO review_stores (uuid, store_key, git_dir, base_url, base_url_source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![uuid, store_key, git_dir, base_url, base_url_source, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_review_store(&self, id: i64) -> Result<Option<ReviewStoreRow>> {
        self.lock()
            .query_row(
                "SELECT id, uuid, store_key, git_dir, base_url, base_url_source, forge_kind,
                        forge_host, forge_slug, forge_verified, cred_kind, cred_reason,
                        cred_account, key_fingerprint, key_read_only, state, state_json, created_at
                 FROM review_stores WHERE id = ?1",
                params![id],
                review_store_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Look up by the store's own directory identity (`<root>/<uuid>.git`,
    /// README §5.1) — the manifest-checked identity, not the DB row id.
    pub fn get_review_store_by_uuid(&self, uuid: &str) -> Result<Option<ReviewStoreRow>> {
        self.lock()
            .query_row(
                "SELECT id, uuid, store_key, git_dir, base_url, base_url_source, forge_kind,
                        forge_host, forge_slug, forge_verified, cred_kind, cred_reason,
                        cred_account, key_fingerprint, key_read_only, state, state_json, created_at
                 FROM review_stores WHERE uuid = ?1",
                params![uuid],
                review_store_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Look up by the normalized forge project (`host/owner/name`, or
    /// `local:<uuid>`) — the "join an existing store, or mint a new one"
    /// check the registration ladder (README §5.1) runs once per repo.
    /// `store_key` carries its own UNIQUE index (see the migration
    /// header), so this can never return more than one row.
    pub fn get_review_store_by_key(&self, store_key: &str) -> Result<Option<ReviewStoreRow>> {
        self.lock()
            .query_row(
                "SELECT id, uuid, store_key, git_dir, base_url, base_url_source, forge_kind,
                        forge_host, forge_slug, forge_verified, cred_kind, cred_reason,
                        cred_account, key_fingerprint, key_read_only, state, state_json, created_at
                 FROM review_stores WHERE store_key = ?1",
                params![store_key],
                review_store_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_review_stores(&self) -> Result<Vec<ReviewStoreRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, uuid, store_key, git_dir, base_url, base_url_source, forge_kind,
                    forge_host, forge_slug, forge_verified, cred_kind, cred_reason,
                    cred_account, key_fingerprint, key_read_only, state, state_json, created_at
             FROM review_stores ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map([], review_store_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Move a store through its seeding lifecycle (README §5.2 step 5:
    /// absent -> seeding -> ready, or broken). Returns `false` when `id`
    /// does not exist. `state` is CHECK-constrained at the schema level
    /// (see the migration header); an invalid value fails the write with a
    /// `rusqlite::Error`, not a silent no-op.
    pub fn set_review_store_state(
        &self,
        id: i64,
        state: &str,
        state_json: Option<&str>,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE review_stores SET state = ?2, state_json = ?3 WHERE id = ?1",
            params![id, state, state_json],
        )?;
        Ok(n > 0)
    }

    /// Record what the forge-detection step (README §5.1) found. `forge_kind`/
    /// `forge_host`/`forge_slug` are route-validated, never CHECK-constrained
    /// (see the migration header); `forge_verified` IS CHECK-constrained
    /// (`verified`/`unverified`, D8).
    pub fn set_review_store_forge(
        &self,
        id: i64,
        forge_kind: Option<&str>,
        forge_host: Option<&str>,
        forge_slug: Option<&str>,
        forge_verified: &str,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE review_stores
             SET forge_kind = ?2, forge_host = ?3, forge_slug = ?4, forge_verified = ?5
             WHERE id = ?1",
            params![id, forge_kind, forge_host, forge_slug, forge_verified],
        )?;
        Ok(n > 0)
    }

    /// Record which credential rung this store fetches with (README §8's
    /// ladder) and why. Never the secret itself — `cred_reason`/
    /// `cred_account` are bookkeeping (D12's pinned-account mismatch
    /// detection), not a place a token could land; that invariant is
    /// enforced by callers (U2's `StoreGit`), not by this method.
    pub fn set_review_store_credential(
        &self,
        id: i64,
        cred_kind: &str,
        cred_reason: Option<&str>,
        cred_account: Option<&str>,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE review_stores SET cred_kind = ?2, cred_reason = ?3, cred_account = ?4
             WHERE id = ?1",
            params![id, cred_kind, cred_reason, cred_account],
        )?;
        Ok(n > 0)
    }

    /// Delete a `review_stores` row. Phase 1 never calls this from a route
    /// (no store-deletion verb ships yet) — it exists for test fixtures and
    /// for a later unit's `store adopt`/cleanup paths. Does NOT cascade
    /// `repo_stores` (no `ON DELETE CASCADE` on `repo_stores.store_id`,
    /// deliberately: an orphaned membership row is a doctor-visible defect,
    /// never a silent one).
    pub fn delete_review_store(&self, id: i64) -> Result<bool> {
        let n = self
            .lock()
            .execute("DELETE FROM review_stores WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    // --- repo_stores: membership (README §5.1 "members"; D2) -------------

    /// Add or move `repo_id`'s membership to `store_id`. Idempotent and an
    /// upsert-by-repo (`repo_id` is the PRIMARY KEY, so a repo belongs to
    /// exactly one store): a second call for the same `repo_id` re-points
    /// it rather than erroring, which is what `store adopt`/a re-registration
    /// needs. Leaves `legacy_refs_state` at its column default (`'present'`)
    /// on first insert and UNTOUCHED on a re-point — moving a repo to a
    /// different store says nothing about whether ITS OWN legacy refs were
    /// ever cleaned.
    pub fn add_repo_to_store(&self, repo_id: i64, store_id: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO repo_stores (repo_id, store_id) VALUES (?1, ?2)
             ON CONFLICT(repo_id) DO UPDATE SET store_id = excluded.store_id",
            params![repo_id, store_id],
        )?;
        Ok(())
    }

    /// This repo's own membership row, if it has one.
    pub fn repo_store(&self, repo_id: i64) -> Result<Option<RepoStoreRow>> {
        self.lock()
            .query_row(
                "SELECT repo_id, store_id, legacy_import_json, legacy_refs_state
                 FROM repo_stores WHERE repo_id = ?1",
                params![repo_id],
                repo_store_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every member `repo_id` of `store_id`, ascending — the "which clones
    /// share this store" listing (README §5.1's rails-01..05 example).
    /// Deliberately NOT unique-constrained at the schema level (D2): this
    /// can return more than one row, which is the whole point of a shared
    /// store.
    pub fn store_members(&self, store_id: i64) -> Result<Vec<i64>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT repo_id FROM repo_stores WHERE store_id = ?1 ORDER BY repo_id")?;
        let rows = stmt
            .query_map(params![store_id], |r| r.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Remove a repo's membership (e.g. the repo was unregistered). Does
    /// NOT touch `refs/kbc/work-<repo_id>/*` in the store itself — that is
    /// the store-wide GC's job (README §5.4, a later unit), which this
    /// method deliberately does not reach into: a membership row and the
    /// refs it once owned are cleaned on two different schedules (a dry
    /// run gates the ref sweep; removing the DB row does not).
    pub fn remove_repo_from_store(&self, repo_id: i64) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM repo_stores WHERE repo_id = ?1",
            params![repo_id],
        )?;
        Ok(n > 0)
    }

    pub fn set_repo_store_legacy_import(
        &self,
        repo_id: i64,
        legacy_import_json: Option<&str>,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE repo_stores SET legacy_import_json = ?2 WHERE repo_id = ?1",
            params![repo_id, legacy_import_json],
        )?;
        Ok(n > 0)
    }

    pub fn set_repo_store_legacy_refs_state(&self, repo_id: i64, state: &str) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE repo_stores SET legacy_refs_state = ?2 WHERE repo_id = ?1",
            params![repo_id, state],
        )?;
        Ok(n > 0)
    }

    /// The store a review's repo belongs to, resolved by NAME — the G2
    /// two-hop join (`verify/store.md`): `reviews.repo` is a free-text repo
    /// name (`store/reviews.rs`'s own "reviews outlive a store
    /// re-register" rationale), never `repos.id` directly, so finding a
    /// review's store has to go name -> `repos.id` -> `repo_stores.store_id`
    /// -> `review_stores`. Exactly the join V0040's own workspace-rekey
    /// triggers already solved for `reviews.worktree_id`
    /// (`WHERE name = NEW.repo`) — this is that same shape as ONE query,
    /// rather than leaving every future caller (U3's `find_repo` growing
    /// into `find_review_store`, U6's capture path, …) to reinvent the
    /// two-hop by hand.
    pub fn store_for_repo_name(&self, repo_name: &str) -> Result<Option<ReviewStoreRow>> {
        self.lock()
            .query_row(
                "SELECT rs.id, rs.uuid, rs.store_key, rs.git_dir, rs.base_url, rs.base_url_source,
                        rs.forge_kind, rs.forge_host, rs.forge_slug, rs.forge_verified,
                        rs.cred_kind, rs.cred_reason, rs.cred_account, rs.key_fingerprint,
                        rs.key_read_only, rs.state, rs.state_json, rs.created_at
                 FROM review_stores rs
                 JOIN repo_stores ON repo_stores.store_id = rs.id
                 JOIN repos ON repos.id = repo_stores.repo_id
                 WHERE repos.name = ?1",
                params![repo_name],
                review_store_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    // --- reviews: the base model (README §5.5, §3) ------------------------

    /// The five new base-model columns for review `id`, plus
    /// `objects_state`. `Ok(None)` means the review itself does not exist;
    /// a review that exists but predates this migration reads back with
    /// `base_mode: None` and `base_set_by: "legacy"` (the column
    /// `DEFAULT`) — see the migration header on why that is a real,
    /// self-describing value rather than a placeholder.
    pub fn get_review_base(&self, id: i64) -> Result<Option<ReviewBaseRow>> {
        self.lock()
            .query_row(
                "SELECT base_mode, base_branch, base_member, base_set_by, base_status, objects_state
                 FROM reviews WHERE id = ?1",
                params![id],
                |r| {
                    Ok(ReviewBaseRow {
                        base_mode: r.get(0)?,
                        base_branch: r.get(1)?,
                        base_member: r.get(2)?,
                        base_set_by: r.get(3)?,
                        base_status: r.get(4)?,
                        objects_state: r.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Set a review's base POLICY (README §3: `track(B)`/`local(B)`/
    /// `pin(sha)`, plus who set it). Never touches `objects_state` — see
    /// [`Self::set_review_objects_state`], a separate write because the
    /// two are set by different passes (base resolution vs. store
    /// connectivity verification). Returns `false` when `id` does not
    /// exist.
    #[allow(clippy::too_many_arguments)]
    pub fn set_review_base(
        &self,
        id: i64,
        base_mode: &str,
        base_branch: Option<&str>,
        base_member: Option<i64>,
        base_set_by: &str,
        base_status_json: Option<&str>,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews
             SET base_mode = ?2, base_branch = ?3, base_member = ?4, base_set_by = ?5,
                 base_status = ?6
             WHERE id = ?1",
            params![
                id,
                base_mode,
                base_branch,
                base_member,
                base_set_by,
                base_status_json
            ],
        )?;
        Ok(n > 0)
    }

    /// RS-U6 — rewrite `reviews.base_ref` when a review's base POLICY
    /// changes (retarget-follow, D15; an explicit re-base): the column stays
    /// the git-resolvable display ref every legacy reader and the pre-store
    /// fallback use (`review_base::BasePolicy::display_base_ref`). Returns
    /// `false` when `id` does not exist.
    pub fn set_review_base_ref(&self, id: i64, base_ref: &str) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews SET base_ref = ?2 WHERE id = ?1",
            params![id, base_ref],
        )?;
        Ok(n > 0)
    }

    /// RS-U6 — sync `reviews.pr_head_sha` to a freshly FETCHED forge PR
    /// head (README §1 defect 5: it used to be refreshed only by reuse and
    /// sweep). Only ever called with a sha a successful PR-head fetch just
    /// resolved — never with a locally moved ref. Leaves the PR metadata
    /// snapshot alone.
    pub fn set_review_pr_head_sha(&self, id: i64, pr_head_sha: &str) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews SET pr_head_sha = ?2 WHERE id = ?1",
            params![id, pr_head_sha],
        )?;
        Ok(n > 0)
    }

    /// `NULL` (ok) | `objects-missing` | `legacy-unverified` — the per-review
    /// connectivity verdict a store's seeding/verification pass leaves
    /// behind (README §5.2 step 4). Pass `None` to clear it back to ok.
    pub fn set_review_objects_state(&self, id: i64, objects_state: Option<&str>) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews SET objects_state = ?2 WHERE id = ?1",
            params![id, objects_state],
        )?;
        Ok(n > 0)
    }

    // --- review_patchsets: base_tip_sha / kind -----------------------------

    /// [`Self::insert_patchset`]'s two new columns, read back for one
    /// patchset. `Ok(None)` covers both "no such patchset" and "a legacy
    /// patchset with nothing to report" identically at the ROW level — a
    /// caller that needs to tell them apart already has
    /// [`Self::get_patchset`] for existence.
    pub fn get_patchset_base(
        &self,
        review_id: i64,
        ps_number: i64,
    ) -> Result<Option<PatchsetBaseFields>> {
        self.lock()
            .query_row(
                "SELECT base_tip_sha, kind FROM review_patchsets
                 WHERE review_id = ?1 AND ps_number = ?2",
                params![review_id, ps_number],
                |r| {
                    Ok(PatchsetBaseFields {
                        base_tip_sha: r.get(0)?,
                        kind: r.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// [`Self::insert_patchset`], plus the base-model's `base_tip_sha`
    /// (the base branch's tip at capture, README §3) and `kind` (why this
    /// patchset exists: push|rebase|base-moved|base-corrected|retarget).
    /// A NEW function beside `insert_patchset`, not a replacement — every
    /// existing caller keeps compiling and keeps writing legacy
    /// (`NULL`/`NULL`) rows via the old 5-arg function.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_patchset_with_base(
        &self,
        review_id: i64,
        ps_number: i64,
        tip_sha: &str,
        base_sha: &str,
        base_tip_sha: Option<&str>,
        kind: Option<&str>,
        captured_at: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO review_patchsets
                (review_id, ps_number, tip_sha, base_sha, base_tip_sha, kind, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                review_id,
                ps_number,
                tip_sha,
                base_sha,
                base_tip_sha,
                kind,
                captured_at
            ],
        )?;
        // Same side effect as `insert_patchset`: bump the parent review's
        // `updated_at` so newest-first listings track the latest capture.
        conn.execute(
            "UPDATE reviews SET updated_at = ?2 WHERE id = ?1",
            params![review_id, captured_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    // --- RS-U3 (review store) — registration / seeding reads ------------

    /// Distinct non-null `reviews.pr_repo_slug` values for `repo` — the
    /// base-URL ladder's rung 3 (README §5.1: "the slug shared by the
    /// repo's existing PR bindings").
    pub fn review_pr_slugs(&self, repo: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pr_repo_slug FROM reviews
             WHERE repo = ?1 AND pr_repo_slug IS NOT NULL AND pr_repo_slug != ''
             ORDER BY pr_repo_slug",
        )?;
        let rows = stmt
            .query_map(params![repo], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every repo NAME that has at least one review (any state) — the boot
    /// seeding job's scope (D4: "for repos that have reviews").
    pub fn repos_with_reviews(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT DISTINCT repo FROM reviews ORDER BY repo")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// `(id, repo, state, base_ref, base_branch)` of every review whose repo
    /// is one of `repos` (a store's member names).
    #[allow(clippy::type_complexity)]
    pub fn reviews_for_repos(
        &self,
        repos: &[String],
    ) -> Result<Vec<(i64, String, String, String, Option<String>)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, state, base_ref, base_branch FROM reviews
             WHERE repo = ?1 ORDER BY id",
        )?;
        let mut out = Vec::new();
        for repo in repos {
            let rows = stmt
                .query_map(params![repo], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out.extend(rows);
        }
        Ok(out)
    }

    /// `(review_id, ps_number, tip_sha, base_sha, base_tip_sha)` of every patchset of the
    /// reviews of `repos` — the store's connectivity check (README §5.2
    /// step 4).
    #[allow(clippy::type_complexity)]
    pub fn patchsets_for_repos(
        &self,
        repos: &[String],
    ) -> Result<Vec<(i64, i64, String, String, Option<String>)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT p.review_id, p.ps_number, p.tip_sha, p.base_sha, p.base_tip_sha
             FROM review_patchsets p JOIN reviews r ON r.id = p.review_id
             WHERE r.repo = ?1 ORDER BY p.review_id, p.ps_number",
        )?;
        let mut out = Vec::new();
        for repo in repos {
            let rows = stmt
                .query_map(params![repo], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out.extend(rows);
        }
        Ok(out)
    }

    /// Every review id belonging to ANY member of store `store_id` — the
    /// two-hop join `repo_stores -> repos.name -> reviews.repo` (G2, DB
    /// only). RS-U5 review fix: this is deliberately independent of which
    /// repos are configured in THIS process's `[[repos]]` — a member whose
    /// `repo_stores` row still exists (and still has reviews) but is no
    /// longer configured (removed, renamed) must never be silently dropped
    /// from a keep-set, or store-wide GC would delete its still-live
    /// review refs.
    pub fn review_ids_for_store(&self, store_id: i64) -> Result<Vec<i64>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT r.id FROM reviews r
             JOIN repos p ON p.name = r.repo
             JOIN repo_stores rs ON rs.repo_id = p.id
             WHERE rs.store_id = ?1
             ORDER BY r.id",
        )?;
        let rows = stmt
            .query_map(params![store_id], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// `(review_id, pr_number, state)` for every review with a PR binding
    /// belonging to any member of store `store_id` — same DB-only two-hop
    /// join as [`Self::review_ids_for_store`], for the same reason.
    pub fn pr_bound_reviews_for_store(&self, store_id: i64) -> Result<Vec<(i64, i64, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT r.id, r.pr_number, r.state FROM reviews r
             JOIN repos p ON p.name = r.repo
             JOIN repo_stores rs ON rs.repo_id = p.id
             WHERE rs.store_id = ?1 AND r.pr_number IS NOT NULL
             ORDER BY CASE WHEN r.state = 'open' THEN 0 ELSE 1 END,
                      r.updated_at DESC, r.id DESC",
        )?;
        let rows = stmt
            .query_map(params![store_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Record a store's canonical base URL and the ladder rung it came from.
    /// NEVER changes `store_key` (README §11: a disagreeing `base_url` is a
    /// doctor error, never a re-key) — callers check the key first.
    pub fn set_review_store_base_url(
        &self,
        id: i64,
        base_url: Option<&str>,
        base_url_source: Option<&str>,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE review_stores SET base_url = ?2, base_url_source = ?3 WHERE id = ?1",
            params![id, base_url, base_url_source],
        )?;
        Ok(n > 0)
    }

    /// Boot: a store left `seeding` by a process that died mid-seed goes
    /// back to `absent` (its `.seed-*.tmp` is swept separately) — except
    /// the ids in `live`, which THIS process is seeding right now. Returns
    /// how many rows changed.
    pub fn reset_interrupted_seeding(&self, live: &[i64]) -> Result<usize> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id FROM review_stores WHERE state = 'seeding'")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut n = 0;
        for id in ids.into_iter().filter(|id| !live.contains(id)) {
            n += conn.execute(
                "UPDATE review_stores SET state = 'absent',
                     state_json = json_object('code', 'seed-interrupted')
                 WHERE id = ?1 AND state = 'seeding'",
                params![id],
            )?;
        }
        Ok(n)
    }
}
