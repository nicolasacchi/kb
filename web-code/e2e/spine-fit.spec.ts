import { expect, test, type Page } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";
import { KNOWN_FILE } from "./fixture-repo";

/// FUSPINE -- the shared Spine fits every window from 861 to 1280 px in
/// kb-code: the bar's right edge stays in the viewport, the page does not
/// scroll sideways and every control is on screen.
async function fit(page: Page) {
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
          (el.getAttribute("aria-label") || el.textContent || el.tagName).trim().slice(0, 40) +
            ` [${Math.round(r.left)}..${Math.round(r.right)}]`,
        );
      }
    }
    return {
      innerWidth: window.innerWidth,
      docScrollWidth: document.documentElement.scrollWidth,
      spineRight: Math.round(sr.right),
      spineScrollWidth: spine.scrollWidth,
      spineClientWidth: spine.clientWidth,
      height: Math.round(sr.height),
      outside,
      controls,
    };
  });
}

const WIDTHS: Array<[number, number]> = [
  [1024, 768],
  [900, 700],
  [1100, 800],
  [1280, 720],
];

test.describe("kb-code Spine fits 861-1280 px (FUSPINE)", () => {
  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  const routes: Array<[string, string, string]> = [
    ["home", "/", `[data-kbc-home-card="${REPO_NAME}"]`],
    ["reader", `/r/${REPO_NAME}/${KNOWN_FILE}?desk=read`, ".kbc-codeview"],
  ];

  for (const [w, h] of WIDTHS) {
    for (const [name, path, ready] of routes) {
      test(`${name} at ${w}x${h}`, async ({ page }) => {
        await page.setViewportSize({ width: w, height: h });
        await page.goto(`${BASE}${path}`);
        await expect(page.locator(ready)).toBeVisible({ timeout: 15_000 });
        await page.evaluate(() => document.fonts.ready);
        const m = await fit(page);
        console.log(`spine-fit: kb-code ${name} ${w} ${JSON.stringify(m)}`);
        expect(m.spineRight, "spine right edge").toBeLessThanOrEqual(m.innerWidth);
        expect(m.docScrollWidth, "page scrollWidth").toBeLessThanOrEqual(m.innerWidth);
        expect(m.spineScrollWidth, "spine content wider than the bar").toBeLessThanOrEqual(
          m.spineClientWidth,
        );
        expect(m.outside, "controls outside the viewport").toEqual([]);
        expect(m.height, "no second row").toBeLessThanOrEqual(45);
        // Reachability by accessible name, however a door collapses.
        const bar = page.locator("[data-kb-spine]");
        await expect(bar.getByRole("link", { name: "Settings", exact: true })).toBeVisible();
        await expect(bar.getByRole("link", { name: "Inbox", exact: true })).toBeVisible();
      });
    }
  }
});
