import { useState } from "react";
import { useRailsNoun } from "../../hooks/useRails";
import { honestyLine, nounTitle, orphanKey, pageCaption, pageNav } from "../../lib/railsCards";
import RailsCard from "./RailsCard";

// V72-I2 — one noun's section on `~rails`.
//
// PAGING IS THE SERVER'S. `limit`/`offset` go to `/api/rails/<noun>` and the
// caption is built from the response's own `offset`/`returned`/`total`/
// `truncated` — this component never slices an array it already holds, which
// is the whole reason `RailsListOut.total` is on the wire (`rails::routes`'s
// "a cap is named, never silent").
//
// THE FILTER IS THE SERVER'S TOO. `q` is sent as `?q=`, applied BEFORE the
// daemon counts `total`, so the caption stays true under a filter. A
// client-side `.filter()` here would make "12 of 300" a lie.
export const RAILS_PAGE_SIZE = 25;

export interface RailsSectionProps {
  repo: string;
  noun: string;
  /// The TRUE total from the passport — shown in the heading even before the
  /// list loads, and never recomputed from the page.
  total: number;
  open: boolean;
  onToggle(): void;
  orphanIndex: Map<string, string[]>;
  /// Set by the page so `rails.section-next`/`prev` can scroll to it.
  sectionRef?: (el: HTMLElement | null) => void;
}

export default function RailsSection({
  repo,
  noun,
  total,
  open,
  onToggle,
  orphanIndex,
  sectionRef,
}: RailsSectionProps) {
  const [q, setQ] = useState("");
  const [offset, setOffset] = useState(0);
  const list = useRailsNoun({ repo, noun, q, limit: RAILS_PAGE_SIZE, offset, enabled: open });
  const honesty = honestyLine(list.data?.honesty);
  const nav = list.data
    ? pageNav(list.data, RAILS_PAGE_SIZE)
    : { canPrev: false, canNext: false, prevOffset: 0, nextOffset: 0 };

  return (
    <section
      className="kbc-rails__section"
      data-kbc-rails-section={noun}
      ref={sectionRef}
      aria-labelledby={`kbc-rails-h-${noun}`}
    >
      <button
        type="button"
        className="kbc-rails__section-head"
        id={`kbc-rails-h-${noun}`}
        aria-expanded={open}
        onClick={onToggle}
        data-kbc-rails-section-toggle={noun}
      >
        <span className="kbc-rails__section-title">{nounTitle(noun)}</span>
        <span className="kbc-rails__section-n" data-kbc-rails-total={total}>
          {total}
        </span>
      </button>

      {open && (
        <div className="kbc-rails__section-body">
          <div className="kbc-rails__section-controls">
            <input
              className="kbc-rails__filter"
              type="search"
              value={q}
              placeholder={`Filter ${nounTitle(noun).toLowerCase()} (name + path)…`}
              aria-label={`filter ${noun}`}
              data-kbc-rails-filter={noun}
              onChange={(e) => {
                setQ(e.target.value);
                setOffset(0);
              }}
            />
            {list.data && (
              <span className="kbc-rails__page-caption" data-kbc-rails-page-caption={noun}>
                {pageCaption(list.data)}
              </span>
            )}
          </div>

          {honesty && (
            <p
              className={`kbc-rails__honesty is-${honesty.state}`}
              role="status"
              data-kbc-rails-honesty={honesty.state}
            >
              {honesty.text}
            </p>
          )}

          {list.isLoading && <p className="kbc-rails__muted">Loading…</p>}
          {list.error && (
            <p className="kbc-rails__error" data-kbc-rails-error>
              {(list.error as Error).message}
            </p>
          )}

          <div className="kbc-rails__cards">
            {(list.data?.rows ?? []).map((row) => (
              <RailsCard
                key={`${row.path}:${row.line ?? 0}:${row.name}`}
                repo={repo}
                row={row}
                orphanLanes={orphanIndex.get(orphanKey(row)) ?? []}
              />
            ))}
          </div>

          {(nav.canPrev || nav.canNext) && (
            <div className="kbc-rails__paging">
              <button
                type="button"
                disabled={!nav.canPrev}
                onClick={() => setOffset(nav.prevOffset)}
                data-kbc-rails-prev={noun}
              >
                Previous
              </button>
              <button
                type="button"
                disabled={!nav.canNext}
                onClick={() => setOffset(nav.nextOffset)}
                data-kbc-rails-next={noun}
              >
                Next
              </button>
            </div>
          )}

          {(list.data?.notes ?? []).map((note) => (
            <p className="kbc-rails__note" key={note} data-kbc-rails-note>
              {note}
            </p>
          ))}
        </div>
      )}
    </section>
  );
}
