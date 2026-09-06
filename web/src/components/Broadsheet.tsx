import { useMemo } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import type { DocSummary } from "../api/client";
import { fetchInbox } from "../api/inbox";
import { artifactHref } from "../lib/artifactHref";
import { relativeAge } from "../lib/derive";
import {
  aggregateOpenCounts,
  censusLine,
  composePage,
  mastheadVolume,
  type LeadStory,
} from "../lib/broadsheet";
import { ScoreExplain, fmtScore, type ScoreTerm } from "./ScoreExplain";

// W2.5 — the broadsheet home (`?view=press`): a fifth gallery mode packing
// the CURRENT filter set into a deterministic front page (lead/features/
// briefs/continuing-coverage + a newspaper masthead). Pure SPA, zero new
// endpoints — the ranking is `lib/broadsheet.ts`'s `composePage` over the
// rows the caller already filtered (mirrors GalleryLobby's contract: it
// takes `rows`, never re-fetches). Calm computing: no images, no infinite
// scroll, no engagement mechanics — a bounded page regardless of corpus
// size.
//
// Comment mass rides its OWN kb-scoped cache entry (`["inbox", kb]`, a
// distinct query key from the fleet-wide `["inbox"]` `useInbox` hook uses)
// — TanStack invalidates by key PREFIX, so the existing SSE bridge's
// `["inbox"]` invalidation on `comments.updated` (queryClient.ts) already
// covers this variant with zero bridge changes. A fetch failure resolves
// to `{items: [], total_open: 0}` (api/inbox.ts's own fallback), so a
// daemon hiccup here just means zero comment mass — silent, never an error
// state of its own.

const EMPTY_MASS = new Map<string, number>();

export default function Broadsheet({
  rows,
  kb,
  docCount,
}: {
  rows: DocSummary[];
  kb: string;
  docCount?: number | null;
}) {
  const inboxQ = useQuery({
    queryKey: ["inbox", kb] as const,
    enabled: !!kb,
    queryFn: ({ signal }) => fetchInbox({ kb }, signal),
    select: (data) => aggregateOpenCounts(data.items),
    staleTime: Infinity,
  });
  const commentMass = inboxQ.data;

  // `now` is captured once per render (not per composePage call inside a
  // loop) — recomputing on every docs/inbox refetch is fine, this is a
  // cheap client-side sort, not a standing clock.
  const page = useMemo(
    () => composePage(rows, commentMass ?? EMPTY_MASS, Date.now()),
    [rows, commentMass],
  );
  const volume = useMemo(() => mastheadVolume(), []);
  const census = useMemo(() => censusLine(rows, docCount), [rows, docCount]);

  const { lead, features, briefs, coverage } = page;
  const hasFeatures = features.length > 0;
  const hasBriefs = briefs.length > 0;
  const hasCoverage = coverage.length > 0;

  return (
    <div className="kb-broadsheet">
      <header className="kb-broadsheet__masthead">
        <h1 className="kb-broadsheet__title">{kb}</h1>
        <div className="kb-broadsheet__mast-sub mono">
          <span className="kb-broadsheet__vol">{volume}</span>
          <span className="kb-broadsheet__census">{census}</span>
        </div>
        <div className="kb-broadsheet__mast-rule" aria-hidden="true" />
      </header>

      {(lead || hasFeatures || hasBriefs || hasCoverage) && (
        <div className="kb-broadsheet__page">
          <div className="kb-broadsheet__main">
            {lead && <LeadBlock kb={kb} story={lead} />}
            {hasFeatures && (
              <section className="kb-broadsheet__features" aria-label="features">
                {features.map((d) => (
                  <FeatureBlock key={d.id} kb={kb} doc={d} />
                ))}
              </section>
            )}
            {hasBriefs && (
              <ul className="kb-broadsheet__briefs" aria-label="briefs">
                {briefs.map((d) => (
                  <BriefRow key={d.id} kb={kb} doc={d} />
                ))}
              </ul>
            )}
          </div>
          {hasCoverage && (
            <aside className="kb-broadsheet__coverage" aria-label="continuing coverage">
              <h2 className="kb-broadsheet__rail-h">Continuing coverage</h2>
              <ul className="kb-broadsheet__coverage-list">
                {coverage.map((d) => (
                  <li key={d.id} className="kb-broadsheet__coverage-item">
                    <Link to={artifactHref(kb, d.source_relative)}>
                      {d.title || "(untitled)"}
                    </Link>
                    <span className="kb-broadsheet__coverage-meta mono">
                      updated {relativeAge(d.mtime_unix)}
                    </span>
                  </li>
                ))}
              </ul>
            </aside>
          )}
        </div>
      )}
    </div>
  );
}

