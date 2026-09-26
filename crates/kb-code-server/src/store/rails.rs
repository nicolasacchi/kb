//! Rails framework edges.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // ── PRR-N3: rails edges ─────────────────────────────────────────────
    // `crate::frameworks::{FrameworkEdge, EdgeKind, Trust}` + migration
    // `V0026__rails_edges.sql`. Content-addressed like `import_specs`/
    // `call_sites` (DELETE-then-INSERT, wholesale replace), but with one
    // deliberate difference: there is NO `has_rails_edges` cache-hit-skip.
    // `call_sites`/`type_relations` are a pure blob→blob CST walk, so an
    // unchanged blob's rows can never go stale; `rails_edges`' view/partial
    // resolution (`frameworks::rails::views`) depends on the LIVE sibling-
    // file set (which `_row.*.erb` variants exist on disk right now), so
    // re-extracting on every visit is correct — a cache-hit skip here would
    // silently miss a resolution that only became (un)ambiguous because a
    // SIBLING file changed, never this blob itself (mirrors
    // `import_graph::resolve_import_edges`'s same choice to always
    // re-resolve). `repo_id` rides on the row (unlike `call_sites`) for
    // exactly that reason — this table's rows are meaningfully repo-scoped
    // facts, not pure content-addressed ones.
    //
    // *R1 fix (v70-a1, recon rails-lens.md §6):* the DELETE scope is now
    // `(repo_id, src_path)`, not `(blob_hash, salt)`. Every read
    // (`rails_edges_by_src_path`/`_by_dst_path`/`_by_kind` below) queries
    // `repo_id + path/kind` with NO `blob_hash` filter, so a delete scoped
    // to only the INCOMING blob's own rows left the PREVIOUS blob's rows
    // live forever on every edit (duplicate/contradictory edges accumulate
    // across edits) and left phantom rows forever after a file's deletion
    // (`Store::delete_file` below now prunes this table too — see its own
    // doc). Reads stay path-keyed; only the replace-time delete changed.
    // Sibling-dependent staleness (a change to `_row.turbo_stream.erb`
    // flipping an UNCHANGED controller's `render_partial` trust from
    // `likely` to `candidate`) is a separate, deferred issue (recon R4) —
    // this fix does not touch it: nothing here re-visits a file that didn't
    // itself change.

    pub fn replace_rails_edges(
        &self,
        repo_id: i64,
        path: &str,
        blob_hash: &str,
        salt: &str,
        edges: &[crate::frameworks::FrameworkEdge],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM rails_edges WHERE repo_id = ?1 AND src_path = ?2",
            params![repo_id, path],
        )?;
        {
            // `INSERT OR REPLACE`, not plain `INSERT`: the table's
            // `UNIQUE(blob_hash, salt, ordinal)` constraint (V0026) is NOT
            // scoped by `(repo_id, src_path)` — a pre-existing schema
            // choice, unchanged here — so two DIFFERENT paths that happen
            // to share byte-identical content (same `blob_hash`) and the
            // same edge count could collide on that key. The delete above
            // only clears THIS path's rows, so a plain `INSERT` could now
            // hit a row still owned by the OTHER colliding path and fail
            // the whole transaction — turning a rare content collision
            // into a hard ingest error. `OR REPLACE` keeps this idempotent
            // instead: whichever path is (re-)indexed last wins the shared
            // slot, the same "last write wins" outcome the old blob-keyed
            // bulk delete already had, just row-scoped rather than wiping
            // every row the other path ever produced.
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO rails_edges
                    (repo_id, kind, src_path, src_line, src_symbol, dst_kind, dst_path,
                     dst_symbol, trust, blob_hash, salt, ordinal, extra_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?;
            for (ordinal, e) in edges.iter().enumerate() {
                stmt.execute(params![
                    repo_id,
                    e.kind.as_str(),
                    e.src_path,
                    e.src_line.map(|n| n as i64),
                    e.src_symbol,
                    e.dst_kind,
                    e.dst_path,
                    e.dst_symbol,
                    e.trust.as_str(),
                    blob_hash,
                    salt,
                    ordinal as i64,
                    e.extra_json,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every `rails_edges` row produced BY this path (e.g. open `routes.rb`,
    /// see every `route_action`/`route_file` edge it emits; open a
    /// controller, see every render/redirect call site).
    pub fn rails_edges_by_src_path(
        &self,
        repo_id: i64,
        src_path: &str,
    ) -> Result<Vec<crate::frameworks::FrameworkEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT kind, src_path, src_line, src_symbol, dst_kind, dst_path, dst_symbol,
                    trust, extra_json
             FROM rails_edges WHERE repo_id = ?1 AND src_path = ?2 ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id, src_path], rails_edge_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every `rails_edges` row that TARGETS this path (e.g. open a partial,
    /// see every render call site across the repo that renders it).
    pub fn rails_edges_by_dst_path(
        &self,
        repo_id: i64,
        dst_path: &str,
    ) -> Result<Vec<crate::frameworks::FrameworkEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT kind, src_path, src_line, src_symbol, dst_kind, dst_path, dst_symbol,
                    trust, extra_json
             FROM rails_edges WHERE repo_id = ?1 AND dst_path = ?2 ORDER BY src_path, ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id, dst_path], rails_edge_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every `rails_edges` row of a given `kind` (the closed rails-lens/1
    /// enum's string form, `EdgeKind::as_str()`) across the repo.
    pub fn rails_edges_by_kind(
        &self,
        repo_id: i64,
        kind: &str,
    ) -> Result<Vec<crate::frameworks::FrameworkEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT kind, src_path, src_line, src_symbol, dst_kind, dst_path, dst_symbol,
                    trust, extra_json
             FROM rails_edges WHERE repo_id = ?1 AND kind = ?2 ORDER BY src_path, ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id, kind], rails_edge_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// EVERY `rails_edges` row for `repo_id`, `(src_path, ordinal)`-ordered
    /// — the whole-repo read the `rails/1` index (V72-I1) joins over.
    ///
    /// ONE query rather than a per-noun fan-out over
    /// [`Self::rails_edges_by_kind`]: the index needs a dozen kinds at
    /// once, and the `(repo_id, kind)` index answers the bare `WHERE
    /// repo_id = ?` prefix just as well. The per-path/per-kind readers stay
    /// for the position-gated consumers (`hover`, `resolve`, `usages`),
    /// which want one file's rows and must not pay for the repo's.
    pub fn rails_edges_for_repo(
        &self,
        repo_id: i64,
    ) -> Result<Vec<crate::frameworks::FrameworkEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT kind, src_path, src_line, src_symbol, dst_kind, dst_path, dst_symbol,
                    trust, extra_json
             FROM rails_edges WHERE repo_id = ?1 ORDER BY src_path, ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id], rails_edge_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
