//! Transcript file state, turns, FTS search, and commit-session rows.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // --- commit_sessions (W3.2 join ladder cache) --------------------------
    //
    // No `bump_generation()` calls here either, same rationale as the
    // transcripts section below: `generation` only exists to invalidate the
    // files/symbols search lanes' in-memory caches, which the join ladder
    // has nothing to do with.

    /// Read-through cache lookup for `(repo_id, sha)` — `sha` MUST already be
    /// the canonical full hex sha (`join::local::resolve_local` disambiguates
    /// any short prefix before this is ever called). `None` on a cache miss;
    /// freshness (`trailer`/`exact` permanent, `fuzzy`/`none` TTL'd) is the
    /// caller's decision (`join::ladder::is_fresh`), not this method's — the
    /// store hands back whatever's on disk, stale or not.
    pub fn get_commit_session(&self, repo_id: i64, sha: &str) -> Result<Option<CommitSessionRow>> {
        self.lock()
            .query_row(
                "SELECT confidence, via, session_id, kb, display_name, started_at, resolved_at
                 FROM commit_sessions WHERE repo_id = ?1 AND sha = ?2",
                params![repo_id, sha],
                |r| {
                    Ok(CommitSessionRow {
                        confidence: r.get(0)?,
                        via: r.get(1)?,
                        session_id: r.get(2)?,
                        kb: r.get(3)?,
                        display_name: r.get(4)?,
                        started_at: r.get(5)?,
                        resolved_at: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Upsert one `(repo_id, sha)` resolution — always overwrites in place
    /// (a re-resolution, whether an upgrade from `none`→`fuzzy` or a
    /// same-confidence refresh that just bumps `resolved_at`, replaces every
    /// column; there is no partial-update path).
    pub fn upsert_commit_session(
        &self,
        repo_id: i64,
        sha: &str,
        row: &CommitSessionRow,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO commit_sessions
                (repo_id, sha, confidence, via, session_id, kb, display_name, started_at, resolved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(repo_id, sha) DO UPDATE SET
                confidence = excluded.confidence,
                via = excluded.via,
                session_id = excluded.session_id,
                kb = excluded.kb,
                display_name = excluded.display_name,
                started_at = excluded.started_at,
                resolved_at = excluded.resolved_at",
            params![
                repo_id,
                sha,
                row.confidence,
                row.via,
                row.session_id,
                row.kb,
                row.display_name,
                row.started_at,
                row.resolved_at,
            ],
        )?;
        Ok(())
    }

    // --- transcripts (W2.5) ------------------------------------------------
    //
    // Deliberately NO `bump_generation()` calls in this section: `generation`
    // exists to invalidate the files/symbols search lanes' in-memory caches
    // (see the field doc above), which have nothing to do with transcripts.
    // The live transcripts watcher tails frequently (every debounced flush
    // of every configured project's activity); bumping the shared counter
    // on every one of those writes would force needless cache rebuilds on
    // the UNRELATED files/symbols lanes for every daemon that has both
    // features live at once.

    /// Tail-state lookup by `src_file` (root-relative, already unique per
    /// the `V0003` schema) — `transcripts::indexer::tail_file`'s
    /// "have I seen this file before, and from where do I resume" check.
    pub fn get_transcript_file(&self, src_file: &str) -> Result<Option<TranscriptFileRow>> {
        self.lock()
            .query_row(
                "SELECT id, inode, byte_offset, mtime FROM transcript_files WHERE src_file = ?1",
                params![src_file],
                |r| {
                    Ok(TranscriptFileRow {
                        id: r.get(0)?,
                        inode: r.get(1)?,
                        byte_offset: r.get(2)?,
                        mtime: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Idempotent upsert of one file's tail state. Called BOTH to create the
    /// row on first sight (`byte_offset = 0`) and, at the end of every
    /// `tail_file` call, to record the new authoritative offset — always the
    /// SAME statement, `ON CONFLICT` decides which case applies. Returns the
    /// row id (stable across calls for a given `src_file`, since it's the
    /// table's own UNIQUE key).
    pub fn upsert_transcript_file_state(
        &self,
        project_dir: &str,
        src_file: &str,
        inode: i64,
        byte_offset: i64,
        mtime: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO transcript_files (project_dir, src_file, inode, byte_offset, mtime)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(src_file) DO UPDATE SET
                inode = excluded.inode,
                byte_offset = excluded.byte_offset,
                mtime = excluded.mtime",
            params![project_dir, src_file, inode, byte_offset, mtime],
        )?;
        let id = conn.query_row(
            "SELECT id FROM transcript_files WHERE src_file = ?1",
            params![src_file],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    /// Insert every turn in `turns` for `file_id` — `transcript_turns` plus
    /// the matching `transcript_fts` row (explicit `rowid` = the freshly
    /// inserted turn's own id), in ONE transaction. See the `V0003`
    /// migration's doc for why this hand-paired insert (not a trigger) is
    /// the whole "manual insert discipline" this schema relies on. A no-op
    /// (not an error) on an empty slice.
    pub fn insert_transcript_turns(&self, file_id: i64, turns: &[IndexedTurn]) -> Result<()> {
        if turns.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        {
            let mut ins_turn = tx.prepare(
                "INSERT INTO transcript_turns
                    (file_id, session_id, uuid, parent_uuid, ts, kind, tool_name, file_paths, is_sidechain, byte_offset, byte_len)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            let mut ins_fts =
                tx.prepare("INSERT INTO transcript_fts(rowid, text) VALUES (?1, ?2)")?;
            for it in turns {
                let file_paths_json = serde_json::to_string(&it.turn.file_paths)?;
                ins_turn.execute(params![
                    file_id,
                    it.turn.session_id,
                    it.turn.uuid,
                    it.turn.parent_uuid,
                    it.turn.ts,
                    it.turn.kind,
                    it.turn.tool_name,
                    file_paths_json,
                    it.turn.is_sidechain as i64,
                    it.byte_offset,
                    it.byte_len,
                ])?;
                let turn_id = tx.last_insert_rowid();
                ins_fts.execute(params![turn_id, it.turn.text])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete every `transcript_turns` row (and its paired `transcript_fts`
    /// row) for `file_id` — `tail_file`'s inode-swap-forces-a-full-reparse
    /// path, always called BEFORE the fresh re-derive so no stale hit can
    /// ever be visible even momentarily. The `transcript_fts` delete runs
    /// FIRST (its subquery reads `transcript_turns` — which must still
    /// exist to resolve the id list) inside the same transaction as the
    /// `transcript_turns` delete. Legal on the `content=''` FTS5 table only
    /// because the schema sets `contentless_delete=1` — see the migration's
    /// doc.
    pub fn delete_transcript_turns_for_file(&self, file_id: i64) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM transcript_fts WHERE rowid IN (SELECT id FROM transcript_turns WHERE file_id = ?1)",
            params![file_id],
        )?;
        tx.execute(
            "DELETE FROM transcript_turns WHERE file_id = ?1",
            params![file_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// FTS5 `MATCH` over every indexed turn, optionally scoped to `session`/
    /// `kind`, newest-first (`ts DESC`, `id DESC` as a same-millisecond
    /// tie-break) — `routes::search_transcripts`' data source. `query` is
    /// passed to sqlite's FTS5 query parser verbatim (its own boolean/
    /// prefix/NEAR syntax); a malformed query surfaces as a `StoreError`
    /// the caller can inspect (`ApiError`'s `fts5` substring check maps it
    /// to 400, everything else to 500).
    pub fn search_transcripts(
        &self,
        query: &str,
        limit: usize,
        session: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<TranscriptSearchRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tt.session_id, tt.uuid, tt.ts, tt.kind, tt.tool_name, tf.project_dir,
                    tf.src_file, tt.byte_offset, tt.byte_len, tt.is_sidechain
             FROM transcript_fts f
             JOIN transcript_turns tt ON tt.id = f.rowid
             JOIN transcript_files tf ON tf.id = tt.file_id
             WHERE f.text MATCH ?1
               AND (?2 IS NULL OR tt.session_id = ?2)
               AND (?3 IS NULL OR tt.kind = ?3)
             ORDER BY tt.ts DESC, tt.id DESC
             LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![query, session, kind, limit as i64], |r| {
                Ok(TranscriptSearchRow {
                    session_id: r.get(0)?,
                    uuid: r.get(1)?,
                    ts: r.get(2)?,
                    kind: r.get(3)?,
                    tool_name: r.get(4)?,
                    project_dir: r.get(5)?,
                    src_file: r.get(6)?,
                    byte_offset: r.get(7)?,
                    byte_len: r.get(8)?,
                    is_sidechain: r.get::<_, i64>(9)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every indexed turn for `session`, OLDEST FIRST (`ts ASC, id ASC` —
    /// the session's own NARRATIVE order, not `search_transcripts`'
    /// newest-first convention) — `sessiondiff::session_diff`'s data
    /// source (W3.5, the session diff's transcript half). Unlike
    /// `search_transcripts`, this is NOT an FTS5 `MATCH` — it returns
    /// EVERY turn for the session, matched or not, since session-diff walks
    /// the whole narrative rather than searching it. Carries `file_paths`
    /// (JSON-decoded back to a `Vec<String>` — see `insert_transcript_turns`'
    /// encode side) which `search_transcripts`/`TranscriptSearchRow` has no
    /// need for, so this is a distinct row type rather than a widened
    /// `TranscriptSearchRow`.
    pub fn transcript_turns_for_session(&self, session: &str) -> Result<Vec<TranscriptTurnRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tt.session_id, tt.uuid, tt.parent_uuid, tt.ts, tt.kind, tt.tool_name,
                    tt.file_paths, tt.is_sidechain, tf.project_dir, tf.src_file,
                    tt.byte_offset, tt.byte_len
             FROM transcript_turns tt
             JOIN transcript_files tf ON tf.id = tt.file_id
             WHERE tt.session_id = ?1
             ORDER BY tt.ts ASC, tt.id ASC",
        )?;
        let rows = stmt
            .query_map(params![session], |r| {
                let file_paths_json: String = r.get(6)?;
                Ok((
                    TranscriptTurnRow {
                        session_id: r.get(0)?,
                        uuid: r.get(1)?,
                        parent_uuid: r.get(2)?,
                        ts: r.get(3)?,
                        kind: r.get(4)?,
                        tool_name: r.get(5)?,
                        file_paths: Vec::new(),
                        is_sidechain: r.get::<_, i64>(7)? != 0,
                        project_dir: r.get(8)?,
                        src_file: r.get(9)?,
                        byte_offset: r.get(10)?,
                        byte_len: r.get(11)?,
                    },
                    file_paths_json,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(mut row, file_paths_json)| {
                row.file_paths = serde_json::from_str(&file_paths_json).unwrap_or_default();
                row
            })
            .collect())
    }

    /// Files tracked / turns indexed / total indexed bytes — `kb-code
    /// transcripts status`'s data source (`GET /api/transcripts/status`).
    /// `indexed_bytes` sums `transcript_turns.byte_len` (the raw JSONL
    /// bytes the index POINTS AT, not the FTS5 shadow tables' own on-disk
    /// footprint — the latter needs sqlite's `dbstat` virtual table, which
    /// isn't guaranteed compiled into every `libsqlite3-sys` build; the
    /// summed byte_len is a always-available, honest proxy for "how much
    /// transcript content is covered").
    pub fn transcript_stats(&self) -> Result<TranscriptStats> {
        let conn = self.lock();
        let files: i64 =
            conn.query_row("SELECT COUNT(*) FROM transcript_files", [], |r| r.get(0))?;
        let turns: i64 =
            conn.query_row("SELECT COUNT(*) FROM transcript_turns", [], |r| r.get(0))?;
        let indexed_bytes: i64 = conn.query_row(
            "SELECT COALESCE(SUM(byte_len), 0) FROM transcript_turns",
            [],
            |r| r.get(0),
        )?;
        Ok(TranscriptStats {
            files: files as u64,
            turns: turns as u64,
            indexed_bytes: indexed_bytes as u64,
        })
    }

    /// W3.4 — `provenance::why`'s UNCOMMITTED-line path: every
    /// `transcript_turns` row whose JSON-encoded `file_paths`
    /// (`transcripts::parse`'s module doc) contains `abs_path` VERBATIM as a
    /// complete JSON string element, newest-first, capped at `limit`. A
    /// plain `LIKE '%"<path>"%'` scan (`%`/`_` escaped so a path containing
    /// either can't smuggle in a stray wildcard) — the quote-delimited
    /// needle means a match can only land on a COMPLETE `file_paths` entry,
    /// never a partial-path false-positive (`".../lib.rs"` never matches a
    /// stored `".../lib.rs.bak"`, since the trailing quote after `lib.rs`
    /// would have to be part of the stored string too). No FTS index backs
    /// this column (unlike `search_transcripts`'s `text` column) — a leading
    /// wildcard can't use one anyway; acceptable for a v1, pull-only,
    /// single-operator-scale scan (see `provenance::why`'s module doc for
    /// why this is the ONE place outside the transcripts lane's own routes
    /// that reads this table).
    pub fn transcript_sessions_touching_path(
        &self,
        abs_path: &str,
        limit: usize,
    ) -> Result<Vec<TranscriptPathHit>> {
        let conn = self.lock();
        let escaped = abs_path
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let needle = format!("%\"{escaped}\"%");
        let mut stmt = conn.prepare(
            "SELECT session_id, ts FROM transcript_turns
             WHERE file_paths LIKE ?1 ESCAPE '\\'
             ORDER BY ts DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![needle, limit as i64], |r| {
                Ok(TranscriptPathHit {
                    session_id: r.get(0)?,
                    ts: r.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
