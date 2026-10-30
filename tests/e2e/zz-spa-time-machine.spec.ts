import { test, expect, type APIRequestContext } from "@playwright/test";
import { writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { PORT } from "./helpers";

// OL TIME2 — the reader's time machine. `?at=<unix>` on the reader serves the
// artifact BODY as it stood (nearest-prior version, through today's pipeline),
// captions it honestly, and `then | now` puts the same artifact at its
// working version beside it. Needs an artifact with two index snapshots, so it
// writes a probe into the watched corpus, rewrites it a few seconds later and
// reads the snapshot timestamps back from the versions API (`zz-` so a leaked
// probe cannot perturb other specs' doc counts).
const lookupId = async (
  request: APIRequestContext,
  name: string,
): Promise<string | null> => {
  const r = await request.get(
    `http://127.0.0.1:${PORT}/api/kb/canon/lookup?q=${encodeURIComponent(name)}`,
  );
  if (!r.ok()) return null;
  const j = await r.json();
  return typeof j.id === "string" ? j.id : null;
};

test.describe("the reader's time machine (?at=)", () => {
  const CORPUS = process.env.KB_E2E_CORPUS;
  const NAME = "time-machine-probe.html";
  const BASE = `http://127.0.0.1:${PORT}`;
  const doc = (marker: string) => `<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Time Machine Probe</title></head>
  <body><h1>Time Machine Probe</h1><p id="marker">${marker}</p></body>
</html>
`;

  test("an instant between two snapshots shows the older body with a caption; then|now splits", async ({
    page,
    request,
  }) => {
    expect(CORPUS, "KB_E2E_CORPUS must be exported by global-setup.ts").toBeTruthy();
    const path = join(CORPUS as string, NAME);
    test.setTimeout(120_000);
    try {
      writeFileSync(path, doc("body-from-the-past"), "utf-8");

      const idOf = () => lookupId(request, NAME);
      const indexTs = async (id: string): Promise<number[]> => {
        const r = await request.get(`${BASE}/api/kb/canon/artifacts/${id}/versions`);
        if (!r.ok()) return [];
        const j = await r.json();
        return (j.versions as { source: string; ts_unix: number }[])
          .filter((v) => v.source === "index")
          .map((v) => v.ts_unix);
      };

      let id = "";
      await expect
        .poll(async () => (id = (await idOf()) ?? ""), { timeout: 30_000 })
        .not.toBe("");
      await expect.poll(async () => (await indexTs(id)).length, { timeout: 30_000 }).toBe(1);
      const tsOld = (await indexTs(id))[0];

      // Snapshot timestamps are whole seconds: leave a clear gap, then rewrite.
      await page.waitForTimeout(3_000);
      writeFileSync(path, doc("body-from-today"), "utf-8");
      await expect.poll(async () => (await indexTs(id)).length, { timeout: 30_000 }).toBe(2);

      const frame = () => page.frameLocator("iframe.detail__frame").first();

      // Today: the newer body, and no caption strip.
      await page.goto(`${BASE}/a/canon/${NAME}`);
      await expect(frame().locator("#marker")).toHaveText("body-from-today");
      await expect(page.getByTestId("asof-bar")).toHaveCount(0);

      // Between the snapshots: the OLDER body, captioned nearest-earlier, with
      // the sibling caveat.
      await page.goto(`${BASE}/a/canon/${NAME}?at=${tsOld + 1}`);
      await expect(frame().locator("#marker")).toHaveText("body-from-the-past");
      const caption = page.getByTestId("asof-caption");
      await expect(caption).toContainText("nearest earlier version");
      await expect(caption).toContainText("relative links resolve to today");
      // The historical frame never asks for the annotator.
      const src = await page.locator("iframe.detail__frame").first().getAttribute("src");
      expect(src).toContain(`at=${tsOld + 1}`);
      expect(src).not.toContain("cm=on");

      // then | now: the same artifact at its working version beside it.
      await page.getByTestId("asof-split").click();
      await expect(page.locator("iframe.detail__frame")).toHaveCount(2);
      await expect(page).toHaveURL(/[?&]pane2=/);
      await expect(
        page.frameLocator("iframe.detail__frame").nth(0).locator("#marker"),
      ).toHaveText("body-from-the-past");
      await expect(
        page.frameLocator("iframe.detail__frame").nth(1).locator("#marker"),
      ).toHaveText("body-from-today");

      // back to today clears ?at= AND the self-split.
      await page.getByTestId("asof-today").click();
      await expect(page).not.toHaveURL(/[?&]at=/);
      await expect(page).not.toHaveURL(/[?&]pane2=/);
      await expect(page.locator("iframe.detail__frame")).toHaveCount(1);
      await expect(frame().locator("#marker")).toHaveText("body-from-today");
    } finally {
      try {
        unlinkSync(path);
      } catch {
        // best-effort cleanup
      }
    }
  });

  test("the versions panel's Open as of… writes ?at=", async ({ page, request }) => {
    expect(CORPUS).toBeTruthy();
    const path = join(CORPUS as string, "time-machine-panel.html");
    try {
      writeFileSync(path, doc("panel-body"), "utf-8");
      await expect
        .poll(async () => (await lookupId(request, "time-machine-panel.html")) ?? "", {
          timeout: 30_000,
        })
        .not.toBe("");
      await page.goto(`${BASE}/a/canon/time-machine-panel.html`);
      await page.locator('[data-kb-act="dock-versions"]').click();
      const input = page.getByTestId("versions-open-as-of-input");
      await expect(input).toBeVisible();
      await input.fill("2020-01-02T03:04");
      await page.getByTestId("versions-open-as-of-go").click();
      await expect(page).toHaveURL(/[?&]at=\d+/);
      // 2020 predates the file: an honest miss caption, not a faked body.
      await expect(page.getByTestId("asof-caption")).toContainText("nothing is that old");
    } finally {
      try {
        unlinkSync(path);
      } catch {
        // best-effort cleanup
      }
    }
  });
});
