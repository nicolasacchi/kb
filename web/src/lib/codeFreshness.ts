// OL-SIBL2 - the freshness ladder + degrade vocabulary, moved verbatim out of
// CodeRefsSection.tsx so the eagerly-loaded Header (the Code chip) can share
// them without statically importing the lazy reader chunk (see HotkeyRoot's
// note on `routes/detail.tsx`). CodeRefsSection re-exports every name.
import type { DocLensOut } from "../api/doclens";
import { relativeAge } from "./time";

export const COMPOUND_UNAVAILABLE =
  "Code bridge unavailable from this origin — kb-code isn't reachable (its CORS allowlist may not include this page, or the daemon may be down).";

/// R12 — the discriminator between degrade state 2 (fetch throws; a browser
/// cannot tell "not CORS-allowlisted" from "kb-code down" from "network
/// unreachable" apart, so those collapse into ONE honest compound message)
/// and state 3 (fetch SUCCEEDED with a non-2xx `{error, reason}` body — a
/// server-authored refusal, rendered verbatim) is the PRESENCE of `.reason`
/// on the thrown error, not any attempt to distinguish the underlying cause.
/// Exported (W1.D.R #3) so the reason-present/reason-absent branches get
/// direct unit coverage instead of only exercising through a full render.
export function degradeMessage(error: unknown): string {
  const reason = (error as (Error & { reason?: string }) | undefined)?.reason;
  if (reason) {
    return error instanceof Error ? error.message : String(error);
  }
  return COMPOUND_UNAVAILABLE;
}

/// CT-E3 — the honest-staleness summary line under the Code heading: the
/// ONE home (#30) for "how fresh are this doc's code citations", shared by
/// the About stack and the Links tab (the same section renders in both).
/// Tier (a) is always available (the coderef/1 header the sub-tab already
/// fetches: ref_count/extracted_at/never_scanned); tier (b) upgrades it to
/// verified drift counts when the doclens lane against the PINNED checkout
/// has data. The rung is explicit on the returned `kind` (and rendered as a
/// `data-kb-coderef-fresh` attribute) so an unreachable kb-code is
/// STRUCTURALLY incapable of rendering as verified — it degrades to the
/// tier-(a) wording, never a fake "fresh" (exact CT-B6 ladder precedent:
/// counts · no-pin · server-refusal · unreachable are four distinct
/// states).
export type CodeFreshness = {
  kind:
    | "never-scanned" // no code_refs_docs row at all — "not scanned" ≠ "0 refs"
    | "verified" // tier (b): lens resolved against the pinned checkout
    | "no-pin" // kb-code reachable, but no checkout pinned — drift unchecked
    | "refusal" // kb-code reached, server-authored refusal (verbatim)
    | "degraded" // kb-code unreachable — tier (a) wording, never fake-fresh
    | "hints-only"; // no code_url configured (or lens still in flight)
  text: string;
  /// `verified` only — drives the `.is-drifted` emphasis + the rail-icon
  /// drift dot; null on every other rung (an unverified doc has no drift
  /// NUMBER, honest or otherwise).
  drifted: number | null;
  /// Hover detail (the compound unreachable message on `degraded`, the
  /// no-pin rationale on `no-pin`); null when the text stands alone.
  title: string | null;
};

/// Pure state ladder (exported for direct unit coverage of every rung).
/// Returns null for a scanned zero-ref doc: a doc that cites nothing makes
/// no claim that can go stale, so there is no freshness line to render
/// (and the section itself doesn't show for that state — `showCodeSection`
/// in PreviewInspector is unchanged).
export function freshnessState(args: {
  neverScanned: boolean;
  /// The coderef/1 SERVER total (`CodeRefsResponse.ref_count`), same field
  /// the heading count reads.
  refCount: number;
  /// `CodeRefsResponse.extracted_at` (unix secs); null iff never_scanned.
  extractedAt: number | null;
  hasCodeUrl: boolean;
  pinned: boolean;
  scorecardOk: boolean;
  lens: DocLensOut | undefined;
  /// The one failed cross-daemon query's error (scorecard first, else the
  /// lens once a repo is pinned) — null when neither failed.
  crossErr: unknown;
  nowMs?: number;
}): CodeFreshness | null {
  const {
    neverScanned,
    refCount,
    extractedAt,
    hasCodeUrl,
    pinned,
    scorecardOk,
    lens,
    crossErr,
    nowMs,
  } = args;
  if (neverScanned) {
    return {
      kind: "never-scanned",
      text: "code refs never scanned",
      drifted: null,
      title: null,
    };
  }
  if (refCount === 0) return null;
  const extracted =
    extractedAt != null
      ? ` · extracted ${relativeAge(extractedAt, nowMs)}`
      : "";
  const tierA = `cites ${refCount} code ref${refCount === 1 ? "" : "s"}${extracted}`;
  if (!hasCodeUrl) {
    return { kind: "hints-only", text: tierA, drifted: null, title: null };
  }
  if (crossErr != null) {
    const reason = (crossErr as Error & { reason?: string }).reason;
    if (reason) {
      // Server-authored refusal (kb-code was REACHED) — its message
      // verbatim, exactly like the body hint below it (R12 discriminator).
      const msg = crossErr instanceof Error ? crossErr.message : String(crossErr);
      return {
        kind: "refusal",
        text: `${tierA} · kb-code: ${msg}`,
        drifted: null,
        title: msg,
      };
    }
    // Unreachable/CORS/down are indistinguishable in a browser — degrade to
    // the tier-(a) wording (the extraction facts kb itself is authoritative
    // for), NEVER a drift verdict.
    return {
      kind: "degraded",
      text: tierA,
      drifted: null,
      title: COMPOUND_UNAVAILABLE,
    };
  }
  if (lens) {
    const drifted = lens.counts.drifted;
    return {
      kind: "verified",
      text: `cites ${refCount} code location${refCount === 1 ? "" : "s"} · ${drifted} drifted · checked ${relativeAge(lens.resolved_unix, nowMs)}`,
      drifted,
      title: null,
    };
  }
  if (scorecardOk && !pinned) {
    return {
      kind: "no-pin",
      text: `${tierA} · freshness unchecked — no checkout pinned`,
      drifted: null,
      title:
        "drift can only be measured against a chosen checkout; pick one below (a checkout is never auto-selected)",
    };
  }
  // Scorecard/lens still in flight — the tier-(a) facts are already honest
  // on their own; the line upgrades in place when the lens lands.
  return { kind: "hints-only", text: tierA, drifted: null, title: null };
}
