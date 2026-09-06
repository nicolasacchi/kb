-- V73-K1 — `kbc-review/1`: the review DOCUMENT + findings v2.
--
-- (V0032/V0033 are pre-assigned to two other in-flight units by the
-- milestone's migration ledger — a NUMBER GAP here is deliberate and
-- expected, not a lost migration. refinery applies by version, so a gap
-- is inert; renumbering after a merge would change this file's checksum
-- and re-arm the kb-sibling/1 volume-ahead guard, which is exactly the
-- rollback trap invariant 11 records.)
--
-- Two halves, both purely ADDITIVE. Every existing `reviews`/
-- `review_findings` row reads back unchanged (the new finding columns all
-- carry a DEFAULT that reproduces today's meaning exactly: an existing
-- finding is an `issue` that does not block, cites nothing, and has no
-- fingerprint because nothing ever computed one for it — see
-- `store::ReviewFindingRow`'s own doc on why a NULL fingerprint is honest
-- rather than backfilled with a guess).
--
-- Part 1 — `review_docs`: the kbc-review/1 document (D9/D9-a). The stored
-- body is MARKDOWN, never HTML: HTML is a permanent XSS surface, cannot be
-- interdiffed across re-reviews, and its references are dead text (D9-a's
-- recorded departure from the original ask). Rendered HTML is an EXPORT,
-- produced per request through an operator template, and is never stored.
--
-- Revisions are APPEND-ONLY, keyed `(review_id, ps_number, revision)`:
-- `compose` never UPDATEs a row, it INSERTs the next revision and the
-- highest revision wins on read. That mirrors kb root invariant #8's
-- append-only `history` posture and the MI-W2.3 soft-forget precedent this
-- crate already follows for `review_findings.superseded` — a re-compose
-- that says something different must not destroy what the previous one
-- said, because a human's disposition may have been formed against it.
--
-- `doc_md` is the WHOLE document (front-matter + body) byte-for-byte as the
-- author sent it: the lossless record. `summary_md`/`risk_*`/`omitted_json`/
-- `author_json` are DENORMALISED copies of parsed front-matter fields, so a
-- list read never has to parse N documents; the document itself always
-- wins on a disagreement (the parse is re-run on every read).
--
-- Vocabularies (`schema`, `tier`, `risk_level`) are stringly-typed and
-- ROUTE-validated, never SQL-`CHECK`ed — this crate's established
-- convention (V0024's own comment; `reading_sets.kind` per
-- kb-code-server/CLAUDE.md invariant 9).
CREATE TABLE review_docs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    review_id     INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    ps_number     INTEGER NOT NULL,
    revision      INTEGER NOT NULL,   -- 1-based, append-only; highest wins
    schema        TEXT    NOT NULL,   -- "kbc-review/1"
    tier          TEXT    NOT NULL,   -- minimal | standard | full
    doc_md        TEXT    NOT NULL,   -- front-matter + body, VERBATIM
    summary_md    TEXT    NOT NULL,   -- denormalised front-matter `summary_md`
    risk_level    TEXT,               -- low | medium | high; NULL = omitted
    risk_why      TEXT,
    -- JSON array of the OPTIONAL block names absent from this revision, so
    -- a read can report the degrade explicitly instead of a reader
    -- discovering the hole (D9: "every other block optional with a stated
    -- omission degrade").
    omitted_json  TEXT    NOT NULL,
    -- The author block ({kind, model?, session_id?, considered[],
    -- not_considered[]}), stored as JSON exactly as parsed. NULL when the
    -- document carries none (only `full` requires one).
    author_json   TEXT,
    byte_len      INTEGER NOT NULL,   -- doc_md.len(), for the soft-cap report
    created_at    INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_review_docs_rev
    ON review_docs(review_id, ps_number, revision);
CREATE INDEX idx_review_docs_latest
    ON review_docs(review_id, ps_number, revision DESC);

-- Part 2 — findings v2 (D9). Five additive columns on `review_findings`.
--
-- `act` and `category` are the two AXES the v1 vocabulary conflated into
-- one three-value `severity` (blocker|concern|ok, unchanged and still the
-- severity column): what KIND of speech act this finding is, and what part
-- of the system it is about. `blocking` is the reviewer's own call and is
-- deliberately NOT derived from `severity` — "a blocker that is not
-- blocking this PR" is a real and common thing to say, and deriving it
-- would make the instrument unable to say it.
--
-- `cites_json` holds SECONDARY refs only. The PRIMARY location stays the
-- linked `annotations` row (V0024's `annotation_id`), because that is what
-- gives a finding its carry-forward ladder, its thread and its GitHub
-- export — a cite never competes with it.
--
-- `fingerprint` is a CHANGE DETECTOR, never an identity: the slug is
-- identity (D9). It is NULL on every pre-V0034 row and is never
-- backfilled — nothing computed one for those rows, and inventing one now
-- would let a re-compose silently adopt a finding it did not write.
--
-- `superseded_by` names the slug that REPLACED a tombstoned finding, and is
-- written only when the composing author declared the supersession
-- (`supersedes: ["f-3"]` on the incoming finding). It is never inferred:
-- guessing which new finding "is really" an old one is exactly the wrong-
-- exact class this crate's oracle bar forbids.
ALTER TABLE review_findings ADD COLUMN act           TEXT    NOT NULL DEFAULT 'issue';
ALTER TABLE review_findings ADD COLUMN blocking      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE review_findings ADD COLUMN cites_json    TEXT;
ALTER TABLE review_findings ADD COLUMN fingerprint   TEXT;
ALTER TABLE review_findings ADD COLUMN superseded_by TEXT;
CREATE INDEX idx_review_findings_fingerprint
    ON review_findings(review_id, fingerprint) WHERE fingerprint IS NOT NULL;

-- Part 3 — the slug LEDGER. D9's slug rule: "`f-<n>` slugs are minted once
-- per review and NEVER reused."
--
-- `review_findings` alone cannot enforce that. A tombstoned finding keeps
-- its row (so its slug is visibly taken), but a review whose findings were
-- hard-deleted with the review, or one whose highest slug belonged to a row
-- an operator removed by hand, would let the counter walk backwards and
-- re-mint a slug a human has already argued about in a GitHub thread. The
-- ledger is the monotonic record: one row per slug ever minted on this
-- review, never deleted (except with the review itself), with the ordinal
-- parsed out of an `f-<n>` slug so the next mint is `max(ordinal) + 1`.
--
-- `ordinal` is NULL for a slug that is not of the `f-<n>` shape (an
-- explicit author-supplied slug such as `f-dedup-race`, which the manual
-- `findings add` path has always allowed): such a slug is still recorded as
-- TAKEN, it just does not participate in the counter.
CREATE TABLE review_finding_slugs (
    review_id  INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    slug       TEXT    NOT NULL,
    ordinal    INTEGER,
    minted_at  INTEGER NOT NULL,
    PRIMARY KEY (review_id, slug)
);
