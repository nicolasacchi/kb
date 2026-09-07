// `kbc-hunk-turns/1` (V73-K2c) — the hunk↔turn join's own two-tier badge.
// Deliberately NOT `TrustBadge` (`components/TrustBadge.tsx`): that
// component's vocabulary is `exact`/`likely`/`candidate` (kbc-theme/1's
// Lane Budget, a THREE-tier FACT ladder) and defaults an unrecognized
// class DOWN to `candidate` — a tier this join's own grammar structurally
// does not have (`docs/kb-code.md`'s own wording: "there is no fuzzy third
// tier: a wrong `exact` is this crate's release blocker"). Forcing a
// two-tier answer through a three-tier badge would silently OFFER a
// `candidate` reading that never applies here. Still LINE STYLE as the
// signal (kbc-theme/1's own rule), just a narrower, purpose-built pair.
import type { TurnTier } from "../../api/types";

export type { TurnTier };

const TIER_TITLE: Record<TurnTier, string> = {
  exact: "three independent witnesses: the bytes, the path, and the carrying commit",
  likely: "the bytes match, but the path moved, or the commit could not be named exactly",
};

export interface TurnTierBadgeProps {
  tier: TurnTier;
}

export default function TurnTierBadge({ tier }: TurnTierBadgeProps) {
  return (
    <span
      className={`kbc-turntier kbc-turntier--${tier}`}
      title={TIER_TITLE[tier]}
      data-kbc-turn-tier={tier}
    >
      {tier}
    </span>
  );
}
