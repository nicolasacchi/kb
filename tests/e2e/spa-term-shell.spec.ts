import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

// BSHELL — the terminal's own page (/term/:id): the Terminals list and the
// session page both lead to it, the legacy `?term=` link redirects to it, and
// the rail's collapsed flag survives a reload. CHROMIUM desktop only (the
// phone layer is in spa-terminal-phone.spec.ts). Transport is the fake tap.

async function admit(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
  });
}

const VIEW = "[data-kb-term]";

async function expectLive(page: Page): Promise<void> {
  await expect(page.locator(VIEW)).toBeVisible({ timeout: 15_000 });
  await expect(page.locator(VIEW)).toHaveAttribute("data-kb-term-phase", "live", { timeout: 30_000 });
}

test.describe("BSHELL /term/:id", () => {
  test("(a) /sessions row Open lands on /term/<id> with the shell and a live viewport", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await page.goto(`${BASE}/sessions`);
    const row = page.locator(`[data-testid="term-row"][data-term="${TERM_ID}"]`);
    await expect(row).toBeVisible({ timeout: 20_000 });
    await row.locator('[data-testid="term-row-open"]').click();
    await expect(page).toHaveURL(new RegExp(`/term/${TERM_ID}$`));
    await expect(page.locator('[data-testid="term-shell"]')).toBeVisible();
    await expect(page.locator('[data-testid="term-shell"]')).toHaveAttribute("data-kb-term-layout", "desktop");
    await expectLive(page);
  });

  test("(b) the legacy /sessions?term=<id> link lands on /term/<id>", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await page.goto(`${BASE}/sessions?term=${TERM_ID}`);
    await expect(page).toHaveURL(new RegExp(`/term/${TERM_ID}$`));
    await expect(page.locator('[data-testid="term-shell"]')).toBeVisible();
    await expectLive(page);
  });

  test("(c) the session page's Open terminal link navigates to the terminal page and mounts no inline panel", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await page.goto(`${BASE}/sessions/${SID}`);
    await expect(page.locator('[data-testid="kb-terminal-slot"]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(VIEW)).toHaveCount(0);
    await page.locator('[data-testid="kb-term-open"]').click();
    await expect(page).toHaveURL(new RegExp(`/term/${TERM_ID}$`));
    await expectLive(page);
  });

  test("(d) the rail collapses with the toggle and stays collapsed after a reload", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await page.goto(`${BASE}/term/${TERM_ID}`);
    const toggle = page.locator('[data-testid="term-rail-toggle"]');
    await expect(toggle).toHaveAttribute("aria-expanded", "true", { timeout: 15_000 });
    await expect(page.locator('[data-testid="rail-placeholder-conversation"]')).toBeVisible();
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-expanded", "false");
    await expect(page.locator('[data-testid="rail-placeholder-conversation"]')).toHaveCount(0);
    await page.reload();
    await expect(page.locator('[data-testid="term-rail-toggle"]')).toHaveAttribute("aria-expanded", "false", {
      timeout: 15_000,
    });
  });

  test("an unknown terminal id shows the not-found card with a way back", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await page.goto(`${BASE}/term/zzz`);
    await expect(page.locator('[data-testid="term-not-found"]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-testid="term-not-found"] a')).toHaveAttribute("href", "/sessions");
  });
});

