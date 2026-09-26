//! Lane facts and retention.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    /// Write one lane run and its facts in ONE transaction, REPLACING this
    /// lane's existing facts for every path the batch names.
    ///
    /// The replace scope is `(lane, repo_id, path)` over the batch's own
    /// paths PLUS `clear_paths` (paths the tool inspected and reported
    /// nothing for) — deliberately the same key the read
    /// (`lane_facts_for_path`) uses, which is invariant 12(a)'s rule
    /// restated: `replace_rails_edges` scoped its DELETE to
    /// `(blob_hash, salt)` while every read was path-keyed, and the
    /// previous blob's rows answered forever. Scoping to the batch's paths
    /// (rather than the whole repo) is what lets a large tool output be
    /// POSTed in several chunks without each chunk erasing the last.
    pub fn replace_lane_facts(
        &self,
        run: &LaneRunIn,
        facts: &[LaneFactIn],
        clear_paths: &[String],
    ) -> Result<LaneGcCounts> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO lane_runs (run_id, lane, repo_id, tool, tool_version, \
             argv_redacted, started_at, finished_at, fact_count, origin, ingested_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run.run_id,
                run.lane,
                run.repo_id,
                run.tool,
                run.tool_version,
                run.argv_redacted,
                run.started_at,
                run.finished_at,
                facts.len() as i64,
                run.origin,
                run.ingested_at,
            ],
        )?;

        let mut replaced: u64 = 0;
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        // The batch's own paths, PLUS the paths the tool inspected and had
        // nothing to say about (`clear_paths`). Without the second set a
        // fixed offense would live forever: a re-run simply would not
        // mention the file, and "no fact" and "no longer a fact" would be
        // indistinguishable.
        for path in facts
            .iter()
            .map(|f| f.path.as_str())
            .chain(clear_paths.iter().map(|p| p.as_str()))
        {
            if seen.insert(path) {
                replaced += tx.execute(
                    "DELETE FROM lane_facts WHERE lane = ?1 AND repo_id = ?2 AND path = ?3",
                    params![run.lane, run.repo_id, path],
                )? as u64;
            }
        }

        {
            let mut stmt = tx.prepare(
                "INSERT INTO lane_facts (lane, repo_id, path, blob_sha, sha_source, \
                 range_start, range_end, snippet, kind, value_json, severity, \
                 source_run_id, produced_at, ingested_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?;
            for f in facts {
                stmt.execute(params![
                    run.lane,
                    run.repo_id,
                    f.path,
                    f.blob_sha,
                    f.sha_source,
                    f.range_start,
                    f.range_end,
                    f.snippet,
                    f.kind,
                    f.value_json,
                    f.severity,
                    run.run_id,
                    f.produced_at,
                    run.ingested_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(LaneGcCounts {
            runs: 0,
            facts: replaced,
        })
    }

    /// Every stored fact for `(repo_id, path)`, newest run first, joined to
    /// its run's provenance. `lane` narrows to one lane. No trust class is
    /// read back because none is stored.
    pub fn lane_facts_for_path(
        &self,
        repo_id: i64,
        path: &str,
        lane: Option<&str>,
        limit: usize,
    ) -> Result<Vec<LaneFactRow>> {
        let conn = self.lock();
        let sql = "SELECT f.id, f.lane, f.path, f.blob_sha, f.sha_source, f.range_start, \
                   f.range_end, f.snippet, f.kind, f.value_json, f.severity, f.produced_at, \
                   f.ingested_at, r.run_id, r.tool, r.tool_version, r.origin \
                   FROM lane_facts f JOIN lane_runs r ON r.run_id = f.source_run_id \
                   WHERE f.repo_id = ?1 AND f.path = ?2 AND (?3 IS NULL OR f.lane = ?3) \
                   ORDER BY f.produced_at DESC, f.id ASC LIMIT ?4";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(params![repo_id, path, lane, limit as i64], |r| {
                Ok(LaneFactRow {
                    id: r.get(0)?,
                    lane: r.get(1)?,
                    path: r.get(2)?,
                    blob_sha: r.get(3)?,
                    sha_source: r.get(4)?,
                    range_start: r.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    range_end: r.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    snippet: r.get(7)?,
                    kind: r.get(8)?,
                    value_json: r.get(9)?,
                    severity: r.get(10)?,
                    produced_at: r.get(11)?,
                    ingested_at: r.get(12)?,
                    run_id: r.get(13)?,
                    tool: r.get(14)?,
                    tool_version: r.get(15)?,
                    origin: r.get(16)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Per-lane fact/run counts and newest ingest, optionally scoped to one
    /// repo. Lanes with no rows are simply absent — the route joins this
    /// against the REGISTRY, so a registered lane with nothing stored is
    /// reported with zeroes rather than being invisible.
    pub fn lane_stats(&self, repo_id: Option<i64>) -> Result<Vec<LaneStatRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT r.lane, COUNT(DISTINCT r.run_id), MAX(r.ingested_at), \
             COALESCE(SUM(r.fact_count), 0) \
             FROM lane_runs r WHERE (?1 IS NULL OR r.repo_id = ?1) GROUP BY r.lane",
        )?;
        let runs = stmt
            .query_map(params![repo_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut facts_stmt = conn.prepare(
            "SELECT lane, COUNT(*) FROM lane_facts WHERE (?1 IS NULL OR repo_id = ?1) \
             GROUP BY lane",
        )?;
        let facts: std::collections::BTreeMap<String, i64> = facts_stmt
            .query_map(params![repo_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(runs
            .into_iter()
            .map(|(lane, run_count, last)| LaneStatRow {
                facts: facts.get(&lane).copied().unwrap_or(0),
                lane,
                runs: run_count,
                last_ingest_at: last,
            })
            .collect())
    }

    /// `(lane, kind, severity)` counts over the NEWEST `scan_cap` facts for
    /// a repo. Returns the buckets plus how many rows were actually
    /// scanned, so the route can state its own bound instead of implying a
    /// corpus-wide total it did not compute.
    pub fn lane_summary(
        &self,
        repo_id: i64,
        scan_cap: usize,
    ) -> Result<(Vec<LaneSummaryRow>, i64)> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT lane, kind, severity, COUNT(*) FROM \
             (SELECT lane, kind, severity FROM lane_facts WHERE repo_id = ?1 \
              ORDER BY id DESC LIMIT ?2) \
             GROUP BY lane, kind, severity ORDER BY lane, kind, severity",
        )?;
        let rows = stmt
            .query_map(params![repo_id, scan_cap as i64], |r| {
                Ok(LaneSummaryRow {
                    lane: r.get(0)?,
                    kind: r.get(1)?,
                    severity: r.get(2)?,
                    count: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let scanned: i64 = rows.iter().map(|r| r.count).sum();
        Ok((rows, scanned))
    }

    /// ONE page of the retention sweep: up to [`LANE_GC_PAGE`] runs whose
    /// `ingested_at` is older than their lane's cutoff, and every fact
    /// belonging to them, in ONE short transaction. Returns the counts and
    /// whether a full page came back (i.e. whether to keep going).
    ///
    /// `cutoffs` is `(lane, oldest_ingested_at_to_keep)` — computed by the
    /// caller from the registry defaults plus `[lanes] retention_days`, so
    /// this method holds no policy. A lane absent from `cutoffs` is never
    /// swept.
    pub fn sweep_lane_retention_page(
        &self,
        cutoffs: &[(String, i64)],
        page: usize,
    ) -> Result<(LaneGcCounts, bool)> {
        if cutoffs.is_empty() || page == 0 {
            return Ok((LaneGcCounts::default(), false));
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut expired: Vec<String> = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT run_id FROM lane_runs WHERE lane = ?1 AND ingested_at < ?2 \
                 ORDER BY ingested_at ASC LIMIT ?3",
            )?;
            for (lane, cutoff) in cutoffs {
                if expired.len() >= page {
                    break;
                }
                let room = page - expired.len();
                let ids = stmt
                    .query_map(params![lane, cutoff, room as i64], |r| {
                        r.get::<_, String>(0)
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                expired.extend(ids);
            }
        }
        if expired.is_empty() {
            tx.commit()?;
            return Ok((LaneGcCounts::default(), false));
        }
        let mut counts = LaneGcCounts::default();
        {
            let mut del_facts = tx.prepare("DELETE FROM lane_facts WHERE source_run_id = ?1")?;
            let mut del_run = tx.prepare("DELETE FROM lane_runs WHERE run_id = ?1")?;
            for id in &expired {
                counts.facts += del_facts.execute(params![id])? as u64;
                counts.runs += del_run.execute(params![id])? as u64;
            }
        }
        tx.commit()?;
        let more = expired.len() >= page;
        Ok((counts, more))
    }

    /// Every lane fact for `(repo_id, path)`, dropped in one statement —
    /// the arm `delete_file` calls. Separate from that transaction only so
    /// a test can exercise it directly.
    pub fn delete_lane_facts_for_path(&self, repo_id: i64, path: &str) -> Result<u64> {
        let conn = self.lock();
        Ok(conn.execute(
            "DELETE FROM lane_facts WHERE repo_id = ?1 AND path = ?2",
            params![repo_id, path],
        )? as u64)
    }
}
