import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// SPA mounts. Topbar visible. /api/* still works alongside.
test.describe("spa load", () => {
  function port(): number {
    return PORT;
  }

  test("GET / returns the SPA shell with the right cache + content-type", async ({
    request,
  }) => {
    const r = await request.get(`http://127.0.0.1:${port()}/`);
    expect(r.status()).toBe(200);
    expect(r.headers()["content-type"]).toContain("text/html");
    expect(r.headers()["cache-control"]).toContain("no-cache");
    const body = await r.text();
    expect(body).toContain('<div id="root">');
    // Vite's main entry filename: pre-B1 was `index-<hash>.js`; B1's
    // multi-entry config renamed it to `main-<hash>.js` because the
    // entry key is now explicit. The pattern stays content-hashed.
    expect(body).toMatch(/\/assets\/main-[A-Za-z0-9_-]+\.js/);
  });

  test("React mounts and topbar renders in chromium", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    // v0.6 gallery refresh: the topbar holds the kb-selector, a search
    // button, and a settings link — no standalone "kb home"/"gallery"
    // links anymore (gallery IS the root route).
    await expect(page.locator(".kb-selector-wrap .kb-ws")).toBeVisible();
    await expect(page.getByRole("link", { name: "Settings" })).toBeVisible();
    // Cmd+K affordance is a button (D5+).
    await expect(
      page.getByRole("button", { name: /open search/i }),
    ).toBeVisible();
  });

  test("kb selector lists configured kbs from /api/kbs", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    // v0.6 moved kb selection out of the left rail into the topbar's
    // kb-selector popover. Open it and confirm the configured kb shows.
    await page.locator(".kb-selector-wrap .kb-ws").click();
    const listbox = page.getByRole("listbox");
    await expect(listbox).toBeVisible();
    await expect(
      listbox.getByRole("option", { name: /canon/ }),
    ).toBeVisible();
  });

  test("status pill renders with at least one daemon", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    await expect(
      page.getByRole("button", { name: /daemon status/i }),
    ).toBeVisible();
  });
});
