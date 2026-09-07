// V73-K6 — the symbol browser's HANDLER CONTRACT, same shape as
// `searchCommands.ts` (V71-D2's own precedent, named in that file's header
// as "a future unit's job"): this is that unit, for `~browser`.
//
// `board.pane-prev`/`board.pane-next`/`board.row-next`/`board.row-prev`/
// `board.drill` shipped in `registry.json` (`scope: "board"`,
// `when: "board == browser"`) since before this unit, with a real, working
// implementation in `BrowserPage`'s own `onKey` — but that implementation
// hard-coded `e.key === "h"`/`"j"`/etc. directly, never asking the registry
// what a key means. That is a SECOND home for a key (`web-code/CLAUDE.md`'s
// keyboard section: "cmd/1 is the ONLY home of a key"), and it is exactly
// why `commands/surfaceRows.test.ts` found these five rows unclaimed: they
// have no `useCommandHandlers` registration and no `vimKind` anywhere.
//
//   1. `BROWSER_COMMAND_IDS` declares, in one place, every command the
//      symbol browser owns.
//   2. `BrowserHandlers` is a `Record` over that union, so `Browser.tsx`
//      cannot compile while a declared id has no function.
//   3. `browserCommands.test.ts` walks the generated registry BOTH WAYS.
//
// The keys themselves stay in `commands/registry.json`; this file names ids
// only. `Browser.tsx`'s `onKeyDown` now asks `commands/dispatch.ts`'s
// `resolve()` — the SAME resolver `CommandRoot` uses — which id a keystroke
// names in scope `"board"`, and runs that id's handler here.

export const BROWSER_COMMAND_IDS = [
  "board.pane-prev",
  "board.pane-next",
  "board.row-next",
  "board.row-prev",
  "board.drill",
] as const;

export type BrowserCommandId = (typeof BROWSER_COMMAND_IDS)[number];

/// Every id above, mapped to the function that runs it. `Record` (not
/// `Partial<Record>`) is the whole point — see the header.
export type BrowserHandlers = Record<BrowserCommandId, () => void>;
