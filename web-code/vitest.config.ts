import { defineConfig } from "vitest/config";

// Pin the timezone to UTC for every launch path — mirrors kb's own
// web/vitest.config.ts rationale (a bare `npx vitest`/IDE runner bypasses
// the package.json script's `TZ=UTC` prefix otherwise).
process.env.TZ = "UTC";

// Unit-test harness for the SPA's pure logic: highlight-span → CM6
// decoration mapping, tree data transforms, breadcrumb/ref derivations,
// unified-diff parsing. None of these touch the DOM, so `node` is the
// right environment (same reasoning as kb's own web/vitest.config.ts).
// Tests are colocated as `*.test.ts` next to their sources. A handful of
// these `*.test.ts` files DO render a real component tree (e.g.
// `components/prose/ProseBlock.test.ts`, `components/home/
// RepoCard.catchingUp.test.ts`) via `react-dom/server`'s
// `renderToStaticMarkup` — that runs fine under plain `node` (it is the
// SSR path, no DOM needed) and is this repo's established way to pin
// rendered markup without React Testing Library; a smaller set instead
// opts a single file into `jsdom` via a `// @vitest-environment jsdom`
// pragma comment for the rare case that needs real DOM events
// (`components/hierarchy/HierarchyPanel.close.test.ts`).
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    clearMocks: true,
  },
});
