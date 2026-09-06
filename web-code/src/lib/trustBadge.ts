// Shared trust-tier badge language (T1 — design-ui.md §9.1): every server
// surface that classifies a candidate/edge into a confidence tier —
// `resolve::Candidate.class` (`CLASS_EXACT`/`CLASS_LIKELY`/`CLASS_CANDIDATE`),
// `HierarchyResolveTarget.class`, `frameworks::Trust.as_str()` — emits the
// SAME three-value string. `components/peek/PeekPanel.tsx`'s per-candidate
// badge and `components/provenance/FrameworkCard.tsx`'s per-row trust chip
// both key off this ONE module rather than inventing their own tier/color
// rules — `components/TrustBadge.tsx` is the shared presentational half.
//
// Pure/DOM-free so it's directly testable without a render harness — this
// app's vitest config is `environment: "node"`, `.test.ts`-only (see
// `vitest.config.ts`); there is no component-render test infra here.

export type TrustTier = "exact" | "likely" | "candidate";

const KNOWN_TIERS: readonly string[] = ["exact", "likely", "candidate"];

/// An unrecognized or missing `class` value (an older daemon that only ever
/// sent `precision`, a genuinely unknown/future string, `frameworks::Trust`
/// which has no `"exact"` variant at all) classifies DOWN to `"candidate"` —
/// mirrors `resolve::class_for_precision`'s own documented policy ("Unknown
/// tiers classify DOWN") rather than ever guessing a MORE confident tier
/// than the server actually vouched for.
export function trustTierFrom(cls: string | null | undefined): TrustTier {
  return KNOWN_TIERS.includes(cls ?? "") ? (cls as TrustTier) : "candidate";
}

/// `resolve::PRECISION_LSP_LIVE` — the one precision tier that renders the
/// `exact` treatment PLUS an extra "live" label naming the provider lane
/// (design-ui.md §9.1: "keep it honest" — `lsp-live` IS class `exact` on the
/// wire, `trustTierFrom` already gets there on its own; this just flags
/// whether the EXTRA live-provider label should also render).
export function isLiveTier(precision: string | null | undefined): boolean {
  return precision === "lsp-live";
}

/// Default tooltip text per tier — overridden by the server's own honesty
/// `note`/`RESOLVE_NOTE` when one is given (never hidden, per this app's
/// honest-precision house style — `PeekPanel.tsx`'s own doc).
export const TRUST_TIER_TITLE: Record<TrustTier, string> = {
  exact: "exact — scope-proven, or verified by a real language server/compiler front-end",
  likely: "likely — a confident but unverified match (today's \"approximate\")",
  candidate: "candidate — a plausible match, name-based only",
};

export const TRUST_LIVE_TITLE = "resolved live via a configured language-server provider (lip/1)";
