import { Link } from "react-router";
import type { KbcAddr } from "../../api/types";
import { addrHref, addrLabel } from "../../lib/recipeAddr";
import TrustBadge from "../TrustBadge";

export interface AddressCellProps {
  repo: string;
  addr: KbcAddr;
  /// Override the display text (a view column's own pre-rendered scalar) —
  /// defaults to `addrLabel(addr)`. The LINK TARGET is always derived from
  /// `addr` itself, never from this text (kbc-recipe/1's honesty contract:
  /// a cell's address comes from the correlated `Addr`, never parsed back
  /// out of a display string).
  text?: string;
  /// Show the row's own trust pill beside the label (list/tree views want
  /// it; a dense table column usually renders trust as its OWN column
  /// instead, via `field: "trust"`, so it opts out with `false`).
  showTrust?: boolean;
}

/// ONE address cell — kbc-recipe/1's "every cell is an address" contract.
/// A `null` href (the address carries no addressable target) renders the
/// label as plain, unlinked text rather than a fabricated link — the honest
/// alternative name for "this row has nothing to click".
export default function AddressCell({ repo, addr, text, showTrust = true }: AddressCellProps) {
  const href = addrHref(repo, addr);
  const label = text ?? addrLabel(addr);
  return (
    <span className="kbc-recipe-addr" data-kbc-recipe-addr-kind={addr.kind}>
      {href ? (
        <Link to={href} className="kbc-recipe-addr__link" data-kbc-recipe-addr-link>
          {label}
        </Link>
      ) : (
        <span className="kbc-recipe-addr__text" data-kbc-recipe-addr-unlinked>
          {label}
        </span>
      )}
      {showTrust && addr.trust !== "unknown" && (
        <TrustBadge cls={addr.trust} title={`trust: ${addr.trust}`} />
      )}
    </span>
  );
}
