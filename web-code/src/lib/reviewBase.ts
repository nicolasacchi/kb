// RS-U11 — pure display helpers for the review's base POLICY (README
// "kb-code reviews that diff like GitHub" §3/§6/§12; RS-U6's
// `ReviewBaseOut`/`BaseWarningOut`, `review_base.rs`).
//
// Pure and total, same discipline as `lib/reviewRoom.ts`: every chip's
// colour comes from a NAME (a kbc-theme/1 token), never a raw hue, and
// every label is a projection of the wire — this module derives no fact
// of its own, only how to SHOW one. `reviewBase.test.ts` golden-pins the
// (mode/kind/warning code) → (token, icon) tables the same way
// `reviewRoom.test.ts` pins severity/act/category.

import type { BaseWarningOut, ReviewBaseOut, ReviewPatchset } from "../api/types";
import { shortSha } from "./format";

export interface BaseChipSpec {
  token: string;
  icon: string;
}

// ── base-policy chip ──────────────────────────────────────────────────────
// `base.mode` → chip tone. `track`/`local` are the healthy, unattended
// case (kb re-resolves them on every fetch); `pin` is a DELIBERATE freeze
// and reads amber, same tone as a row `classify_base`/`legacy_spec`
// couldn't turn into a policy at all (`mode` absent — README §10 step 3,
// "legacy rows are classified on read, never rewritten").

export const BASE_MODE_CHIPS: Record<string, BaseChipSpec> = {
  track: { token: "--blue", icon: "Branch" },
  local: { token: "--blue", icon: "Branch" },
  pin: { token: "--warn", icon: "Pin" },
};

/// A row with no resolved policy at all (`base.mode` absent).
export const BASE_LEGACY_CHIP: BaseChipSpec = { token: "--warn", icon: "Clock" };

export function baseChipSpec(base: Pick<ReviewBaseOut, "mode">): BaseChipSpec {
  if (base.mode == null) return BASE_LEGACY_CHIP;
  return BASE_MODE_CHIPS[base.mode] ?? BASE_LEGACY_CHIP;
}

/// The chip's primary label — `tracking main`, `local main`, `pinned
/// 7c1ed0c`, or `legacy` for a row with no resolved policy. A `pin`'s own
/// commit IS its merge-base (an ancestor pin's merge-base with any later
/// head is that same commit — the README §1 root cause, now surfaced
/// honestly as "pinned <sha>" instead of silently behaving like a track).
export function baseChipLabel(base: Pick<ReviewBaseOut, "mode" | "branch" | "merge_base">): string {
  switch (base.mode) {
    case "track":
      return `tracking ${base.branch ?? "?"}`;
    case "local":
      return `local ${base.branch ?? "?"}`;
    case "pin":
      return base.merge_base ? `pinned ${shortSha(base.merge_base)}` : "pinned";
    default:
      return "legacy";
  }
}

/// The `· merge-base <sha>` suffix shown for every mode EXCEPT `pin`
/// (whose own label already IS the merge-base — showing it twice would
/// say the same fact in two places). `null` when the review has no
/// captured merge-base yet (no patchset).
export function baseMergeBaseSuffix(base: Pick<ReviewBaseOut, "mode" | "merge_base">): string | null {
  if (base.mode === "pin") return null;
  if (!base.merge_base) return null;
  return `merge-base ${shortSha(base.merge_base)}`;
}

/// Mirrors `BaseSource::label()` (`review_base.rs`) — display text for
/// `base.source`'s wire slug, shown as the base chip's tooltip ("source
/// shown on hover/title", README §12). An unknown slug (a future rung
/// this build predates) degrades to the slug with dashes turned to
/// spaces, never a guess at what it means.
const BASE_SOURCE_LABELS: Record<string, string> = {
  explicit: "explicit",
  "forge-api": "forge API",
  caller: "caller",
  "merge-ref": "merge ref",
  "default-assumed": "default branch, assumed",
  upstream: "upstream",
  "stack-parent": "stack parent",
  legacy: "legacy row",
};

export function baseSourceLabel(source: string | null | undefined): string | null {
  if (!source) return null;
  return BASE_SOURCE_LABELS[source] ?? source.replace(/-/g, " ");
}

/// Retrack is offered exactly for the two chip variants the header names
/// "pin"/"legacy" for (README D17's `--pinned` bulk class + a fully
/// unclassified row) — an actively-tracked `track`/`local` base needs no
/// fixing.
export function baseNeedsRetrack(base: Pick<ReviewBaseOut, "mode">): boolean {
  return base.mode === "pin" || base.mode == null;
}

// ── warnings[] chips ──────────────────────────────────────────────────────
// Every warning is the SAME tone — the wire carries no severity axis on
// `BaseWarningOut` (just `code`/`message`), so inventing one client-side
// would be a claim the server never made.

