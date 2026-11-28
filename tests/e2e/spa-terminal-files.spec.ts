import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, TERM_ID } from "./fixtures/fake-tap.mjs";

// FXUI — the Files tab of the terminal's right panel. /files, /ls, /file and
// /paste are mocked (synthetic names and bytes only). Upload a non-image, see
// its row, paste its path (body {text}, no Enter), browse to a file and
// download it (a real download event, the intent header on the request).
// CHROMIUM desktop only.

const RUNNER_PATH = "/run/synthetic/up/notes.txt";

async function admit(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
  });
}

test.describe("FXUI Files tab", () => {
  test("upload a non-image, paste its path, browse and download a file", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    const uploads: { url: string; intent: string | undefined }[] = [];
    const pastes: unknown[] = [];
    const downloads: { url: string; intent: string | undefined }[] = [];
    await page.route(`**/api/terms/${TERM_ID}/files?*`, async (route) => {
      uploads.push({ url: route.request().url(), intent: route.request().headers()["x-kb-intent"] });
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          path: RUNNER_PATH,
          name: "notes.txt",
          bytes: 5,
          kind: "other",
          sha256: "0".repeat(64),
          mime: null,
          kb: { status: "skipped", reason: "not-configured", id: null, stub_rel: null, created: null, inlined: null },
        }),
      });
    });
    await page.route(`**/api/terms/${TERM_ID}/paste`, async (route) => {
      pastes.push(route.request().postDataJSON());
      await route.fulfill({ status: 200, contentType: "application/json", body: "{}" });
    });
    await page.route(`**/api/terms/${TERM_ID}/ls?*`, async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          root: "cwd",
          dir: "/work/synthetic",
          entries: [{ name: "out.txt", kind: "file", bytes: 5, mtime_unix: 1_700_000_000, hidden: false }],
          truncated: false,
          skipped: 0,
        }),
      });
    });
    await page.route(`**/api/terms/${TERM_ID}/file?*`, async (route) => {
      downloads.push({ url: route.request().url(), intent: route.request().headers()["x-kb-intent"] });
      await route.fulfill({
        status: 200,
        headers: { "Content-Type": "application/octet-stream", "Content-Disposition": 'attachment; filename="out.txt"', "Content-Length": "5" },
        body: "hello",
      });
    });

    await page.goto(`${BASE}/term/${TERM_ID}`);
    await page.locator('[data-testid="term-tab-files"]').click({ timeout: 20_000 });

    await page.locator('[data-testid="files-input"]').setInputFiles({ name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("hello") });
    const row = page.locator('[data-testid="files-upload-row"]');
    await expect(row).toHaveCount(1);
    await expect(row.locator('[data-testid="files-runner-path"]')).toHaveText(RUNNER_PATH);
    expect(uploads).toHaveLength(1);
    expect(uploads[0].url).toContain("name=notes.txt");
    expect(uploads[0].url).not.toContain("index=1");
    expect(uploads[0].intent).toBe("terminal");

    await row.getByText("Paste path into terminal").click();
    await expect.poll(() => pastes.length).toBe(1);
    expect(pastes[0]).toEqual({ text: RUNNER_PATH });

    const entry = page.locator('[data-testid="files-entry"]');
    await expect(entry).toHaveCount(1);
    const dl = page.waitForEvent("download");
    await entry.getByText("Download").click();
    const download = await dl;
    expect(download.suggestedFilename()).toBe("out.txt");
    expect(downloads).toHaveLength(1);
    expect(downloads[0].url).toContain("root=cwd");
    expect(downloads[0].url).toContain("path=out.txt");
    expect(downloads[0].intent).toBe("terminal");
  });
});
