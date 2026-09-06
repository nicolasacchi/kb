import { memo, type CSSProperties, type ReactNode } from "react";
import { Link } from "react-router-dom";
import type { SearchHit, SearchMode } from "../../api/client";
import { artifactHref } from "../../lib/artifactHref";
import { recordSearch } from "../../api/history";
import { pathToTags, relativeAge, tagColor } from "../../lib/derive";
import { Icon } from "../icons";
import TagPill from "../TagPill";
import ReadingChip from "../ReadingChip";
import type { Progress } from "../../hooks/useReadingProgress";
import { ScoreExplain, type ScoreTerm } from "../ScoreExplain";

// W1.search — Batch 1 (W1.C) widens the wire `Hit` with a query-time
// match snippet and the pre-fusion per-arm ranks (both 0-based, absent
// when that arm didn't run or didn't return the hit). This worktree
// doesn't yet have the regenerated bindings for those fields (see the
// phase brief's wire caveat) — intersected on locally until
// `web/src/api/generated/Hit.ts` catches up, then this type folds away.
type HitScoreExt = {
  snippet?: string | null;
  bm25_rank?: number | null;
  vec_rank?: number | null;
};

type Glyph = { I: (p: { className?: string }) => JSX.Element; n: number | null };

// Capability glyphs against the wire Hit. Mirrors CapabilityGlyphs.glyphsFor
// (which is typed against the wider DocSummary); kept local so we read Hit
// fields directly rather than casting a Hit through DocSummary.
function glyphsForHit(h: SearchHit): Glyph[] {
  const items: Glyph[] = [];
  if (h.svg_count && h.svg_count > 0) items.push({ I: Icon.Spark, n: h.svg_count });
  if (h.code_block_count && h.code_block_count > 0)
    items.push({ I: Icon.Code, n: h.code_block_count });
  if (h.table_count && h.table_count > 0)
    items.push({ I: Icon.Table, n: h.table_count });
  if (h.has_canvas || h.has_form || h.has_animation || h.has_details || h.has_drag)
    items.push({ I: Icon.Spark, n: null });
  if (h.longread) items.push({ I: Icon.BookOpen, n: null });
  return items.slice(0, 4);
}

// Wrap query terms (≥2 chars) in <mark> for a lightweight client-side
// snippet highlight over the server's doc summary. Dumb case-insensitive
// substring match — a visual aid, not a relevance signal (the server has no
// match-highlighted snippet; see plan).
function highlight(text: string, query: string): ReactNode {
  const terms = query
    .trim()
    .split(/\s+/)
    .filter((t) => t.length >= 2);
  if (terms.length === 0) return text;
  const escaped = terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  const re = new RegExp(`(${escaped.join("|")})`, "gi");
  const lower = new Set(terms.map((t) => t.toLowerCase()));
  return text.split(re).map((part, i) =>
    part && lower.has(part.toLowerCase()) ? (
      <mark key={i} className="kb-hl">
        {part}
      </mark>
    ) : (
      part
    ),
  );
}

function folderOf(rel: string): string {
  const i = rel.lastIndexOf("/");
  return i > 0 ? rel.slice(0, i) : "";
}

export type SearchResultCardProps = {
  hit: SearchHit;
  kb: string;
  query: string;
  // Result-set max score, to normalize the score bar (0..1). 0 hides it.
  scoreMax: number;
  // Latest-visit reading progress (single-kb scope only; omitted federated).
  progress?: Progress;
  // Show the originating corpus name (federated scope).
  showCorpus?: boolean;
  // W1.search — search scorechip context: the mode this hit was ranked
  // under, and its 1-based position within the list it's rendered in
  // (client-side, always available — distinct from the wire's
  // bm25_rank/vec_rank, which are the per-arm ranks BEFORE RRF fusion).
  mode?: SearchMode;
  rank?: number;
  total?: number;
};

