// V74-L3c — the ONE `KbcAddr → URL` mapping (kbc-recipe/1's honesty
// contract: "every result cell is an address"), plus the client-side mirror
// of the server's `StepCensus::explain()` (Rust, `crates/kb-code-server/src/
// recipe/census.rs`) and the closed intent-group taxonomy.
//
// Exhaustive switch on `KbcAddrKind` (9 values) — a `never` default makes an
// unhandled future kind a COMPILE error here rather than a silently
// unlinked cell, mirroring `lib/actionOps.ts`'s `resolveOp` convention (the
// SPA research note this module follows).

import type { KbcAddr, KbcEmptyReason, KbcRecipeIntent, KbcStepCensus } from "../api/types";
import type { SetSpanInput } from "../api/client";
import { boardUrl, commitUrl, entityUrl, findingUrl, symbolUrl } from "./codeUrl";
import { readerUrl } from "./breadcrumbs";

function scalarString(addr: KbcAddr, key: string): string | undefined {
  const v = addr.scalars?.[key];
  return typeof v === "string" ? v : undefined;
}

/// The href a click on this address cell should open, or `null` when the
/// row carries no addressable target at all (rendered as plain, unlinked
/// text — never a fabricated or best-guess link). `path`/`line` on a
/// non-`file`/`line` kind (a `comment`/`fact`/`finding`/`node` anchored to
/// code) is a legitimate, common case per the ops that emit them
/// (`ops.rs`'s `Addr::new(...).with_path(...)`), so several kinds share the
/// same reader fallback.
export function addrHref(repo: string, addr: KbcAddr): string | null {
  switch (addr.kind) {
    case "file":
      return addr.path ? readerUrl(repo, addr.path) : null;
    case "line":
      return addr.path ? readerUrl(repo, addr.path, undefined, addr.line) : null;
    case "symbol":
      if (addr.path && addr.symbol) {
        return symbolUrl(repo, addr.symbol, { fallbackPath: addr.path, fallbackLine: addr.line });
      }
      return addr.path ? readerUrl(repo, addr.path, undefined, addr.line) : null;
    case "entity":
      return addr.entity ? entityUrl(repo, addr.entity, { path: addr.path, line: addr.line }) : null;
    case "commit":
      return addr.commit ? commitUrl(repo, addr.commit) : null;
    case "comment":
    case "fact":
      return addr.path ? readerUrl(repo, addr.path, undefined, addr.line) : null;
    case "finding": {
      const reviewId = addr.scalars?.review_id;
      if ((typeof reviewId === "number" || typeof reviewId === "string") && addr.id) {
        return findingUrl(repo, reviewId, addr.id);
      }
      return addr.path ? readerUrl(repo, addr.path, undefined, addr.line) : null;
    }
    case "node": {
      // A node with attached code (`ops.rs`'s boards op sets `path`/`line`
      // whenever the board node names one) reads best as the code itself;
      // otherwise fall back to the board it lives on (`scalars.board`,
      // always set by that same op — `id` is the composite `"{slug}/{node}"`
      // form, never itself a route segment).
      if (addr.path) return readerUrl(repo, addr.path, undefined, addr.line);
      const board = scalarString(addr, "board");
      return board ? boardUrl(repo, board) : null;
    }
    default: {
      const _exhaustive: never = addr.kind;
      return _exhaustive;
    }
  }
}

/// A short display label for the cell when no `header`/`field`-specific
/// text applies (used by the tree/graph views, which group by address
/// rather than by column) — the best available human-readable identity for
/// this kind, in priority order, falling back to the kind name itself so a
/// row is never rendered fully blank.
export function addrLabel(addr: KbcAddr): string {
  switch (addr.kind) {
    case "file":
    case "line":
      return addr.path ?? addr.kind;
    case "symbol":
      return addr.symbol ?? addr.path ?? addr.kind;
    case "entity":
      return addr.entity ?? addr.kind;
    case "commit":
      return addr.commit ?? addr.kind;
    case "comment":
    case "fact":
    case "finding":
    case "node": {
      const title = scalarString(addr, "title");
      return title ?? addr.path ?? addr.id ?? addr.kind;
    }
    default: {
      const _exhaustive: never = addr.kind;
      return _exhaustive;
    }
  }
}

