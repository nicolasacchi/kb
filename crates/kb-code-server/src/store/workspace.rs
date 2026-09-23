//! Workspace identity and the re-key backfill.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    /// Idempotent: the id is a pure function of (common dir, root commit),
    /// so a second boot rewrites the same row and only moves `seen_at`.
    pub fn upsert_workspace(
        &self,
        id: &str,
        common_dir: &str,
        root_commit: Option<&str>,
        now: i64,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO workspaces (id, common_dir, root_commit, created_at, seen_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 common_dir = excluded.common_dir,
                 root_commit = COALESCE(excluded.root_commit, workspaces.root_commit),
                 seen_at = excluded.seen_at",
            params![id, common_dir, root_commit, now],
        )?;
        Ok(())
    }

    /// Look a workspace up by its canonical common dir — the lookup that
    /// lets `resolve_and_upsert` SKIP the root-commit history walk on
    /// every boot after the first.
    pub fn workspace_by_common_dir(&self, common_dir: &str) -> Result<Option<WorkspaceRow>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, common_dir, root_commit, created_at FROM workspaces
                 WHERE common_dir = ?1",
                params![common_dir],
                |r| {
                    Ok(WorkspaceRow {
                        id: r.get(0)?,
                        common_dir: r.get(1)?,
                        root_commit: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, common_dir, root_commit, created_at FROM workspaces ORDER BY common_dir",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WorkspaceRow {
                    id: r.get(0)?,
                    common_dir: r.get(1)?,
                    root_commit: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Replace one workspace's worktree rows wholesale, in ONE transaction.
    ///
    /// Wholesale because `git worktree list` IS the answer: a worktree the
    /// enumeration no longer reports has been removed, and keeping a stale
    /// row would make `mounted`/`prunable` lie. The `(workspace_id, id)`
    /// key is what makes a MOVED worktree keep its identity across this —
    /// the path changes, the row does not become a second one.
    pub fn replace_worktrees(
        &self,
        workspace_id: &str,
        rows: &[crate::workspace::WorktreeRow],
        now: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        // Preserve `created_by_daemon` across re-enumeration: the path is
        // a mutable attribute, the "we created this" bit is not.
        let mut flags: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        {
            let mut stmt =
                tx.prepare("SELECT id, created_by_daemon FROM worktrees WHERE workspace_id = ?1")?;
            let existing = stmt.query_map(params![workspace_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
            })?;
            for row in existing {
                let (id, flag) = row?;
                flags.insert(id, flag);
            }
        }
        tx.execute(
            "DELETE FROM worktrees WHERE workspace_id = ?1",
            params![workspace_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO worktrees (
                     workspace_id, id, path, branch, head_sha, is_main, bare, detached,
                     locked, lock_reason, prunable, prunable_reason, mounted,
                     path_resolution, repo_id, seen_at, created_by_daemon)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                         (SELECT id FROM repos WHERE name = ?15), ?16, ?17)",
            )?;
            for w in rows {
                let created = flags.get(&w.id).copied().unwrap_or(w.created_by_daemon);
                stmt.execute(params![
                    workspace_id,
                    w.id,
                    w.path,
                    w.branch,
                    w.head_sha,
                    w.is_main as i64,
                    w.bare as i64,
                    w.detached as i64,
                    w.locked as i64,
                    w.lock_reason,
                    w.prunable as i64,
                    w.prunable_reason,
                    w.mounted as i64,
                    w.path_resolution,
                    w.repo,
                    now,
                    created as i64,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn worktrees_for_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<crate::workspace::WorktreeRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT w.workspace_id, w.id, w.path, w.branch, w.head_sha, w.is_main, w.bare,
                    w.detached, w.locked, w.lock_reason, w.prunable, w.prunable_reason,
                    w.mounted, w.path_resolution, r.name, w.created_by_daemon
             FROM worktrees w LEFT JOIN repos r ON r.id = w.repo_id
             WHERE w.workspace_id = ?1
             ORDER BY w.is_main DESC, w.id",
        )?;
        let rows = stmt
            .query_map(params![workspace_id], worktree_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_all_worktrees(&self) -> Result<Vec<crate::workspace::WorktreeRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT w.workspace_id, w.id, w.path, w.branch, w.head_sha, w.is_main, w.bare,
                    w.detached, w.locked, w.lock_reason, w.prunable, w.prunable_reason,
                    w.mounted, w.path_resolution, r.name, w.created_by_daemon
             FROM worktrees w LEFT JOIN repos r ON r.id = w.repo_id
             ORDER BY w.workspace_id, w.is_main DESC, w.id",
        )?;
        let rows = stmt
            .query_map([], worktree_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn worktree_by_pk(
        &self,
        workspace_id: &str,
        id: &str,
    ) -> Result<Option<crate::workspace::WorktreeRow>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT w.workspace_id, w.id, w.path, w.branch, w.head_sha, w.is_main, w.bare,
                        w.detached, w.locked, w.lock_reason, w.prunable, w.prunable_reason,
                        w.mounted, w.path_resolution, r.name, w.created_by_daemon
                 FROM worktrees w LEFT JOIN repos r ON r.id = w.repo_id
                 WHERE w.workspace_id = ?1 AND w.id = ?2",
                params![workspace_id, id],
                worktree_row_from,
            )
            .optional()?;
        Ok(row)
    }

    pub fn worktrees_with_id(&self, id: &str) -> Result<Vec<crate::workspace::WorktreeRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT w.workspace_id, w.id, w.path, w.branch, w.head_sha, w.is_main, w.bare,
                    w.detached, w.locked, w.lock_reason, w.prunable, w.prunable_reason,
                    w.mounted, w.path_resolution, r.name, w.created_by_daemon
             FROM worktrees w LEFT JOIN repos r ON r.id = w.repo_id
             WHERE w.id = ?1
             ORDER BY w.workspace_id",
        )?;
        let rows = stmt
            .query_map(params![id], worktree_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn mark_worktree_created_by_daemon(&self, workspace_id: &str, id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE worktrees SET created_by_daemon = 1
             WHERE workspace_id = ?1 AND id = ?2",
            params![workspace_id, id],
        )?;
        Ok(())
    }

    /// Stamp `repos.workspace_id`/`worktree_id`. THE canonical write of
    /// both identities — every V0040 trigger reads them from here.
    pub fn set_repo_identity(
        &self,
        name: &str,
        workspace_id: &str,
        worktree_id: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE repos SET workspace_id = ?2, worktree_id = ?3 WHERE name = ?1",
            params![name, workspace_id, worktree_id],
        )?;
        Ok(())
    }

    /// `(workspace_id, worktree_id)` for a repo NAME, or `None` when
    /// resolution has not run yet (`rekey: "pending"`).
    pub fn repo_identity(&self, name: &str) -> Result<Option<(String, String)>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT workspace_id, worktree_id FROM repos WHERE name = ?1",
                params![name],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.and_then(|(w, t)| match (w, t) {
            (Some(w), Some(t)) => Some((w, t)),
            _ => None,
        }))
    }

    /// The re-key's RESOLUTION FUNCTION: every registered repo sharing
    /// `repo_id`'s workspace, in id order.
    ///
    /// Falls back to `[repo_id]` when the identity is not resolved yet —
    /// which is what makes every caller byte-identical while
    /// `rekey: "pending"`, and byte-identical forever on the one-repo-
    /// per-workspace deployment that is the only one today. See
    /// `crate::rekey`'s module doc for why no READ widens onto this yet.
    pub fn workspace_repo_ids(&self, repo_id: i64) -> Result<Vec<i64>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id FROM repos
             WHERE workspace_id IS NOT NULL
               AND workspace_id = (SELECT workspace_id FROM repos WHERE id = ?1)
             ORDER BY id",
        )?;
        let ids = stmt
            .query_map(params![repo_id], |r| r.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(if ids.is_empty() { vec![repo_id] } else { ids })
    }

    /// Per object-class table, how many rows this workspace owns — the
    /// read that goes through the key the re-key added. Tables with zero
    /// rows are omitted (an absent entry is "none", never a failure).
    pub fn workspace_derived_census(
        &self,
        workspace_id: &str,
    ) -> Result<std::collections::BTreeMap<String, i64>> {
        let conn = self.lock();
        let mut out = std::collections::BTreeMap::new();
        for t in crate::rekey::REPO_KEYED_TABLES
            .iter()
            .filter(|t| t.class == crate::rekey::RepoKeyClass::Object)
        {
            // Table name comes from a `const` in this binary, never from a
            // request (`rekey::tests::every_declared_table_name_is_a_plain_
            // identifier` pins that).
            let n: i64 = conn.query_row(
                &format!("SELECT count(*) FROM {} WHERE workspace_id = ?1", t.table),
                params![workspace_id],
                |r| r.get(0),
            )?;
            if n > 0 {
                out.insert(t.table.to_string(), n);
            }
        }
        Ok(out)
    }

    /// One page of the re-key backfill. Returns `(rows keyed, table done)`.
    ///
    /// Keyset-paged by `rowid` (V72-B0's `sweep_stale_salt_page` shape),
    /// one SHORT transaction per page, cursor persisted in the SAME
    /// transaction as the page it describes. Idempotent: the UPDATE's
    /// `<key> IS NULL` predicate makes a repeated page a no-op, so a crash
    /// mid-page costs one replayed page and nothing else.
    pub fn rekey_backfill_page(
        &self,
        t: &crate::rekey::KeyedTable,
        fingerprint: &str,
        page: usize,
    ) -> Result<(u64, bool)> {
        // `backfill_sql` is `None` for exactly the `meta` class, i.e. a
        // table with no key column — one guard, not two.
        let Some(sql) = crate::rekey::backfill_sql(t) else {
            return Ok((0, true));
        };
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let progress: Option<(i64, i64, Option<String>)> = tx
            .query_row(
                "SELECT cursor, done, fingerprint FROM rekey_progress WHERE table_name = ?1",
                params![t.table],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (cursor, already_done) = match progress {
            // A different fingerprint means the repo set moved: the
            // recorded cursor is about a different question, so start over.
            Some((c, d, fp)) if fp.as_deref() == Some(fingerprint) => (c, d != 0),
            _ => (0, false),
        };
        if already_done {
            return Ok((0, true));
        }
        let (in_page, last): (i64, Option<i64>) = tx.query_row(
            &format!(
                "SELECT count(*), max(rowid) FROM
                 (SELECT rowid FROM {} WHERE rowid > ?1 ORDER BY rowid LIMIT ?2)",
                t.table
            ),
            params![cursor, page as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let done = (in_page as usize) < page;
        let keyed = match last {
            Some(last) => tx.execute(&sql, params![cursor, last])? as u64,
            None => 0,
        };
        let next_cursor = last.unwrap_or(cursor);
        let now = chrono::Utc::now().timestamp();
        tx.execute(
            "INSERT INTO rekey_progress (table_name, cursor, done, rows_keyed, fingerprint, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(table_name) DO UPDATE SET
                 cursor = excluded.cursor,
                 done = excluded.done,
                 rows_keyed = CASE
                     WHEN rekey_progress.fingerprint IS excluded.fingerprint
                     THEN rekey_progress.rows_keyed + excluded.rows_keyed
                     ELSE excluded.rows_keyed END,
                 fingerprint = excluded.fingerprint,
                 updated_at = excluded.updated_at",
            params![t.table, next_cursor, done as i64, keyed as i64, fingerprint, now],
        )?;
        tx.commit()?;
        Ok((keyed, done))
    }

    /// `true` when every keyed table has been walked to completion — the
    /// boot-time seed for the `rekey` honesty flag.
    pub fn rekey_is_done(&self) -> Result<bool> {
        let conn = self.lock();
        let want = crate::rekey::keyed_tables().count() as i64;
        let got: i64 = conn.query_row(
            "SELECT count(*) FROM rekey_progress WHERE done = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(got >= want)
    }
}
