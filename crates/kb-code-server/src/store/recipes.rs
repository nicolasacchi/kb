//! Recipe trust, servers, and runs.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // -----------------------------------------------------------------
    // V74-L3a (kbc-recipe/1, migration V0038) — recipe trust, the server
    // recipe home, and materialised runs.
    //
    // None of these bump the generation: a recipe is a QUESTION asked of
    // the index, never an input to it (the `canvas_boards` precedent
    // directly above, for the same reason).
    // -----------------------------------------------------------------

    /// The trust-on-first-use row for one repo-versioned recipe.
    pub fn get_recipe_trust(&self, repo_id: i64, slug: &str) -> Result<Option<RecipeTrustRow>> {
        self.lock()
            .query_row(
                "SELECT repo_id, slug, source_path, content_hash, trusted_body, trusted_unix \
                 FROM recipe_trust WHERE repo_id = ?1 AND slug = ?2",
                params![repo_id, slug],
                |r| {
                    Ok(RecipeTrustRow {
                        repo_id: r.get(0)?,
                        slug: r.get(1)?,
                        source_path: r.get(2)?,
                        content_hash: r.get(3)?,
                        trusted_body: r.get(4)?,
                        trusted_unix: r.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Record (or re-record) trust. Keyed on `(repo_id, slug)`, so
    /// accepting a CHANGED file replaces the old bytes — which is exactly
    /// what "trust this version now" means.
    pub fn put_recipe_trust(
        &self,
        repo_id: i64,
        slug: &str,
        source_path: &str,
        content_hash: &str,
        trusted_body: &str,
        now: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO recipe_trust \
             (repo_id, slug, source_path, content_hash, trusted_body, trusted_unix) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(repo_id, slug) DO UPDATE SET \
               source_path = excluded.source_path, \
               content_hash = excluded.content_hash, \
               trusted_body = excluded.trusted_body, \
               trusted_unix = excluded.trusted_unix",
            params![repo_id, slug, source_path, content_hash, trusted_body, now],
        )?;
        Ok(())
    }

    /// Every server-stored recipe visible to `repo` — the rows scoped to
    /// it plus the repo-agnostic ones.
    pub fn list_recipes_server(&self, repo: Option<&str>) -> Result<Vec<RecipeServerRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT slug, repo, title, body_json, created_unix, updated_unix \
             FROM recipes_server WHERE repo IS NULL OR ?1 IS NULL OR repo = ?1 \
             ORDER BY slug ASC",
        )?;
        let rows = stmt
            .query_map(params![repo], |r| {
                Ok(RecipeServerRow {
                    slug: r.get(0)?,
                    repo: r.get(1)?,
                    title: r.get(2)?,
                    body_json: r.get(3)?,
                    created_unix: r.get(4)?,
                    updated_unix: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn put_recipe_server(
        &self,
        slug: &str,
        repo: Option<&str>,
        title: &str,
        body_json: &str,
        now: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO recipes_server \
             (slug, repo, title, body_json, created_unix, updated_unix) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
             ON CONFLICT(slug) DO UPDATE SET \
               repo = excluded.repo, title = excluded.title, \
               body_json = excluded.body_json, updated_unix = excluded.updated_unix",
            params![slug, repo, title, body_json, now],
        )?;
        Ok(())
    }

    pub fn delete_recipe_server(&self, slug: &str) -> Result<bool> {
        let n = self
            .lock()
            .execute("DELETE FROM recipes_server WHERE slug = ?1", params![slug])?;
        Ok(n > 0)
    }

    /// Store a MATERIALISED run. `generation` is the mirror generation it
    /// was computed at, so a replay can caption itself stale rather than
    /// reading as live.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_recipe_run(
        &self,
        id: &str,
        repo_id: i64,
        slug: &str,
        params_json: &str,
        scope: Option<&str>,
        generation: u64,
        result_json: &str,
        now: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO recipe_runs \
             (id, repo_id, slug, params_json, scope, generation, result_json, created_unix) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                repo_id,
                slug,
                params_json,
                scope,
                generation as i64,
                result_json,
                now
            ],
        )?;
        Ok(())
    }

    pub fn get_recipe_run(&self, id: &str) -> Result<Option<RecipeRunRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo_id, slug, params_json, scope, generation, result_json, \
                 created_unix FROM recipe_runs WHERE id = ?1",
                params![id],
                |r| {
                    Ok(RecipeRunRow {
                        id: r.get(0)?,
                        repo_id: r.get(1)?,
                        slug: r.get(2)?,
                        params_json: r.get(3)?,
                        scope: r.get(4)?,
                        generation: r.get::<_, i64>(5)? as u64,
                        result_json: r.get(6)?,
                        created_unix: r.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
}
