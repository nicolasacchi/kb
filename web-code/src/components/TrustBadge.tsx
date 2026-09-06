import { isLiveTier, trustTierFrom, TRUST_LIVE_TITLE, TRUST_TIER_TITLE } from "../lib/trustBadge";

// T1 — design-ui.md §9.1's shared trust-tier badge: `exact` (solid, `--green`
// tint) / `likely` (amber, today's "approximate") / `candidate` (dashed
// outline). `PeekPanel`'s per-candidate badge and `FrameworkCard`'s per-row
// trust chip both render through this ONE component rather than duplicating
// the tier→color mapping. See `lib/trustBadge.ts` for the pure tier logic.

export interface TrustBadgeProps {
  /// `resolve::Candidate.class` / `HierarchyResolveTarget.class` /
  /// `frameworks::Trust.as_str()` — `undefined`/unrecognized classifies DOWN
  /// to `"candidate"` (`trustTierFrom`), never guessed UP.
  cls?: string | null;
  /// `resolve::Candidate.precision`, when known — consulted ONLY for the
  /// `lsp-live` "live" label; every other value is inert here (`cls` alone
  /// drives the badge's own tier).
  precision?: string | null;
  /// Overrides the tier's default tooltip (e.g. the server's own honesty
  /// `note`/`RESOLVE_NOTE`) — never hidden, per this app's honest-precision
  /// house style.
  title?: string;
}

export default function TrustBadge({ cls, precision, title }: TrustBadgeProps) {
  const tier = trustTierFrom(cls);
  const live = isLiveTier(precision);
  return (
    <span className="kbc-trust" data-kbc-trust={tier} data-kbc-trust-live={live || undefined}>
      {/* V70-A7 — the Lane Budget's trust channel is LINE STYLE, in one hue:
          `kbc-trust-${tier}` (tokens.css) sets `--kbc-trust-style` to solid /
          dashed / dotted, and the pill's underline + outline read it. That is
          what makes exact/likely/candidate survive any theme, any contrast
          slider and every colour-vision deficiency — the tint below is
          reinforcement, never the signal. */}
      <span
        className={`kbc-trust__pill kbc-trust__pill--${tier} kbc-trust-${tier}`}
        title={title ?? TRUST_TIER_TITLE[tier]}
      >
        {tier}
      </span>
      {live && (
        <span className="kbc-trust__live" title={TRUST_LIVE_TITLE} data-kbc-trust-live-label>
          live
        </span>
      )}
    </span>
  );
}
