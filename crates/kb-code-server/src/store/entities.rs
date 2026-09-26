//! Entity-definition claims.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // --- V71-G0 — the entity index (`entities/1`, `crate::entities`) -----

    /// Replace every `entity_defs` row for one `(repo_id, worktree, path)`
    /// in a single transaction — the same delete-then-reinsert discipline
    /// (and the same "no stable per-row identity to diff against" reason)
    /// as [`Self::replace_rails_edges`], which this method is otherwise
    /// modelled on.
    ///
    /// Deliberately NOT content-addressed, unlike `symbols`/`call_sites`:
    /// an entity's FQN depends on the PATH (via the Zeitwerk convention)
    /// and on the CHECKOUT (via that checkout's own config), so the same
    /// blob at two paths legitimately yields two different claims and a
    /// `(blob_hash, salt)` cache key would collapse them into one. There
    /// is therefore no cache-hit skip either: every visit re-derives.
    ///
    /// `zeitwerk_state` is stored per row rather than per repo because it
    /// records what the config read was worth AT INDEX TIME — the read
    /// path classes against that, not against a config that may have
    /// changed since.
    pub fn replace_entity_defs(
        &self,
        repo_id: i64,
        worktree: &str,
        path: &str,
        blob_hash: &str,
        zeitwerk_state: &str,
        defs: &[crate::entities::EntityDefClaim],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM entity_defs WHERE repo_id = ?1 AND worktree = ?2 AND path = ?3",
            params![repo_id, worktree, path],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO entity_defs
                     (repo_id, worktree, path, ordinal, fqn, kind, nesting,
                      line_start, line_end, zeitwerk_fqn, zeitwerk_state, blob_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for (ordinal, d) in defs.iter().enumerate() {
                stmt.execute(params![
                    repo_id,
                    worktree,
                    path,
                    ordinal as i64,
                    d.fqn,
                    d.kind,
                    d.nesting,
                    i64::from(d.line_start),
                    i64::from(d.line_end),
                    d.zeitwerk_fqn,
                    zeitwerk_state,
                    blob_hash,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every definition site addressed by `name` — matching the FQN the
    /// TREE proved, the FQN the Zeitwerk convention derived, or (only when
    /// neither matches anything) the LAST SEGMENT of either. The
    /// last-segment arm is what makes `kb-code entity Order` useful in a
    /// namespaced monolith; it never merges those hits (the caller groups
    /// by FQN and reports `ambiguous`), it only finds them.
    ///
    /// `LIKE` is escaped explicitly: `_` is a LIKE wildcard, and a Ruby
    /// constant may legitimately contain one (`Order_v2`), so an
    /// unescaped pattern would silently over-match. Callers pass
    /// `MAX_DEFS_PER_QUERY + 1` so the route can report truncation rather
    /// than silently capping.
    pub fn entity_defs_for_name(
        &self,
        repo_id: i64,
        worktree: Option<&str>,
        name: &str,
        limit: usize,
    ) -> Result<Vec<EntityDefRow>> {
        let exact = self.entity_defs_query(repo_id, worktree, name, None, limit)?;
        if !exact.is_empty() {
            return Ok(exact);
        }
        let suffix = format!("%::{}", like_escape(name));
        self.entity_defs_query(repo_id, worktree, name, Some(&suffix), limit)
    }

    /// V71-F1 — every `entity_defs` CLAIM in one repo, for the tree's
    /// `namespace` projection and kbc-scope/1's `ns:` atom. Deliberately a
    /// second, whole-repo read beside [`Self::entity_defs_for_name`]
    /// rather than a widened one: that query's job is to ADDRESS a name
    /// (two `LIKE` arms, a truncation limit the route reports), and this
    /// one's is to enumerate. Rows are still CLAIMS — `entities::class_for`
    /// is the only thing that turns one into a trust class, per request
    /// (crate invariant 13), which is why the live blob hash is joined here
    /// too.
    pub fn entity_defs_for_repo(
        &self,
        repo_id: i64,
        worktree: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EntityDefRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT d.worktree, d.path, d.fqn, d.kind, d.nesting, d.line_start,
                    d.line_end, d.zeitwerk_fqn, d.zeitwerk_state, d.blob_hash, f.blob_hash
             FROM entity_defs d
             LEFT JOIN files f ON f.repo_id = d.repo_id AND f.path = d.path
             WHERE d.repo_id = ?1 AND (?2 IS NULL OR d.worktree = ?2)
             ORDER BY d.fqn ASC, d.path ASC, d.ordinal ASC
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![repo_id, worktree, limit as i64], |r| {
                Ok(EntityDefRow {
                    worktree: r.get(0)?,
                    path: r.get(1)?,
                    fqn: r.get(2)?,
                    kind: r.get(3)?,
                    nesting: r.get(4)?,
                    line_start: r.get(5)?,
                    line_end: r.get(6)?,
                    zeitwerk_fqn: r.get(7)?,
                    zeitwerk_state: r.get(8)?,
                    blob_hash: r.get(9)?,
                    live_blob_hash: r.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
