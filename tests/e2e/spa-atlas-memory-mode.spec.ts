import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// MI-W4.5 — atlas memory mode. The seeded `mem` kb (global-setup.ts) is
// `memory_scope = "global"` with two memories ("Prefers tabs" salience 0.9,
// "Deploy pipeline" salience 0.6, both slow-decay) — enough for the atlas's
// salience/decay color-mode switcher to appear and repaint.

test.describe("atlas memory mode — MI-W4.5", () => {
  test("the color-by switcher appears on a memory-scoped kb and repaints on click", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas&kb=mem`);
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();

    const colorMode = page.getByTestId("atlas-colormode");
    await expect(colorMode).toBeVisible({ timeout: 10_000 });

    await page.getByTitle(/color dots by memory salience/).click();
    await expect(
      page.getByRole("button", { name: "salience" }),
    ).toHaveAttribute("aria-pressed", "true");

    await page.getByTitle(/color dots by decay bucket/).click();
    await expect(page.getByRole("button", { name: "decay" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  test("the color-by switcher is absent on a non-memory-scoped kb", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas&kb=canon`);
    await expect(page.locator(".atlas__canvas")).toBeVisible();
    await expect(page.getByTestId("atlas-colormode")).toHaveCount(0);
  });
});
