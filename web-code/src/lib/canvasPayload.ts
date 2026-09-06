// V3.4-C2 — SPA-owned canvas payload (opaque to the server; see
// `crates/kb-code-server/src/canvas.rs`). Pure encode/decode + fragment
// helpers. Schema is versioned so a future layout can migrate without
// wiping saved canvases.

export const CANVAS_PAYLOAD_VERSION = 1 as const;

/** Server hard cap (256 KiB) — surface byte count next to save state. */
export const CANVAS_PAYLOAD_MAX_BYTES = 256 * 1024;

export interface CanvasFragment {
  path: string;
  symbol: string;
  /** 1-based declaration line at the time the fragment was pinned. */
  line: number;
  x: number;
  y: number;
  /** Optional card width override (px). */
  w?: number;
}

export interface CanvasViewState {
  x: number;
  y: number;
  zoom: number;
}

export interface CanvasPayloadV1 {
  version: 1;
  fragments: CanvasFragment[];
  view: CanvasViewState;
}

export type CanvasPayload = CanvasPayloadV1;

export const EMPTY_CANVAS_PAYLOAD: CanvasPayloadV1 = {
  version: 1,
  fragments: [],
  view: { x: 0, y: 0, zoom: 1 },
};

function isFiniteNumber(v: unknown): v is number {
  return typeof v === "number" && Number.isFinite(v);
}

function parseFragment(raw: unknown): CanvasFragment | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.path !== "string" || o.path === "") return null;
  if (typeof o.symbol !== "string" || o.symbol === "") return null;
  if (!isFiniteNumber(o.line) || o.line < 1) return null;
  if (!isFiniteNumber(o.x) || !isFiniteNumber(o.y)) return null;
  const frag: CanvasFragment = {
    path: o.path,
    symbol: o.symbol,
    line: Math.floor(o.line),
    x: o.x,
    y: o.y,
  };
  if (isFiniteNumber(o.w) && o.w > 0) frag.w = o.w;
  return frag;
}

function parseView(raw: unknown): CanvasViewState {
  if (!raw || typeof raw !== "object") return { ...EMPTY_CANVAS_PAYLOAD.view };
  const o = raw as Record<string, unknown>;
  return {
    x: isFiniteNumber(o.x) ? o.x : 0,
    y: isFiniteNumber(o.y) ? o.y : 0,
    zoom: isFiniteNumber(o.zoom) && o.zoom > 0 ? o.zoom : 1,
  };
}

/**
 * Decode an opaque server payload into a v1 canvas. Unknown/malformed
 * shapes degrade to an empty canvas rather than throwing — a corrupt row
 * must still open so the operator can wipe or rebuild it.
 */
export function decodeCanvasPayload(raw: unknown): CanvasPayloadV1 {
  if (!raw || typeof raw !== "object") return { ...EMPTY_CANVAS_PAYLOAD, fragments: [], view: { ...EMPTY_CANVAS_PAYLOAD.view } };
  const o = raw as Record<string, unknown>;
  // Accept missing version as v1 (early drafts); reject only explicit foreign versions.
  if (o.version !== undefined && o.version !== 1 && o.version !== "1") {
    return { ...EMPTY_CANVAS_PAYLOAD, fragments: [], view: { ...EMPTY_CANVAS_PAYLOAD.view } };
  }
  const fragsRaw = Array.isArray(o.fragments) ? o.fragments : [];
  const fragments: CanvasFragment[] = [];
  for (const f of fragsRaw) {
    const parsed = parseFragment(f);
    if (parsed) fragments.push(parsed);
  }
  return {
    version: 1,
    fragments,
    view: parseView(o.view),
  };
}

/** Serialize a payload for PUT/POST. Always emits version:1 + clean fields. */
export function encodeCanvasPayload(payload: CanvasPayloadV1): CanvasPayloadV1 {
  return {
    version: 1,
    fragments: payload.fragments.map((f) => {
      const out: CanvasFragment = {
        path: f.path,
        symbol: f.symbol,
        line: f.line,
        x: f.x,
        y: f.y,
      };
      if (f.w !== undefined && Number.isFinite(f.w) && f.w > 0) out.w = f.w;
      return out;
    }),
    view: {
      x: payload.view.x,
      y: payload.view.y,
      zoom: payload.view.zoom > 0 ? payload.view.zoom : 1,
    },
  };
}

/** UTF-8 byte length of the JSON the server will store (matches server cap check). */
export function canvasPayloadBytes(payload: CanvasPayloadV1): number {
  return new TextEncoder().encode(JSON.stringify(encodeCanvasPayload(payload))).length;
}

/** Stable fragment key for selection / edge matching (path + symbol + line). */
export function fragmentKey(f: Pick<CanvasFragment, "path" | "symbol" | "line">): string {
  return `${f.path}\0${f.symbol}\0${f.line}`;
}

/**
 * Resolve a pinned fragment against a file's live symbol table.
 * Prefer exact name + nearest line; fall back to unique name match.
 * Returns null when the symbol no longer resolves at HEAD (stale).
 */
export function resolveFragmentSymbol(
  fragment: Pick<CanvasFragment, "path" | "symbol" | "line">,
  symbols: Array<{ name: string; line_start: number; line_end: number }>,
): { name: string; line_start: number; line_end: number } | null {
  const byName = symbols.filter((s) => s.name === fragment.symbol);
  if (byName.length === 0) return null;
  if (byName.length === 1) return byName[0]!;
  // Nearest line_start to the pinned line.
  let best = byName[0]!;
  let bestDist = Math.abs(best.line_start - fragment.line);
  for (let i = 1; i < byName.length; i++) {
    const s = byName[i]!;
    const d = Math.abs(s.line_start - fragment.line);
    if (d < bestDist) {
      best = s;
      bestDist = d;
    }
  }
  return best;
}
