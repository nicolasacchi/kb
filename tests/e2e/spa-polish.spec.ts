import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// OL P3 — search bar fit, search focus ring, one desktop status home.

for (const width of [1280, 1440]) {
  test(`the search ViewBar does not clip at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(`http://127.0.0.1:${PORT}/search?q=canon`);
    const bar = page.locator("[data-kb-viewbar]");
    await expect(bar).toBeVisible();
    const fits = await bar.evaluate((el) => el.scrollWidth <= el.clientWidth);
    expect(fits).toBe(true);
  });
}

test("focusing the search input rings the container, not the input", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`http://127.0.0.1:${PORT}/search`);
  const input = page.locator(".kb-search__input");
  await input.focus();
  const [box, inner] = await page.evaluate(() => {
    const b = getComputedStyle(document.querySelector(".kb-search__box")!);
    const i = getComputedStyle(document.querySelector(".kb-search__input")!);
    return [
      { outline: b.outlineStyle, width: b.outlineWidth, shadow: b.boxShadow },
      { outline: i.outlineStyle, width: i.outlineWidth },
    ];
  });
  expect(box.outline).not.toBe("none");
  expect(box.width).not.toBe("0px");
  expect(inner.outline === "none" || inner.width === "0px").toBe(true);
});

test.describe("one status home", () => {
  async function withWarning(page: import("@playwright/test").Page, request: import("@playwright/test").APIRequestContext) {
    // Same drift setup as spa-iframe: a DNS-name parent whose derived suffix
    // disagrees with identity, so the app has an environment warning to show.
    const real = await (await request.get(`http://127.0.0.1:${PORT}/api/identity`)).json();
    await page.route("**/api/identity", (route) =>
      route.fulfill({ json: { ...real, artifact_host_suffix: ".artifacts.localhost" } }),
    );
    await page.goto(`http://kb-host.test:${PORT}/`);
  }

  test("desktop: the Spine indicator carries it and no floating pill mounts", async ({ page, request }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await withWarning(page, request);
    await expect(page.getByRole("button", { name: /environment warning/i })).toBeVisible();
    await expect(page.locator(".pill-container")).toHaveCount(0);
  });

  test("phone: the floating pill is still there", async ({ page, request }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    await withWarning(page, request);
    await expect(page.locator(".pill-container")).toBeVisible();
  });
});
