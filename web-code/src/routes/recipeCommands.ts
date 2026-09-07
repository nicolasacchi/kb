// V74-L3c — the recipe home's HANDLER CONTRACT, same shape as
// `searchCommands.ts` (that file's own header explains the defect class
// this closes: a shipped registry row with no handler is silently dead).
//
//   1. `RECIPE_COMMAND_IDS` declares, in one place, every command the
//      recipe home page owns.
//   2. `RecipeHandlers` is a `Record` over that union, so `Recipes.tsx`
//      cannot compile while a declared id has no function.
//   3. `recipeCommands.test.ts` walks the generated registry BOTH WAYS.
//
// The keys themselves stay in `commands/registry.json` (kbc-cmd/1 is the
// ONE declaration home); this file names ids only.

export const RECIPE_COMMAND_IDS = [
  "recipe.run",
  "recipe.view-next",
  "recipe.view-prev",
  "recipe.step-next",
  "recipe.step-prev",
  "recipe.census-open",
  "recipe.materialise",
  "recipe.save-as-set",
  "recipe.trust",
  "recipe.copy-cli",
  "recipe.row-next",
  "recipe.row-prev",
  "recipe.open",
] as const;

export type RecipeCommandId = (typeof RECIPE_COMMAND_IDS)[number];

/// Every id above, mapped to the function that runs it. `Record` (not
/// `Partial<Record>`) is the whole point — see the header.
export type RecipeHandlers = Record<RecipeCommandId, () => void>;
