// V76-R3d — file-scoped time scrubber. Pure: nearest-prior pick, miss
// before the floor, step prev/next, label derivation. The SPA re-reads
// through `?ref=` (`applyRef`); this module never fetches.

export type AuthorKind = "exact" | "likely" | "none";

export interface ScrubStop {
  sha: string;
  when: number;
  author_kind: AuthorKind;
  subject: string;
  insertions: number;
  deletions: number;
  path: string;
  renamed_from?: string | null;
}

export interface ScrubFloor {
  sha: string;
  when: number;
}

export type ScrubPos =
  | { kind: "working-tree" }
  | { kind: "stop"; index: number; resolution: "nearest-prior" | "exact" }
  | { kind: "before-floor" };

/** Newest-first. Same pick as `kb_core::versions::resolve_as_of`. */
export function resolveAsOf(stops: readonly ScrubStop[], at: number): ScrubStop | undefined {
  return stops.find((s) => s.when <= at);
}

export function resolutionOf(stop: ScrubStop, at: number): "nearest-prior" | "exact" {
  return stop.when === at ? "exact" : "nearest-prior";
}

export function posFromAt(stops: readonly ScrubStop[], at: number): ScrubPos {
  const hit = resolveAsOf(stops, at);
  if (!hit) return { kind: "before-floor" };
  const index = stops.indexOf(hit);
  return { kind: "stop", index, resolution: resolutionOf(hit, at) };
}

/** Initialise from the reader's `?ref=`. A named ref that is not a stop
 * sha is treated as the newest stop (the usual "I opened the file at a
 * branch tip" case) so stepping back walks history rather than no-op'ing. */
export function posFromRef(stops: readonly ScrubStop[], ref: string | undefined): ScrubPos {
  if (!ref) return { kind: "working-tree" };
  const index = stops.findIndex(
    (s) => s.sha === ref || s.sha.startsWith(ref) || ref.startsWith(s.sha),
  );
  if (index >= 0) return { kind: "stop", index, resolution: "exact" };
  if (stops.length === 0) return { kind: "working-tree" };
  return { kind: "stop", index: 0, resolution: "nearest-prior" };
}

/** Older. From the working tree → newest stop; from the oldest stop → miss. */
export function stepPrev(pos: ScrubPos, stops: readonly ScrubStop[]): ScrubPos {
  if (stops.length === 0) return { kind: "before-floor" };
  if (pos.kind === "working-tree") {
    return { kind: "stop", index: 0, resolution: "nearest-prior" };
  }
  if (pos.kind === "before-floor") return pos;
  const next = pos.index + 1;
  if (next >= stops.length) return { kind: "before-floor" };
  return { kind: "stop", index: next, resolution: "nearest-prior" };
}

/** Newer. From the miss → oldest stop; from the newest stop → working tree. */
export function stepNext(pos: ScrubPos, stops: readonly ScrubStop[]): ScrubPos {
  if (pos.kind === "working-tree") return pos;
  if (pos.kind === "before-floor") {
    if (stops.length === 0) return pos;
    return { kind: "stop", index: stops.length - 1, resolution: "nearest-prior" };
  }
  if (pos.index <= 0) return { kind: "working-tree" };
  return { kind: "stop", index: pos.index - 1, resolution: "nearest-prior" };
}

export function shortSha(sha: string, n = 12): string {
  return sha.length <= n ? sha : sha.slice(0, n);
}

export function labelFor(
  pos: ScrubPos,
  stops: readonly ScrubStop[],
  floor: ScrubFloor | null | undefined,
): string {
  if (pos.kind === "before-floor") {
    const oldest = floor ? shortSha(floor.sha) : "none";
    return `before the floor (${oldest})`;
  }
  if (pos.kind === "working-tree") return "working tree";
  const s = stops[pos.index];
  if (!s) return "working tree";
  if (pos.resolution === "exact") return `exact · ${shortSha(s.sha)} · ${s.when}`;
  return `nearest-prior · ${shortSha(s.sha)} · ${s.when}`;
}

export function shaForPos(pos: ScrubPos, stops: readonly ScrubStop[]): string | undefined {
  if (pos.kind !== "stop") return undefined;
  return stops[pos.index]?.sha;
}
