-- PRR-N12 (N1) -- scip_runs: an APPEND-ONLY log of successful
-- `POST /api/scip/ingest` calls, one row per call, stamped inside
-- `scip::scip_ingest_route` after it resolves the repo -- a `git
-- rev-parse HEAD` at the repo path the route already has open (see that
-- module's doc). `GET /api/repos`'s `ScipStatus` (staleness surfacing)
-- reads only the LATEST row per `repo_id` (`ORDER BY ingested_at DESC`)
-- to compare `head_sha_at_ingest` against the repo's CURRENT head.
--
-- Mirrors `file_opens`' (V0002) append-only-log shape: no `id` PRIMARY
-- KEY, a covering index instead, queried via ORDER-BY-LIMIT rather than
-- an update-in-place row -- an operator can `scip run` the same repo
-- many times; every run is its own row, never overwritten.
CREATE TABLE scip_runs (
    repo_id        INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    head_sha       TEXT NOT NULL,
    ingested_at    INTEGER NOT NULL,
    docs_accepted  INTEGER NOT NULL
);
CREATE INDEX idx_scip_runs_repo_ingested ON scip_runs(repo_id, ingested_at DESC);
