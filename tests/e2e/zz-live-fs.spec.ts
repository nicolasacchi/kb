import { test, expect } from "@playwright/test";
import { writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { PORT } from "./helpers";

// Live filesystem updates — the headline v0.7 SPA feature. The daemon
// watches each kb's source dir; a new or removed artifact fires an SSE
// `artifact.indexed` / `artifact.removed` frame, and gallery.tsx
// refetches the docs list off it (leading-edge + cooldown). This spec
// writes a real artifact into the watched corpus and asserts the card
// appears with no page reload, then removes it and asserts it leaves —
// which also restores the corpus to baseline for any later spec.
//
// Named `zz-` so it runs last: a mid-test failure that leaks the probe
// file then can't perturb the doc-count assertions in the other specs.
test.describe("live filesystem updates", () => {
  const CORPUS = process.env.KB_E2E_CORPUS;
  const PROBE_NAME = "live-fs-probe.html";
  const PROBE_TITLE = "Live FS Probe Artifact";
  const PROBE_HTML = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>${PROBE_TITLE}</title>
    <meta name="kb-tags" content="e2e, live-fs" />
    <meta name="kb-category" content="notes" />
  </head>
  <body>
    <h1>${PROBE_TITLE}</h1>
    <p>Written by zz-live-fs.spec.ts to exercise watcher → SSE → gallery.</p>
  </body>
</html>
`;

  test("a newly-written artifact appears in the gallery; removal clears it", async ({
    page,
  }) => {
    expect(
      CORPUS,
      "KB_E2E_CORPUS must be exported by global-setup.ts",
    ).toBeTruthy();
    const probePath = join(CORPUS as string, PROBE_NAME);
    const card = page.getByRole("link", { name: new RegExp(PROBE_TITLE) });

    await page.goto(`http://127.0.0.1:${PORT}/`);
    // Gallery is up (a known canon card is visible) and the probe
    // artifact is not present yet.
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();
    await expect(card).toHaveCount(0);
    // Let the SSE stream settle before mutating the corpus so the
    // artifact.indexed frame isn't emitted before the browser subscribes.
    await page.waitForTimeout(1500);

    try {
      writeFileSync(probePath, PROBE_HTML, "utf-8");
      // No page.reload() — gallery.tsx must refetch off the SSE frame.
      await expect(card).toBeVisible({ timeout: 20_000 });
    } finally {
      try {
        unlinkSync(probePath);
      } catch {
        // already gone / never written — best-effort cleanup
      }
    }
    // Removal fires artifact.removed → the card leaves, and the corpus
    // is back to its baseline for any later spec.
    await expect(card).toHaveCount(0, { timeout: 20_000 });
  });

  // v0.12 redesign retired the IndexHero block from the gallery (the
  // `.gallery-hero` CSS still ships but nothing renders it). When/if
  // a landing-artifact surface returns, retarget the assertions to its
  // new selector and un-skip.
  test.skip("an index.html landing artifact appears as the gallery hero, and live-updates on edit + delete", async ({
    page,
  }) => {
    // v0.7.x — `IndexHero` renders the kb's hand-authored landing
    // artifact above the card grid. Drop a fresh `index.html` into the
    // corpus and assert the hero block appears with the right title;
    // edit the title and assert it updates without a page reload; then
    // delete the file and assert the hero disappears (restoring the
    // corpus baseline so later specs see no leftover state).
    expect(CORPUS).toBeTruthy();
    const indexPath = join(CORPUS as string, "index.html");
    const initialTitle = "kb e2e hero — initial";
    const editedTitle = "kb e2e hero — edited";
    const buildHtml = (title: string) => `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>${title}</title>
    <meta name="kb-tags" content="e2e, landing" />
  </head>
  <body>
    <h1>${title}</h1>
    <p>Written by zz-live-fs.spec.ts to exercise the IndexHero block.</p>
  </body>
</html>
`;

    await page.goto(`http://127.0.0.1:${PORT}/`);
    // Wait for the gallery to be live (canon card visible) and for SSE
    // to be settled before mutating the corpus.
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();
    await page.waitForTimeout(1500);

    const hero = page.locator(".gallery-hero");
    // Hero is absent when no index.html exists.
    await expect(hero).toHaveCount(0);

    try {
      writeFileSync(indexPath, buildHtml(initialTitle), "utf-8");
      await expect(hero).toBeVisible({ timeout: 20_000 });
      await expect(hero).toContainText(initialTitle);

      // Edit the file in place — the watcher fires watch.modify, the
      // indexer re-indexes, SSE emits artifact.indexed, and the hero's
      // text should update without a navigation.
      writeFileSync(indexPath, buildHtml(editedTitle), "utf-8");
      await expect(hero).toContainText(editedTitle, { timeout: 20_000 });
    } finally {
      try {
        unlinkSync(indexPath);
      } catch {
        // already gone / never written — best-effort cleanup
      }
    }
    await expect(hero).toHaveCount(0, { timeout: 20_000 });
  });
});
