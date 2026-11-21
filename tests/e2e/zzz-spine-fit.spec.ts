import { test, expect, type APIRequestContext, type Page } from "@playwright/test";
import { BASE } from "./helpers";

// FUSPINE -- the shared Spine fits every window from 861 to 1280 px: the bar's
// right edge stays inside the viewport and no page scrolls sideways. At 1024 px
// with the "N waiting" chip showing, the bar used to measure ~1158 px wide.
//
// Named `zzz-` (like zzz-blocked-lane) because it seeds a live session so the
// header chip is present; files run alphabetically on one worker, so the
// session never leaks into an earlier visual spec. The session is ended in
// afterAll.
const SID = `e2e-spine-fit-${Date.now()}`;

async function beat(request: APIRequestContext, event: string, detail?: object) {
  const r = await request.post(`${BASE}/api/sessions/beat`, {
    data: {
      v: 1,
      session_id: SID,
      harness: "claude",
      event,
      at: new Date().toISOString().replace(/\.\d+Z$/, "Z"),
      cwd: "/work/e2e-spine-fit-proj",
      ...(detail ? { detail } : {}),
    },
  });
  expect(r.status(), `beat ${event}`).toBe(200);
}

interface SpineFit {
  innerWidth: number;
  docScrollWidth: number;
  spineRight: number;
  spineScrollWidth: number;
  spineClientWidth: number;
  /** Visible controls (a/button) that start or end outside the viewport. */
  outside: string[];
  controls: number;
}

async function measure(page: Page): Promise<SpineFit> {
  return page.evaluate(() => {
    const spine = document.querySelector<HTMLElement>("[data-kb-spine]")!;
    const sr = spine.getBoundingClientRect();
    const outside: string[] = [];
    let controls = 0;
    for (const el of Array.from(spine.querySelectorAll<HTMLElement>("a, button"))) {
      const cs = getComputedStyle(el);
      if (cs.display === "none" || cs.visibility === "hidden") continue;
      const r = el.getBoundingClientRect();
      if (r.width < 1 || r.height < 1) continue;
      controls += 1;
      if (r.left < -0.5 || r.right > window.innerWidth + 0.5) {
        outside.push(
          (el.getAttribute("aria-label") || el.getAttribute("data-testid") || el.textContent || el.tagName)
            .trim()
            .slice(0, 40) + ` [${Math.round(r.left)}..${Math.round(r.right)}]`,
        );
      }
    }
    return {
      innerWidth: window.innerWidth,
      docScrollWidth: document.documentElement.scrollWidth,
      spineRight: Math.round(sr.right),
      spineScrollWidth: spine.scrollWidth,
      spineClientWidth: spine.clientWidth,
      outside,
      controls,
    };
  });
}

function expectFits(label: string, m: SpineFit) {
  console.log(`spine-fit: ${label} ${JSON.stringify(m)}`);
  expect(m.spineRight, `${label}: spine right edge`).toBeLessThanOrEqual(m.innerWidth);
  expect(m.docScrollWidth, `${label}: page scrollWidth`).toBeLessThanOrEqual(m.innerWidth);
  expect(m.spineScrollWidth, `${label}: spine content wider than the bar`).toBeLessThanOrEqual(
    m.spineClientWidth,
  );
  expect(m.outside, `${label}: controls outside the viewport`).toEqual([]);
}

const WIDTHS: Array<[number, number]> = [
  [1024, 768],
  [900, 700],
  [1100, 800],
  [1280, 720],
];

test.describe("the Spine fits 861-1280 px", () => {
  test.beforeAll(async ({ request }) => {
    await beat(request, "start");
    await beat(request, "blocked", { reason: "Claude needs your permission to use Bash", tool: "Bash" });
  });
  test.afterAll(async ({ request }) => {
    await beat(request, "end");
  });

  const routes: Array<[string, string]> = [
    ["gallery", "/?kb=canon"],
    ["sessions", "/sessions"],
    ["reader", "/a/canon/kitchen-sink.html"],
  ];

  for (const [w, h] of WIDTHS) {
    for (const [name, path] of routes) {
      test(`${name} at ${w}x${h}`, async ({ page }) => {
        await page.setViewportSize({ width: w, height: h });
        await page.goto(`${BASE}${path}`);
        await expect(page.locator("[data-kb-spine]")).toBeVisible();
        // The Now chip is part of what we measure: it must be present.
        await expect(page.getByTestId("header-waiting")).toBeVisible({ timeout: 15_000 });
        await page.evaluate(() => document.fonts.ready);
        const m = await measure(page);
        expectFits(`kb ${name} ${w}`, m);
        // Reachability: the five doors, search and Settings all exist by
        // accessible name even when a door collapses to its icon.
        const doors = page.getByRole("navigation", { name: "Primary" }).getByRole("link");
        await expect(doors).toHaveCount(5);
        for (const label of ["Library", "Search", "Memory", "Worklog", "Lists"]) {
          await expect(
            page.getByRole("navigation", { name: "Primary" }).getByRole("link", { name: label }),
          ).toBeVisible();
        }
        await expect(page.getByRole("button", { name: /open search/ })).toBeVisible();
        await expect(page.getByRole("link", { name: "Settings" })).toBeVisible();
      });
    }
  }

  test("the 44 px bar never wraps to a second row at 1024", async ({ page }) => {
    await page.setViewportSize({ width: 1024, height: 768 });
    await page.goto(`${BASE}/sessions`);
    await expect(page.getByTestId("header-waiting")).toBeVisible({ timeout: 15_000 });
    const h = await page.locator("[data-kb-spine]").evaluate((e) => e.getBoundingClientRect().height);
    expect(h).toBeLessThanOrEqual(45);
  });
});
