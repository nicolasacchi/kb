import { readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { buildRegionRoutes } from "./regions-routes";
import { REPO_DIR } from "./helpers";

/// V70-A0 — the landmark/aria snapshot GATE for kb-code v7's Track A (the
/// Desk shell decomposition of `Reader.tsx`). docs/research/
/// kb-code-v7-continuum-2026-09.html §P1 names two goldens the refactor
/// must hold: "a landmark golden (region identity, order and keyboard
/// addressing identical across center modes; a mode may collapse a
/// region, never move or rename one)" and a viewport golden (out of scope
/// here — `visual.spec.ts` is the pixel-level check). This spec is that
/// landmark golden, built one wave before the Desk shell itself lands
/// (unit A3): it snapshots TODAY's DOM (the `data-region` attributes this
/// same unit adds to TopBar / the reader's tree `<aside>` / `<main>` /
/// both pane wrappers / the inspector rail `<aside>` / the review side
/// panel `<aside>`) so a future refactor that silently drops, reorders, or
/// renames one of those regions fails HERE first.
///
/// ## Why hand-authored templates instead of `toMatchAriaSnapshot({name})`
///
/// Playwright's own external-snapshot form —
/// `toMatchAriaSnapshot({ name: "<route>.aria.yml" })` — auto-generates the
/// FULL pruned accessibility tree of the target locator on first run and
/// then requires an EXACT match forever after. Run against `body` on a
/// route like `~reviews/:id`, that tree includes the review's own
/// auto-increment id, patchset counts, file lists and timestamps —
/// exactly the "data rows, counts, timestamps, file lists" this
/// deliverable's brief says must never gate the test. Two routes 30
/// seconds apart in the same worker run can legitimately render different
/// review ids; an exact-tree snapshot would flake on that alone.
///
/// Instead, each route's expected template lives as a hand-authored,
/// checked-in `.aria.yml` file under `__snapshots__/regions.spec.ts/`
/// (still "snapshots stored as text under `web-code/e2e/__snapshots__/`",
/// the deliverable's actual ask) listing ONLY the landmark roles that
/// matter — `banner` (TopBar + every route's own local `<header>`, which
/// also computes to `banner` per the HTML-AAM mapping since none of these
/// routes nest their local header inside `<main>`/`<article>`/`<section>`
/// — a pre-existing, not-introduced-here shape this golden simply
/// records), `complementary` (every `<aside>`: the tree, the inspector
/// rail, the review side panel), and `main` (the reader's `<main>`), in
/// DOM order. `toMatchAriaSnapshot`'s default "contain" matching mode
/// (docs/aria-snapshots.md §"Partial matching") checks that the listed
/// roles are present IN THAT ORDER and ignores everything else — headings,
/// button labels, row text, symbol names, counts — so the same template
/// stays green whether a review has 1 file or 40, and across machines/
/// fonts/timestamps. This is deliberately narrower than "diff the whole
/// tree"; it is exactly "region identity and order," which is what the
/// design doc's landmark golden asks for.
///
/// A route whose fixture can't reach it (see `regions-routes.ts`) is
/// recorded as an explicit, reasoned skip — logged to the test report, not
/// silently dropped from the route count.

function loadTemplate(name: string): string {
  const path = join(__dirname, "__snapshots__", "regions.spec.ts", `${name}.aria.yml`);
  return readFileSync(path, "utf-8");
}

test.describe("region landmarks (V70-A0 — the pre-Desk-refactor safety net)", () => {
  test("every reachable client route keeps its landmark shell", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const { routes, cleanup } = await buildRegionRoutes(request);
    const skipped: string[] = [];
    let snapshotted = 0;

    try {
      for (const route of routes) {
        if (route.skip) {
          skipped.push(`${route.name}: ${route.skip}`);
          continue;
        }
        await test.step(route.name, async () => {
          await page.goto(route.url);
          // The one element every route renders — waiting on it means a
          // route that's still fetching its own data doesn't get snapshotted
          // mid-skeleton (the topbar itself never depends on route data).
          // `toMatchAriaSnapshot` itself polls/retries below, so a route
          // whose OWN content is still loading (e.g. a "Loading…" flash)
          // self-corrects within the assertion's own timeout — no extra
          // fixed wait needed.
          await expect(page.locator('[data-region="topbar"]')).toBeVisible();
          const template = loadTemplate(route.name);
          await expect(page.locator("body")).toMatchAriaSnapshot(template);
          snapshotted++;
        });
      }
    } finally {
      // MUST run before the next spec file starts (this daemon/worker is
      // shared across the whole suite, workers:1/fullyParallel:false) — see
      // `regions-routes.ts`'s header doc for the `sets.spec.ts` collision
      // this prevents. `finally` so a mid-loop assertion failure still
      // cleans up rather than leaking the fixture into every later spec.
      await cleanup();
    }

    expect(snapshotted, "at least the always-reachable routes must have snapshotted").toBeGreaterThan(0);
    console.log(
      `regions.spec.ts: ${snapshotted} route(s) snapshotted, ${skipped.length} skipped — ${
        skipped.join(" | ") || "(none)"
      }`,
    );
  });
});