export const BASE_WARNING_CHIP: BaseChipSpec = { token: "--warn", icon: "Warn" };

/// `base-pinned` → `base pinned` — the chip's short on-screen label. The
/// FULL sentence rides the chip's `title` from the wire's own `message`
/// (server-composed, never re-derived here) — same "short label, full
/// sentence on hover" split the base chip's own tooltip uses. Forward-
/// compatible with a warning code this build does not know by name.
export function warningShortLabel(code: string): string {
  return code.replace(/-/g, " ");
}

export function warningChipSpec(_warning: Pick<BaseWarningOut, "code">): BaseChipSpec {
  return BASE_WARNING_CHIP;
}

// ── forge-unverified chip ─────────────────────────────────────────────────
// D8: GitHub is the ONLY live-verified forge for Phase 1; GitLab/Gitea/
// Forgejo/Bitbucket ship `forge-unverified` — a store-level fact
// (`GET /api/repos/{name}/store`'s `store.forge_verified`), not a
// per-review one, so the header reads it off `useReviewStoreCard`.

export const FORGE_UNVERIFIED_CHIP: BaseChipSpec = { token: "--blue", icon: "Unlink" };

/// The SAME predicate the daemon's own `store_card` doctor uses
/// (`review_store/routes.rs`: `forge_verified != "verified" &&
/// forge_kind.is_some()`) — mirrored here rather than trusting a doctor
/// finding's free-text message to stay parseable.
export function forgeUnverified(store: { forge_verified: string; forge_kind: string | null } | null | undefined): boolean {
  if (!store) return false;
  return store.forge_verified !== "verified" && store.forge_kind != null;
}

// ── retrack command line ──────────────────────────────────────────────────

/// The CLI line to copy for `review retrack <id> --dry-run` (README §12,
/// §13's agent-CLI table). RS-U7's own `POST /api/reviews/{id}/retrack`
/// route hasn't shipped on this build, so the header offers exactly this
/// line to copy rather than a button that would 404 — the SAME "copy the
/// exact line an agent would run" posture `lib/reviewDoc.ts`'s
/// `composeCommandLine` documents for D22's loopback-only authoring.
///
/// TODO(RS-U7): once the retrack HTTP route ships, wire the header's
/// button to call it directly (with a `--dry-run` toggle mirroring the
/// CLI flag) instead of only copying this line.
export function retrackCommandLine(reviewId: number, dryRun = true): string {
  return `kb-code review retrack ${reviewId}${dryRun ? " --dry-run" : ""}`;
}

// ── patchset kind badge (`PatchsetStrip`) ─────────────────────────────────
// `review_patchsets.kind` (`PatchsetKind` in `review_base.rs`): why a
// patchset was minted. `null` on a patchset captured before RS-U6 landed
// — a legacy patchset shows no badge at all rather than guessing one.

export const PATCHSET_KIND_CHIPS: Record<string, BaseChipSpec> = {
  initial: { token: "--ink-mute", icon: "Dot" },
  push: { token: "--ink-mute", icon: "Dot" },
  rebase: { token: "--blue", icon: "Swap" },
  "base-moved": { token: "--warn", icon: "Branch" },
  "base-corrected": { token: "--warn", icon: "Branch" },
  retarget: { token: "--warn", icon: "Fork" },
  // RS-U10b's own doc (`review_sync.rs::SyncReason::from_capture`): a
  // `--force` re-mint of an UNCHANGED pair is "nothing moved" — neutral,
  // same tone as `initial`/`push`, not a base-tracking anomaly like the
  // four rows above it.
  forced: { token: "--ink-mute", icon: "Refresh" },
};

const PATCHSET_KIND_FALLBACK: BaseChipSpec = { token: "--ink-mute", icon: "Dot" };

export function patchsetKindSpec(kind: string | null | undefined): BaseChipSpec {
  if (!kind) return PATCHSET_KIND_FALLBACK;
  return PATCHSET_KIND_CHIPS[kind] ?? PATCHSET_KIND_FALLBACK;
}

/// `base-moved` → `base moved` — same short-label convention as
/// `warningShortLabel`. `null` for a legacy patchset (no badge at all,
/// per `PatchsetStrip`'s own contract).
export function patchsetKindLabel(kind: string | null | undefined): string | null {
  if (!kind) return null;
  return kind.replace(/-/g, " ");
}

/// The patchset's own base, short — `base_tip_sha` (RS-U6+, the resolved
/// policy's tip at capture) when present, else the merge-base every
/// patchset has always carried (`base_sha_full`/`base_sha`).
export function patchsetBaseShort(p: Pick<ReviewPatchset, "base_tip_sha" | "base_sha_full" | "base_sha">): string {
  return shortSha(p.base_tip_sha ?? p.base_sha_full ?? p.base_sha);
}
