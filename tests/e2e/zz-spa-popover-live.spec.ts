import { test, expect } from "@playwright/test";
import { writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { PORT } from "./helpers";

// G6/G7 — the PreviewInspector's Folder section refreshes its
// descendants list when the daemon emits artifact.indexed /
// artifact.removed. This spec writes a real file into pm/, asserts
// the section shows the new sibling, then removes the file and asserts
// the row disappears. (X1 finish retired the popover-open refetch path;
// G7 made the section permanently visible, so the SSE refresh is the
// only live-update channel.)
//
// `zz-` prefix so it runs after spa-siblings (which assumes pm/ has 4
// direct children); the try/finally guarantees cleanup even on test
// failure so later runs aren't poisoned.

test.describe("live folder section (PreviewInspector)", () => {
  const CORPUS = process.env.KB_E2E_CORPUS;
  const PROBE_NAME = "z-live-sibling.html";
  const PROBE_TITLE = "Live sibling probe";
  const PROBE_HTML = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>${PROBE_TITLE}</title>
  </head>
  <body>
    <h1>${PROBE_TITLE}</h1>
    <p>Written by zz-spa-popover-live.spec.ts to exercise G6.</p>
  </body>
</html>
`;

  async function pmFirstRel(
    request: import("@playwright/test").APIRequestContext,
  ): Promise<string> {
    const r = await request.get(
      `http://127.0.0.1:${PORT}/api/kb/canon/docs?limit=50`,
    );
    expect(r.status()).toBe(200);
    type Doc = { source_relative: string; folder: string; path: string };
    const docs = (await r.json()) as Doc[];
    const pm = docs
      .filter((d) => d.folder === "pm")
      .sort((a, b) =>
        (a.path.split("/").pop() ?? "").localeCompare(
          b.path.split("/").pop() ?? "",
        ),
      );
    expect(pm.length, "pm/ should have 4 direct docs at baseline").toBe(4);
    return pm[0].source_relative;
  }

  test.beforeEach(async ({ page }) => {
    // Make sure the inspector starts expanded so the Folder section is
    // visible — earlier test workers may have toggled it shut.
    await page.goto(`http://127.0.0.1:${PORT}/`);
    await page.evaluate(() => {
      try {
        localStorage.removeItem("kb:inspector-collapsed.detail");
        localStorage.removeItem("kb:siblings");
      } catch {
        /* noop */
      }
    });
  });

  test("artifact.indexed grows the folder list; artifact.removed shrinks it", async ({
    page,
    request,
  }) => {
    expect(
      CORPUS,
      "KB_E2E_CORPUS must be exported by global-setup.ts",
    ).toBeTruthy();
    const probePath = join(CORPUS as string, "pm", PROBE_NAME);
    const firstRel = await pmFirstRel(request);

    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${firstRel}`);
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();
    const list = page.locator(".kb-pinsp__folder-list");
    // F2 — subfolder DIR rows are ambient; count FILE rows only.
    const fileRows = list.locator(
      ".kb-pinsp__folder-row:not(.kb-pinsp__folder-row--dir)",
    );
    await expect(fileRows).toHaveCount(4);
    // Let the SSE stream settle before writing the probe so the
    // artifact.indexed frame isn't emitted before the browser subscribes.
    await page.waitForTimeout(1500);

    try {
      writeFileSync(probePath, PROBE_HTML, "utf-8");

      // Section grows to 5 — no page reload, just SSE →
      // setSiblingsTick → fetchDocs → re-render.
      await expect(fileRows).toHaveCount(5, {
        timeout: 20_000,
      });
      await expect(
        list
          .locator(".kb-pinsp__folder-row")
          .filter({ hasText: PROBE_NAME }),
      ).toBeVisible();

      // Removing the file shrinks the list back.
      unlinkSync(probePath);
      await expect(fileRows).toHaveCount(4, {
        timeout: 20_000,
      });
    } finally {
      try {
        unlinkSync(probePath);
      } catch {
        // already gone
      }
    }
  });
});
