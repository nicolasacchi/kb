//! Comment blocks and the TODO index.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // --- comments/1 (V72-J1, migration V0032) -----------------------------
    //
    // Path-keyed derived data with a freshness stamp: rows are replaced
    // wholesale per `(repo_id, path)` on every re-extraction, and the
    // stored `(blob_sha, comments_version)` pair is what lets
    // `has_comments` skip the parse for an unchanged file. The DELETE is
    // scoped to `(repo_id, path)` — NOT to the blob — for the reason
    // kb-code-server invariant 12(a) records for `rails_edges`: every read
    // here is path-keyed, so a blob-scoped delete would leave the previous
    // blob's rows answering queries forever after an edit.

    /// Are `path`'s comment rows already derived from exactly this blob
    /// and this taxonomy+grammar version? A hit skips the whole
    /// tree-sitter pass in `ingest::index_file`.
    pub fn has_comments(
        &self,
        repo_id: i64,
        path: &str,
        blob_sha: &str,
        comments_version: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT 1 FROM comments
             WHERE repo_id = ?1 AND path = ?2 AND blob_sha = ?3 AND comments_version = ?4
             LIMIT 1",
        )?;
        let hit: Option<i64> = stmt
            .query_row(params![repo_id, path, blob_sha, comments_version], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(hit.is_some())
    }

    /// Replace every `comments` row for `(repo_id, path)` in one
    /// transaction. An empty `blocks` slice clears the file's rows AND
    /// leaves nothing to prove the file was scanned — which is correct:
    /// a file with no comments is re-scanned on its next visit, and the
    /// scan is one already-warm tree-sitter parse.
    pub fn replace_comments(
        &self,
        repo_id: i64,
        path: &str,
        blob_sha: &str,
        comments_version: &str,
        blocks: &[NewComment],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM comments WHERE repo_id = ?1 AND path = ?2",
            params![repo_id, path],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO comments (
                     repo_id, path, blob_sha, comments_version, ordinal, kind,
                     keyword, keyword_text, fields_json, line_start, line_end,
                     text, text_truncated, symbol_name, symbol_kind,
                     symbol_line_start, symbol_line_end, directive_tool,
                     directive_has_reason
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                           ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            )?;
            for b in blocks {
                stmt.execute(params![
                    repo_id,
                    path,
                    blob_sha,
                    comments_version,
                    b.ordinal,
                    b.kind,
                    b.keyword,
                    b.keyword_text,
                    b.fields_json,
                    b.line_start,
                    b.line_end,
                    b.text,
                    b.text_truncated as i64,
                    b.symbol_name,
                    b.symbol_kind,
                    b.symbol_line_start,
                    b.symbol_line_end,
                    b.directive_tool,
                    b.directive_has_reason.map(i64::from),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every comment row for `repo_id`, optionally narrowed by exact
    /// `kind`, exact `keyword`, and a `path` PREFIX. Ordered by path then
    /// line — the route layer applies the state filter, the limit and the
    /// truncation flag, because `state` is computed per request and no
    /// column holds it.
    pub fn list_comments(
        &self,
        repo_id: i64,
        kind: Option<&str>,
        keyword: Option<&str>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CommentRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path, blob_sha, comments_version, ordinal, kind, keyword,
                    keyword_text, fields_json, line_start, line_end, text,
                    text_truncated, symbol_name, symbol_kind, symbol_line_start,
                    symbol_line_end, directive_tool, directive_has_reason
             FROM comments
             WHERE repo_id = ?1
               AND (?2 IS NULL OR kind = ?2)
               AND (?3 IS NULL OR keyword = ?3)
               AND (?4 IS NULL OR path LIKE (?4 || '%'))
             ORDER BY path ASC, line_start ASC, ordinal ASC
             LIMIT ?5",
        )?;
        let rows = stmt
            .query_map(
                params![repo_id, kind, keyword, path_prefix, limit as i64],
                comment_row_from,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The TRUE count of rows matching the same SQL-side filters
    /// `list_comments` applies — the honest denominator behind a page,
    /// taken from the database rather than inferred from the scan.
    pub fn comment_count(
        &self,
        repo_id: i64,
        kind: Option<&str>,
        keyword: Option<&str>,
        path_prefix: Option<&str>,
    ) -> Result<i64> {
        let conn = self.lock();
        let n = conn.query_row(
            "SELECT COUNT(*) FROM comments
             WHERE repo_id = ?1
               AND (?2 IS NULL OR kind = ?2)
               AND (?3 IS NULL OR keyword = ?3)
               AND (?4 IS NULL OR path LIKE (?4 || '%'))",
            params![repo_id, kind, keyword, path_prefix],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Every comment row for one exact path — the per-file gutter feed.
    pub fn comments_for_file(&self, repo_id: i64, path: &str) -> Result<Vec<CommentRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path, blob_sha, comments_version, ordinal, kind, keyword,
                    keyword_text, fields_json, line_start, line_end, text,
                    text_truncated, symbol_name, symbol_kind, symbol_line_start,
                    symbol_line_end, directive_tool, directive_has_reason
             FROM comments
             WHERE repo_id = ?1 AND path = ?2
             ORDER BY line_start ASC, ordinal ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, path], comment_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// `(kind, count)` over every comment row in `repo_id` — a GROUP BY,
    /// never a full read into memory (a monolith carries tens of
    /// thousands of these).
    pub fn comment_kind_counts(&self, repo_id: i64) -> Result<Vec<(String, i64)>> {
        self.comment_group_counts(repo_id, "kind", false)
    }

    /// `(keyword, count)` over the annotation rows in `repo_id`.
    pub fn comment_keyword_counts(&self, repo_id: i64) -> Result<Vec<(String, i64)>> {
        self.comment_group_counts(repo_id, "keyword", true)
    }

    /// Total comment rows in `repo_id` — the honest denominator behind
    /// every bounded count the summary route reports.
    pub fn comment_total(&self, repo_id: i64) -> Result<i64> {
        let conn = self.lock();
        let n = conn.query_row(
            "SELECT COUNT(*) FROM comments WHERE repo_id = ?1",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Every DIRECTIVE row whose suppression carries no reason, and every
    /// ANNOTATION row carrying smart_todo fields — the two state lanes the
    /// summary can count WITHOUT running `git blame`. Bounded by `limit`.
    pub fn comment_state_candidates(&self, repo_id: i64, limit: usize) -> Result<Vec<CommentRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path, blob_sha, comments_version, ordinal, kind, keyword,
                    keyword_text, fields_json, line_start, line_end, text,
                    text_truncated, symbol_name, symbol_kind, symbol_line_start,
                    symbol_line_end, directive_tool, directive_has_reason
             FROM comments
             WHERE repo_id = ?1
               AND (directive_has_reason = 0 OR fields_json IS NOT NULL)
             ORDER BY path ASC, line_start ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![repo_id, limit as i64], comment_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The Phase-N `GET /api/todos` view, now a FILTERED READ of
    /// `comments/1` rather than its own table: annotation rows whose
    /// keyword is in the legacy TODO family, ordered by path then line.
    ///
    /// The signature and the row shape are unchanged from the deleted
    /// `todo_items` implementation, so `routes::list_todos` and both
    /// `tree::sources` decoration lanes are untouched. Two row-set
    /// changes are documented on that route: outline-tier languages
    /// (yaml/toml) are now scanned, and a two-marker line reports the
    /// LEFTMOST keyword.
    pub fn list_todo_items(
        &self,
        repo_id: i64,
        marker: Option<&str>,
        path_prefix: Option<&str>,
    ) -> Result<Vec<TodoItemRow>> {
        let family = crate::comments::keywords::TODO_FAMILY;
        let placeholders = family
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 4))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT path, line_start, keyword, COALESCE(keyword_text, '')
             FROM comments
             WHERE repo_id = ?1
               AND kind = 'annotation'
               AND keyword IS NOT NULL
               AND (?2 IS NULL OR keyword = ?2)
               AND (?3 IS NULL OR path LIKE (?3 || '%'))
               AND keyword IN ({placeholders})
             ORDER BY path ASC, line_start ASC"
        );
        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        let mut binds: Vec<&dyn rusqlite::ToSql> = vec![&repo_id, &marker, &path_prefix];
        for kw in family {
            binds.push(kw);
        }
        let rows = stmt
            .query_map(binds.as_slice(), |r| {
                Ok(TodoItemRow {
                    path: r.get(0)?,
                    line: r.get(1)?,
                    marker: r.get(2)?,
                    text: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// One `comments` row (V72-J1, migration V0032).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentRow {
    pub path: String,
    pub blob_sha: String,
    pub comments_version: String,
    pub ordinal: i64,
    pub kind: String,
    pub keyword: Option<String>,
    pub keyword_text: Option<String>,
    /// `keywords::SmartTodoFields` as JSON, or `None`.
    pub fields_json: Option<String>,
    pub line_start: i64,
    pub line_end: i64,
    pub text: String,
    pub text_truncated: bool,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub symbol_line_start: Option<i64>,
    pub symbol_line_end: Option<i64>,
    pub directive_tool: Option<String>,
    /// `None` for a magic comment / build tag (nothing to justify);
    /// `Some` only for a suppression directive.
    pub directive_has_reason: Option<bool>,
}

/// One comment block to write via [`Store::replace_comments`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewComment {
    pub ordinal: i64,
    pub kind: String,
    pub keyword: Option<String>,
    pub keyword_text: Option<String>,
    pub fields_json: Option<String>,
    pub line_start: i64,
    pub line_end: i64,
    pub text: String,
    pub text_truncated: bool,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub symbol_line_start: Option<i64>,
    pub symbol_line_end: Option<i64>,
    pub directive_tool: Option<String>,
    pub directive_has_reason: Option<bool>,
}

/// The ONE row mapper every `comments` SELECT shares — the column list is
/// identical across four queries, so a reordering can never desync one of
/// them from the others.
fn comment_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<CommentRow> {
    Ok(CommentRow {
        path: r.get(0)?,
        blob_sha: r.get(1)?,
        comments_version: r.get(2)?,
        ordinal: r.get(3)?,
        kind: r.get(4)?,
        keyword: r.get(5)?,
        keyword_text: r.get(6)?,
        fields_json: r.get(7)?,
        line_start: r.get(8)?,
        line_end: r.get(9)?,
        text: r.get(10)?,
        text_truncated: r.get::<_, i64>(11)? != 0,
        symbol_name: r.get(12)?,
        symbol_kind: r.get(13)?,
        symbol_line_start: r.get(14)?,
        symbol_line_end: r.get(15)?,
        directive_tool: r.get(16)?,
        directive_has_reason: r.get::<_, Option<i64>>(17)?.map(|v| v != 0),
    })
}
