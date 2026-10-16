import { test, expect, type Page } from "@playwright/test";
import { PORT } from "./helpers";

// OL FX4 — semantic zoom: ctrl-wheel (trackpad pinch) and the `-` / `=` keys
// step the Library atlas <-> index <-> shelf through ?view=.
const libUrl = (view: string) => `http://127.0.0.1:${PORT}/?kb=canon${view === "grid" ? "" : `&view=${view}`}`;

async function pinch(page: Page, selector: string, deltaY: number) {
  await page.locator(selector).first().evaluate((el, dy) => {
    el.dispatchEvent(new WheelEvent("wheel", { deltaY: dy, ctrlKey: true, bubbles: true, cancelable: true }));
  }, deltaY);
}
const settle = (page: Page) => page.waitForTimeout(450);
const view = (page: Page) => new URL(page.url()).searchParams.get("view") ?? "grid";

test.describe("Semantic zoom", () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => localStorage.removeItem("kb:prefs"));
  });

  test("pinch steps shelf -> index -> atlas and back, one level per gesture", async ({ page }) => {
    await page.goto(libUrl("grid"));
    await expect(page.getByTestId("zoom-indicator")).toBeVisible();
    await pinch(page, ".gallery", 120); // pinch in = zoom out
    await expect.poll(() => view(page)).toBe("list");
    // the tail of the same gesture must not take a second step
    await pinch(page, ".gallery", 120);
    await page.waitForTimeout(100);
    expect(view(page)).toBe("list");
    await settle(page);
    await pinch(page, ".gallery", 120);
    await expect.poll(() => view(page)).toBe("atlas");
    await expect(page.getByTestId("zoom-live")).toHaveText("Library view: Atlas");
    await settle(page);
    await pinch(page, ".gallery", -120);
    await expect.poll(() => view(page)).toBe("list");
    await settle(page);
    await pinch(page, ".gallery", -120);
    await expect.poll(() => view(page)).toBe("grid");
  });

  test("no step at the ends of the ladder", async ({ page }) => {
    await page.goto(libUrl("grid"));
    await pinch(page, ".gallery", -120);
    await page.waitForTimeout(500);
    expect(view(page)).toBe("grid");
  });

  test("no step over the Spine or the ViewBar", async ({ page }) => {
    await page.goto(libUrl("list"));
    await expect(page.getByTestId("zoom-indicator")).toBeVisible();
    await pinch(page, "[data-kb-spine]", -120);
    await pinch(page, ".kb-gallery-viewbar", -120);
    await page.waitForTimeout(500);
    expect(view(page)).toBe("list");
  });

  test("no step with the preference off", async ({ page }) => {
    await page.goto(libUrl("list"));
    await page.evaluate(() => {
      const p = JSON.parse(localStorage.getItem("kb:prefs") || "{}");
      localStorage.setItem("kb:prefs", JSON.stringify({ ...p, pinchZoom: false }));
    });
    await pinch(page, ".gallery", -120);
    await page.waitForTimeout(500);
    expect(view(page)).toBe("list");
  });

  test("the indicator is hidden off the ladder", async ({ page }) => {
    await page.goto(libUrl("history"));
    await expect(page.locator(".kb-gallery-viewbar")).toBeVisible();
    await expect(page.getByTestId("zoom-indicator")).toHaveCount(0);
    await pinch(page, ".gallery", -120);
    await page.waitForTimeout(500);
    expect(view(page)).toBe("history");
  });

  test("- and = step the ladder from the keyboard", async ({ page }) => {
    await page.goto(libUrl("list"));
    await expect(page.getByTestId("zoom-indicator")).toBeVisible();
    await page.keyboard.press("=");
    await expect.poll(() => view(page)).toBe("grid");
    await page.keyboard.press("-");
    await expect.poll(() => view(page)).toBe("list");
    await page.keyboard.press("-");
    await expect.poll(() => view(page)).toBe("atlas");
  });
});
