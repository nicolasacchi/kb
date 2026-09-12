import { Link } from "react-router";
import type { RailsRow } from "../../api/types";
import TrustBadge from "../TrustBadge";
import { cardOf, flagText } from "../../lib/railsCards";
import { fullSearchUrl } from "../../lib/searchLanes";

// V72-I2 — ONE `rails/1` row, as a card.
//
// It renders `lib/railsCards.ts`'s projection and computes nothing itself.
// Three things the card is required to get right, each pinned by
// `railsCards.test.ts` on the projection side:
//
//   * ONE address. The card head is a `<Link>` to `cardOf().href`, built
//     through `lib/codeUrl.ts` — the Location Contract's single door.
//   * The trust LINE STYLE, never solid. `cardOf().trustClass` is
//     `kbc-trust-likely` (dashed) or `kbc-trust-candidate` (dotted); a Rails
//     row has no `exact` tier to draw.
//   * Every flag the daemon set, rendered — a `blob-drifted`/`action-missing`
//     row must not look like a clean one.
export interface RailsCardProps {
  repo: string;
  row: RailsRow;
  /// The orphan lanes that listed this row, from `orphanIndex()`. Empty ⇒ no
  /// badge. NEVER a verdict: the badge names the lane and the lane names its
  /// own doubt.
  orphanLanes?: readonly string[];
}

export default function RailsCard({ repo, row, orphanLanes = [] }: RailsCardProps) {
  const card = cardOf(repo, row);
  return (
    <article
      className={`kbc-railscard ${card.trustClass}`}
      data-kbc-rails-card={card.noun}
      data-kbc-rails-trust={card.trust}
      data-kbc-rails-name={card.title}
    >
      <header className="kbc-railscard__head">
        <Link className="kbc-railscard__name" to={card.href} data-kbc-rails-open>
          {card.title}
        </Link>
        <TrustBadge cls={row.trust} />
      </header>

      <p className="kbc-railscard__addr" data-kbc-rails-addr>
        {card.addressLabel}
      </p>

      {orphanLanes.length > 0 && (
        <p className="kbc-railscard__orphan" data-kbc-rails-orphan title={orphanLanes.join(" · ")}>
          in the orphan report: {orphanLanes.join(" · ")}
        </p>
      )}

      {card.facts.length > 0 && (
        <dl className="kbc-railscard__facts">
          {card.facts.map((f) => (
            <div
              key={f.label}
              className={"kbc-railscard__fact" + (f.warn ? " is-warn" : "")}
              data-kbc-rails-fact={f.label}
            >
              <dt>{f.label}</dt>
              <dd>{f.value}</dd>
            </div>
          ))}
        </dl>
      )}

      {card.flags.length > 0 && (
        <ul className="kbc-railscard__flags">
          {card.flags.map((flag) => (
            <li key={flag} data-kbc-rails-flag={flag}>
              {flagText(flag)}
            </li>
          ))}
        </ul>
      )}

      <div className="kbc-railscard__chips">
        {card.chips.map((chip) => (
          <Link
            key={chip.clause}
            className="kbc-railscard__chip"
            to={fullSearchUrl(chip.clause, repo)}
            title={chip.note}
            data-kbc-rails-chip={chip.clause}
          >
            {chip.label}
          </Link>
        ))}
      </div>

      <p className="kbc-railscard__witness" data-kbc-rails-witnesses={card.witnessCount}>
        {card.witnessCount} witness{card.witnessCount === 1 ? "" : "es"} — every fact above is a
        convention, an indexed definition or a lens edge, never a proof
      </p>
    </article>
  );
}
