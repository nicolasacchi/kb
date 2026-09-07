// The Review Room's DOCUMENT tab — `kbc-review/1` rendered (V73-K2b, design
// D9/D9-a).
//
// The document is the agent's review, written once and read here. Four
// honesty rules, each with a home:
//
// **Every count and every caption is the wire's.** `revisions`, `omitted[]`,
// the reading order's `source`/`caption`, each card's `state`/`trust`/
// `caption` and the lint census are RENDERED, not recomputed. The one thing
// this panel derives is the act/blocking FACETING of the findings the wire
// already sent — a second view of one list, never a second count.
//
// **An absence is stated.** `omitted[]` renders as a captioned degrade list
// rather than as silence, because the daemon sends it precisely so that a
// missing block is a fact rather than a discovery.
//
// **A derived reading order says "derived".** The daemon composes one when
// the author declared none, and it is captioned as such — the author's own
// order and a fallback must never look alike.
//
// **Authoring is not here.** Composing is loopback-only (D22); the header
// offers the `kb-code review compose` line to copy, and nothing else.
import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import type { ReviewDocLintOut, ReviewDocOut } from "../../api/types";
import { Icon } from "../icons";
import DocLintPanel from "./DocLintPanel";
import DocMarkdown from "./DocMarkdown";
import RefCard from "./RefCard";
import {
  blockLabel,
  bodyOf,
  cardCensus,
  cardCensusText,
  cardIndex,
  composeCommandLine,
  docBlocks,
  docCommandLine,
  findingFacetText,
  findingFacets,
  findingTombstoneText,
  omittedIsSection,
  omittedLabel,
  orderedBlocks,
  readingOrderIsDerived,
  refSpanFor,
} from "../../lib/reviewDoc";
import { findingUrl } from "../../lib/codeUrl";
import { toast } from "../../lib/toast";

export interface DocPanelProps {
  repo: string;
  id: number;
  doc: ReviewDocOut;
  lint: ReviewDocLintOut | null | undefined;
  lintLoading: boolean;
  /** `?cards=folded` — every card shows its summary line only. */
  cardsFolded: boolean;
  onSetCardsFolded: (folded: boolean) => void;
  focusedRef: string | null;
  onFocusRef: (ref: string | null) => void;
}