test.describe("BHEAD terminal header", () => {
  const STATUS = { model: "opus-synth", context_pct: 42, branch: "feat/synthetic" };

  async function route(page: Page, withStatus: () => boolean): Promise<void> {
    await page.route("**/api/sessions/live-status", (r) =>
      r.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          sessions: [
            {
              session_id: SID,
              harness: "omp",
              holder: "agent",
              state: "working",
              source: "hook",
              confidence: "observed",
              since_unix: 1760000000,
              since_secs: 10,
              resume: "",
              blocked: false,
              term: TERM_ID,
              ...(withStatus() ? { status: STATUS } : {}),
            },
          ],
        }),
      }),
    );
  }

  test("chips appear without changing the header height; verbs stay inside the bar", async ({ page }) => {
    let on = false;
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await route(page, () => on);
    await admit(page);
    await page.goto(`${BASE}/term/${TERM_ID}`);
    await expectLive(page);
    const bar = page.locator('[data-testid="kb-term-bar"]');
    await expect(bar).toBeVisible();
    await expect(page.locator('[data-testid="kb-term-state"]')).toHaveAttribute("data-phase", "live");
    await expect(page.locator('[data-testid="kb-term-status-chips"]')).toHaveCount(0);
    const before = (await bar.boundingBox())!;
    on = true;
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(page.locator('[data-testid="kb-term-status-chips"]')).toContainText("ctx 42%", { timeout: 25_000 });
    await expect(page.locator('[data-testid="kb-term-status-chips"]')).toContainText("opus-synth");
    const after = (await bar.boundingBox())!;
    expect(after.height).toBe(before.height);
    const close = (await page.locator('[data-testid="kb-term-close"]').boundingBox())!;
    expect(close.x).toBeGreaterThanOrEqual(after.x);
    expect(close.x + close.width).toBeLessThanOrEqual(after.x + after.width + 1);
    expect(close.y).toBeGreaterThanOrEqual(after.y - 1);
    expect(close.y + close.height).toBeLessThanOrEqual(after.y + after.height + 1);
  });

  test.describe("phone chrome", () => {
    test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

    test("every header button is at least 40px and the state dot is in the bar", async ({ page }) => {
      await installFakeTerminal(page, { total: 300, seedLines: 100 });
      await route(page, () => true);
      await admit(page);
      await page.goto(`${BASE}/term/${TERM_ID}`);
      const vp = page.locator("[data-kb-term-viewport]");
      await expect(vp).toBeVisible({ timeout: 15_000 });
      await expect(vp).toHaveAttribute("data-kb-term-phase", "live", { timeout: 30_000 });
      const bar = page.locator(".kb-term-phone__bar");
      await expect(bar).toBeVisible({ timeout: 15_000 });
      await expect(bar.locator('[data-testid="kb-term-state"]')).toHaveAttribute("data-phase", "live");
      const buttons = bar.locator("button");
      const n = await buttons.count();
      expect(n).toBeGreaterThan(0);
      for (let i = 0; i < n; i += 1) {
        const box = (await buttons.nth(i).boundingBox())!;
        expect(box.height).toBeGreaterThanOrEqual(40);
        expect(box.width).toBeGreaterThanOrEqual(40);
      }
      await expect(page.locator('[data-testid="kb-term-status-chips"]')).toContainText("opus-synth", { timeout: 25_000 });
    });
  });
});

test.describe("BSTATE terminal state overlays", () => {
  test("reconnecting shows a card over an unchanged pane box, announces politely and clears on recovery", async ({ page }) => {
    const stats = await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await page.goto(`${BASE}/term/${TERM_ID}`);
    await expectLive(page);
    const view = page.locator(VIEW);
    const announce = page.locator('[data-testid="kb-term-announce"]');
    await expect(page.locator('[data-testid="kb-term-overlay"]')).toHaveCount(0);
    await expect(announce).toHaveAttribute("aria-live", "polite");
    const before = (await view.boundingBox())!;
    stats.refuse(503);
    stats.dropNow();
    const card = page.locator('[data-testid="kb-term-overlay"][data-overlay="reconnecting"]');
    await expect(card).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('[data-testid="kb-term-overlay-action"]')).toHaveText("Reconnect now");
    await expect(announce).toHaveText("Reconnecting");
    const during = (await view.boundingBox())!;
    expect(during).toEqual(before);
    // The card sits inside the pane's box, not outside it.
    const box = (await card.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(before.x - 1);
    expect(box.y).toBeGreaterThanOrEqual(before.y - 1);
    expect(box.x + box.width).toBeLessThanOrEqual(before.x + before.width + 1);
    expect(box.y + box.height).toBeLessThanOrEqual(before.y + before.height + 1);
    stats.refuse(null);
    await page.locator('[data-testid="kb-term-overlay-action"]').click();
    await expect(view).toHaveAttribute("data-kb-term-phase", "live", { timeout: 30_000 });
    await expect(page.locator('[data-testid="kb-term-overlay"]')).toHaveCount(0);
    expect(await view.boundingBox()).toEqual(before);
  });

  test("an ended session shows the exit card and Back to terminals leaves the terminal page", async ({ page }) => {
    await installFakeTerminal(page, { total: 200, seedLines: 100, exitCode: 3 });
    await admit(page);
    await page.goto(`${BASE}/term/${TERM_ID}`);
    const card = page.locator('[data-testid="kb-term-overlay"][data-overlay="ended"]');
    await expect(card).toBeVisible({ timeout: 30_000 });
    await expect(card).toContainText("exited (code 3)");
    await page.locator('[data-testid="kb-term-overlay-action"]').click();
    await expect(page).not.toHaveURL(new RegExp(`/term/${TERM_ID}$`));
  });
});
