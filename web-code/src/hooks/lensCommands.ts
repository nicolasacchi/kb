// V73-K6 — the doc↔code lens page's HANDLER CONTRACT, same shape as
// `searchCommands.ts`/`browserCommands.ts`.
//
// `lens.row-next`/`lens.row-prev`/`lens.group-next`/`lens.group-prev`
// shipped in `registry.json` (`scope: "board"`, `when: "board == lens"`)
// with a real, working implementation in `hooks/useLensKeys.ts` — but that
// hook hard-coded `e.key === "j"`/`"k"`/`"("`/`")"` directly, never asking
// the registry what a key means (a second home for these keys). This file
// closes it, in the shape `commands/surfaceRows.test.ts` wants: a fixed id
// list, a `Record` handler type the hook cannot compile without filling,
// and `lensCommands.test.ts`'s bidirectional walk.

export const LENS_COMMAND_IDS = [
  "lens.row-next",
  "lens.row-prev",
  "lens.group-next",
  "lens.group-prev",
] as const;

export type LensCommandId = (typeof LENS_COMMAND_IDS)[number];

/// Every id above, mapped to the function that runs it. `Record` (not
/// `Partial<Record>`) is the whole point — see the header.
export type LensHandlers = Record<LensCommandId, () => void>;
