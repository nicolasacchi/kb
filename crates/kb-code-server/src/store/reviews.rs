//! Reviews, patchsets, viewed state, and PR binding.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // --- local reviews (V3.R1, migration V0014) ---------------------------
    //
    // Deliberately NO `bump_generation()` — reviews never touch the
    // files/symbols search caches. `repo` is the configured repo NAME
    // (text), matching bookmarks/reading-sets' human-facing key rather
    // than an internal `repo_id` (reviews outlive a store re-register).

    /// Insert a new review row; returns the auto-assigned `id`.
    pub fn create_review(
        &self,
        repo: &str,
        title: Option<&str>,
        base_ref: &str,
        head_ref: &str,
        session_id: Option<&str>,
        now: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO reviews
                (repo, title, base_ref, head_ref, session_id, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?6)",
            params![repo, title, base_ref, head_ref, session_id, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_review(&self, id: i64) -> Result<Option<ReviewRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                        created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
                 FROM reviews WHERE id = ?1",
                params![id],
                review_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// [`Self::get_review`] for a whole SET of `ids` — one query (dynamic
    /// `IN (…)`, plain `prepare`) instead of N round trips. A vanished id
    /// (deleted between a caller's initial fetch and this lookup) is
    /// simply absent from the map, matching `get_review`'s own `None`.
    pub fn get_reviews_by_ids(&self, ids: &[i64]) -> Result<HashMap<i64, ReviewRow>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.lock();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                    created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
             FROM reviews WHERE id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), review_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().map(|r| (r.id, r)).collect())
    }

    /// List reviews for `repo`, optionally filtered to `state` (`"open"` /
    /// `"closed"`). Newest-first.
    pub fn list_reviews(&self, repo: &str, state: Option<&str>) -> Result<Vec<ReviewRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                    created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
             FROM reviews
             WHERE repo = ?1 AND (?2 IS NULL OR state = ?2)
             ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(params![repo, state], review_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every OPEN review across every repo — the auto-capture worker's
    /// scan surface on each `repo.head_moved`.
    pub fn list_open_reviews(&self) -> Result<Vec<ReviewRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                    created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
             FROM reviews WHERE state = 'open'
             ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map([], review_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Open reviews whose `repo` equals `repo` (auto-capture filter).
    pub fn list_open_reviews_for_repo(&self, repo: &str) -> Result<Vec<ReviewRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                    created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
             FROM reviews WHERE repo = ?1 AND state = 'open'
             ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map(params![repo], review_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn update_review(
        &self,
        id: i64,
        title: Option<Option<&str>>,
        state: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        let conn = self.lock();
        // Read-then-write: only touch provided fields. `title: Some(None)`
        // clears the title; `title: None` leaves it alone.
        let Some(mut row) = conn
            .query_row(
                "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                        created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
                 FROM reviews WHERE id = ?1",
                params![id],
                review_row_from,
            )
            .optional()?
        else {
            return Ok(false);
        };
        if let Some(t) = title {
            row.title = t.map(|s| s.to_string());
        }
        if let Some(s) = state {
            row.state = s.to_string();
        }
        let n = conn.execute(
            "UPDATE reviews SET title = ?2, state = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, row.title, row.state, now],
        )?;
        Ok(n > 0)
    }

    /// V4.C2 — set (or replace) the review-pass verdict. Compares
    /// `(state, note)` only — `verdict_at` is ignored so a re-PUT of the
    /// same pair is a no-op (kb-core `ReviewFile::set_verdict` G8).
    /// Returns `Ok(None)` when `id` is missing, `Ok(Some(false))` on a
    /// no-op, `Ok(Some(true))` when the four columns were written.
    /// Never `bump_generation`.
    pub fn set_review_verdict(
        &self,
        id: i64,
        state: &str,
        note: Option<&str>,
        at: i64,
        ps: i64,
    ) -> Result<Option<bool>> {
        let conn = self.lock();
        let Some((cur_state, cur_note)): Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT verdict, verdict_note FROM reviews WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let unchanged = cur_state.as_deref() == Some(state) && cur_note.as_deref() == note;
        if unchanged {
            return Ok(Some(false));
        }
        conn.execute(
            "UPDATE reviews
             SET verdict = ?2, verdict_note = ?3, verdict_at = ?4, verdict_ps = ?5
             WHERE id = ?1",
            params![id, state, note, at, ps],
        )?;
        Ok(Some(true))
    }

    /// V4.C2 — clear all four verdict columns. Returns `Ok(None)` when
    /// `id` is missing, `Ok(Some(false))` when there was nothing to
    /// clear, `Ok(Some(true))` when a verdict was wiped. Never
    /// `bump_generation`.
    pub fn clear_review_verdict(&self, id: i64) -> Result<Option<bool>> {
        let conn = self.lock();
        let Some(cur): Option<Option<String>> = conn
            .query_row(
                "SELECT verdict FROM reviews WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        if cur.is_none() {
            return Ok(Some(false));
        }
        conn.execute(
            "UPDATE reviews
             SET verdict = NULL, verdict_note = NULL, verdict_at = NULL, verdict_ps = NULL
             WHERE id = ?1",
            params![id],
        )?;
        Ok(Some(true))
    }

    /// Delete the review row (cascades patchsets + viewed via SQL FKs)
    /// AND every review-scoped annotation + its `annotation_suggestions`
    /// row, all in ONE transaction. Annotations have no SQL FK on
    /// `review_id` (V0023 / parent_id precedent), so the cascade is
    /// code-owned. Caller is responsible for deleting the matching
    /// `refs/kbc/review/<id>/ps*` refs first. Returns `true` iff a
    /// review row was deleted. Never `bump_generation`.
    pub fn delete_review(&self, id: i64) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM annotation_suggestions
             WHERE annotation_id IN (SELECT id FROM annotations WHERE review_id = ?1)",
            params![id],
        )?;
        tx.execute("DELETE FROM annotations WHERE review_id = ?1", params![id])?;
        let n = tx.execute("DELETE FROM reviews WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn insert_patchset(
        &self,
        review_id: i64,
        ps_number: i64,
        tip_sha: &str,
        base_sha: &str,
        captured_at: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO review_patchsets
                (review_id, ps_number, tip_sha, base_sha, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![review_id, ps_number, tip_sha, base_sha, captured_at],
        )?;
        // Bump the parent review's updated_at so list-newest-first tracks
        // the latest capture.
        conn.execute(
            "UPDATE reviews SET updated_at = ?2 WHERE id = ?1",
            params![review_id, captured_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_patchsets(&self, review_id: i64) -> Result<Vec<ReviewPatchsetRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, review_id, ps_number, tip_sha, base_sha, captured_at
             FROM review_patchsets
             WHERE review_id = ?1
             ORDER BY ps_number ASC",
        )?;
        let rows = stmt
            .query_map(params![review_id], review_patchset_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_patchset(
        &self,
        review_id: i64,
        ps_number: i64,
    ) -> Result<Option<ReviewPatchsetRow>> {
        self.lock()
            .query_row(
                "SELECT id, review_id, ps_number, tip_sha, base_sha, captured_at
                 FROM review_patchsets
                 WHERE review_id = ?1 AND ps_number = ?2",
                params![review_id, ps_number],
                review_patchset_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn latest_patchset(&self, review_id: i64) -> Result<Option<ReviewPatchsetRow>> {
        // PF-K1 — hot on the (now-batched, see `latest_patchsets`) review
        // list/inbox composition paths; `prepare_cached` (identical SQL
        // every call) avoids a full re-parse/re-plan per call.
        let conn = self.lock();
        // Bound local: a `CachedStatement` tail-temporary outlives `conn`
        // (its Drop returns it to the cache) — E0597 otherwise.
        let mut stmt = conn.prepare_cached(
            "SELECT id, review_id, ps_number, tip_sha, base_sha, captured_at
             FROM review_patchsets
             WHERE review_id = ?1
             ORDER BY ps_number DESC
             LIMIT 1",
        )?;
        stmt.query_row(params![review_id], review_patchset_row_from)
            .optional()
            .map_err(Into::into)
    }

    /// [`Self::latest_patchset`] for a whole SET of `review_ids` — one
    /// query (an `IN (…)` placeholder list, dynamic per call — deliberately
    /// left as plain `prepare`, not `prepare_cached`, per this file's
    /// "dynamic placeholder counts thrash the cache" convention) instead of
    /// N round trips. A review with no captured patchset (or a vanished
    /// id) is simply ABSENT from the map, never a synthesized/default row —
    /// callers already branch on `Option`/`.get()` the same way the
    /// singular form's `Option` return does.
    pub fn latest_patchsets(&self, review_ids: &[i64]) -> Result<HashMap<i64, ReviewPatchsetRow>> {
        if review_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.lock();
        let placeholders = review_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, review_id, ps_number, tip_sha, base_sha, captured_at
             FROM review_patchsets rp
             WHERE review_id IN ({placeholders})
               AND ps_number = (
                 SELECT MAX(ps_number) FROM review_patchsets rp2
                 WHERE rp2.review_id = rp.review_id
               )"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(review_ids.iter()),
                review_patchset_row_from,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().map(|r| (r.review_id, r)).collect())
    }

    /// Next `ps_number` for a review (`MAX + 1`, or `1` when empty).
    pub fn next_ps_number(&self, review_id: i64) -> Result<i64> {
        let max: Option<i64> = self.lock().query_row(
            "SELECT MAX(ps_number) FROM review_patchsets WHERE review_id = ?1",
            params![review_id],
            |r| r.get(0),
        )?;
        Ok(max.unwrap_or(0) + 1)
    }

    /// Patchset count for a review.
    pub fn patchset_count(&self, review_id: i64) -> Result<i64> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM review_patchsets WHERE review_id = ?1",
            params![review_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Oldest patchset by `ps_number` (for GC of the oldest when over
    /// `max_patchsets`). `None` when the review has no patchsets.
    pub fn oldest_patchset(&self, review_id: i64) -> Result<Option<ReviewPatchsetRow>> {
        self.lock()
            .query_row(
                "SELECT id, review_id, ps_number, tip_sha, base_sha, captured_at
                 FROM review_patchsets
                 WHERE review_id = ?1
                 ORDER BY ps_number ASC
                 LIMIT 1",
                params![review_id],
                review_patchset_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn delete_patchset(&self, review_id: i64, ps_number: i64) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM review_patchsets WHERE review_id = ?1 AND ps_number = ?2",
            params![review_id, ps_number],
        )?;
        Ok(n > 0)
    }

    pub fn upsert_viewed(
        &self,
        review_id: i64,
        path: &str,
        blob_sha: &str,
        viewed_at: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO review_viewed (review_id, path, blob_sha, viewed_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(review_id, path) DO UPDATE SET
                blob_sha = excluded.blob_sha,
                viewed_at = excluded.viewed_at",
            params![review_id, path, blob_sha, viewed_at],
        )?;
        Ok(())
    }

    pub fn delete_viewed(&self, review_id: i64, path: &str) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM review_viewed WHERE review_id = ?1 AND path = ?2",
            params![review_id, path],
        )?;
        Ok(n > 0)
    }

    /// All viewed rows for a review, keyed by path.
    pub fn list_viewed(&self, review_id: i64) -> Result<Vec<ReviewViewedRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT review_id, path, blob_sha, viewed_at
             FROM review_viewed WHERE review_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![review_id], review_viewed_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// [`Self::list_viewed`] for a whole SET of `review_ids` — one query
    /// (dynamic `IN (…)`, plain `prepare`) instead of N round trips. A
    /// review with no viewed rows is simply absent from the map.
    pub fn list_viewed_batch(
        &self,
        review_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<ReviewViewedRow>>> {
        let mut out: HashMap<i64, Vec<ReviewViewedRow>> = HashMap::new();
        if review_ids.is_empty() {
            return Ok(out);
        }
        let conn = self.lock();
        let placeholders = review_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT review_id, path, blob_sha, viewed_at
             FROM review_viewed WHERE review_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(review_ids.iter()),
                review_viewed_row_from,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for row in rows {
            out.entry(row.review_id).or_default().push(row);
        }
        Ok(out)
    }

    // --- V73-K2a — per-HUNK viewed state (`review_hunk_viewed`, V0031) ---
    //
    // Beside `review_viewed`, never instead of it: the file-level rows
    // above answer "has this file been read at this blob", these answer
    // "has this CHANGE been read". `hunk_id` is an opaque content address
    // the SPA mints (`kbc-hunkid/1` — see the migration's own header for
    // why the daemon does not mint it and what an unrecognised id degrades
    // to).

    pub fn upsert_hunk_viewed(
        &self,
        review_id: i64,
        hunk_id: &str,
        path: &str,
        viewed_at: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO review_hunk_viewed (review_id, hunk_id, path, viewed_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(review_id, hunk_id) DO UPDATE SET
                path = excluded.path,
                viewed_at = excluded.viewed_at",
            params![review_id, hunk_id, path, viewed_at],
        )?;
        Ok(())
    }

    pub fn delete_hunk_viewed(&self, review_id: i64, hunk_id: &str) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM review_hunk_viewed WHERE review_id = ?1 AND hunk_id = ?2",
            params![review_id, hunk_id],
        )?;
        Ok(n > 0)
    }

    /// Every viewed-hunk row for a review, path-then-hunk ordered so the
    /// wire array is deterministic (a set with a stable order is what lets
    /// the SPA's own golden compare two responses).
    pub fn list_hunk_viewed(&self, review_id: i64) -> Result<Vec<ReviewHunkViewedRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT review_id, hunk_id, path, viewed_at
             FROM review_hunk_viewed WHERE review_id = ?1
             ORDER BY path, hunk_id",
        )?;
        let rows = stmt
            .query_map(params![review_id], |row| {
                Ok(ReviewHunkViewedRow {
                    review_id: row.get(0)?,
                    hunk_id: row.get(1)?,
                    path: row.get(2)?,
                    viewed_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_viewed(&self, review_id: i64, path: &str) -> Result<Option<ReviewViewedRow>> {
        self.lock()
            .query_row(
                "SELECT review_id, path, blob_sha, viewed_at
                 FROM review_viewed WHERE review_id = ?1 AND path = ?2",
                params![review_id, path],
                review_viewed_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    // -- PR binding (V0024) -------------------------------------------------

    /// The PR-binding + artifact-hint columns on `reviews`, as read back.
    /// `Ok(None)` iff `id` does not exist; every field inside `Some` is its
    /// own independent `Option` (a review can be PR-bound with no artifact
    /// hint yet, or vice versa — `PATCH /api/reviews/{id}` sets the hint
    /// independently of `POST /api/reviews/pr` setting the binding).
    pub fn get_review_pr_binding(&self, id: i64) -> Result<Option<ReviewPrBinding>> {
        // PF-K1 — hot on the (now-batched, see `get_review_pr_bindings`)
        // review list/inbox/recurrence composition paths; `prepare_cached`
        // (identical SQL every call) avoids a full re-parse/re-plan per call.
        let conn = self.lock();
        // Bound local: a `CachedStatement` tail-temporary outlives `conn`
        // (its Drop returns it to the cache) — E0597 otherwise.
        let mut stmt = conn.prepare_cached(
            "SELECT pr_number, pr_repo_slug, pr_head_sha, pr_meta_json, pr_meta_fetched_at,
                    artifact_hint_kb, artifact_hint_id
             FROM reviews WHERE id = ?1",
        )?;
        stmt.query_row(params![id], |r| {
            Ok(ReviewPrBinding {
                pr_number: r.get(0)?,
                pr_repo_slug: r.get(1)?,
                pr_head_sha: r.get(2)?,
                pr_meta_json: r.get(3)?,
                pr_meta_fetched_at: r.get(4)?,
                artifact_hint_kb: r.get(5)?,
                artifact_hint_id: r.get(6)?,
            })
        })
        .optional()
        .map_err(Into::into)
    }

    /// [`Self::get_review_pr_binding`] for a whole SET of `ids` — one
    /// query (dynamic `IN (…)`, plain `prepare`) instead of N round trips.
    /// Shared by the review list route ([`Self::latest_patchsets`]'s
    /// sibling), the inbox composition, and the findings-recurrence route's
    /// prior-review lookup. A missing id is simply absent from the map —
    /// every caller already treats a missing/`None` binding as
    /// [`ReviewPrBinding::default`].
    pub fn get_review_pr_bindings(&self, ids: &[i64]) -> Result<HashMap<i64, ReviewPrBinding>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.lock();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, pr_number, pr_repo_slug, pr_head_sha, pr_meta_json, pr_meta_fetched_at,
                    artifact_hint_kb, artifact_hint_id
             FROM reviews WHERE id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    ReviewPrBinding {
                        pr_number: r.get(1)?,
                        pr_repo_slug: r.get(2)?,
                        pr_head_sha: r.get(3)?,
                        pr_meta_json: r.get(4)?,
                        pr_meta_fetched_at: r.get(5)?,
                        artifact_hint_kb: r.get(6)?,
                        artifact_hint_id: r.get(7)?,
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }

    /// Bind a review to a PR — `POST /api/reviews/pr`'s (a later phase's)
    /// write. `pr_repo_slug` is required at bind time (resolved from the
    /// git origin); `pr_head_sha`/`pr_meta_json`/`pr_meta_fetched_at` are
    /// best-effort (the git fetch that creates the binding is load-bearing,
    /// the GitHub metadata enrichment call is not — design doc §2 row 1).
    /// Returns `true` iff `id` existed. A `(repo, pr_number)` collision
    /// among OPEN reviews surfaces as `StoreError::Sqlite` (the
    /// `idx_reviews_pr_binding` UNIQUE index, V0042: open-only). Closed
    /// rows may share a PR with an open successor (`start-pr --new`).
    #[allow(clippy::too_many_arguments)]
    pub fn set_review_pr_binding(
        &self,
        id: i64,
        pr_number: i64,
        pr_repo_slug: &str,
        pr_head_sha: Option<&str>,
        pr_meta_json: Option<&str>,
        pr_meta_fetched_at: Option<i64>,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews
             SET pr_number = ?2, pr_repo_slug = ?3, pr_head_sha = ?4,
                 pr_meta_json = ?5, pr_meta_fetched_at = ?6
             WHERE id = ?1",
            params![
                id,
                pr_number,
                pr_repo_slug,
                pr_head_sha,
                pr_meta_json,
                pr_meta_fetched_at
            ],
        )?;
        Ok(n > 0)
    }

    /// Refresh just the GitHub metadata snapshot on an ALREADY-bound review
    /// (`GET /api/prs/{n}` re-fetch, a later phase) — leaves `pr_number`/
    /// `pr_repo_slug` untouched. Returns `true` iff `id` existed.
    pub fn set_review_pr_meta(
        &self,
        id: i64,
        pr_head_sha: Option<&str>,
        pr_meta_json: Option<&str>,
        pr_meta_fetched_at: i64,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews
             SET pr_head_sha = ?2, pr_meta_json = ?3, pr_meta_fetched_at = ?4
             WHERE id = ?1",
            params![id, pr_head_sha, pr_meta_json, pr_meta_fetched_at],
        )?;
        Ok(n > 0)
    }

    /// Set (or, with both `None`, clear) the artifact hint — the ONLY write
    /// path design doc §4.2 sanctions (`PATCH /api/reviews/{id}` gaining
    /// two optional fields, a later phase). Never verified here (kb-code
    /// has no business validating a kb doc id against a schema it doesn't
    /// own — verification is `GET /api/reviews/{id}/artifact`'s live,
    /// unpersisted job). Returns `true` iff `id` existed.
    pub fn set_review_artifact_hint(
        &self,
        id: i64,
        kb: Option<&str>,
        doc_id: Option<&str>,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews SET artifact_hint_kb = ?2, artifact_hint_id = ?3 WHERE id = ?1",
            params![id, kb, doc_id],
        )?;
        Ok(n > 0)
    }

    /// PRR-R2 — look up a review already bound to `(repo, pr_number)`, the
    /// `POST /api/reviews/pr` pre-check (design doc §2 row 1: "Unique
    /// (repo, pr_number) violations -> 409 pointing at the existing review
    /// id"). Reading BEFORE the insert (rather than catching
    /// `idx_reviews_pr_binding`'s UNIQUE-constraint violation the way
    /// `name_conflict_or` does for reading sets) is deliberate here: the
    /// 409 body needs the EXISTING review's full id/repo/title, which a
    /// bare constraint-violation error carries none of, and this route's
    /// mutation is loopback-only / effectively single-operator, so the
    /// pre-check-then-insert race this leaves open is the same one
    /// `create_review`'s own two-step "resolve refs, then insert" already
    /// accepts.
    pub fn get_review_by_pr_binding(
        &self,
        repo: &str,
        pr_number: i64,
    ) -> Result<Option<ReviewRow>> {
        // V76-R1b: V0042 lets several CLOSED rows share a PR with at most
        // one OPEN row. Prefer the open review; else the newest closed.
        // LIMIT 1 — `query_row` errors on multiple matches.
        self.lock()
            .query_row(
                "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                        created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
                 FROM reviews WHERE repo = ?1 AND pr_number = ?2
                 ORDER BY CASE WHEN state = 'open' THEN 0 ELSE 1 END,
                          updated_at DESC, id DESC
                 LIMIT 1",
                params![repo, pr_number],
                review_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every review in `repo` that still carries a `pr_number` — `(id,
    /// pr_number, state)`. Used by `GET /api/reviews/refs` to attribute
    /// `refs/kbc/pr/<n>` (open preferred at the call site).
    pub fn list_pr_bound_reviews(&self, repo: &str) -> Result<Vec<(i64, i64, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, pr_number, state FROM reviews
             WHERE repo = ?1 AND pr_number IS NOT NULL
             ORDER BY CASE WHEN state = 'open' THEN 0 ELSE 1 END,
                      updated_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(params![repo], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// `(review_id, ps_number)` for every retained patchset of reviews in
    /// `repo`. A `refs/kbc/review/<id>/ps<n>` whose pair is absent here is
    /// an orphan (deleted review, or a patchset already GC'd from sqlite).
    pub fn list_patchset_keys_for_repo(&self, repo: &str) -> Result<Vec<(i64, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT p.review_id, p.ps_number
             FROM review_patchsets p
             JOIN reviews r ON r.id = p.review_id
             WHERE r.repo = ?1",
        )?;
        let rows = stmt
            .query_map(params![repo], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// RS-U10a — every review bound to PR `pr_number`, in any repo (or only
    /// `repo` when given), each with its `(pr_repo_slug, pr_head_sha)`.
    /// Ordered the way [`Self::get_review_by_pr_binding`] prefers: repo,
    /// then open before closed, then newest first — so the FIRST row per
    /// repo is the review `pr:<N>` addressing resolves to. `GET
    /// /api/reviews/find`'s one query (the CLI's `review find` and the
    /// `pr:<N>` address both ride it, never a per-repo list + git diff).
    #[allow(clippy::type_complexity)]
    pub fn find_reviews_by_pr(
        &self,
        pr_number: i64,
        repo: Option<&str>,
    ) -> Result<Vec<(ReviewRow, Option<String>, Option<String>)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                    created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps,
                    pr_repo_slug, pr_head_sha
             FROM reviews
             WHERE pr_number = ?1 AND (?2 IS NULL OR repo = ?2)
             ORDER BY repo,
                      CASE WHEN state = 'open' THEN 0 ELSE 1 END,
                      updated_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(params![pr_number, repo], |r| {
                Ok((
                    review_row_from(r)?,
                    r.get::<_, Option<String>>(13)?,
                    r.get::<_, Option<String>>(14)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// How many reviews in `repo` still bind `pr_number` (any state).
    /// `delete_review_with_refs` uses this to decide whether `refs/kbc/pr/<n>`
    /// is still claimed.
    pub fn count_reviews_by_pr_binding(&self, repo: &str, pr_number: i64) -> Result<i64> {
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM reviews WHERE repo = ?1 AND pr_number = ?2",
                params![repo, pr_number],
                |r| r.get(0),
            )
            .map_err(Into::into)
    }

    // -- Review report (V0024) ----------------------------------------------

    /// `Ok(None)` iff `id` does not exist; `report_json: None` inside
    /// `Some` means "no report authored yet" (`GET /api/reviews/{id}/
    /// report`'s `{report: null}` case, a later phase).
    pub fn get_review_report(&self, id: i64) -> Result<Option<ReviewReport>> {
        self.lock()
            .query_row(
                "SELECT report_json, report_updated_at FROM reviews WHERE id = ?1",
                params![id],
                |r| {
                    Ok(ReviewReport {
                        report_json: r.get(0)?,
                        report_updated_at: r.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// [`Self::get_review_report`] for a whole SET of `ids` — one query
    /// (dynamic `IN (…)`, plain `prepare`) instead of N round trips. A
    /// missing id is simply absent from the map — callers already treat a
    /// missing/`None` report the same as [`ReviewReport::default`].
    pub fn get_review_reports(&self, ids: &[i64]) -> Result<HashMap<i64, ReviewReport>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.lock();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, report_json, report_updated_at FROM reviews WHERE id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    ReviewReport {
                        report_json: r.get(1)?,
                        report_updated_at: r.get(2)?,
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }

    /// Wholesale-replace the report (`PUT /api/reviews/{id}/report`, a
    /// later phase — never a partial-field merge, same reasoning as
    /// `pr_meta_json`). Returns `true` iff `id` existed.
    pub fn set_review_report(&self, id: i64, report_json: &str, updated_at: i64) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews SET report_json = ?2, report_updated_at = ?3 WHERE id = ?1",
            params![id, report_json, updated_at],
        )?;
        Ok(n > 0)
    }

    // -- Verdict publish record (V0024) -------------------------------------

    /// `Ok(None)` iff `id` does not exist.
    pub fn get_review_verdict_published(
        &self,
        id: i64,
    ) -> Result<Option<(Option<i64>, Option<String>)>> {
        self.lock()
            .query_row(
                "SELECT verdict_published_at, verdict_published_url FROM reviews WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Record that the review-level verdict was published to GitHub
    /// (`POST /api/reviews/{id}/verdict/published`, a later phase) —
    /// advisory only, see design doc Risk #5. Returns `true` iff `id`
    /// existed.
    pub fn set_review_verdict_published(
        &self,
        id: i64,
        url: Option<&str>,
        published_at: i64,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews SET verdict_published_at = ?2, verdict_published_url = ?3 WHERE id = ?1",
            params![id, published_at, url],
        )?;
        Ok(n > 0)
    }

    /// Up to `limit` indexed paths in this repo whose BASENAME is
    /// `basename` — `review_doc::lint`'s "did you mean" candidates for an
    /// unresolvable `code:` ref.
    ///
    /// A trailing-`LIKE` scan of one repo's `files` rows: fine for a
    /// pre-flight the operator runs once per compose, and deliberately NOT
    /// something any keystroke path may call (`search::matcher` is the
    /// answer there — kb-code-server/CLAUDE.md invariant 16(b)).
    pub fn paths_with_basename(
        &self,
        repo_id: i64,
        basename: &str,
        limit: usize,
    ) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path FROM files
             WHERE repo_id = ?1 AND (path = ?2 OR path LIKE '%/' || ?3 ESCAPE '\\')
             ORDER BY path LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(
                params![repo_id, basename, like_escape(basename), limit as i64],
                |r| r.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
