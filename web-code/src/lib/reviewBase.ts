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

/// The chip's primary label — `tracking main`, `local main`, a bare
/// `pinned`, or `legacy` for a row with no resolved policy.
///
/// A `pin` never names a commit here, because the wire cannot support the
/// claim. `ReviewBaseOut` carries NO pin field: the only sha on it is
/// `merge_base`, the latest patchset's `base_sha` = `merge_base(base_tip,
/// head)`, and for a pin `base_tip` IS the pin — so the two are the same
/// commit only when the pin is an ANCESTOR of the head. The daemon models
/// the other case as first-class: `RetrackClass::Custom` is "a `pin` that is
/// NOT an ancestor — hand-chosen, unrelated history", and `classify_base`
/// makes any resolvable non-branch rev (a tag, a short sha, `HEAD~3`) one.
/// There, `merge_base` is an OLDER COMMON ANCESTOR the operator never
/// named, and the old `pinned <sha>` label put that commit's short sha
/// under the word "pinned" as though it were the pin itself. The sha is
/// still on screen — `baseMergeBaseSuffix` renders it for every mode — but
/// under the name the wire actually gives it, and `basePinNote` says why the
/// pin's own commit is absent.
export function baseChipLabel(base: Pick<ReviewBaseOut, "mode" | "branch">): string {
  switch (base.mode) {
    case "track":
      return `tracking ${base.branch ?? "?"}`;
    case "local":
      return `local ${base.branch ?? "?"}`;
    case "pin":
      return "pinned";
    default:
      return "legacy";
  }
}

/// The `· merge-base <sha>` suffix, shown whenever a merge-base has been
/// captured — INCLUDING for a `pin`, where it is the only sha on screen and
/// the label above it says only that the base is frozen. `null` when the
/// review has no captured merge-base yet (no patchset).
export function baseMergeBaseSuffix(base: Pick<ReviewBaseOut, "merge_base">): string | null {
  if (!base.merge_base) return null;
  return `merge-base ${shortSha(base.merge_base)}`;
}

/// The pin-only half of the base chip's tooltip: the sha beside a `pinned`
/// chip is the merge-base, NOT the pin, and why the pin's own commit is
/// missing from `base` at all. `null` for every other mode, whose label
/// makes no commit claim to correct. (The pin itself is not lost: the
/// server-composed `base-pinned` warning beside the chip names it, and
/// `kb-code review status <id> --json` reports it.)
export function basePinNote(base: Pick<ReviewBaseOut, "mode">): string | null {
  if (base.mode !== "pin") return null;
  return "The sha beside it is the patchset's merge-base, NOT the pin: `base` carries no pin field, and the two are the same commit only when the pin is an ancestor of the head.";
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

// ── store-card-unknown chip ───────────────────────────────────────────────
// `forgeUnverified` above answers from a LOADED card, and its two
// non-true answers ("no store row yet", "still loading") are true claims
// about a card that was actually read. A card that could NOT be read is a
// third state, and it must never be folded into either: the card is
// `GET /api/repos/{name}/store`, which `router.rs` pins to the
// loopback-only sub-router for the WHOLE family (it reports `store.git_dir`,
// the daemon's absolute state path), so a remote session's `getJson`
// throws on a bodyless 404 and `storeQ.data` is `undefined` — exactly the
// shape of "no store row yet". `forgeUnverified` cannot tell those apart, so
// the caller checks the query's error state itself and renders THIS chip:
// the store's state is UNKNOWN, which is neither "no store" nor "fine".

export const STORE_UNKNOWN_CHIP: BaseChipSpec = { token: "--warn", icon: "Warn" };

/// Why the card could not be read. `"loopback"` is the bodyless 404 the
/// loopback-only store family answers a non-loopback caller — the case
/// `loopback_only` (`transcripts::search`) produces — where the correct
/// advice is "open kb-code on the daemon's host". `"transport"` is any other
/// failure, where the detail is the honest thing to show.
export type StoreCardFailure = "loopback" | "transport";

/// The chip's on-screen text. Deliberately NOT "no store" and NOT anything
/// else about the store's health: a card that did not load is a hole in what
/// this session knows, and only that.
export const STORE_UNKNOWN_LABEL = "store unknown";

/// The chip's tooltip: the fact kb-code could not DETERMINE from here, and
/// the reason, so an operator knows whether to move or to look again.
export function storeUnknownTitle(failure: StoreCardFailure, detail?: string | null): string {
  const why =
    failure === "loopback"
      ? "`GET /api/repos/{name}/store` is loopback-only and this session is not on the daemon's host, so it answered a bodyless 404"
      : `the card request failed${detail ? ` (${detail})` : ""}`;
  return `kb-code could not read the review store — ${why}. The forge verification and base facts that card carries are UNKNOWN from here, not verified: open kb-code on the machine running kb-code-server.`;
}

// ── retrack command line ──────────────────────────────────────────────────

/// The CLI line to copy for `review retrack <id> --dry-run` (README §12,
/// §13's agent-CLI table). `POST /api/reviews/{id}/retrack` is live but
/// LOOPBACK-ONLY, so a non-loopback session keeps this copy-the-line
/// fallback (`BaseChip.tsx`'s `RetrackButton`) — the SAME "copy the exact
/// line an agent would run" posture `lib/reviewDoc.ts`'s
/// `composeCommandLine` documents for D22's loopback-only authoring.
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
