// V71-D2 — the results page's HANDLER CONTRACT.
//
// The v7.0 defect class was a registry row with no handler: five `pane.*`
// commands shipped in `commands/registry.json` and did nothing, and the
// SPA-side note in `web-code/CLAUDE.md` says plainly that "there is no test
// today that a shipped central row has a registered handler in every
// reachable route (a real gap; a future unit's job)".
//
// This is that test, for this one surface, in the cheapest form that
// actually holds:
//
//   1. `SEARCH_COMMAND_IDS` declares, in one place, every command the
//      results page owns.
//   2. `SearchHandlers` is a `Record` over that union, so `Search.tsx`
//      cannot compile while a declared id has no function. That is a
//      COMPILE-time guarantee, not a runtime hope.
//   3. `searchCommands.test.ts` walks the generated registry BOTH WAYS: a
//      `scope: "search"` row missing from this list fails, and an id here
//      with no registry row fails. So neither half can drift.
//
// The keys themselves stay in `commands/registry.json` (kbc-cmd/1 is the ONE
// declaration home); this file names ids only.

export const SEARCH_COMMAND_IDS = [
  "search.row.next",
  "search.row.prev",
  "search.group.next",
  "search.group.prev",
  "search.open",
  "search.refine",
  "search.refine.clear",
  "search.facets",
  "search.group.cycle",
  "search.preview",
  "search.copy-cli",
  "search.save",
  "search.history",
  "search.keep",
  "search.stack.next",
  "search.stack.prev",
] as const;

export type SearchCommandId = (typeof SEARCH_COMMAND_IDS)[number];

/// Every id above, mapped to the function that runs it. `Record` (not
/// `Partial<Record>`) is the whole point — see the header.
export type SearchHandlers = Record<SearchCommandId, () => void>;
