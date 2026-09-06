import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// G2 (retargeted, v0.12 X1 finish): the floating pill's path chip retired;
// the path is now surfaced as the ContextBar breadcrumb (.kb-ctxbar__crumb)
// and the "copy link" button copies the full permalink URL rather than just
// the path. The old click-to-copy of the bare relative path is gone.
//
// What still holds true and is worth covering:
//   * crumb shows `<folder>` separator + bold filename (or just filename
//     when folder is "")
//   * `[data-kb-act="copy-link"]` puts the full /a/<kb>/<path> URL on
//     the clipboard
test.describe("spa contextbar path + copy-link", () => {
  async function firstDoc(
    request: import("@playwright/test").APIRequestContext,
    filterFolder: "" | "pm",
  ) {
    const r = await request.get(
      `http://127.0.0.1:${PORT}/api/kb/canon/docs?limit=50`,
    );
    expect(r.status()).toBe(200);
    const docs = (await r.json()) as Array<{
      id: string;
      path: string;
      source_relative: string;
      folder: string;
    }>;
    const hit = docs.find((d) => d.folder === filterFolder);
    expect(hit, `no doc with folder="${filterFolder}"`).toBeDefined();
    return hit!;
  }

  test("root-level artifact: crumb shows just the filename; copy-link copies the permalink", async ({
    context,
    page,
    request,
  }) => {
    await context.grantPermissions(["clipboard-read", "clipboard-write"], {
      origin: `http://127.0.0.1:${PORT}`,
    });

    const doc = await firstDoc(request, "");
    const filename = doc.path.split("/").pop() ?? doc.path;

    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${doc.source_relative}`);
    const ctxbar = page.getByRole("navigation", { name: "artifact context" });
    await expect(ctxbar).toBeVisible();

    const crumb = ctxbar.locator(".kb-ctxbar__crumb");
    await expect(crumb).toContainText(filename);
    // No folder separator for a root-level artifact.
    await expect(crumb.locator(".kb-ctxbar__sep")).toHaveCount(0);

    await ctxbar.locator('[data-kb-act="copy-link"]').click();
    const clip = await page.evaluate(() => navigator.clipboard.readText());
    expect(clip).toBe(
      `http://127.0.0.1:${PORT}/a/canon/${doc.source_relative}`,
    );
  });

  test("nested artifact: crumb shows <folder>/<filename>", async ({
    page,
    request,
  }) => {
    const doc = await firstDoc(request, "pm");
    const filename = doc.path.split("/").pop() ?? doc.path;

    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${doc.source_relative}`);
    const crumb = page
      .getByRole("navigation", { name: "artifact context" })
      .locator(".kb-ctxbar__crumb");
    await expect(crumb).toContainText(doc.folder);
    await expect(crumb).toContainText(filename);
    await expect(crumb.locator(".kb-ctxbar__sep")).toHaveCount(1);
  });
});
