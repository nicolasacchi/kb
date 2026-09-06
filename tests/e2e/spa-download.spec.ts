import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// Download controls — single-artifact (raw source HTML) and whole-folder
// (.zip). v0.12 X1 finish retired the FloatingPill download button + the
// siblings-popover "download all" — both were the only Detail-view
// downloads. The remaining live surfaces are gallery-side: per-card
// download (`.card-download`) + folder-header zip (`.gallery-download`),
// each a same-origin link to the daemon's `?download=1` / `/download`
// endpoints, which set Content-Disposition so the browser saves rather
// than navigates. A future PreviewInspector action will restore the
// Detail-view download — covered when it ships.

type Doc = {
  id: string;
  source_relative: string;
  folder: string;
  path: string;
};

async function pmDocs(
  request: import("@playwright/test").APIRequestContext,
): Promise<Doc[]> {
  const r = await request.get(
    `http://127.0.0.1:${PORT}/api/kb/canon/docs?limit=50`,
  );
  expect(r.status()).toBe(200);
  const docs = (await r.json()) as Doc[];
  return docs
    .filter((d) => d.folder === "pm")
    .sort((a, b) =>
      (a.path.split("/").pop() ?? "").localeCompare(b.path.split("/").pop() ?? ""),
    );
}

test.describe("spa download controls", () => {
  test("gallery folder header downloads the folder .zip", async ({ page }) => {
    // A folder filter is active → the controls show the download button.
    await page.goto(`http://127.0.0.1:${PORT}/?kb=canon&folder=pm`);
    const dl = page.locator(".gallery-download");
    await expect(dl).toBeVisible();

    const downloadPromise = page.waitForEvent("download");
    await dl.click();
    const download = await downloadPromise;
    expect(download.suggestedFilename()).toBe("canon-pm.zip");
  });

  test("gallery card download saves the single artifact", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    await page.goto(`http://127.0.0.1:${PORT}/?kb=canon&folder=pm&view=grid`);

    // Hover the first card to reveal its download button, then click it.
    // Cards are rendered as `.kb-card` (renamed from `.card` in the
    // v0.12 redesign); the download anchor is `.kb-card__download`.
    const card = page.locator(".kb-card").first();
    await expect(card).toBeVisible();
    await card.hover();
    const cardDl = card.locator(".kb-card__download");

    const downloadPromise = page.waitForEvent("download");
    await cardDl.click();
    const download = await downloadPromise;
    // Some pm/ file — the suggested name is one of the folder's basenames.
    const names = pm.map((d) => d.path.split("/").pop()!);
    expect(names).toContain(download.suggestedFilename());
    // The download button is a SIBLING of the card link (not nested inside an
    // <a> — invalid HTML + a11y regression), so clicking it downloads without
    // navigating: we're still on the gallery, not an artifact permalink.
    await expect(page).not.toHaveURL(/\/a\/canon\//);
  });

  test("card body navigates via the stretched link (button un-nested)", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?kb=canon&folder=pm&view=grid`);
    const card = page.locator(".kb-card").first();
    await expect(card).toBeVisible();
    // The navigation link is `.kb-card__link` (sibling of the action
    // buttons). Clicking the card body still routes to the artifact.
    await card.locator(".kb-card__link").click();
    await expect(page).toHaveURL(/\/a\/canon\//);
  });
});
