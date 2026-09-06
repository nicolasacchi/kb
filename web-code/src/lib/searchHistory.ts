// V71-D2 — search history and saved searches, BROWSER-LOCAL.
//
// A recorded cut, not an omission. The search research proposes a
// `saved_searches` table on the daemon; this ships them in `localStorage`
// instead, for the reason the v7 design ratified for prefs/attention/trail
// v0 (D-list: "Prefs/attention/trail v0 browser-local; kb-code has one
// identity"). Two consequences, both stated rather than hidden:
//
//   - `kb-code search saved` does not exist and this unit does not pretend
//     it does. CLI parity for a saved search is the COPIED COMMAND LINE
//     (`kb-code search '<kbcq>'`) the results page puts one key away —
//     which works today, needs no server round trip, and is pasteable into
//     an agent prompt.
//   - A saved search does not follow you to another browser. When the
//     server grows a home for them, this module's shape (a name plus ONE
//     kbcq/1 string) is what it should store — there is nothing here that
//     is not already expressible in the grammar.
//
// Pure over an injected `StorageLike`, exactly like `desk/deskState.ts`, so
// the unit suite exercises every branch — including a corrupt blob — with
// no DOM.

/// The subset of `Storage` this module needs. Injected so tests (which run
/// `environment: "node"`) never touch a global.
export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export const HISTORY_KEY = "kbc.search.history.v1";
export const SAVED_KEY = "kbc.search.saved.v1";

/// How many past queries the ring keeps. Small on purpose: history is a
/// convenience for the last few reformulations (Sadowski's ~⅓-of-searches-
/// are-reformulations finding), not an archive.
export const HISTORY_CAP = 30;

/// A saved search is a NAME and a kbcq/1 string. Nothing else — no result
/// snapshot, no scope object, no filters sidecar. Anything a saved search
/// needs to say must be sayable in the grammar, which is what keeps it
/// pasteable into the CLI.
export interface SavedSearch {
  name: string;
  query: string;
}

function readList(storage: StorageLike, key: string): unknown {
  try {
    const raw = storage.getItem(key);
    if (raw === null) return null;
    return JSON.parse(raw) as unknown;
  } catch {
    // A corrupt blob degrades to "no history", never to a thrown render.
    return null;
  }
}

function writeJson(storage: StorageLike, key: string, value: unknown): void {
  try {
    storage.setItem(key, JSON.stringify(value));
  } catch {
    // A full/blocked quota loses the write, never the page.
  }
}

/// Most recent first. Anything that is not an array of non-empty strings is
/// discarded wholesale rather than partially trusted.
export function loadHistory(storage: StorageLike): string[] {
  const v = readList(storage, HISTORY_KEY);
  if (!Array.isArray(v)) return [];
  return v.filter((x): x is string => typeof x === "string" && x.trim() !== "").slice(0, HISTORY_CAP);
}

/// Record `query` as the newest entry. De-duplicates (an exact repeat MOVES
/// to the front rather than adding a second row) and never records a blank.
/// Returns the new list; the caller persists it.
export function pushHistory(storage: StorageLike, query: string): string[] {
  const q = query.trim();
  if (q === "") return loadHistory(storage);
  const next = [q, ...loadHistory(storage).filter((x) => x !== q)].slice(0, HISTORY_CAP);
  writeJson(storage, HISTORY_KEY, next);
  return next;
}

export function clearHistory(storage: StorageLike): string[] {
  writeJson(storage, HISTORY_KEY, []);
  return [];
}

export function loadSaved(storage: StorageLike): SavedSearch[] {
  const v = readList(storage, SAVED_KEY);
  if (!Array.isArray(v)) return [];
  return v.filter(
    (x): x is SavedSearch =>
      typeof x === "object" &&
      x !== null &&
      typeof (x as SavedSearch).name === "string" &&
      typeof (x as SavedSearch).query === "string" &&
      (x as SavedSearch).name.trim() !== "",
  );
}

/// Save `query` under `name`, replacing a same-named entry IN PLACE (so the
/// list order a human arranged does not shuffle under an update). A blank
/// name or a blank query is refused — returning the list unchanged rather
/// than writing an unusable row.
export function saveSearch(storage: StorageLike, name: string, query: string): SavedSearch[] {
  const n = name.trim();
  const q = query.trim();
  if (n === "" || q === "") return loadSaved(storage);
  const cur = loadSaved(storage);
  const at = cur.findIndex((s) => s.name === n);
  const next = at >= 0 ? cur.map((s, i) => (i === at ? { name: n, query: q } : s)) : [...cur, { name: n, query: q }];
  writeJson(storage, SAVED_KEY, next);
  return next;
}

export function deleteSaved(storage: StorageLike, name: string): SavedSearch[] {
  const next = loadSaved(storage).filter((s) => s.name !== name);
  writeJson(storage, SAVED_KEY, next);
  return next;
}

/// The CLI line that runs `query` — the parity affordance the results page
/// copies with one key. Single-quoted with POSIX `'\''` escaping, so a
/// query containing a quote still pastes and runs.
export function cliLineFor(query: string, repo?: string): string {
  const shell = `'${query.replace(/'/g, `'\\''`)}'`;
  const scope = repo ? ` --repo ${repo}` : "";
  return `kb-code search ${shell}${scope}`;
}
