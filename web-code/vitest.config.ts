import { defineConfig } from "vitest/config";

// Pin the timezone to UTC for every launch path — mirrors kb's own
// web/vitest.config.ts rationale (a bare `npx vitest`/IDE runner bypasses
// the package.json script's `TZ=UTC` prefix otherwise).
process.env.TZ = "UTC";

// Unit-test harness for the SPA's pure logic: highlight-span → CM6
// decoration mapping, tree data transforms, breadcrumb/ref derivations,
// unified-diff parsing. None of these touch the DOM, so `node` is the
// right environment (same reasoning as kb's own web/vitest.config.ts).
// Tests are colocated as `*.test.ts` next to their sources.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    clearMocks: true,
  },
});
