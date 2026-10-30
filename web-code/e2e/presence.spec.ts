import { expect, test, type Page } from "@playwright/test";
import { KNOWN_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// OL PRES3 - presence in the tree. The relay is mocked (`GET /api/kb/presence`,
/// kbc-presence/1): the harness starts no kb daemon. The TRIGGER is the SPA's
/// own `kbc:mirror.updated` window event (what the SSE bridge re-dispatches),
/// dispatched here directly; the mocked relay only ATTRIBUTES.

interface Beat {
  repo: string;
  path: string;
  session_id: string;
  harness: string;
  state: string;
  age_secs: number;
  worktree?: string;
}

const beat = (o: Partial<Beat> = {}): Beat => ({
  repo: REPO_NAME,
  path: KNOWN_FILE,
  session_id: "3f2a9c11",
  harness: "claude",
  state: "working",
  age_secs: 1,
  ...o,
});

async function mockRelay(page: Page, files: Beat[], extra: Record<string, unknown> = {}) {
  let hits = 0;
  await page.route("**/api/kb/presence*", (route) => {
    hits += 1;
    return route.fulfill({
      json: { schema: "kbc-presence/1", available: true, now_unix: 1_700_000_000, files, ...extra },
    });
  });
  return () => hits;
}

async function mirrorTouch(page: Page, paths: string[]) {
  await page.evaluate(
    ({ repo, paths }) =>
      window.dispatchEvent(new CustomEvent("kbc:mirror.updated", { detail: { repo, paths } })),
    { repo: REPO_NAME, paths },
  );
}

const row = (page: Page) => page.locator(".kbc-tree__row", { hasText: KNOWN_FILE }).first();

async function open(page: Page) {
  const first = page.waitForResponse("**/api/kb/presence*");
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await first;
  await expect(row(page)).toBeVisible();
}

test.describe("Presence in the file tree", () => {
  test("is on by default, glows nothing until the mirror names a file, then attributes it exactly", async ({
    page,
  }) => {
    await mockRelay(page, [beat()]);
    await open(page);
    await expect(page.locator("[data-kbc-presence-toggle]")).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator("[data-kbc-presence-level]")).toHaveCount(0);

    await mirrorTouch(page, [KNOWN_FILE]);
    await expect(row(page)).toHaveAttribute("data-kbc-presence-trust", "exact");
    await expect(row(page)).toHaveAttribute("data-kbc-presence-level", "3");
    await expect(row(page)).toHaveAttribute("data-kbc-presence-sid", "3f2a9c11");
    await expect(row(page)).toHaveAttribute("aria-label", /^Edited by claude session 3f2a /);
  });

  test("no beat for the path but one working session is a candidate; no session at all is neutral", async ({
    page,
  }) => {
    await mockRelay(page, [beat({ path: "elsewhere.rs" })]);
    await open(page);
    await mirrorTouch(page, [KNOWN_FILE]);
    await expect(row(page)).toHaveAttribute("data-kbc-presence-trust", "candidate");
    await expect(row(page)).toHaveAttribute("aria-label", /^Probably claude session 3f2a/);

    await page.unroute("**/api/kb/presence*");
    await mockRelay(page, []);
    await page.reload();
    await expect(row(page)).toBeVisible();
    await mirrorTouch(page, [KNOWN_FILE]);
    await expect(row(page)).toHaveAttribute("data-kbc-presence-trust", "neutral");
    await expect(row(page)).toHaveAttribute("aria-label", /^Changed /);
    await expect(row(page)).not.toHaveAttribute("data-kbc-presence-sid", /.+/);
  });

  test("opening the file clears the mark; a later touch lights it again", async ({ page }) => {
    await mockRelay(page, [beat()]);
    await open(page);
    await mirrorTouch(page, [KNOWN_FILE]);
    await expect(row(page)).toHaveAttribute("data-kbc-presence-level", "3");
    await row(page).click();
    await expect(row(page)).not.toHaveAttribute("data-kbc-presence-level", /.+/);
    await mirrorTouch(page, [KNOWN_FILE]);
    await expect(row(page)).toHaveAttribute("data-kbc-presence-level", /.+/);
  });

  test("an unopened touch dims to level 1 after ten minutes and leaves with the one-hour ring", async ({
    page,
  }) => {
    await page.clock.install();
    await mockRelay(page, [beat()]);
    await open(page);
    await mirrorTouch(page, [KNOWN_FILE]);
    await expect(row(page)).toHaveAttribute("data-kbc-presence-level", "3");

    // The relay's beat ages with real time: after the jump kb would report it
    // 11 minutes old, so the same absolute beat time keeps the attribution.
    await page.unroute("**/api/kb/presence*");
    await mockRelay(page, [beat({ age_secs: 661 })]);
    await page.clock.fastForward("11:00");
    await expect(row(page)).toHaveAttribute("data-kbc-presence-level", "1");
    await expect(row(page)).toHaveAttribute("aria-label", /not opened since$/);

    await page.clock.fastForward("55:00");
    await expect(row(page)).not.toHaveAttribute("data-kbc-presence-level", /.+/);
  });

  test("an unavailable relay is captioned, not an error toast", async ({ page }) => {
    await mockRelay(page, [], { available: false, caption: "kb unreachable" });
    await open(page);
    await expect(page.locator("[data-kbc-presence-note]")).toContainText("kb unreachable");
    await expect(page.locator(".kbc-toast")).toHaveCount(0);
  });

  test("a worktree touch is a muted count, never a mark on the main checkout's file", async ({ page }) => {
    await mockRelay(page, [beat({ worktree: "fixture-feature" })]);
    await open(page);
    await mirrorTouch(page, [KNOWN_FILE]);
    await expect(page.locator("[data-kbc-presence-worktree='fixture-feature']")).toContainText("1 file");
    await expect(row(page)).toHaveAttribute("data-kbc-presence-trust", "neutral");
  });

  test("with the pref off no presence request is made and nothing glows", async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem("kbc:prefs", JSON.stringify({ presenceOverlay: false }));
    });
    const hits = await mockRelay(page, [beat()]);
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await expect(row(page)).toBeVisible();
    await expect(page.locator("[data-kbc-presence-toggle]")).toHaveAttribute("aria-pressed", "false");
    await mirrorTouch(page, [KNOWN_FILE]);
    await page.waitForTimeout(1500);
    expect(hits()).toBe(0);
    await expect(page.locator("[data-kbc-presence-level]")).toHaveCount(0);
  });

  test.describe("mobile", () => {
    test.use({ viewport: { width: 390, height: 844 } });

    test("the rail shows in the tree drawer at 390px", async ({ page }) => {
      await mockRelay(page, [beat()]);
      await page.goto(`${BASE}/r/${REPO_NAME}`);
      await page.locator("[data-kbc-burger]").click();
      const drawerRow = page.locator("[data-kbc-drawer] .kbc-tree__row", { hasText: KNOWN_FILE });
      await expect(drawerRow).toBeVisible();
      await mirrorTouch(page, [KNOWN_FILE]);
      await expect(drawerRow).toHaveAttribute("data-kbc-presence-level", /.+/);
    });
  });
});