/// `KbcAddr -> SetSpanInput` for "save as set" — `SetSpanInput.path` is
/// required, so any address with no `path` (an `entity`/`commit`/`fact` with
/// no code anchor, a bare `finding`/`node`) has NO span form and returns
/// `null`; the caller reports it as skipped rather than inventing a path.
export function addrToSetSpan(addr: KbcAddr): SetSpanInput | null {
  if (!addr.path) return null;
  return addr.line !== undefined ? { path: addr.path, line_start: addr.line, line_end: addr.line } : { path: addr.path };
}

/// Convert a whole row set, honestly reporting what couldn't be included.
export function addrsToSetSpans(addrs: readonly KbcAddr[]): { spans: SetSpanInput[]; skipped: number } {
  const spans: SetSpanInput[] = [];
  let skipped = 0;
  for (const addr of addrs) {
    const span = addrToSetSpan(addr);
    if (span) spans.push(span);
    else skipped++;
  }
  return { spans, skipped };
}

// ── the census sentence (mirrors `census.rs`'s `StepCensus::explain()`) ────

const EMPTY_REASON_TEXT: Record<KbcEmptyReason, string> = {
  "no-inputs": "the step reads per-address and got none",
  "upstream-empty": "the previous step returned nothing",
  "filtered-out": "rows existed and every one failed this step's filters",
  "scope-excluded": "the scope excluded every row",
  "lane-disabled": "an aug-lane/1 lane this step reads is turned off",
  "lane-unknown": "no lane by that name is configured",
  "lane-unavailable": "a known lane couldn't answer",
  "no-index": "the index this step reads has no rows",
  "not-a-rails-app": "rails/1 found no Rails structure in this repo",
  "param-empty": "a param this step needs resolved to empty",
  "budget-exhausted": "the run's time budget was spent before this step started",
};

/// Human sentence naming WHY a step is empty — the client-side mirror of
/// `StepCensus::explain()`. `filtered-out` cites the largest `inputs` count
/// (rows that existed before the filter ran); every reason with
/// `filters_applied` lists them, joined with `; `, exactly as the Rust side
/// does — kept in lock-step so the SPA never invents its own copy.
export function censusExplain(census: KbcStepCensus): string {
  if (!census.empty_reason) return "";
  let sentence = EMPTY_REASON_TEXT[census.empty_reason];
  if (census.empty_reason === "filtered-out" && census.inputs) {
    const max = Math.max(0, ...Object.values(census.inputs));
    if (max > 0) sentence = `${max} row(s) existed and every one failed this step's filters`;
  }
  if (census.filters_applied && census.filters_applied.length > 0) {
    sentence += ` (${census.filters_applied.join("; ")})`;
  }
  return sentence;
}

// ── the closed intent-group taxonomy (docs/kb-code.md's Recipes section) ───

export const RECIPE_INTENT_LABEL: Record<KbcRecipeIntent, string> = {
  orienting: "I'm getting oriented",
  reviewing: "I'm reviewing a PR",
  "checking-tests": "I'm checking the e2e tests",
  rails: "Rails conventions",
  hygiene: "Repo hygiene",
};

export const RECIPE_INTENT_ORDER: readonly KbcRecipeIntent[] = [
  "orienting",
  "reviewing",
  "checking-tests",
  "rails",
  "hygiene",
];

/// Unrecognized/future intent strings sort after the five known groups, in
/// the server's own catalog order, rather than being dropped — an unknown
/// group is a stale client, not a reason to hide the recipe.
export function recipeIntentLabel(intent: string): string {
  return (RECIPE_INTENT_LABEL as Record<string, string>)[intent] ?? intent;
}
