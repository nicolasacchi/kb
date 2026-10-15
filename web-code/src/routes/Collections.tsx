// `/r/{repo}/~collections` — OL-C1. The one page behind the Collections door:
// the twelve former Explore destinations (plus Boards and Lanes, which no
// menu used to reach), grouped Sets / Boards / Instruments. Every card is a
// plain `<Link>` to a route that already exists; nothing here owns data.
//
// A card's count is kbc-seq/1's (`GET /api/seq`), shown only for the cards
// whose projection that wire carries and only once it has answered — a card
// without a count says nothing, never a 0 (`seqCounts` in `lib/navItems.ts`).

import { Link, useParams } from "react-router";
import MetaLine from "../components/MetaLine";
import PageHeader from "../components/PageHeader";
import { useSeq } from "../hooks/useSeq";
import { COLLECTION_GROUPS, COLLECTION_ITEMS, seqCounts } from "../lib/navItems";
import "../styles/collections.css";

export default function Collections() {
  const { repo = "" } = useParams<{ repo: string }>();
  const seq = useSeq(repo);
  const counts = seq.data ? seqCounts(seq.data.projections) : {};

  return (
    <div className="kbc-collections" data-kbc-collections>
      <PageHeader title={`Collections — ${repo}`} />
      <MetaLine
        items={[
          `${COLLECTION_ITEMS.length} destinations`,
          seq.isError ? "counts unavailable" : null,
        ]}
      />
      {COLLECTION_GROUPS.map(({ group, label }) => (
        <section
          key={group}
          className="kbc-collections__group"
          aria-labelledby={`kbc-collections-${group}`}
          data-kbc-collections-group={group}
        >
          <h2 className="kbc-collections__h" id={`kbc-collections-${group}`}>
            {label}
          </h2>
          <ul className="kbc-collections__grid">
            {COLLECTION_ITEMS.filter((i) => i.group === group).map((item) => {
              const ItemIcon = item.icon;
              const count = counts[item.key];
              return (
                <li key={item.key} className="kbc-collections__cell">
                  <Link
                    to={item.url(repo)}
                    className="kbc-collections__card"
                    data-kbc-collection={item.key}
                  >
                    <span className="kbc-collections__name">
                      <ItemIcon aria-hidden />
                      {item.label}
                      {count !== undefined && (
                        <span className="kbc-collections__count" data-kbc-collection-count>
                          {count}
                        </span>
                      )}
                    </span>
                    <span className="kbc-collections__purpose">{item.purpose}</span>
                  </Link>
                </li>
              );
            })}
          </ul>
        </section>
      ))}
    </div>
  );
}