/// Mirrors `resurfaceScoreTerms` (ResurfaceReview.tsx) — the shared
/// ScoreExplain chip renders whatever terms it's given plus the total; the
/// three factors here multiply rather than sum, so the bar reads as
/// relative magnitude among the factors, not a share of the total (the
/// chip doesn't require the terms to sum to `total`).
function leadScoreTerms(story: LeadStory): ScoreTerm[] {
  const { explain } = story;
  return [
    {
      label: "words",
      value: explain.wordFactor,
      detail: `ln(1+${story.doc.word_count ?? 0})`,
      color: "var(--accent)",
    },
    {
      label: "recency",
      value: explain.recencyFactor,
      detail: "0.5^(age/30d)",
      color: "var(--blue)",
    },
    {
      label: "comments",
      value: explain.commentFactor,
      detail: `1 + min(${explain.openComments}, 4)/4`,
      color: "var(--green)",
    },
  ];
}

function LeadBlock({ kb, story }: { kb: string; story: LeadStory }) {
  const { doc } = story;
  const words = doc.word_count ?? 0;
  return (
    <article className="kb-broadsheet__lead">
      <Link to={artifactHref(kb, doc.source_relative)} className="kb-broadsheet__lead-link">
        {doc.folder && <div className="kb-broadsheet__kicker">{doc.folder}</div>}
        <h2 className="kb-broadsheet__lead-title">{doc.title || "(untitled)"}</h2>
        {doc.summary && <p className="kb-broadsheet__lead-dek">{doc.summary}</p>}
      </Link>
      <div className="kb-broadsheet__lead-meta mono">
        <span>{relativeAge(doc.mtime_unix)}</span>
        {words > 0 && <span>· {words.toLocaleString()}w</span>}
        {story.explain.openComments > 0 && (
          <span>
            · {story.explain.openComments} open comment
            {story.explain.openComments === 1 ? "" : "s"}
          </span>
        )}
        <ScoreExplain
          label="lead score"
          total={story.explain.score}
          terms={leadScoreTerms(story)}
          footnote="score = ln(1+words) × recency × comments"
          className="kb-broadsheet__lead-chip"
        >
          {fmtScore(story.explain.score)}
        </ScoreExplain>
      </div>
    </article>
  );
}

function FeatureBlock({ kb, doc }: { kb: string; doc: DocSummary }) {
  const words = doc.word_count ?? 0;
  return (
    <Link to={artifactHref(kb, doc.source_relative)} className="kb-broadsheet__feature">
      {doc.folder && <div className="kb-broadsheet__feature-kicker">{doc.folder}</div>}
      <h3 className="kb-broadsheet__feature-title">{doc.title || "(untitled)"}</h3>
      {doc.summary && <p className="kb-broadsheet__feature-dek">{doc.summary}</p>}
      <div className="kb-broadsheet__feature-meta mono">
        <span>{relativeAge(doc.mtime_unix)}</span>
        {words > 0 && <span>· {words.toLocaleString()}w</span>}
      </div>
    </Link>
  );
}

function BriefRow({ kb, doc }: { kb: string; doc: DocSummary }) {
  const words = doc.word_count ?? 0;
  return (
    <li className="kb-broadsheet__brief">
      <Link to={artifactHref(kb, doc.source_relative)} className="kb-broadsheet__brief-link">
        {doc.title || "(untitled)"}
      </Link>
      <span className="kb-broadsheet__brief-meta mono">
        {relativeAge(doc.mtime_unix)}
        {words > 0 ? ` · ${words.toLocaleString()}w` : ""}
      </span>
    </li>
  );
}
