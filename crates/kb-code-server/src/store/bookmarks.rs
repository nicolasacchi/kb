//! Bookmarks and branch favourites.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // --- bookmarks (Phase N) ---------------------------------------------
    //
    // Deliberately NO `bump_generation()` — bookmarks are operator-owned
    // places, not files/symbols search-lane inputs.

    /// Insert a bookmark. When `mnemonic` is `Some`, any existing bookmark
    /// in the same `repo` that already owns that mnemonic is DELETED first
    /// (vim-style mark reassignment — "delete-then-set"). Returns the new
    /// row's `id`.
    pub fn create_bookmark(
        &self,
        repo: &str,
        path: &str,
        line: i64,
        mnemonic: Option<&str>,
        note: Option<&str>,
        now: i64,
    ) -> Result<i64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if let Some(m) = mnemonic {
            tx.execute(
                "DELETE FROM bookmarks WHERE repo = ?1 AND mnemonic = ?2",
                params![repo, m],
            )?;
        }
        tx.execute(
            "INSERT INTO bookmarks (repo, path, line, mnemonic, note, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![repo, path, line, mnemonic, note, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Every bookmark in `repo`, mnemonic-first (non-null mnemonics ASC,
    /// then anonymous ones), then `created_at` ASC — `GET
    /// /api/bookmarks?repo=`'s data source.
    pub fn list_bookmarks(&self, repo: &str) -> Result<Vec<BookmarkRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, path, line, mnemonic, note, created_at, updated_at
             FROM bookmarks
             WHERE repo = ?1
             ORDER BY (mnemonic IS NULL), mnemonic ASC, created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![repo], bookmark_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- V75-M3: branch favourites (`branch_favourites`, V0041) ---------

    /// Every starred FULL ref in `repo`, newest star first. The set is
    /// small by construction (a star is a deliberate act), so this returns
    /// it whole rather than paginating — `branch-facts/1` needs the whole
    /// set to mark rows anyway, not just the starred page.
    pub fn list_branch_favourites(&self, repo: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT ref_name FROM branch_favourites
             WHERE repo = ?1
             ORDER BY created_at DESC, ref_name ASC",
        )?;
        let rows = stmt
            .query_map(params![repo], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Star (`on = true`) or unstar a full ref. IDEMPOTENT in both
    /// directions — starring twice is one row, unstarring an unstarred ref
    /// is a no-op — so the route needs no read-modify-write and a retried
    /// request can never 409. Returns whether the row set actually
    /// changed, which is what the route reports back.
    pub fn set_branch_favourite(
        &self,
        repo: &str,
        ref_name: &str,
        on: bool,
        now: i64,
    ) -> Result<bool> {
        let conn = self.lock();
        let changed = if on {
            conn.execute(
                "INSERT INTO branch_favourites (repo, ref_name, created_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(repo, ref_name) DO NOTHING",
                params![repo, ref_name, now],
            )?
        } else {
            conn.execute(
                "DELETE FROM branch_favourites WHERE repo = ?1 AND ref_name = ?2",
                params![repo, ref_name],
            )?
        };
        Ok(changed > 0)
    }

    /// Single bookmark lookup by id.
    pub fn get_bookmark(&self, id: i64) -> Result<Option<BookmarkRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo, path, line, mnemonic, note, created_at, updated_at
                 FROM bookmarks WHERE id = ?1",
                params![id],
                bookmark_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Look up a bookmark by `(repo, mnemonic)` — the CLI's
    /// `bookmark rm <mnemonic>` resolution path.
    pub fn get_bookmark_by_mnemonic(
        &self,
        repo: &str,
        mnemonic: &str,
    ) -> Result<Option<BookmarkRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo, path, line, mnemonic, note, created_at, updated_at
                 FROM bookmarks WHERE repo = ?1 AND mnemonic = ?2",
                params![repo, mnemonic],
                bookmark_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Update whichever of `line`/`note`/`mnemonic` is provided.
    /// `mnemonic: Some(None)` clears the mnemonic; `mnemonic: None` leaves
    /// it alone; `mnemonic: Some(Some(m))` assigns `m` (delete-then-set
    /// move semantics apply — any other bookmark in the same repo owning
    /// `m` is deleted first). Returns `true` iff `id` existed.
    pub fn update_bookmark(
        &self,
        id: i64,
        line: Option<i64>,
        note: Option<Option<&str>>,
        mnemonic: Option<Option<&str>>,
        updated_at: i64,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let repo: Option<String> = tx
            .query_row(
                "SELECT repo FROM bookmarks WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(repo) = repo else {
            return Ok(false);
        };
        if let Some(Some(m)) = mnemonic {
            tx.execute(
                "DELETE FROM bookmarks WHERE repo = ?1 AND mnemonic = ?2 AND id != ?3",
                params![repo, m, id],
            )?;
        }
        // Build the SET clause from the present fields. Always stamp
        // updated_at. Note/mnemonic use the double-option shape so an
        // explicit null clears (sets SQL NULL) rather than leaving alone.
        let n = tx.execute(
            "UPDATE bookmarks SET
                line = COALESCE(?2, line),
                note = CASE WHEN ?3 != 0 THEN ?4 ELSE note END,
                mnemonic = CASE WHEN ?5 != 0 THEN ?6 ELSE mnemonic END,
                updated_at = ?7
             WHERE id = ?1",
            params![
                id,
                line,
                if note.is_some() { 1i64 } else { 0i64 },
                note.flatten(),
                if mnemonic.is_some() { 1i64 } else { 0i64 },
                mnemonic.flatten(),
                updated_at,
            ],
        )?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Hard-delete a bookmark by id. Returns `true` iff `id` existed.
    pub fn delete_bookmark(&self, id: i64) -> Result<bool> {
        let n = self
            .lock()
            .execute("DELETE FROM bookmarks WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }
}
