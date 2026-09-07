-- V73-K3 — `kbc-claim/1`: the agent-prose CLAIM register (design D18).
--
-- ONE table, and the whole point of the unit is that it is one table.
-- D18 names six renderings — commit-time explain cards, the rejected-
-- alternatives ledger, decision threads, branch stories, trail notes and
-- entity answers — and rules that each is a KIND plus a rendering, never a
-- table of its own. Six tables would be six migrations, six cascade
-- registrations and six chances for one of them to disagree with the
-- Ladder; one table with a closed `kind` vocabulary is the same posture
-- kb-code-server/CLAUDE.md invariant 9 states for `reading_sets.kind`
-- (a workspace IS a reading set) and invariant 14 restates for kbc-seq/1.
--
-- SURFACED, NEVER SCORED. A claim is agent PROSE about code. It is
-- rendered beside the fact it is about and it never becomes a ranking
-- term, a boost, a filter default or a trust class — the same law
-- `unified_inbox` and kb root invariant #10's provenance lane already
-- follow. `src/claims.rs` carries the structural pin: a source scan that
-- fails if any ranking module in this crate so much as imports it.
--
-- THE CLASS IS COMPUTED PER REQUEST AND IS NOT A COLUMN. There is no
-- `trust` here, exactly as `lane_facts` (V0030) has none and `entity_defs`
-- (V0029) has none: root invariant #2's "kb-code mints classes, nothing is
-- cached", applied to the LLM's own prose. What IS stored is the claim's
-- WITNESS — `blob_sha`, the bytes the author was looking at — and
-- `claims::ladder_state` turns that into `pinned` / `drifted{caption}` /
-- `unanchored` at read time by comparing it with the file's live blob.
--
-- `confidence` is the AGENT'S OWN DECLARATION, 0.0..=1.0, and is
-- deliberately not derived from anything: it is a number the author chose,
-- surfaced verbatim, and nothing in this daemon reads it back except to
-- print it. It is NOT a probability this crate computed and must never be
-- multiplied into anything.
--
-- `evidence_json` is a JSON array of kbc-review/1 REFS (`refs::SCHEMES` —
-- `code:`/`sym:`/`ent:`/`finding:`/`gh:`/`kb:`/`hunk:`), stored as the
-- author wrote them and re-parsed on read. Storing refs rather than
-- resolved positions is the same rule K1 states for a review document: a
-- resolved position is a per-request derivation, and persisting one would
-- make a stale answer indistinguishable from a fresh one.
--
-- NOT registered in `Store::delete_file`'s cascade, and that is a ruling,
-- not an omission. `rails_edges`/`entity_defs`/`lane_facts` are DERIVED
-- rows whose path key makes a deleted file answer forever; a claim is
-- AUTHORED content, like an `annotations` row (which `delete_file` also
-- leaves alone). Deleting an agent's reasoning because the file it was
-- about was deleted would destroy the record that explains WHY it was
-- deleted. A claim about a vanished path reads back `unanchored` with the
-- reason — an honest orphan, never a silent removal.
--
-- `review_id` is a SOFT reference with no FK, matching `canvas_sets`'s own
-- documented precedent (review GC deliberately leaves the row alive): a
-- claim written during a review outlives it.
CREATE TABLE claims (
    id            TEXT    PRIMARY KEY,          -- "clm_" + 12 hex
    repo_id       INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    -- path | sym | ent | commit | hunk | review | branch — Rust-validated
    -- (`claims::SUBJECT_KINDS`), never a SQL CHECK: this crate's house
    -- convention for a small stringly-typed vocabulary that must stay
    -- relaxable without a table rebuild (V0024's own comment, invariant 9).
    subject_kind  TEXT    NOT NULL,
    -- The address VERBATIM, in the subject kind's own grammar (a repo
    -- relative path, a `Namespace::Class#method`, an FQN, a sha, a
    -- `<path>@<ps>#<n>` hunk address, a review id, a ref name).
    subject       TEXT    NOT NULL,
    -- The repo-relative path the subject implies, when it implies one
    -- (`path` and `hunk` subjects do; `sym`/`ent`/`commit`/`review`/
    -- `branch` do not). Denormalised so the by-path read is one index seek
    -- rather than a scan that has to re-parse every address.
    subject_path  TEXT,
    review_id     INTEGER,
    -- explain | alternative | decision | story | note | answer
    kind          TEXT    NOT NULL,
    body_md       TEXT    NOT NULL,
    confidence    REAL,
    evidence_json TEXT    NOT NULL,             -- JSON array of kbc refs
    session_id    TEXT,
    model         TEXT,
    blob_sha      TEXT,
    created_at    INTEGER NOT NULL
);
CREATE INDEX idx_claims_subject ON claims(repo_id, subject_kind, subject, created_at);
CREATE INDEX idx_claims_path    ON claims(repo_id, subject_path) WHERE subject_path IS NOT NULL;
CREATE INDEX idx_claims_review  ON claims(review_id, created_at)  WHERE review_id IS NOT NULL;
