-- PRR-R1 ("The PR Room" — kb v0.39, T2) — PR binding on `reviews` + the
-- `review_findings` sibling table. Spec of record: /tmp/design-server.md
-- §1.2+§1.3, arbitrated by the milestone plan (ticklish-sleeping-avalanche):
-- severity = blocker|concern|ok (the generator's real 3-set, NOT the design
-- doc's original nit/praise/info sketch), disposition =
-- agree|dispute|waive|fix-later, this migration number (V0024) fixed by the
-- plan regardless of build order among W1's parallel units.
--
-- Both halves are purely ADDITIVE — every existing `reviews` row (V0014/
-- V0023) reads back with every new column NULL, exactly as it did before
-- this migration (mirrors V0008's/V0023's own byte-for-byte pin; see
-- `store::tests::legacy_review_row_reads_back_with_pr_binding_report_and_
-- verdict_publish_columns_null`). `review_findings` is a brand-new table —
-- there is no pre-migration shape to preserve for it.

-- Part 1 — PR binding + artifact hint + agent-authored report + verdict
-- publish record, all as columns on `reviews` (not a side table — see the
-- design doc's §1.2 rationale: nothing today needs a relational query over
-- these, every consumer reads the whole snapshot at once, and promoting to
-- a table later is cheap if that ever changes).
ALTER TABLE reviews ADD COLUMN pr_number          INTEGER;
ALTER TABLE reviews ADD COLUMN pr_repo_slug       TEXT;     -- "owner/name" from git origin at bind time
ALTER TABLE reviews ADD COLUMN pr_head_sha        TEXT;     -- GitHub-reported head sha, snapshot at last (re)fetch
ALTER TABLE reviews ADD COLUMN pr_meta_json       TEXT;     -- title/author/branches/draft/labels/checks snapshot
ALTER TABLE reviews ADD COLUMN pr_meta_fetched_at INTEGER;  -- unix secs of the snapshot; NULL = never fetched successfully
-- The artifact<->review join (design doc §4.2) — a HINT only, never
-- resolved/verified at write time (kb-code has no business validating a kb
-- doc id against a schema it doesn't own). Set via the EXISTING
-- `PATCH /api/reviews/{id}` route (a later phase's job), not a new route.
ALTER TABLE reviews ADD COLUMN artifact_hint_kb   TEXT;
ALTER TABLE reviews ADD COLUMN artifact_hint_id   TEXT;
-- Review report -- verdict/risk/summary AUTHORED BY AN AGENT, distinct from
-- kb-code's own behavioral `risk` composite and from `verdict` (comment|
-- approve|request-changes). One owner writes this WHOLESALE, never a
-- partial-field SQL query -- same reasoning as `pr_meta_json`.
ALTER TABLE reviews ADD COLUMN report_json        TEXT;
ALTER TABLE reviews ADD COLUMN report_updated_at  INTEGER;
-- Verdict publish record (mirrors the per-finding one below) -- advisory
-- only (design doc Risk #5): kb-code cannot verify a `gh` call actually
-- ran, this is just what the agent told it happened after the fact.
ALTER TABLE reviews ADD COLUMN verdict_published_at  INTEGER;
ALTER TABLE reviews ADD COLUMN verdict_published_url TEXT;

CREATE UNIQUE INDEX idx_reviews_pr_binding ON reviews(repo, pr_number)
    WHERE pr_number IS NOT NULL;

-- Part 2 -- findings vocabulary, sibling to `annotations`, 1:1 on
-- `annotation_id` -- mirrors `annotation_suggestions`'s existing shape
-- (V0008/V0023) rather than adding six nullable columns to the shared,
-- high-traffic `annotations` table for the benefit of one intent value.
-- Each finding owns exactly one `annotations` row (the anchor, the
-- exact->fuzzy->snippet-guard resolution ladder, thread replies, and an
-- optional suggestion all come for free -- zero new code in
-- `review_comments.rs`/`suggestions.rs`). `review_id` is denormalized from
-- the annotation's own `review_id` for cheap review-scoped queries (findings
-- list, inbox) without a join, kept in sync in the SAME store transaction
-- that writes the annotation -- same convention `annotation_suggestions`
-- already follows for its own independent timestamps.
--
-- Vocabularies (`severity`, `location_kind`, `disposition`,
-- `published_state`, `superseded_reason`) are stringly-typed and
-- ROUTE-validated, never SQL-`CHECK`ed -- the established convention (see
-- V0008's own comment: "matching this table's existing convention of
-- leaving ... every other flag/enum-ish column unconstrained at the SQL
-- layer"). `superseded` follows invariant #10's MI-W2.3 soft-forget
-- precedent: a finding absent from a re-import is TOMBSTONED, never
-- hard-deleted -- disposition history and thread replies survive.
CREATE TABLE review_findings (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    review_id          INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    annotation_id      TEXT    NOT NULL UNIQUE REFERENCES annotations(id),
    slug               TEXT    NOT NULL,   -- "f-dedup-race" -- stable, author-supplied, unique per review
    severity           TEXT    NOT NULL,   -- blocker | concern | ok
    category           TEXT    NOT NULL,   -- free text, mirrors the generator template's category label
    -- Structured location -- see `store`'s `derive_finding_anchor` doc for
    -- the location_kind -> annotations-anchor ladder this feeds.
    -- `location_lines` is always the ground truth for DISPLAY; the derived
    -- `annotations` anchor is only a best-effort input to carry-forward
    -- resolution, never re-surfaced as the authoritative citation.
    location_kind      TEXT    NOT NULL,   -- single | range | multi | whole_file
    location_path      TEXT    NOT NULL,
    location_lines      TEXT,              -- JSON array of line numbers; NULL for whole_file
    location_removed   INTEGER NOT NULL DEFAULT 0,  -- cited line/file was DELETED by this diff
    title              TEXT    NOT NULL,
    rationale          TEXT    NOT NULL,
    recommendation     TEXT,
    evidence_lang      TEXT,
    evidence_source    TEXT,
    -- Scope extension (operator-ratified mid-build, human-authored
    -- findings): 'import' = created via `findings/import` (the generator
    -- agent's batch route); 'manual' = created via a later phase's
    -- single-finding create route, authored directly by a human in the
    -- browser. Closed vocab, route-validated like every other flag/enum-ish
    -- column here. Gates the reconciliation supersede step (see `store`'s
    -- `reconcile_findings_import` doc): a 'manual' row is NEVER superseded
    -- by an agent's re-import.
    origin             TEXT NOT NULL DEFAULT 'import',
    -- Identity string of whoever created this finding row -- independent
    -- of the linked `annotations.author` (the review-comment thread
    -- identity). NULL is acceptable (and typical) for origin='import' v1.
    author             TEXT,
    -- Human disposition -- the ONE thing a browsing human sets (loopback,
    -- see design doc Risk #3). NULL = undecided.
    disposition        TEXT,               -- agree | dispute | waive | fix-later | NULL
    disposition_note   TEXT,
    disposition_by     TEXT,
    disposition_at     INTEGER,
    content_updated_at INTEGER,            -- last time severity/rationale/etc changed on a carry-forward re-import
    -- Publish tracking -- advisory only (design doc Risk #5): the guard is
    -- "the export route filters out published_state='published' findings by
    -- default," not a lock.
    published_state    TEXT NOT NULL DEFAULT 'unpublished', -- unpublished | published
    published_at       INTEGER,
    published_url      TEXT,
    -- Soft-forget precedent (invariant #10, MI-W2.3): never hard-deleted --
    -- still readable, still `include_superseded=true`-visible, just
    -- excluded from the default findings list and from export by default.
    superseded         INTEGER NOT NULL DEFAULT 0,
    superseded_at       INTEGER,
    superseded_reason  TEXT,               -- "not_in_reimport" (closed vocab, route-owned)
    import_batch_id    TEXT NOT NULL,      -- groups findings landed by one import call
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_review_findings_review_slug ON review_findings(review_id, slug);
CREATE INDEX idx_review_findings_review ON review_findings(review_id, superseded);
CREATE INDEX idx_review_findings_disposition
    ON review_findings(review_id, disposition) WHERE superseded = 0;