export default function DocPanel({
  repo,
  id,
  doc,
  lint,
  lintLoading,
  cardsFolded,
  onSetCardsFolded,
  focusedRef,
  onFocusRef,
}: DocPanelProps) {
  const cards = useMemo(() => cardIndex(doc.cards), [doc.cards]);
  const resolved = doc.cards_resolved;
  const [foldedRefs, setFoldedRefs] = useState<Set<string>>(new Set());
  const toggleFold = (ref: string) =>
    setFoldedRefs((cur) => {
      const next = new Set(cur);
      if (next.has(ref)) next.delete(ref);
      else next.add(ref);
      return next;
    });

  const summaryBlocks = useMemo(
    () => docBlocks(doc.summary_md, cards, resolved),
    [doc.summary_md, cards, resolved],
  );
  const bodyBlocks = useMemo(
    () => docBlocks(bodyOf(doc), cards, resolved),
    [doc, cards, resolved],
  );
  const sections = useMemo(() => orderedBlocks(doc.blocks), [doc.blocks]);
  const facets = useMemo(() => findingFacets(doc.findings), [doc.findings]);
  const census = useMemo(() => cardCensus(doc.cards ?? []), [doc.cards]);

  const mdProps = {
    repo,
    reviewId: id,
    foldedRefs,
    cardsFolded,
    focusedRef,
    onToggleFold: toggleFold,
  };

  function copy(text: string, what: string) {
    navigator.clipboard.writeText(text).then(
      () => toast.ok(`${what} copied`),
      () => toast.err(`couldn't copy the ${what}`),
    );
  }

  return (
    <section className="kbc-doc" data-kbc-doc={doc.revision} aria-label="Review document">
      <header className="kbc-doc__head" data-kbc-doc-head>
        <span className="kbc-doc__tier" data-kbc-doc-tier={doc.tier} title="What this document PROMISES — priced authoring, not a quality grade. An honest `minimal` beats a `full` with three empty sections.">
          {doc.tier}
        </span>
        {doc.risk && (
          <span
            className={`kbc-doc__risk kbc-doc__risk--${doc.risk.level}`}
            data-kbc-doc-risk={doc.risk.level}
            title={doc.risk.why}
          >
            risk {doc.risk.level}
          </span>
        )}
        <span className="kbc-doc__rev" data-kbc-doc-rev={doc.revision}>
          revision {doc.revision} of {doc.revisions}
        </span>
        <span className="kbc-doc__ps" data-kbc-doc-ps={doc.ps_number}>
          ps{doc.ps_number}
        </span>
        <span className="kbc-doc__cards-census" data-kbc-doc-card-census>
          {resolved
            ? cardCensusText(census)
            : "refs not resolved on this read — cards show their address only"}
        </span>
        <button
          type="button"
          className={"kbc-review__action" + (cardsFolded ? " is-active" : "")}
          aria-pressed={cardsFolded}
          onClick={() => onSetCardsFolded(!cardsFolded)}
          title="Fold every ref card to its address line (Space r). A folded card still names what it points at."
          data-kbc-doc-fold-toggle={cardsFolded ? "1" : "0"}
        >
          {cardsFolded ? "Unfold cards" : "Fold cards"}
        </button>
        <button
          type="button"
          className="kbc-review__action"
          onClick={() => copy(composeCommandLine(id, doc.tier), "compose line")}
          title="Composing a review document is loopback-only (D22) — this copies the exact line an agent would run."
          data-kbc-doc-compose-copy
        >
          <Icon.Copy />
          Compose line
        </button>
        <code className="kbc-doc__cli" data-kbc-doc-cli>
          {docCommandLine(id, doc.ps_number)}
        </code>
      </header>

      <DocLintPanel lint={lint} loading={lintLoading} />

      {doc.summary_md.trim() !== "" && (
        <section className="kbc-doc__summary" data-kbc-doc-summary aria-label="Summary">
          <DocMarkdown blocks={summaryBlocks} {...mdProps} />
        </section>
      )}

      {doc.author && (
        <section className="kbc-doc__author" data-kbc-doc-author={doc.author.kind}>
          <h3>
            Author — {doc.author.kind}
            {doc.author.model ? ` · ${doc.author.model}` : ""}
          </h3>
          {/* Considered vs NOT considered is the author's own scope
              statement, and the second half is the load-bearing one: a
              review that says what it did not look at is the only kind whose
              silence means anything. */}
          <div className="kbc-doc__considered">
            <h4>Considered</h4>
            {doc.author.considered.length === 0 ? (
              <p className="kbc-doc__none">nothing declared</p>
            ) : (
              <ul data-kbc-doc-considered>
                {doc.author.considered.map((c, i) => (
                  <li key={i}>{c}</li>
                ))}
              </ul>
            )}
            <h4>Not considered</h4>
            {doc.author.not_considered.length === 0 ? (
              <p className="kbc-doc__none">nothing declared</p>
            ) : (
              <ul data-kbc-doc-not-considered>
                {doc.author.not_considered.map((c, i) => (
                  <li key={i}>{c}</li>
                ))}
              </ul>
            )}
          </div>
        </section>
      )}

      {doc.omitted.length > 0 && (
        <section className="kbc-doc__omitted" data-kbc-doc-omitted={doc.omitted.length}>
          <h3>Not in this document</h3>
          <p className="kbc-doc__caption">
            The daemon reports every optional block a document does not carry, so an absence is
            stated rather than discovered.
          </p>
          <ul>
            {doc.omitted.map((name) => (
              <li key={name} data-kbc-doc-omitted-item={name}>
                {omittedLabel(name)}
                {omittedIsSection(name) && <span className="kbc-doc__omitted-kind">section</span>}
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="kbc-doc__order" data-kbc-doc-order={doc.reading_order.source}>
        <h3>
          Reading order
          {readingOrderIsDerived(doc) && (
            <span className="kbc-doc__derived" data-kbc-doc-order-derived>
              derived
            </span>
          )}
        </h3>
        <p className="kbc-doc__caption" data-kbc-doc-order-caption>
          {doc.reading_order.caption}
        </p>
        {doc.reading_order.chapters.map((ch, i) => (
          <div className="kbc-doc__chapter" key={i} data-kbc-doc-chapter={ch.chapter}>
            <h4>{ch.chapter}</h4>
            <ol>
              {ch.stops.map((st, j) => (
                <li key={j}>
                  <RefCard
                    span={refSpanFor(st.ref, cards, resolved)}
                    repo={repo}
                    reviewId={id}
                    folded={cardsFolded || foldedRefs.has(st.ref)}
                    focused={focusedRef === st.ref}
                    onToggleFold={toggleFold}
                  />
                  {st.why && <span className="kbc-doc__why">{st.why}</span>}
                </li>
              ))}
            </ol>
          </div>
        ))}
      </section>

      {sections.map(({ name, text }) => (
        <section className="kbc-doc__block" key={name} data-kbc-doc-block={name}>
          <h3>{blockLabel(name)}</h3>
          <DocMarkdown blocks={docBlocks(text, cards, resolved)} {...mdProps} />
        </section>
      ))}

      {doc.flows.length > 0 && (
        <section className="kbc-doc__flows" data-kbc-doc-flows={doc.flows.length}>
          <h3>Flows</h3>
          {doc.flows.map((f, i) => (
            <div className="kbc-doc__flow" key={i} data-kbc-doc-flow={f.name}>
              <h4>{f.name}</h4>
              <ol>
                {f.steps.map((step, j) => (
                  <li key={j}>
                    <RefCard
                      span={refSpanFor(step, cards, resolved)}
                      repo={repo}
                      reviewId={id}
                      folded={cardsFolded || foldedRefs.has(step)}
                      focused={focusedRef === step}
                      onToggleFold={toggleFold}
                    />
                  </li>
                ))}
              </ol>
            </div>
          ))}
        </section>
      )}

      {doc.questions.length > 0 && (
        <section className="kbc-doc__questions" data-kbc-doc-questions={doc.questions.length}>
          <h3>Questions</h3>
          <ul>
            {doc.questions.map((q, i) => (
              <li key={i} data-kbc-doc-question={q.to}>
                <span className="kbc-doc__q-to" data-kbc-doc-question-to={q.to}>
                  {q.to.replace(/_/g, " ")}
                </span>
                <span className="kbc-doc__q-ask">{q.ask}</span>
                {q.ref && (
                  <RefCard
                    span={refSpanFor(q.ref, cards, resolved)}
                    repo={repo}
                    reviewId={id}
                    folded={cardsFolded || foldedRefs.has(q.ref)}
                    focused={focusedRef === q.ref}
                    onToggleFold={toggleFold}
                  />
                )}
              </li>
            ))}
          </ul>
        </section>
      )}

      {doc.findings.length > 0 && (
        <section className="kbc-doc__findings" data-kbc-doc-findings={doc.findings.length}>
          <h3>Findings</h3>
          {/* DERIVED from the rows above and nowhere else — the facet line is
              a second VIEW of one list, never a second count. */}
          <p className="kbc-doc__caption" data-kbc-doc-finding-facets>
            {findingFacetText(facets)}
          </p>
          <ul className="kbc-doc__finding-list">
            {doc.findings.map((f) => {
              const tomb = findingTombstoneText(f);
              return (
                <li
                  key={f.slug}
                  className={
                    "kbc-doc__finding" +
                    (f.blocking ? " kbc-doc__finding--blocking" : "") +
                    (f.superseded ? " kbc-doc__finding--superseded" : "")
                  }
                  data-kbc-doc-finding={f.slug}
                  data-kbc-doc-finding-act={f.act}
                  data-kbc-doc-finding-blocking={f.blocking ? "1" : "0"}
                >
                  <span className={`kbc-finding__act kbc-finding__act--${f.act}`} data-kbc-finding-act>
                    {f.act}
                  </span>
                  <span className="kbc-doc__finding-sev">{f.severity}</span>
                  {/* `blocking` is the reviewer's OWN call and deliberately
                      not derived from `severity`. It reads as WEIGHT, never
                      as a score. */}
                  {f.blocking && (
                    <span className="kbc-doc__finding-blocking" data-kbc-finding-blocking>
                      blocking
                    </span>
                  )}
                  <span className="kbc-doc__finding-cat">{f.category}</span>
                  <Link to={findingUrl(repo, id, f.slug)} className="kbc-doc__finding-title">
                    {f.slug} — {f.title}
                  </Link>
                  <span className="kbc-doc__finding-loc">{f.location_path}</span>
                  {f.disposition && (
                    <span className="kbc-doc__finding-disp" data-kbc-finding-disposition={f.disposition}>
                      {f.disposition}
                    </span>
                  )}
                  {tomb && (
                    <span className="kbc-doc__finding-tomb" data-kbc-finding-tombstone>
                      {f.superseded_by ? (
                        <Link to={findingUrl(repo, id, f.superseded_by)}>{tomb}</Link>
                      ) : (
                        tomb
                      )}
                    </span>
                  )}
                  {f.cites && f.cites.length > 0 && (
                    <span className="kbc-doc__finding-cites" data-kbc-finding-cites={f.cites.length}>
                      {f.cites.map((c) => (
                        <RefCard
                          key={c}
                          span={refSpanFor(c, cards, resolved)}
                          repo={repo}
                          reviewId={id}
                          folded
                          onToggleFold={toggleFold}
                        />
                      ))}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        </section>
      )}

      <DocMarkdown blocks={bodyBlocks} {...mdProps} />

      {/* The rail's own jump list lives in `RefCardsCard`; this is the
          keyboard focus target's announcement so `] r`/`[ r` are legible
          without one. */}
      {focusedRef && (
        <p className="kbc-sr-only" role="status" data-kbc-doc-focused-ref={focusedRef}>
          focused ref {focusedRef}
        </p>
      )}
      <button
        type="button"
        className="kbc-sr-only"
        onClick={() => onFocusRef(null)}
        aria-label="clear the focused ref"
        data-kbc-doc-clear-focus
      />
    </section>
  );
}
