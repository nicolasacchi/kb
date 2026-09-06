import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";

// kb share — the ContextBar "Share" button (`data-kb-act="share"`) opens a
// modal that POSTs to /api/kb/{kb}/share. The deploy is mocked here (real
// Cloudflare/GitHub creds live in the daemon env, exercised only in the
// #[ignore] lane), so these specs verify the UI wiring: the button shows,
// the modal opens, a successful publish surfaces the URL, and the
// GitHub-Pages host drops the gate options.

async function firstDocSourceRel(
  request: import("@playwright/test").APIRequestContext,
): Promise<string> {
  const r = await request.get(`${BASE}/api/kb/canon/docs?limit=1`);
  expect(r.status()).toBe(200);
  const docs = (await r.json()) as { source_relative: string }[];
  expect(docs.length).toBeGreaterThan(0);
  return docs[0].source_relative;
}

async function gotoArtifact(page: Page, sourceRelative: string) {
  await page.goto(`${BASE}/a/canon/${sourceRelative}`);
  await expect(
    page.getByRole("navigation", { name: "artifact context" }),
  ).toBeVisible();
}

test.describe("spa share", () => {
  test("Share button opens the modal and surfaces the URL on success", async ({
    page,
    request,
  }) => {
    const rel = await firstDocSourceRel(request);
    // Mock the deploy → no Cloudflare/GitHub credentials needed.
    await page.route("**/api/kb/canon/share", (route) =>
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          name: "kb-share-x-abc123",
          url: "https://kb-share-x-abc123.pages.dev/index.html",
          host: "cloudflare-pages",
          gate: "email:example.com",
          danglers: [],
          files: 1,
          updated: false,
        }),
      }),
    );
    await gotoArtifact(page, rel);

    await page.locator('[data-kb-act="share"]').click();
    const dialog = page.locator("dialog.share-modal");
    await expect(dialog).toBeVisible();

    // Default host = Cloudflare, gate = email → type an allowed domain.
    await dialog.locator(".share-modal__domain").fill("example.com");
    await dialog.getByRole("button", { name: "publish" }).click();

    const urlInput = dialog.locator(".share-modal__url input");
    await expect(urlInput).toHaveValue(
      "https://kb-share-x-abc123.pages.dev/index.html",
    );
    await expect(dialog.getByRole("button", { name: "copy" })).toBeVisible();
  });

  test("Download bundle method exports a self-contained .zip", async ({
    page,
    request,
  }) => {
    const rel = await firstDocSourceRel(request);
    // Mock the export → a small fake zip + the out-of-band metadata headers.
    await page.route("**/api/kb/canon/share/export", (route) =>
      route.fulfill({
        status: 200,
        contentType: "application/zip",
        headers: {
          "content-disposition": 'attachment; filename="canon-bundle.zip"',
          "x-kb-share-entry": "index.html",
          "x-kb-share-files": "3",
          "x-kb-share-danglers": "",
        },
        body: "PK fake zip bytes",
      }),
    );
    await gotoArtifact(page, rel);

    await page.locator('[data-kb-act="share"]').click();
    const dialog = page.locator("dialog.share-modal");
    await expect(dialog).toBeVisible();

    // Switch method → host/gate options vanish, the CTA flips to download.
    await dialog.getByRole("radio", { name: /Download bundle/ }).check();
    await expect(
      dialog.getByRole("radio", { name: /Cloudflare Pages/ }),
    ).toHaveCount(0);
    await expect(dialog.getByText(/opens offline/)).toBeVisible();

    const cta = dialog.getByRole("button", { name: "download .zip" });
    await expect(cta).toBeVisible();
    const downloadPromise = page.waitForEvent("download");
    await cta.click();
    const download = await downloadPromise;
    expect(download.suggestedFilename()).toBe("canon-bundle.zip");
    await expect(dialog.getByText(/Downloaded/)).toBeVisible();
  });

  test("GitHub Pages host drops the gate options", async ({
    page,
    request,
  }) => {
    const rel = await firstDocSourceRel(request);
    await gotoArtifact(page, rel);

    await page.locator('[data-kb-act="share"]').click();
    const dialog = page.locator("dialog.share-modal");
    await expect(dialog).toBeVisible();
    // Cloudflare (default) shows the gate fieldset.
    await expect(dialog.getByText("Who can open it")).toBeVisible();
    // Switching to GitHub Pages removes it (public-only lane).
    await dialog.getByRole("radio", { name: /GitHub Pages/ }).check();
    await expect(dialog.getByText("Who can open it")).toHaveCount(0);
    await expect(dialog.getByText(/world-readable/)).toBeVisible();
  });
});