// Track F — a rich search result. Reuses the gallery card's visual
// vocabulary (accent spine, tag pills, capability glyphs, reading chip,
// relative age) over the wire Hit, plus search-only chrome: a relevance
// score bar and a query-term-highlighted snippet. A real anchor, so
// cmd/ctrl/middle-click opens the artifact in a new tab natively;
// recordSearch fires the same H4 intent the popup does.
function SearchResultCard({
  hit,
  kb,
  query,
  scoreMax,
  progress,
  showCorpus,
  mode,
  rank,
  total,
}: SearchResultCardProps) {
  const tags =
    hit.tags && hit.tags.length > 0 ? hit.tags : pathToTags(hit.path);
  const accentTag = tags[0];
  const accent = accentTag ? tagColor(accentTag) : "var(--accent)";
  const age = relativeAge(hit.indexed_at_unix ?? hit.mtime_unix);
  // FS9 — last-opened recency from the server reading rollup (works in
  // federated scope, where `progress` is absent). Distinct from `age`
  // (indexed/modified); backs the "opened …" chip + the sort=opened axis.
  const openedAge =
    hit.last_opened_unix != null ? relativeAge(hit.last_opened_unix) : null;
  const glyphs = glyphsForHit(hit);
  const words = hit.word_count ?? 0;
  const folder = folderOf(hit.source_relative);
  const scorePct =
    hit.score != null && scoreMax > 0
      ? Math.max(0.04, Math.min(1, hit.score / scoreMax))
      : 0;
  // W1.search — match-passage vs. summary fallback (item 2), and the
  // score decomposition (item 3). See HitScoreExt above.
  const ext = hit as SearchHit & HitScoreExt;
  const isMatchSnippet = !!ext.snippet;
  const snippetText = ext.snippet ?? hit.summary;
  const scoreTerms: ScoreTerm[] = [];
  // 0-based on the wire; display 1-based, matching the human-readable
  // "rank #N of M" footnote below.
  if (ext.bm25_rank != null) {
    scoreTerms.push({ label: "bm25", value: `#${ext.bm25_rank + 1}` });
  }
  if (ext.vec_rank != null) {
    scoreTerms.push({ label: "vector", value: `#${ext.vec_rank + 1}` });
  }
  const footnoteParts: string[] = [];
  if (mode) footnoteParts.push(mode);
  if (rank != null && total != null) footnoteParts.push(`rank #${rank} of ${total}`);
  const scoreFootnote = footnoteParts.length ? footnoteParts.join(" · ") : undefined;

  return (
    <Link
      to={artifactHref(kb, hit.source_relative)}
      className="kb-search-card"
      style={{ "--card-accent": accent } as CSSProperties}
      aria-label={`Open ${hit.title || hit.id}`}
      onClick={() => void recordSearch(kb, query)}
    >
      {(folder || (showCorpus && hit.kb)) && (
        <div className="kb-search-card__head">
          {folder && (
            <span className="kb-search-card__folder" title={folder}>
              {folder}
            </span>
          )}
          {showCorpus && hit.kb && (
            <span className="kb-search-card__corpus">{hit.kb}</span>
          )}
        </div>
      )}

      <h3 className="kb-search-card__title">
        {highlight(hit.title || "(untitled)", query)}
      </h3>

      {snippetText && (
        <p
          className={
            isMatchSnippet
              ? "kb-search-card__snippet kb-search-card__snippet--match"
              : "kb-search-card__snippet"
          }
          title={
            isMatchSnippet
              ? "Matched passage from the document text"
              : undefined
          }
        >
          {highlight(snippetText, query)}
        </p>
      )}

      {(hit.kb_category || hit.kb_status || hit.kb_severity) && (
        <div className="kb-search-card__badges">
          {hit.kb_category && (
            <span className="kb-search-card__badge kb-search-card__cat">
              {hit.kb_category}
            </span>
          )}
          {hit.kb_status && (
            <span className="kb-search-card__badge kb-search-card__status">
              {hit.kb_status}
            </span>
          )}
          {hit.kb_severity && (
            <span
              className="kb-search-card__badge kb-search-card__sev"
              data-sev={hit.kb_severity.toLowerCase()}
            >
              {hit.kb_severity}
            </span>
          )}
        </div>
      )}

      {tags.length > 0 && (
        <div className="kb-search-card__tags">
          {tags.slice(0, 3).map((t) => (
            <TagPill key={t} tag={t} accent={t === accentTag} />
          ))}
        </div>
      )}

      <div className="kb-search-card__foot">
        {glyphs.length > 0 ? (
          <span className="kb-search-card__glyphs">
            {glyphs.map((g, i) => (
              <span key={i} className="kb-search-card__glyph">
                <g.I />
                {g.n && g.n > 1 && (
                  <span className="kb-search-card__glyph-n">{g.n}</span>
                )}
              </span>
            ))}
          </span>
        ) : (
          <span />
        )}
        {words > 0 && (
          <span className="kb-search-card__words">{words.toLocaleString()}w</span>
        )}
        {scorePct > 0 && (
          <ScoreExplain
            label="relevance"
            total={hit.score ?? Number.NaN}
            terms={scoreTerms}
            footnote={scoreFootnote}
            inline
          >
            <span
              className="kb-search-card__score"
              title={`relevance ${hit.score?.toFixed(3)} — click to explain`}
              aria-label={`relevance score ${hit.score?.toFixed(3)}`}
            >
              <i style={{ width: `${Math.round(scorePct * 100)}%` }} />
            </span>
          </ScoreExplain>
        )}
        {hit.read_pct != null ? (
          <ReadingChip
            pct={hit.read_pct}
            isDone={hit.read_state === "read"}
            compact
          />
        ) : progress ? (
          <ReadingChip pct={progress.pct} isDone={progress.isDone} compact />
        ) : null}
        {openedAge && (
          <span className="kb-search-card__opened" title="last opened">
            opened {openedAge}
          </span>
        )}
        {age && <span className="kb-search-card__age">{age}</span>}
      </div>
    </Link>
  );
}

function propsEqual(
  a: SearchResultCardProps,
  b: SearchResultCardProps,
): boolean {
  return (
    a.hit === b.hit &&
    a.kb === b.kb &&
    a.query === b.query &&
    a.scoreMax === b.scoreMax &&
    a.progress === b.progress &&
    a.showCorpus === b.showCorpus &&
    a.mode === b.mode &&
    a.rank === b.rank &&
    a.total === b.total
  );
}

export default memo(SearchResultCard, propsEqual);
