-- RP-track — per-section reading capture. Today the history 'open' row holds
-- only a single furthest-scroll position (scroll_y_max): how FAR the reader
-- got, not whether they READ vs skimmed, where they got stuck, or which
-- sections held their attention. This table records, per artifact VISIT, one
-- row per heading-delimited section the reader's viewport entered, with the
-- accumulated active dwell time. Classification (read/skim/unseen) and the
-- cross-visit summary are derived at read time in kb_core::reading.
--
-- Lifecycle: a section row is a CHILD of the history 'open' row via
-- visit_id ... ON DELETE CASCADE (foreign_keys is ON, sqlite.rs). It shares
-- history's lifecycle, which is DELIBERATELY DIFFERENT from snapshots /
-- sessions / bookmarks: history is append-only and OUTLIVES artifact
-- deletion (the indexer's unlink pass does NOT touch history — invariant #8),
-- so the indexer must NOT delete reading_sections either. These rows vanish
-- ONLY when their parent visit is purged (history_purge / purge_kb_data).
-- artifact_id is denormalized purely so reading_sections_for_artifact /
-- reading_latest_for_artifact can aggregate without a JOIN; it is a query
-- convenience, NOT a lifecycle key — never wire it into the delete pass.
--
-- Cumulative + idempotent model: the iframe runtime holds CUMULATIVE
-- per-visit dwell_ms/enters (seeded on a 30-min visit resume from the open
-- response so a remount can't reset them), and the server UPSERTs with
-- max() so a re-sent beacon — or a remount that restored prior state —
-- can never double-count or regress. first_at/last_at bound the section's
-- observed window. words/content_px are measured client-side at read time
-- (words = layout-independent text estimate; content_px = pixel height
-- between this heading and the next) and feed classification + the heatmap.
--
-- section_id is the runtime's LIVE DOM heading id (the author's `id`, or the
-- `kb-h-<slug>` / `kb-h-h<N>` fallback buildToc assigns) — the SAME id the
-- TOC mini-spy renders, so the SPA heatmap can join section rows to TOC rows
-- by id. Never re-derive it server-side from parsed headings.

CREATE TABLE reading_sections (
    id           INTEGER PRIMARY KEY,                 -- rowid; rows deleted only in bulk purge
    visit_id     INTEGER NOT NULL REFERENCES history(id) ON DELETE CASCADE,
    artifact_id  TEXT NOT NULL,                       -- denormalized; query convenience only
    section_id   TEXT NOT NULL,                       -- live DOM heading id (joins the TOC)
    section_idx  INTEGER NOT NULL,                    -- 0-based document order
    section_text TEXT NOT NULL,                       -- heading text at read time
    level        INTEGER NOT NULL,                    -- 1..3 (h1/h2/h3)
    words        INTEGER NOT NULL DEFAULT 0,          -- estimated words in the section's range
    content_px   INTEGER NOT NULL DEFAULT 0,          -- pixel height (next.top - this.top)
    dwell_ms     INTEGER NOT NULL DEFAULT 0,          -- cumulative active dwell, milliseconds
    enters       INTEGER NOT NULL DEFAULT 0,          -- times the viewport entered the section
    first_at     INTEGER NOT NULL,                    -- unix epoch seconds, first observation
    last_at      INTEGER NOT NULL,                    -- unix epoch seconds, latest observation
    UNIQUE(visit_id, section_id)
);
CREATE INDEX idx_reading_visit ON reading_sections(visit_id);
CREATE INDEX idx_reading_artifact ON reading_sections(artifact_id, section_idx);

-- Additive per-visit roll-ups on the existing history 'open' row.
-- active_ms is the visit's total ACTIVE reading time (excludes idle +
-- hidden-tab time, unlike updated_at - started_at). last_section is the
-- stop-point: the section the reader was in when they left the visit.
ALTER TABLE history ADD COLUMN active_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE history ADD COLUMN last_section TEXT;
