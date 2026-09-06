// V70-A6 — Trail v0 (§P7, browser-local).
//
// A trail is a recorded reading journey: an origin plus a list of typed
// hops. It exists so a tab opened from a result row can say WHERE IT CAME
// FROM and WHY, and so `u` can walk back there — the gap the recon named G7
// ("no concept of a trail / origin when following a usage") and the research
// calls the open slot nobody in code tooling fills.
//
// SCOPE OF THIS SLICE — deliberately small, and the boundary is a ruling:
//
//   * **Browser-local, per tab, `sessionStorage`.** Server trails
//     (`kbc-trail/1`: tables, dwell, pause, purge, GC, `kb-code trail`) are
//     v7.4 by the design's own sequencing; this unit adds NO tables and no
//     routes. What it adds is the shape the server version will persist.
//   * **The URL carries an id + an ordinal, never a serialised trail**
//     (`?trail=&step=&via=`, `lib/codeUrl.ts`). A cold tab that cannot
//     resolve the id shows NO chip rather than a dead one — see
//     `lib/tabRegistry.ts` for the BroadcastChannel hand-off that usually
//     resolves it, and note that `sessionStorage` is itself inherited by a
//     `window.open`-ed tab in the browsers we target, so the channel is the
//     second line, not the only one.
//   * **`u` never focuses another tab.** The research is explicit: a
//     `WindowProxy` handle silently no-ops under `noopener`, tab discarding
//     or an OS focus policy, so "never build a feature whose only
//     implementation is the WindowProxy handle". `u` navigates IN PLACE, as
//     a push, so the destination stays exactly one Back away.
//
// The pure half (everything above the `--- session storage ---` line) is what
// the tests pin; the impure half is a thin `sessionStorage` wrapper.

import type { TrailVia } from "./codeUrl";

/// The closed `via` vocabulary, at runtime. Kept in lock-step with
/// `lib/codeUrl.ts`'s `TrailVia` type by `trail.test.ts` (the same
/// hand-mirrored-grammar discipline `langIdForPath` uses for `lang.rs`).
export const TRAIL_VIA: readonly TrailVia[] = [
  "search",
  "definition_of",
  "usage_of",
  "caller_of",
  "blame",
  "why",
  "story",
  "review",
  "framework",
  "bookmark",
  "tree",
  "manual",
];

/// How a `via` reads in a chip. Snake case is the wire vocabulary (it has to
/// match `kbc-trail/1` when the server version lands); this is the display
/// twin, and it is the ONLY place the two forms are related.
export const VIA_LABEL: Readonly<Record<TrailVia, string>> = {
  search: "search",
  definition_of: "definition of",
  usage_of: "usage of",
  caller_of: "caller of",
  blame: "blame",
  why: "why",
  story: "story",
  review: "review",
  framework: "framework",
  bookmark: "bookmark",
  tree: "tree",
  manual: "manual",
};

export const TRAIL_STEPS_CAP = 200;

export interface TrailStep {
  /// Encoded URL of where this hop STARTED (`nav/location.ts`'s `encode`).
  from: string;
  /// Human label for `from` — `"app/models/order.rb:88"`. Stored rather than
  /// re-derived so a chip can render before (or without) parsing the URL.
  fromLabel: string;
  /// Encoded URL of where the hop LANDED.
  to: string;
  via?: TrailVia;
  /// What the edge was ABOUT — a symbol, a query, a finding slug. Free text
  /// from the surface that minted the hop; absent when it does not know one.
  subject?: string;
  /// The trust class the surface carried for this edge (`exact` | `likely` |
  /// `candidate`), when it had one. Never inferred here.
  trust?: string;
  at: number;
}

export interface Trail {
  id: string;
  repo: string;
  /// Encoded URL of the trail's first `from` — where the journey began.
  origin: string;
  originLabel: string;
  created: number;
  steps: TrailStep[];
}

export function newTrailId(rand: () => number = Math.random): string {
  // 8 hex chars is plenty for a per-tab, per-session id and keeps the URL
  // short; collisions across two tabs would only ever mean one tab answering
  // the other's `trail.request` with the wrong journey, and both are the
  // operator's own.
  return Math.floor(rand() * 0xffffffff).toString(16).padStart(8, "0");
}

export function newTrail(
  repo: string,
  origin: string,
  originLabel: string,
  now = Date.now(),
  id = newTrailId(),
): Trail {
  return { id, repo, origin, originLabel, created: now, steps: [] };
}

