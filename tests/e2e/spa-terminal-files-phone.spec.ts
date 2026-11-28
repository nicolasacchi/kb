import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, TERM_ID } from "./fixtures/fake-tap.mjs";

// FXPHONE — the Files tab inside the phone sheet at 360x740 with touch
// emulation. /files, /ls, /files/keep, /kept/ and /paste are mocked (synthetic
// names and bytes only). Emulation only: a real Android Firefox / Chrome run
// (downloads, the OS chooser, the real 360 px layout) is a manual check.
// CHROMIUM only.

test.use({ viewport: { width: 360, height: 740 }, hasTouch: true, isMobile: true });

const NAME = "a b'c.txt";

async function admit(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
  });
}

const answer = (name: string) => ({
  path: `/run/synthetic/up/${name}`,
  name,
  bytes: 5,
  kind: "other",
  sha256: "0".repeat(64),
  mime: null,
  kb: { status: "skipped", reason: "not-configured", id: null, stub_rel: null, created: null, inlined: null },
});

async function openFiles(page: Page): Promise<void> {
  await page.goto(`${BASE}/term/${TERM_ID}`);
  await page.locator('[data-testid="term-sheet-handle"]').click({ timeout: 20_000 });
  await page.locator('[data-testid="term-tab-files"]').click();
  await expect(page.locator('[data-testid="files-rail"]')).toBeVisible();
}

test.describe("FXPHONE Files tab on a phone", () => {
  test("fits 360 px, every control is 44 px, uploads queue, keep sends index:false, paste is quoted", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    const uploads: string[] = [];
    const keeps: unknown[] = [];
    const pastes: unknown[] = [];
    await page.route(`**/api/terms/${TERM_ID}/files?*`, async (route) => {
      const u = new URL(route.request().url());
      uploads.push(u.searchParams.get("name") ?? "");
      await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(answer(u.searchParams.get("name") ?? "x")) });
    });
    await page.route(`**/api/terms/${TERM_ID}/files/keep`, async (route) => {
      keeps.push(route.request().postDataJSON());
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ name: NAME, bytes: 5, sha256: "0".repeat(64), kb: { status: "stored", reason: null, id: "kid1", stub_rel: "files/a.html", created: true, inlined: false } }),
      });
    });
    await page.route(`**/api/terms/${TERM_ID}/kept/*`, (route) =>
      route.fulfill({ status: 200, headers: { "Content-Type": "application/octet-stream", "Content-Length": "5" }, body: "hello" }),
    );
    await page.route(`**/api/terms/${TERM_ID}/paste`, async (route) => {
      pastes.push(route.request().postDataJSON());
      await route.fulfill({ status: 200, contentType: "application/json", body: "{}" });
    });
    await page.route(`**/api/terms/${TERM_ID}/ls?*`, (route) =>
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          root: "cwd",
          dir: "/work/synthetic",
          entries: [{ name: NAME, kind: "file", bytes: 5, mtime_unix: 1_700_000_000, hidden: false }],
          truncated: false,
          skipped: 0,
        }),
      }),
    );

    await openFiles(page);
    const rail = page.locator('[data-testid="files-rail"]');
    await expect(rail.locator('[data-testid="files-entry"]')).toHaveCount(1);

    // No horizontal scroll at 360 px.
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(360);

    // Two files through the picker: sequential rows.
    await page.locator('[data-testid="files-input"]').setInputFiles([
      { name: "one.txt", mimeType: "text/plain", buffer: Buffer.from("hello") },
      { name: "two.txt", mimeType: "text/plain", buffer: Buffer.from("hello") },
    ]);
    await expect(rail.locator('[data-testid="files-upload-row"]')).toHaveCount(2);
    await expect(rail.locator('[data-testid="files-runner-path"]')).toHaveCount(2);
    expect(uploads).toEqual(["one.txt", "two.txt"]);

    // Keep a file: index is off.
    await rail.locator('[data-testid="files-entry"]').getByText("Keep in kb").click();
    await expect(rail.locator('[data-testid="files-keep-result"]')).toContainText("Kept in kb");
    expect(keeps).toEqual([{ path: NAME, root: "cwd", index: false }]);

    // Paste: the single-quoted path, no Enter.
    await rail.locator('[data-testid="files-entry"]').getByText("Paste path into terminal").click();
    await expect.poll(() => pastes.length).toBe(1);
    expect(pastes[0]).toEqual({ text: `'/work/synthetic/a b'\\''c.txt'` });
    expect(JSON.stringify(pastes[0])).not.toMatch(/enter/i);

    // Every control is a >= 44 px target and nothing leaves the viewport.
    const controls = rail.locator("button, label");
    const n = await controls.count();
    expect(n).toBeGreaterThan(5);
    for (let i = 0; i < n; i += 1) {
      const el = controls.nth(i);
      if (!(await el.isVisible())) continue;
      const box = (await el.boundingBox())!;
      expect(box.height, `control ${i} height`).toBeGreaterThanOrEqual(43.5);
      expect(box.x + box.width, `control ${i} right edge`).toBeLessThanOrEqual(361);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(360);
  });
});
