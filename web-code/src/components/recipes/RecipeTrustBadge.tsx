import type { KbcTrustState } from "../../api/types";

// A recipe's OWN trust state (`trusted|untrusted|changed` — whether ITS
// BYTES are accepted) is a DIFFERENT, closed 3-value enum from a per-cell
// `Addr.trust` (`exact|likely|candidate|…`, rendered via `TrustBadge` +
// `trustTierFrom`, which classifies anything unrecognized DOWN to
// `candidate`). Feeding a recipe's trust state through that classifier
// would silently misclassify every value here — so this is its own small
// badge, following the SAME "trust is line style, not hue" rule
// (kbc-theme/1's Lane Budget, `--kbc-trust-style`) with its own CSS classes
// rather than reusing `.kbc-trust-*`.

const TITLE: Record<KbcTrustState, string> = {
  trusted: "This recipe's bytes are accepted (a builtin, a server row, or a repo file already trusted).",
  untrusted: "A repo-versioned recipe seen for the first time — run refuses until it's trusted.",
  changed: "A repo-versioned recipe's bytes changed since it was last trusted — see the diff below.",
};

export interface RecipeTrustBadgeProps {
  state: KbcTrustState;
}

export default function RecipeTrustBadge({ state }: RecipeTrustBadgeProps) {
  return (
    <span
      className={`kbc-recipe-trust kbc-recipe-trust--${state} kbc-recipe-trust-${state}`}
      data-kbc-recipe-trust={state}
      title={TITLE[state]}
    >
      {state}
    </span>
  );
}