/// Append a hop. Returns the NEW trail and the 0-based ordinal of the hop —
/// which is exactly what goes in the destination's `?step=`.
///
/// Capped at `TRAIL_STEPS_CAP` from the FRONT (oldest hops fall off), so an
/// all-day session cannot grow the sessionStorage blob without bound. A hop
/// whose ordinal has fallen off resolves to `null` in `stepAt`, and the chip
/// then says nothing — a truthful "I no longer know", never a wrong origin.
export function appendStep(
  trail: Trail,
  step: Omit<TrailStep, "at">,
  now = Date.now(),
  cap = TRAIL_STEPS_CAP,
): { trail: Trail; ordinal: number } {
  const steps = [...trail.steps, { ...step, at: now }];
  const dropped = Math.max(0, steps.length - cap);
  return {
    trail: { ...trail, steps: steps.slice(dropped) },
    // The ordinal is an index into the CAPPED array, so it stays valid for
    // the link we are about to mint.
    ordinal: steps.length - 1 - dropped,
  };
}

export function stepAt(trail: Trail | null, ordinal: number): TrailStep | null {
  if (!trail) return null;
  return trail.steps[ordinal] ?? null;
}

/// The origin chip's text (§P7's literal example:
/// `"↩ from app/models/order.rb:88 (usage_of Order#total, exact)"` — rendered
/// here without the arrow, which the component supplies).
///
/// Every parenthesised part is OPTIONAL and omitted when unknown; the chip
/// never claims an edge kind, a subject or a trust class it was not given.
export function originChipText(step: TrailStep | null): string | null {
  if (!step) return null;
  const bits: string[] = [];
  if (step.via) bits.push(VIA_LABEL[step.via]);
  if (step.subject) bits.push(step.subject);
  const head = `from ${step.fromLabel}`;
  const detail = bits.join(" ");
  if (!detail && !step.trust) return head;
  const inner = [detail, step.trust].filter(Boolean).join(", ");
  return `${head} (${inner})`;
}

/// How many times `path` appears among the trail's landings — the scent
/// card's "visited N× in this trail". Counts LANDINGS only (a `from` is where
/// you already were when you looked, not a visit you chose).
export function visitCount(trail: Trail | null, matches: (url: string) => boolean): number {
  if (!trail) return 0;
  return trail.steps.reduce((n, s) => (matches(s.to) ? n + 1 : n), 0);
}

// --- session storage ------------------------------------------------------

const TRAIL_KEY_PREFIX = "kbc:trail:";
const CURRENT_TRAIL_KEY = "kbc:trail:current";

function ss(): Storage | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    return null;
  }
}

export function loadTrail(id: string): Trail | null {
  const s = ss();
  if (!s || !id) return null;
  try {
    const raw = s.getItem(TRAIL_KEY_PREFIX + id);
    if (!raw) return null;
    const t = JSON.parse(raw) as Trail;
    return t && typeof t.id === "string" && Array.isArray(t.steps) ? t : null;
  } catch {
    return null;
  }
}

export function saveTrail(trail: Trail): void {
  const s = ss();
  if (!s) return;
  try {
    s.setItem(TRAIL_KEY_PREFIX + trail.id, JSON.stringify(trail));
  } catch {
    // Quota/private mode: the trail is an enhancement, never a dependency.
  }
}

/// This tab's own trail id (the one new hops are appended to).
export function currentTrailId(): string | null {
  return ss()?.getItem(CURRENT_TRAIL_KEY) ?? null;
}

export function setCurrentTrailId(id: string): void {
  try {
    ss()?.setItem(CURRENT_TRAIL_KEY, id);
  } catch {
    /* see saveTrail */
  }
}

/// Get this tab's trail, creating it (origin = `origin`/`originLabel`) the
/// first time. Idempotent.
export function ensureTrail(repo: string, origin: string, originLabel: string): Trail {
  const existing = currentTrailId();
  const loaded = existing ? loadTrail(existing) : null;
  if (loaded) return loaded;
  const t = newTrail(repo, origin, originLabel);
  saveTrail(t);
  setCurrentTrailId(t.id);
  return t;
}

/// Record a hop on this tab's trail and return the link to hand the
/// destination (`?trail=&step=&via=`). The whole point of returning the link
/// rather than a boolean: the ONLY way a caller can mint one is by actually
/// recording the hop, so a `?trail=` in the wild always indexes a real step.
export function recordHop(
  repo: string,
  step: Omit<TrailStep, "at">,
): { id: string; step: number; via?: TrailVia } {
  const trail = ensureTrail(repo, step.from, step.fromLabel);
  const { trail: next, ordinal } = appendStep(trail, step);
  saveTrail(next);
  return { id: next.id, step: ordinal, ...(step.via ? { via: step.via } : {}) };
}

/// Adopt a trail handed over by another tab (`lib/tabRegistry.ts`'s
/// `trail.offer`). Never overwrites a trail this tab already has — the
/// offering tab's copy may be older than ours if we have since walked on.
export function adoptTrail(trail: Trail): void {
  if (loadTrail(trail.id)) return;
  saveTrail(trail);
}
