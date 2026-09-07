// V75-M3 — the `~branches` HANDLER CONTRACT, the same shape as
// `recipeCommands.ts` and `searchCommands.ts` (their headers explain the
// defect class it closes: a shipped registry row with no handler is
// silently dead, and `commands/surfaceRows.test.ts` is the general gate).
//
//   1. `BRANCHES_COMMAND_IDS` declares, in one place, every command the
//      branches page owns.
//   2. `BranchesHandlers` is a `Record` over that union, so `BranchViews`
//      cannot compile while a declared id has no function.
//   3. `branchesCommands.test.ts` walks the generated registry BOTH ways.
//
// The keys stay in `commands/registry.json` (kbc-cmd/1 is the ONE
// declaration home); this file names ids only.

export const BRANCHES_COMMAND_IDS = [
  "branches.next",
  "branches.prev",
  "branches.compare",
  "branches.start-review",
  "branches.view-next",
  "branches.view-prev",
  "branches.favourite",
  "branches.radar",
  "branches.fold-prefix",
  "branches.density",
] as const;

export type BranchesCommandId = (typeof BRANCHES_COMMAND_IDS)[number];

/// Every id above, mapped to the function that runs it. `Record` (not
/// `Partial<Record>`) is the whole point — see the header.
export type BranchesHandlers = Record<BranchesCommandId, () => void>;
