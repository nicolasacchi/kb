import { test, expect, type APIRequestContext, type Page } from "@playwright/test";
import { PORT, artifactUrlRe } from "./helpers";

// G8 — Recent section in Cmdk. Shown only when the search input is
// empty; lists recently-opened artifacts (deduped by artifact id;
// currently-open artifact excluded when invoked from the detail route).
//
// Shared session-history caveat: previous test workers will have left
// open-events in history for many artifacts, so assertions here check
// for the presence/absence of specific rows rather than the absolute
// position of a given artifact in the list.

const BASE = `http://127.0.0.1:${PORT}`;

type Doc = {
  id: string;
  source_relative: string;
  folder: string;
  path: string;
};

async function pmDocs(request: APIRequestContext): Promise<Doc[]> {
  const r = await request.get(`${BASE}/api/kb/canon/docs?limit=50`);
  expect(r.status()).toBe(200);
  const docs = (await r.json()) as Doc[];
  return docs
    .filter((d) => d.folder === "pm")
    .sort((a, b) =>
      (a.path.split("/").pop() ?? "").localeCompare(
        b.path.split("/").pop() ?? "",
      ),
    );
}

async function gotoArtifact(page: Page, sourceRelative: string): Promise<void> {
  await page.goto(`${BASE}/a/canon/${sourceRelative}`);
  await expect(
    page.getByRole("navigation", { name: "artifact context" }),
  ).toBeVisible();
}

async function openCmdk(page: Page): Promise<void> {
  // Click the header button rather than press Ctrl+K — Playwright's
  // synthetic keystroke doesn't reliably reach window-level keydown
  // handlers in headless chromium (mirrors spa-cmdk.spec.ts's openCmdk).
  await page.getByRole("button", { name: /open search/i }).click();
  await expect(page.getByRole("dialog", { name: "search" })).toBeVisible();
}

test.describe("spa cmdk recent section", () => {
  test("Recent section appears when q is empty and lists open artifacts", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    // Navigate through three artifacts so they land in history.
    await gotoArtifact(page, pm[0].source_relative);
    await gotoArtifact(page, pm[1].source_relative);
    await gotoArtifact(page, pm[2].source_relative);

    // Go to gallery — Cmdk on the detail route would exclude pm[2].
    await page.goto(`${BASE}/`);
    await openCmdk(page);

    const recentHeader = page.locator(".cmdk__section--recent");
    await expect(recentHeader).toBeVisible();
    await expect(recentHeader).toHaveText("Recent");
    await expect(page.locator(".cmdk__hit--recent").first()).toBeVisible();

    // The three artifacts we opened are all present in Recent.
    const hrefs = await page
      .locator(".cmdk__hit--recent")
      .evaluateAll((els) =>
        els.map((el) => (el as HTMLAnchorElement).getAttribute("href") ?? ""),
      );
    for (const target of [pm[0], pm[1], pm[2]]) {
      expect(
        hrefs.some((h) => h.includes(target.source_relative)),
        `Recent should include ${target.source_relative}`,
      ).toBe(true);
    }
  });

  test("clicking a Recent row navigates to the artifact and closes the palette", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    await gotoArtifact(page, pm[1].source_relative);

    await page.goto(`${BASE}/`);
    await openCmdk(page);

    const row = page
      .locator(".cmdk__hit--recent")
      .filter({
        has: page.locator("a", { hasText: pm[1].source_relative }),
      })
      .or(
        page
          .locator(".cmdk__hit--recent")
          .filter({ hasText: pm[1].path.split("/").pop()! }),
      )
      .first();
    await expect(row).toBeVisible();
    await row.click();
    await expect(page).toHaveURL(artifactUrlRe("canon", pm[1].source_relative));
    await expect(page.locator(".cmdk")).toHaveCount(0);
  });

  test("Recent hides as soon as the user types in the search box", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    await gotoArtifact(page, pm[0].source_relative);

    await page.goto(`${BASE}/`);
    await openCmdk(page);
    await expect(page.locator(".cmdk__hit--recent").first()).toBeVisible();

    await page.locator(".cmdk__input").fill("x");
    await expect(page.locator(".cmdk__hit--recent")).toHaveCount(0);
    await expect(page.locator(".cmdk__section--recent")).toHaveCount(0);

    // Clearing brings it back.
    await page.locator(".cmdk__input").fill("");
    await expect(page.locator(".cmdk__hit--recent").first()).toBeVisible();
  });

  test("Recent excludes the currently-open artifact on the detail route", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    // Visit pm[1] first so something else lands in history, then open
    // pm[0] and assert pm[0] is filtered out of its own Recent.
    await gotoArtifact(page, pm[1].source_relative);
    await gotoArtifact(page, pm[0].source_relative);
    await openCmdk(page);

    const recentRows = page.locator(".cmdk__hit--recent");
    await expect(recentRows.first()).toBeVisible();
    const hrefs = await recentRows.evaluateAll((els) =>
      els.map((el) => (el as HTMLAnchorElement).getAttribute("href") ?? ""),
    );
    for (const href of hrefs) {
      expect(
        href,
        "current artifact must not appear in Recent",
      ).not.toContain(pm[0].source_relative);
    }
    // Sanity — the OTHER artifact we just opened is still there.
    expect(hrefs.some((h) => h.includes(pm[1].source_relative))).toBe(true);
  });

  test("Recent dedupes by artifact when the same file is opened repeatedly", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    // Hit pm[3] three times then a different artifact, so pm[3] is
    // not the current one when we open Cmdk on the gallery.
    await gotoArtifact(page, pm[3].source_relative);
    await page.reload();
    await page.reload();
    await gotoArtifact(page, pm[0].source_relative);

    await page.goto(`${BASE}/`);
    await openCmdk(page);

    const recentRows = page.locator(".cmdk__hit--recent");
    await expect(recentRows.first()).toBeVisible();
    const hrefs = await recentRows.evaluateAll((els) =>
      els.map((el) => (el as HTMLAnchorElement).getAttribute("href") ?? ""),
    );
    const pm3Hits = hrefs.filter((h) => h.includes(pm[3].source_relative));
    expect(
      pm3Hits.length,
      "deduped Recent should show pm[3] exactly once",
    ).toBe(1);
  });
});
