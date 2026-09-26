//! Doc-lens pins, doc refs, and sync cursors.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    pub fn get_doc_lens_pin(&self, kb: &str, doc_id: &str) -> Result<Option<DocLensPin>> {
        self.lock()
            .query_row(
                "SELECT kb, doc_id, repo, repo_root, doc_hash, pinned_at
                 FROM doc_lens_pins WHERE kb = ?1 AND doc_id = ?2",
                params![kb, doc_id],
                doc_lens_pin_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// `INSERT … ON CONFLICT(kb, doc_id) DO UPDATE` — last write wins.
    pub fn put_doc_lens_pin(&self, pin: &DocLensPin) -> Result<()> {
        self.lock().execute(
            "INSERT INTO doc_lens_pins (kb, doc_id, repo, repo_root, doc_hash, pinned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(kb, doc_id) DO UPDATE SET
                 repo = excluded.repo,
                 repo_root = excluded.repo_root,
                 doc_hash = excluded.doc_hash,
                 pinned_at = excluded.pinned_at",
            params![
                pin.kb,
                pin.doc_id,
                pin.repo,
                pin.repo_root,
                pin.doc_hash,
                pin.pinned_at
            ],
        )?;
        Ok(())
    }

    /// `true` when a row was actually removed (the caller's "there WAS a pin
    /// and it is gone now" signal — the `DELETE` itself is idempotent).
    pub fn delete_doc_lens_pin(&self, kb: &str, doc_id: &str) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM doc_lens_pins WHERE kb = ?1 AND doc_id = ?2",
            params![kb, doc_id],
        )?;
        Ok(n > 0)
    }

    /// All pins, `kb` then `doc_id` ordered; `Some(kb)` scopes to one corpus.
    pub fn list_doc_lens_pins(&self, kb: Option<&str>) -> Result<Vec<DocLensPin>> {
        let conn = self.lock();
        match kb {
            Some(kb) => {
                let mut stmt = conn.prepare(
                    "SELECT kb, doc_id, repo, repo_root, doc_hash, pinned_at
                     FROM doc_lens_pins WHERE kb = ?1 ORDER BY kb, doc_id",
                )?;
                let rows = stmt
                    .query_map(params![kb], doc_lens_pin_from)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows)
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT kb, doc_id, repo, repo_root, doc_hash, pinned_at
                     FROM doc_lens_pins ORDER BY kb, doc_id",
                )?;
                let rows = stmt
                    .query_map([], doc_lens_pin_from)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows)
            }
        }
    }

    /// Move a pin from `old` to `new` in one statement (kb's moves 301 chain
    /// — kb invariant #27). No-op `Ok(false)` when there was no pin.
    /// `OR REPLACE` so a pre-existing pin on the DESTINATION id (an operator
    /// pinned the moved doc under its new id before the lens ever followed
    /// the chain) loses to the row actually being re-keyed rather than
    /// aborting the migration with a PK conflict.
    pub fn rekey_doc_lens_pin(&self, kb: &str, old: &str, new: &str) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE OR REPLACE doc_lens_pins SET doc_id = ?3
             WHERE kb = ?1 AND doc_id = ?2",
            params![kb, old, new],
        )?;
        Ok(n > 0)
    }

    // --- doc_refs — the reverse "cited by" index (DCB W3.A) --------------
    //
    // Deliberately NO `bump_generation()` anywhere in this section — same
    // rationale as reading sets / doc-lens pins above: `generation` only
    // invalidates the files/symbols search lanes' in-memory caches, which
    // `doc_refs` has nothing to do with.

    /// Replace ONE document's claims in one transaction: DELETE by
    /// `(kb, doc_id)` across EVERY `repo_id` (so a doc re-pinned to a
    /// different checkout doesn't leave stale rows behind under the old one),
    /// then insert the new set. Mirrors [`Self::create_reading_set`]'s
    /// one-transaction insert-loop shape, this crate's own precedent.
    ///
    /// Returns how many claims were written.
    pub fn replace_doc_refs(&self, w: &DocRefWrite<'_>, refs: &[NewDocRef]) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM doc_refs WHERE kb = ?1 AND doc_id = ?2",
            params![w.kb, w.doc_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO doc_refs
                    (repo_id, kb, doc_id, ordinal, kind, raw_hint, resolved_path,
                     line_start, line_end, line_state, group_key, group_label,
                     doc_title, doc_path, doc_hash, head_sha, dirty, seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            )?;
            for r in refs {
                stmt.execute(params![
                    w.repo_id,
                    w.kb,
                    w.doc_id,
                    r.ordinal,
                    r.kind,
                    r.raw_hint,
                    r.resolved_path,
                    r.line_start,
                    r.line_end,
                    r.line_state,
                    r.group_key,
                    r.group_label,
                    w.doc_title,
                    w.doc_path,
                    w.doc_hash,
                    w.head_sha,
                    w.dirty,
                    w.seen_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(refs.len())
    }

    /// Drop every claim filed under `(kb, doc_id)`, across every repo — the
    /// 404 arm (`doclens::sync`) and the moves-re-key arm both need exactly
    /// this. Returns the number of rows removed.
    pub fn delete_doc_refs_for_doc(&self, kb: &str, doc_id: &str) -> Result<usize> {
        let n = self.lock().execute(
            "DELETE FROM doc_refs WHERE kb = ?1 AND doc_id = ?2",
            params![kb, doc_id],
        )?;
        Ok(n)
    }

    /// "Which documents cite path X in repo Y" — `GET /api/doc-refs`'s own
    /// lookup, served by `idx_doc_refs_resolved_path`. Deterministic order
    /// (kb, doc_id, ordinal) so a response never depends on insert order.
    pub fn doc_refs_for_path(&self, repo_id: i64, path: &str) -> Result<Vec<DocRefRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT kb, doc_id, ordinal, kind, raw_hint, resolved_path, line_start, line_end,
                    line_state, group_key, group_label, doc_title, doc_path, doc_hash,
                    head_sha, dirty, seen_at
               FROM doc_refs
              WHERE repo_id = ?1 AND resolved_path = ?2
              ORDER BY kb, doc_id, ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id, path], |r| {
                Ok(DocRefRow {
                    kb: r.get(0)?,
                    doc_id: r.get(1)?,
                    ordinal: r.get(2)?,
                    kind: r.get(3)?,
                    raw_hint: r.get(4)?,
                    resolved_path: r.get(5)?,
                    line_start: r.get(6)?,
                    line_end: r.get(7)?,
                    line_state: r.get(8)?,
                    group_key: r.get(9)?,
                    group_label: r.get(10)?,
                    doc_title: r.get(11)?,
                    doc_path: r.get(12)?,
                    doc_hash: r.get(13)?,
                    head_sha: r.get(14)?,
                    dirty: r.get(15)?,
                    seen_at: r.get(16)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The sync SET's kb list: every corpus with at least one pin. NOT a
    /// config allowlist — a kb only enters this list by a human having pinned
    /// a checkout for one of its documents ("grows as you read"). `ORDER BY
    /// kb` so a `batch_cap` truncation always favours the same corpora rather
    /// than being iteration-order flaky.
    pub fn doc_lens_pin_kbs(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT DISTINCT kb FROM doc_lens_pins ORDER BY kb")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_doclens_sync_cursor(&self, kb: &str) -> Result<Option<DoclensSyncCursor>> {
        self.lock()
            .query_row(
                "SELECT kb, cursor, last_run_at, last_error FROM doclens_sync_cursors WHERE kb = ?1",
                params![kb],
                |r| {
                    Ok(DoclensSyncCursor {
                        kb: r.get(0)?,
                        cursor: r.get(1)?,
                        last_run_at: r.get(2)?,
                        last_error: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Upsert one kb's sync progress. `cursor = None` means "start from the
    /// beginning next time"; `last_error = None` CLEARS a previous failure
    /// (the row always reflects the most recent pass, never a stale one).
    pub fn set_doclens_sync_cursor(
        &self,
        kb: &str,
        cursor: Option<&str>,
        last_run_at: i64,
        last_error: Option<&str>,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO doclens_sync_cursors (kb, cursor, last_run_at, last_error)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(kb) DO UPDATE SET
                cursor = excluded.cursor,
                last_run_at = excluded.last_run_at,
                last_error = excluded.last_error",
            params![kb, cursor, last_run_at, last_error],
        )?;
        Ok(())
    }

    /// `POST /api/doc-lens/sync {"force": true}` — reset the watermark
    /// WITHOUT dropping the row (its `last_run_at`/`last_error` are still the
    /// operator's record of what happened last).
    pub fn clear_doclens_sync_cursor(&self, kb: &str) -> Result<()> {
        self.lock().execute(
            "UPDATE doclens_sync_cursors SET cursor = NULL WHERE kb = ?1",
            params![kb],
        )?;
        Ok(())
    }
}
