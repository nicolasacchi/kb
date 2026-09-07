// `kbc-claim/1` (V73-K2c, design D18) — the SPA's pure half of the claim
// register: text formatting only, no fetch, no ranking. Three rules from
// `docs/kb-code.md`'s own header apply here directly:
//
//   (a) Surfaced, never scored — `ClaimRegister.tsx` renders `claims[]` in
//       WIRE order (never sorted by `confidence`), pinned by
//       `claims.test.ts`'s wire-order test.
//   (b) The ladder state is a per-request classification, not a trust
//       class — this module never renders it through `TrustBadge` (the
//       Lane Budget's FACT channel); it gets its own small badge so the
//       claim register stays visually distinct from facts (D18).
//   (c) `confidence` is the agent's OWN declaration, rendered as TEXT
//       ("agent-declared 0.7"), never a bar or a score.
import type { ClaimKind, ClaimLadderState, ClaimOut } from "../api/types";
import { readerUrl } from "./breadcrumbs";
import { entityUrl, reviewDiffHref, symbolUrl } from "./codeUrl";
import { parseRef, type KbcRef } from "./kbcRefs";

export const CLAIM_KIND_LABELS: Record<ClaimKind, string> = {
  explain: "explains",
  alternative: "alternative considered",
  decision: "decision",
  story: "branch story",
  note: "note",
  answer: "answer",
};

export function claimKindLabel(kind: ClaimKind | string): string {
  return CLAIM_KIND_LABELS[kind as ClaimKind] ?? kind;
}

/// Claims render in WIRE order — never re-sorted by `confidence` or
/// anything else (surfaced, never scored; rule (a) above). An identity
/// function on purpose: it turns "the component doesn't call `.sort()`"
/// from an unstated property into a single, named seam `claims.test.ts`
/// can pin by referential equality.
export function claimsRenderOrder(claims: readonly ClaimOut[]): readonly ClaimOut[] {
  return claims;
}

/// The agent's own declaration, verbatim — never a percentage bar, never
/// multiplied into anything. `undefined` renders as an explicit "not
/// stated" rather than a blank, so the register never implies a number
/// that was never given.
export function confidenceText(confidence: number | undefined): string {
  return confidence === undefined
    ? "agent-declared — not stated"
    : `agent-declared ${confidence}`;
}

export const LADDER_LABELS: Record<ClaimLadderState, string> = {
  pinned: "pinned",
  drifted: "drifted",
  unanchored: "unanchored",
};

export function ladderLabel(state: ClaimLadderState | string): string {
  return LADDER_LABELS[state as ClaimLadderState] ?? state;
}

/// The author register's one-line provenance — "agent · <model> · session
/// <id>" with either half dropped when the claim didn't carry it (a human-
/// authored claim carries neither).
export function claimProvenanceText(claim: Pick<ClaimOut, "model" | "session_id">): string {
  const parts: string[] = [];
  if (claim.model) parts.push(claim.model);
  if (claim.session_id) parts.push(`session ${claim.session_id.slice(0, 12)}`);
  return parts.length > 0 ? parts.join(" · ") : "no session recorded";
}

/// One evidence ref, parsed for display. `href` is `null` for a scheme this
/// register cannot honestly resolve into a link (`gh:`/`kb:`/`hunk:` — the
/// same "no host it could resolve" posture `RefCard.tsx` already takes for
/// an inert card) rather than a fabricated address.
export interface EvidenceRefView {
  raw: string;
  label: string;
  href: string | null;
}

export function evidenceRefView(
  raw: string,
  repo: string,
  reviewId: number | undefined,
): EvidenceRefView {
  const ref: KbcRef | null = parseRef(raw);
  if (!ref) return { raw, label: raw, href: null };
  switch (ref.scheme) {
    case "code":
      return {
        raw,
        label: raw,
        href:
          reviewId !== undefined
            ? reviewDiffHref(repo, reviewId, ref.path)
            : readerUrl(repo, ref.path, ref.sha, ref.line),
      };
    case "sym":
      return {
        raw,
        label: raw,
        href: symbolUrl(repo, ref.container ? `${ref.container}::${ref.name}` : ref.name, {
          fallbackPath: "",
        }),
      };
    case "ent":
      return { raw, label: raw, href: entityUrl(repo, ref.fqn) };
    case "finding":
      return {
        raw,
        label: raw,
        href:
          reviewId !== undefined
            ? reviewDiffHref(repo, reviewId, undefined, { finding: ref.slug })
            : null,
      };
    default:
      // `gh:` / `kb:` / `hunk:` — this register does not resolve these
      // (kb-code never calls GitHub, and a bare `hunk:` needs the SAME
      // hunk-turns join this claim did not carry). Named, not linked.
      return { raw, label: raw, href: null };
  }
}
