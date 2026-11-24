// HUBCORE — browser-local terminal labels and pins.
//
// Terminal ids are runner-ephemeral, so nothing here roams or reaches the
// daemon. Both stores are bounded (MAX_PREFS, oldest-inserted evicted), keyed
// by ids that pass the id grammar, and every storage failure (private mode,
// quota, corrupt JSON) degrades to "no prefs" instead of throwing. Labels are
// stored as an ordered pair list because an object would reorder integer-like
// ids and break oldest-first eviction.

export type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

export const LABELS_KEY = "kb-term-labels";
export const PINS_KEY = "kb-term-pins";
export const MAX_LABEL_CHARS = 40;
export const MAX_PREFS = 64;

const ID_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
// eslint-disable-next-line no-control-regex
const CONTROL_RE = /[\u0000-\u001f\u007f-\u009f]/g;

type Pair = [string, string];

function defaultStore(): StorageLike | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

function readJson(store: StorageLike | null, key: string): unknown {
  if (store === null) return null;
  try {
    const raw = store.getItem(key);
    return raw === null ? null : (JSON.parse(raw) as unknown);
  } catch {
    return null;
  }
}

function writeJson(store: StorageLike | null, key: string, value: unknown[]): void {
  if (store === null) return;
  try {
    if (value.length === 0) store.removeItem(key);
    else store.setItem(key, JSON.stringify(value));
  } catch {
    // quota / private mode: prefs are a convenience, never an error.
  }
}

export function cleanLabel(raw: string): string {
  const stripped = raw.replace(CONTROL_RE, "").trim();
  return Array.from(stripped).slice(0, MAX_LABEL_CHARS).join("").trim();
}

function readLabels(store: StorageLike | null): Pair[] {
  const v = readJson(store, LABELS_KEY);
  if (!Array.isArray(v)) return [];
  const seen = new Set<string>();
  const out: Pair[] = [];
  for (const e of v) {
    if (!Array.isArray(e) || e.length !== 2) continue;
    const [id, label] = e as unknown[];
    if (typeof id !== "string" || typeof label !== "string" || !ID_RE.test(id)) continue;
    const clean = cleanLabel(label);
    if (clean === "" || seen.has(id)) continue;
    seen.add(id);
    out.push([id, clean]);
  }
  return out.slice(-MAX_PREFS);
}

function readPins(store: StorageLike | null): string[] {
  const v = readJson(store, PINS_KEY);
  if (!Array.isArray(v)) return [];
  const out: string[] = [];
  for (const id of v) {
    if (typeof id === "string" && ID_RE.test(id) && !out.includes(id)) out.push(id);
  }
  return out.slice(-MAX_PREFS);
}

export function loadPrefs(store: StorageLike | null = defaultStore()): {
  labels: Record<string, string>;
  pins: Set<string>;
} {
  try {
    const labels: Record<string, string> = {};
    for (const [id, l] of readLabels(store)) labels[id] = l;
    return { labels, pins: new Set(readPins(store)) };
  } catch {
    return { labels: {}, pins: new Set() };
  }
}

/** Set (or, with null/empty, clear) a label. An id outside the grammar is ignored. */
export function setLabel(
  id: string,
  label: string | null,
  store: StorageLike | null = defaultStore(),
): void {
  if (typeof id !== "string" || !ID_RE.test(id)) return;
  const pairs = readLabels(store).filter(([k]) => k !== id);
  const clean = typeof label === "string" ? cleanLabel(label) : "";
  if (clean !== "") pairs.push([id, clean]);
  writeJson(store, LABELS_KEY, pairs.slice(-MAX_PREFS));
}

/** Toggle a pin; returns the new pinned state (false for an id outside the grammar). */
export function togglePin(id: string, store: StorageLike | null = defaultStore()): boolean {
  if (typeof id !== "string" || !ID_RE.test(id)) return false;
  const pins = readPins(store);
  const had = pins.includes(id);
  const next = had ? pins.filter((p) => p !== id) : [...pins, id].slice(-MAX_PREFS);
  writeJson(store, PINS_KEY, next);
  return !had;
}

/**
 * Drop every label and pin whose id is not in `liveIds`; writes only what
 * changed. It prunes literally: callers must invoke it only after a SUCCESSFUL
 * list load (`isError` false), never from a failed or empty-because-failed
 * fetch, or one network blip would erase every label.
 */
export function prunePrefs(
  liveIds: readonly string[],
  store: StorageLike | null = defaultStore(),
): void {
  const live = new Set(liveIds);
  const labels = readLabels(store);
  const keptLabels = labels.filter(([id]) => live.has(id));
  if (keptLabels.length !== labels.length) writeJson(store, LABELS_KEY, keptLabels);
  const pins = readPins(store);
  const keptPins = pins.filter((id) => live.has(id));
  if (keptPins.length !== pins.length) writeJson(store, PINS_KEY, keptPins);
}
