-- V0030 (V72-H4a, design of record docs/research/
-- kb-code-v7-continuum-2026-09.html §The spine P8 / §Decisions D7, Track H)
-- — `aug-lane/1`'s fact store: the two tables an augmentation lane writes.
--
-- schema-epoch: rides the SAME refinery epoch sequence as every other
-- kb-code-server migration (kb #2's `kb_core::sibling::
-- refuse_if_volume_ahead` boot guard) — no bump of its own, V0030 IS the
-- next epoch. Every statement is O(1) DDL on two brand-new EMPTY tables:
-- nothing scans, nothing backfills, so this adds no boot latency (the
-- V0027/V0028/V72-B0 lesson — no whole-corpus work between `Store::open`
-- and the `TcpListener::bind`).
--
-- ## What a lane fact IS, and what it is deliberately NOT
--
-- A fact is a CLAIM a tool made about `(path, blob_sha)` at a moment.
-- There is NO trust-class column here, on purpose, and adding one would
-- be the defect this design exists to prevent: the class is
-- `min(lane ceiling, per-fact cap, anchor state)` computed PER REQUEST by
-- `lanes::classing::class_for`, so a fact whose blob has since moved can
-- never be read back as fresh. That is root invariant #2's "kb-code mints
-- classes, nothing is cached", the same posture `entity_defs` (V0029) and
-- the lsp-live tier already take.
--
-- `sha_source` is the honesty bit that makes `exact` reachable at all:
--   'tool'             — the PRODUCER named the blob it computed against
--                        (a SARIF run pinned to a revision, a derived
--                        git lane naming the HEAD blob it read, a
--                        blob-guarded lip round trip).
--   'mirror_at_ingest' — the producer named NO blob and the daemon
--                        attributed the mirror's current one at ingest
--                        time. Structurally weaker: the tool may have run
--                        against different bytes. Capped at `likely`,
--                        never `exact`.
--
-- `snippet` is the anchor the carry-forward Ladder re-resolves against
-- (`annotations::anchor_for_line` + `annotations::resolve`, the SAME
-- helper review comments and findings ride — there is exactly one ladder
-- in this crate and this table feeds it rather than growing a second).
-- It is captured at INGEST, and ONLY when the fact's blob equals the
-- mirror's blob for that path at that moment — i.e. only when the daemon
-- can read the very bytes the fact is about. A NULL snippet on a
-- line-anchored fact is therefore not a gap to paper over: once the blob
-- moves, that fact is an honest orphan, because nothing in this daemon
-- knows what text it was pointing at.
--
-- ## Retention
--
-- A run and its facts age out TOGETHER (`lanes::gc`), on the per-lane
-- retention days from `[lanes] retention_days`. The sweep is paged and
-- background — never on the bind path, never one long transaction (the
-- V72-B0 rules, restated for a table that grows with every ingest).
-- `mutations` deliberately has no retention (a ledger that prunes itself
-- is not a ledger); a lane fact is the opposite kind of row — a cache of
-- someone else's output — and unbounded growth of it is a resource bug.

CREATE TABLE lane_runs (
    run_id        TEXT    PRIMARY KEY,
    lane          TEXT    NOT NULL,
    repo_id       INTEGER NOT NULL REFERENCES repos(id),
    tool          TEXT    NOT NULL,
    tool_version  TEXT,
    -- The argv the OPERATOR ran, already redacted by the CLI that ran it.
    -- Recorded as evidence, never re-executed and never parsed: this
    -- daemon spawns nothing but git (kb-code-server CLAUDE.md invariant
    -- 10), so a stored argv is a provenance string, not a command.
    argv_redacted TEXT,
    started_at    INTEGER,
    finished_at   INTEGER,
    fact_count    INTEGER NOT NULL DEFAULT 0,
    -- 'cli'    — an operator-box tool run, POSTed over the loopback-only
    --            ingest route.
    -- 'daemon' — this daemon derived the facts itself from git.
    origin        TEXT    NOT NULL CHECK (origin IN ('cli', 'daemon')),
    ingested_at   INTEGER NOT NULL
);

-- `GET /api/lanes` reports per-lane fact counts and the newest ingest.
CREATE INDEX idx_lane_runs_lane ON lane_runs (lane, repo_id, ingested_at DESC);

CREATE TABLE lane_facts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    lane          TEXT    NOT NULL,
    repo_id       INTEGER NOT NULL REFERENCES repos(id),
    path          TEXT    NOT NULL,
    blob_sha      TEXT    NOT NULL,
    sha_source    TEXT    NOT NULL CHECK (sha_source IN ('tool', 'mirror_at_ingest')),
    -- 1-based inclusive line range; BOTH NULL for a file-level fact.
    range_start   INTEGER,
    range_end     INTEGER,
    -- The Ladder's anchor text for `range_start`; see the header.
    snippet       TEXT,
    kind          TEXT    NOT NULL,
    value_json    TEXT    NOT NULL,
    severity      TEXT,
    source_run_id TEXT    NOT NULL REFERENCES lane_runs(run_id),
    produced_at   INTEGER NOT NULL,
    ingested_at   INTEGER NOT NULL
);

-- `GET /api/lanes/facts?repo=&path=` — the per-request read, and the
-- `(repo_id, path)` scope every ingest REPLACES (the `replace_rails_edges`
-- rule, invariant 12(a): the delete must be keyed the way the read is, or
-- a previous blob's rows answer forever).
CREATE INDEX idx_lane_facts_path ON lane_facts (repo_id, path, blob_sha);
-- `GET /api/lanes` / `GET /api/lanes/summary` per-lane rollups.
CREATE INDEX idx_lane_facts_lane ON lane_facts (lane, repo_id);
-- The retention sweep deletes a run's facts by run id.
CREATE INDEX idx_lane_facts_run ON lane_facts (source_run_id);
