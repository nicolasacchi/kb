import { test, expect, type Page } from "@playwright/test";
import { PORT } from "./helpers";

// RLs1 — section permalinks. `?sec=<heading-id>` scrolls the artifact
// iframe to the section (with an arrival flash) and BEATS scroll-resume;
// the TocSpy rows offer copy-link. The canon artifacts carry no author
// heading ids, so the runtime's deterministic `kb-h-*` ids are
// discovered from the live DOM first.

const BASE = `http://127.0.0.1:${PORT}`;

async function discoverHeadingIds(page: Page, n: number): Promise<string[]> {
  await page.goto(`${BASE}/a/canon/kitchen-sink.html`);
  const frame = page.frameLocator(".detail__frame");
  await expect(frame.locator("h1").first()).toBeVisible();
  // The runtime assigns ids during buildToc (post-load rAF) — poll.
  await expect
    .poll(() =>
      frame
        .locator("h2[id]")
        .count()
        .catch(() => 0),
    )
    .toBeGreaterThanOrEqual(n);
  const ids: string[] = [];
  for (let i = 0; i < n; i++) {
    const id = await frame.locator("h2[id]").nth(i).getAttribute("id");
    expect(id).toBeTruthy();
    ids.push(id as string);
  }
  return ids;
}

test("?sec= scrolls the section into view and wins over scroll-resume", async ({
  page,
  request,
}) => {
  const ids = await discoverHeadingIds(page, 4);
  const target = ids[ids.length - 1]; // far enough down to require a scroll

  // Seed a near-top resume position for this artifact: without ?sec=
  // a remount would land at y≈1, NOT at the target section.
  const doc = await request.get(
    `${BASE}/api/kb/canon/docs/by-path/kitchen-sink.html`,
  );
  const { id: artifactId } = (await doc.json()) as { id: string };
  const open = await request.post(`${BASE}/api/kb/canon/history/open`, {
    data: { artifact_id: artifactId },
  });
  const { visit_id } = (await open.json()) as { visit_id: number };
  await request.post(`${BASE}/api/kb/canon/history/scroll`, {
    data: { visit_id, scroll_y: 1, scroll_max: 10_000 },
  });

  await page.goto(`${BASE}/a/canon/kitchen-sink.html?sec=${target}`);
  const frame = page.frameLocator(".detail__frame");
  await expect(frame.locator(`#${target}`)).toBeInViewport({
    timeout: 10_000,
  });
  // The frame really scrolled (deep-link intent beat the y=1 resume).
  await expect
    .poll(() =>
      frame.locator("body").evaluate(() => window.scrollY),
    )
    .toBeGreaterThan(50);
});

test("TocSpy row copies a ?sec= permalink", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"], {
    origin: BASE,
  });
  await discoverHeadingIds(page, 2); // ensures the runtime assigned ids

  // TocSpy renders from the runtime's kb:toc message; expand if shrunk.
  const shrunk = page.locator(".kb-toc-spy--shrunk");
  if (await shrunk.isVisible().catch(() => false)) {
    await shrunk.click();
  }
  const row = page.locator(".kb-toc-spy__row").nth(1);
  await expect(row).toBeVisible();
  await row.hover();
  await row.getByRole("button", { name: /copy link to section/ }).click();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toContain("/a/canon/kitchen-sink.html?sec=");
  // TOC rows include the h1, so row N ≠ the Nth h2 — instead assert the
  // copied id addresses a real element in the live document.
  const secId = new URL(copied).searchParams.get("sec") as string;
  expect(secId).toBeTruthy();
  await expect(
    page.frameLocator(".detail__frame").locator(`#${secId}`),
  ).toHaveCount(1);
});

test("arrival flash animates the target section", async ({ page }) => {
  const ids = await discoverHeadingIds(page, 3);
  const target = ids[2];
  await page.goto(`${BASE}/a/canon/kitchen-sink.html?sec=${target}`);
  const frame = page.frameLocator(".detail__frame");
  await expect(frame.locator(`#${target}`)).toBeInViewport({
    timeout: 10_000,
  });
  // The Web-Animations flash runs ~1.8s from arrival — sample inside
  // that window. (Scroll behaviour is the load-bearing assertion above;
  // this pins that `flash: true` actually animates.)
  const animated = await frame
    .locator(`#${target}`)
    .evaluate((el) => el.getAnimations().length > 0);
  expect(animated).toBe(true);
});
