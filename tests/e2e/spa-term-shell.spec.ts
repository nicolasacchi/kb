import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

// BSHELL — the terminal's own page (/terminals/:id; /term/:id redirects): the Terminals list and the
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

test.describe("BSHELL /terminals/:id", () => {
  test("(a) /sessions row Open lands on /terminals/<id> with the shell and a live viewport", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await page.goto(`${BASE}/sessions`);
    const row = page.locator(`[data-testid="term-row"][data-term="${TERM_ID}"]`);
    await expect(row).toBeVisible({ timeout: 20_000 });
    await row.locator('[data-testid="term-row-open"]').click();
    await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`));
    await expect(page.locator('[data-testid="term-shell"]')).toBeVisible();
    await expect(page.locator('[data-testid="term-shell"]')).toHaveAttribute("data-kb-term-layout", "desktop");
    await expectLive(page);
  });

  test("(b) the legacy /sessions?term=<id> link lands on /terminals/<id>", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await page.goto(`${BASE}/sessions?term=${TERM_ID}`);
    await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`));
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
    await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`));
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
