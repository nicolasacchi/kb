import { defineConfig } from "vitest/config";

// Pin the timezone to UTC for EVERY launch path, not just `npm test`. The
// `npm test` script also sets `TZ=UTC`, but a bare `npx vitest` or an IDE
// Vitest runner bypasses the package.json script and would inherit the host
// TZ — flaking the calendar helpers in lib/time.ts (getHours/getDate are
// local-time). This line runs in the main vitest process before any worker
// spawns, so workers inherit TZ=UTC regardless of how vitest was invoked.
process.env.TZ = "UTC";

// Unit-test harness for the SPA's pure logic — the config deep-merge
// (api/config.ts `mergeConfig`), the card derivations (lib/derive.ts),
// the gallery sort/group helpers (lib/sort.ts), and the small permalink
// / formatting helpers. None of these touch the DOM, so `node` is the
// default environment — jsdom would only slow the run and pull in a heavy
// dep for the majority of tests that don't need it.
//
// MI-W3.R — `.test.tsx` files (React-Testing-Library component tests, e.g.
// routes/memory.test.tsx, components/ProposalRow.test.tsx) opt into jsdom
// per-file via a `// @vitest-environment jsdom` pragma comment at the top of
// the file, rather than flipping the default here — every existing `.ts`
// test stays on the fast `node` environment unchanged.
//
// Timezone is pinned to UTC by the `npm test` script (`TZ=UTC vitest`)
// so the calendar-derived helpers in lib/time.ts assert deterministically
// on a dev box and in CI alike. Tests are colocated as `*.test.ts(x)` next
// to their sources; they're picked up here and by `tsc -b` typecheck, and
// never enter the vite production bundle (unreachable from the entries).
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    clearMocks: true,
  },
});
