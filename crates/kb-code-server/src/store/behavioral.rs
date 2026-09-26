//! Path, author, co-change, and session-signal stats.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // --- behavioral store (V3.2-B1, migration V0017) ----------------------
    //
    // Repo-addressed history counters — NOT content-addressed. No
    // `bump_generation()`: these never feed the files/symbols search
    // caches. Incremental updates are additive; only a full rebuild
    // prunes paths/pairs outside the window.

    pub fn behavioral_meta(&self, repo_id: i64) -> Result<Option<BehavioralMetaRow>> {
        self.lock()
            .query_row(
                "SELECT repo_id, last_commit_sha, updated_at
                 FROM behavioral_meta WHERE repo_id = ?1",
                params![repo_id],
                |r| {
                    Ok(BehavioralMetaRow {
                        repo_id: r.get(0)?,
                        last_commit_sha: r.get(1)?,
                        updated_at: r.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_behavioral_meta(
        &self,
        repo_id: i64,
        last_commit_sha: Option<&str>,
        updated_at: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO behavioral_meta (repo_id, last_commit_sha, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(repo_id) DO UPDATE SET
               last_commit_sha = excluded.last_commit_sha,
               updated_at = excluded.updated_at",
            params![repo_id, last_commit_sha, updated_at],
        )?;
        Ok(())
    }

    /// Drop every behavioral counter row for `repo_id` (full rebuild).
    pub fn clear_behavioral_stats(&self, repo_id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM path_stats WHERE repo_id = ?1",
            params![repo_id],
        )?;
        conn.execute(
            "DELETE FROM author_stats WHERE repo_id = ?1",
            params![repo_id],
        )?;
        conn.execute(
            "DELETE FROM cochange_pairs WHERE repo_id = ?1",
            params![repo_id],
        )?;
        Ok(())
    }

    /// Apply one commit's path/author/cochange deltas in a single
    /// transaction. `co_pairs` entries MUST already be `a < b` ordered.
    ///
    /// V3.2-B2: when `session_id` is `Some`, also records
    /// `author = "session:<id>"` beside the human author (both rows —
    /// human authored the commit, session produced the patch; never collapse).
    pub fn apply_behavioral_commit(
        &self,
        repo_id: i64,
        files: &[(String, i64, i64)], // (path, added, deleted)
        author: &str,
        commit_unix: i64,
        co_pairs: &[(String, String)], // (path_a, path_b) with a < b
        session_id: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for (path, added, deleted) in files {
            tx.execute(
                "INSERT INTO path_stats
                    (repo_id, path, revisions, lines_added, lines_deleted,
                     first_seen_unix, last_touch_unix)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?5)
                 ON CONFLICT(repo_id, path) DO UPDATE SET
                   revisions = revisions + 1,
                   lines_added = lines_added + excluded.lines_added,
                   lines_deleted = lines_deleted + excluded.lines_deleted,
                   first_seen_unix = MIN(
                       COALESCE(first_seen_unix, excluded.first_seen_unix),
                       excluded.first_seen_unix),
                   last_touch_unix = MAX(
                       COALESCE(last_touch_unix, excluded.last_touch_unix),
                       excluded.last_touch_unix)",
                params![repo_id, path, added, deleted, commit_unix],
            )?;
            // Human author row (always).
            Self::upsert_author_stat(&tx, repo_id, path, author, commit_unix)?;
            // Session dual-author row when join resolved (V3.2-B2).
            if let Some(sid) = session_id {
                if !sid.is_empty() {
                    let session_author = format!("session:{sid}");
                    Self::upsert_author_stat(&tx, repo_id, path, &session_author, commit_unix)?;
                }
            }
        }
        for (a, b) in co_pairs {
            tx.execute(
                "INSERT INTO cochange_pairs (repo_id, path_a, path_b, co_commits)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(repo_id, path_a, path_b) DO UPDATE SET
                   co_commits = co_commits + 1",
                params![repo_id, a, b],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_path_stats(&self, repo_id: i64) -> Result<Vec<PathStatsRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path, revisions, lines_added, lines_deleted,
                    first_seen_unix, last_touch_unix
             FROM path_stats WHERE repo_id = ?1
             ORDER BY revisions DESC, path ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok(PathStatsRow {
                    path: r.get(0)?,
                    revisions: r.get(1)?,
                    lines_added: r.get(2)?,
                    lines_deleted: r.get(3)?,
                    first_seen_unix: r.get(4)?,
                    last_touch_unix: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn path_stats_for(&self, repo_id: i64, path: &str) -> Result<Option<PathStatsRow>> {
        self.lock()
            .query_row(
                "SELECT path, revisions, lines_added, lines_deleted,
                        first_seen_unix, last_touch_unix
                 FROM path_stats WHERE repo_id = ?1 AND path = ?2",
                params![repo_id, path],
                |r| {
                    Ok(PathStatsRow {
                        path: r.get(0)?,
                        revisions: r.get(1)?,
                        lines_added: r.get(2)?,
                        lines_deleted: r.get(3)?,
                        first_seen_unix: r.get(4)?,
                        last_touch_unix: r.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Partners of `path` from cochange_pairs (either side), ordered by
    /// co_commits desc then path.
    pub fn cochange_partners(&self, repo_id: i64, path: &str) -> Result<Vec<(String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path_b AS partner, co_commits FROM cochange_pairs
             WHERE repo_id = ?1 AND path_a = ?2
             UNION ALL
             SELECT path_a AS partner, co_commits FROM cochange_pairs
             WHERE repo_id = ?1 AND path_b = ?2
             ORDER BY co_commits DESC, partner ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, path], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn author_stats_for(&self, repo_id: i64, path: &str) -> Result<Vec<AuthorStatsRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT author, commits, first_seen_unix FROM author_stats
             WHERE repo_id = ?1 AND path = ?2
             ORDER BY commits DESC, author ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, path], |r| {
                Ok(AuthorStatsRow {
                    author: r.get(0)?,
                    commits: r.get(1)?,
                    first_seen_unix: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- session_signals (V3.2-B2, migration V0018) -----------------------

    pub fn upsert_session_signals(
        &self,
        repo_id: i64,
        session_id: &str,
        fail_count: i64,
        error_count: i64,
        duration_secs: i64,
        captured_at: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO session_signals
                (repo_id, session_id, fail_count, error_count, duration_secs, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(repo_id, session_id) DO UPDATE SET
               fail_count = excluded.fail_count,
               error_count = excluded.error_count,
               duration_secs = excluded.duration_secs,
               captured_at = excluded.captured_at",
            params![
                repo_id,
                session_id,
                fail_count,
                error_count,
                duration_secs,
                captured_at
            ],
        )?;
        Ok(())
    }

    pub fn session_signals_for(
        &self,
        repo_id: i64,
        session_id: &str,
    ) -> Result<Option<SessionSignalsRow>> {
        self.lock()
            .query_row(
                "SELECT session_id, fail_count, error_count, duration_secs, captured_at
                 FROM session_signals WHERE repo_id = ?1 AND session_id = ?2",
                params![repo_id, session_id],
                |r| {
                    Ok(SessionSignalsRow {
                        session_id: r.get(0)?,
                        fail_count: r.get(1)?,
                        error_count: r.get(2)?,
                        duration_secs: r.get(3)?,
                        captured_at: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// All session_signals rows for a repo (for pain-weighted hotspots).
    pub fn list_session_signals(&self, repo_id: i64) -> Result<Vec<SessionSignalsRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT session_id, fail_count, error_count, duration_secs, captured_at
             FROM session_signals WHERE repo_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok(SessionSignalsRow {
                    session_id: r.get(0)?,
                    fail_count: r.get(1)?,
                    error_count: r.get(2)?,
                    duration_secs: r.get(3)?,
                    captured_at: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Paths touched by a session via dual-author rows (`session:<id>`).
    pub fn paths_for_session_author(&self, repo_id: i64, session_id: &str) -> Result<Vec<String>> {
        let author = format!("session:{session_id}");
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path FROM author_stats
             WHERE repo_id = ?1 AND author = ?2
             ORDER BY path ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, author], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Earliest `first_seen_unix` among `session:*` authors for `path`.
    pub fn earliest_session_author(
        &self,
        repo_id: i64,
        path: &str,
    ) -> Result<Option<(String, i64)>> {
        self.lock()
            .query_row(
                "SELECT author, first_seen_unix FROM author_stats
                 WHERE repo_id = ?1 AND path = ?2
                   AND author LIKE 'session:%'
                   AND first_seen_unix IS NOT NULL
                 ORDER BY first_seen_unix ASC, author ASC
                 LIMIT 1",
                params![repo_id, path],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    /// V3.3-Q1 — does this repo have any dual-author session rows
    /// (`author_stats.author LIKE 'session:%'`)? Same source the ownership
    /// `agents[]` surface uses. Missing ⇒ agent-only recipes report
    /// `inputs_missing: ["agent_attribution"]`.
    pub fn has_session_authors(&self, repo_id: i64) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM author_stats
                 WHERE repo_id = ?1 AND author LIKE 'session:%'
             )",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n != 0)
    }

    /// V3.3-Q1 — any non-empty `commit_sessions.session_id` for the repo
    /// (join-ladder agent attribution without requiring behavioral dual-
    /// author rows).
    pub fn has_commit_session_ids(&self, repo_id: i64) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM commit_sessions
                 WHERE repo_id = ?1
                   AND session_id IS NOT NULL
                   AND session_id != ''
             )",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n != 0)
    }
}
