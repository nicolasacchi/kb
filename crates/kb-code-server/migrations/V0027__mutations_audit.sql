-- V0027 (V70-A2, SEC-20) — the append-only mutations audit ledger.
--
-- kb-code has ONE identity: "loopback, or the shared bearer"
-- (`lib.rs`'s auth wiring). In prod that bearer is injected by a Traefik
-- middleware for EVERY Authelia admin (docker-compose.yml), so a mutation
-- is attributable to a rung, not a person — and `git reflog` covers ref
-- writes only, never a suggestion apply, a viewed-state flip or a canvas
-- write. This table is the missing record: one row per mutating `/api`
-- request, success OR failure.
--
-- APPEND-ONLY BY CONTRACT: nothing in this crate UPDATEs or DELETEs a
-- `mutations` row. There is no retention sweep either — a row is ~200
-- bytes and the mutation rate of a single-operator daemon is a handful an
-- hour; a ledger that prunes itself is not a ledger.
--
-- `admission` is the rung the request was admitted on, and the CHECK
-- constraint pins the vocabulary to exactly the three sub-routers
-- `router.rs` builds:
--   'loopback'    — `transcripts::search::loopback_only`
--   'bearer'      — the ordinary `auth_bearer` `/api` router
--   'review_gate' — `review_gate::review_mutations_gate` (S2-B)
--
-- `blob_before`/`blob_after` are the working-tree blob hashes around a
-- content mutation (suggestion apply / apply-batch); NULL for every
-- mutation that does not touch a file, which is most of them. They are
-- recorded, never verified — this ledger is evidence, not a lock.
--
-- schema_epoch moves to 27 (`store::schema_epoch`, kb-sibling/1): a kbc
-- volume migrated by this binary refuses to boot under a pre-V70-A2 one
-- (`kb_core::sibling::refuse_if_volume_ahead`), which is the intended
-- rollback posture — an older binary would silently stop auditing.

CREATE TABLE mutations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_unix     INTEGER NOT NULL,
    route       TEXT    NOT NULL,
    method      TEXT    NOT NULL,
    admission   TEXT    NOT NULL CHECK (admission IN ('loopback', 'bearer', 'review_gate')),
    repo        TEXT,
    target      TEXT,
    blob_before TEXT,
    blob_after  TEXT,
    request_id  TEXT    NOT NULL,
    outcome     TEXT    NOT NULL
);

-- `GET /api/audit?since=&limit=` reads newest-first over a time window;
-- the id tiebreak keeps two mutations inside the same second ordered by
-- the sequence they were written in.
CREATE INDEX idx_mutations_ts ON mutations (ts_unix DESC, id DESC);
