import { test, expect, type Page, type Route } from "@playwright/test";
import { BASE } from "./helpers";

// A6 — the SPA work view (`/sessions?view=work`, kb-session-work/1).
// The daemon is never run for this spec: `/api/sessions/work` is
// intercepted with a fixed body (the page.route() fixture idiom, as in
// spa-coderef-doors.spec.ts), which isolates the SPA's rendering of the
// grouping, the Work toggle's URL, and the one-column 390px layout.

function workBody() {
  return {
    schema: "kb-session-work/1",
    since: 1_700_000_000,
    scan_capped: false,
    truncated: false,
    groups: [
      {
        key: "smoke:root-x",
        term: "tree",
        terms: ["tree"],
        started_at: 1_700_000_000,
        last_activity: 1_700_000_500,
        members: [
          {
            kb: "smoke",
            session_id: "root-x",
            role: "root",
            via: "tree",
            started_at: 1_700_000_000,
            ended_at: 1_700_000_500,
            harness: "omp",
            title: "Root work",
          },
        ],
        members_total: 3,
        counts: { logical_sessions: 3, subagents: 2, parts: 3, active_secs: 400 },
      },
      {
        key: "smoke:lone",
        term: "alone",
        terms: [],
        started_at: 1_700_001_000,
        last_activity: 1_700_001_100,
        members: [
          {
            kb: "smoke",
            session_id: "lone",
            role: "member",
            via: "alone",
            started_at: 1_700_001_000,
            ended_at: 1_700_001_100,
            harness: "claude",
            title: "Lone work",
          },
        ],
        members_total: 1,
        counts: { logical_sessions: 1, subagents: 0, parts: 1, active_secs: 100 },
      },
    ],
    skipped: [
      { term: "job", reason: "fanout_cap", detail: "ulid-big on 51 sessions" },
    ],
  };
}

async function fulfillJson(route: Route, json: unknown) {
  await route.fulfill({
    status: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(json),
  });
}

async function stubWork(page: Page) {
  await page.route(
    (url) => url.pathname === "/api/sessions/work",
    (route) => fulfillJson(route, workBody()),
  );
}

test.describe("/sessions work view", () => {
  for (const [width, height] of [
    [1280, 800],
    [390, 844],
  ] as const) {
    test(`Work toggle shows the groups, one column, no overflow (${width}px)`, async ({ page }) => {
      await page.setViewportSize({ width, height });
      await stubWork(page);
      await page.goto(`${BASE}/sessions`);
      await page.getByTestId("sessions-view-work").click();
      await expect(page).toHaveURL(/view=work/);
      const groups = page.getByTestId("work-group");
      await expect(groups).toHaveCount(2);
      await expect(groups.first()).toContainText("subagent tree");
      await expect(groups.first()).toContainText("3 sessions");
      await expect(groups.nth(1)).toContainText("alone");
      await expect(page.getByTestId("work-skipped")).toContainText("ulid-big on 51 sessions");
      // No horizontal overflow at this width.
      const overflow = await page.evaluate(() => {
        const el = document.querySelector('[data-testid="sessions-view"]');
        return el ? el.scrollWidth - el.clientWidth : 0;
      });
      expect(overflow).toBeLessThanOrEqual(1);
      if (width === 390) {
        const boxes = await page.locator(".kb-ses__work-group").evaluateAll((els) =>
          els.map((e) => {
            const r = e.getBoundingClientRect();
            return { x: r.x, width: r.width };
          }),
        );
        // One column: every group spans the same left edge (no side-by-side columns).
        for (const b of boxes) {
          expect(Math.abs(b.x - boxes[0].x)).toBeLessThanOrEqual(1);
          expect(b.width).toBeLessThanOrEqual(width + 1);
        }
      }
    });
  }
});
