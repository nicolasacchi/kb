import { test, expect } from "@playwright/test";
import { writeFileSync, unlinkSync, utimesSync } from "node:fs";
import { join } from "node:path";
import { BASE } from "./helpers";

// OL IDEA2 — "Changed since your last visit". Writes a real probe into the
// watched `canon` corpus, opens it (a web visit), rewrites ONLY the Risks
// section with a bumped mtime, reopens, and asserts the banner names Risks
// and not Status; "View changes" opens the versions dock on the new text;
// dismiss survives a reload; and the Library's `read=changed` shelf lists the
// probe but not a never-opened canon card. Named `zz-` so a leaked probe
// can't perturb the other specs' doc counts; cleanup is in `finally`.
test.describe("since your last visit", () => {
  const CORPUS = process.env.KB_E2E_CORPUS;
  const NAME = "since-visit-probe.html";
  const TITLE = "Since Visit Probe";
  const html = (risks: string) => `<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>${TITLE}</title></head>
  <body>
    <h1>${TITLE}</h1>
    <h2>Status</h2><p>Everything is on track.</p>
    <h2>Risks</h2><p>${risks}</p>
    <h2>Appendix</h2><p>Reference material.</p>
  </body>
</html>
`;

  test("banner names the changed section, View changes shows the diff, dismiss sticks, read=changed lists it", async ({
    page,
  }) => {
    expect(CORPUS, "KB_E2E_CORPUS must be exported by global-setup.ts").toBeTruthy();
    const path = join(CORPUS as string, NAME);
    const url = `${BASE}/a/canon/${NAME}`;
    const banner = page.getByTestId("since-visit-banner");

    try {
      // 1. Write the probe and wait until it is indexed (gallery card).
      writeFileSync(path, html("Low risk for now."), "utf-8");
      await page.goto(`${BASE}/?kb=canon`);
      await expect(page.getByRole("link", { name: new RegExp(TITLE) })).toBeVisible({
        timeout: 30_000,
      });

      // 2. Open it: a first visit, no banner.
      const opened = page.waitForResponse(
        (r) => r.url().includes("/history/open") && r.request().method() === "POST",
      );
      await page.goto(url);
      await opened;
      await expect(page.locator("iframe").first()).toBeVisible({ timeout: 20_000 });
      await expect(banner).toHaveCount(0);

      // 3. Away; rewrite only Risks with mtime +5s and wait for the reindex.
      await page.goto(`${BASE}/?kb=canon`);
      writeFileSync(path, html("Severe risk: the migration may slip."), "utf-8");
      const t = new Date(Date.now() + 5_000);
      utimesSync(path, t, t);

      // 4. Reopen: the banner names Risks and not Status.
      await expect(async () => {
        await page.goto(url);
        await expect(banner).toBeVisible({ timeout: 4_000 });
      }).toPass({ timeout: 40_000 });
      await expect(banner).toContainText("Risks");
      await expect(banner).not.toContainText("Status");

      // 5. View changes: the versions dock opens on the new Risks text.
      await banner.getByText(/View changes/).click();
      await expect(page.locator(".versions-panel")).toContainText(
        "Severe risk: the migration may slip.",
        { timeout: 20_000 },
      );

      // 6. Dismiss: hidden now, and still hidden after a reload.
      await banner.getByLabel("dismiss").click();
      await expect(banner).toHaveCount(0);
      await page.reload();
      await expect(page.locator("iframe").first()).toBeVisible({ timeout: 20_000 });
      await expect(banner).toHaveCount(0);

      // 7. The Library shelf: the probe is listed, a never-opened canon card is not.
      await page.goto(`${BASE}/?kb=canon&read=changed`);
      await expect(page.getByRole("link", { name: new RegExp(TITLE) })).toBeVisible({
        timeout: 20_000,
      });
      await expect(
        page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
      ).toHaveCount(0);
    } finally {
      try {
        unlinkSync(path);
      } catch {
        // never written / already gone — best-effort cleanup
      }
    }
  });
});
