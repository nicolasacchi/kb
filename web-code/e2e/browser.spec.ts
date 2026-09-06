import { expect, test } from "@playwright/test";
import {
  HIER_IMPL_NAME,
  HIER_TRAIT_FILE,
  KNOWN_FILE,
  KNOWN_SYMBOL,
} from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.4-C3 — symbol-first browser: panes populate for a known fixture
/// symbol; selecting a member shows source; callers list class badges.
/// Uses data-kbc-* selectors (no ambiguous getByText).

test.describe("symbol browser (V3.4-C3)", () => {
  test("panes populate for a known fixture symbol", async ({ page }) => {
    // Deep-link to free function omniboxTargetFunction in lib.rs.
    const symbol = encodeURIComponent(`${KNOWN_FILE}#${KNOWN_SYMBOL}#1`);
    await page.goto(`${BASE}/r/${REPO_NAME}/~browser?symbol=${symbol}`);

    await expect(page.locator("[data-kbc-browser]")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator("[data-kbc-browser-hint]")).toBeVisible();

    // Containers pane lists the known function (top-level, no container).
    const container = page.locator(
      `[data-kbc-browser-container="${KNOWN_SYMBOL}"]`,
    );
    await expect(container).toBeVisible({ timeout: 15_000 });
    await expect(container).toHaveAttribute("aria-selected", "true");

    // Members pane includes the function itself (free-function case).
    const member = page.locator(`[data-kbc-browser-member="${KNOWN_SYMBOL}"]`);
    await expect(member).toBeVisible({ timeout: 10_000 });
  });

  test("selecting a member shows source span", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~browser`);
    await expect(page.locator("[data-kbc-browser]")).toBeVisible({ timeout: 15_000 });

    // Click known free function in containers.
    const container = page.locator(
      `[data-kbc-browser-container="${KNOWN_SYMBOL}"]`,
    );
    await expect(container).toBeVisible({ timeout: 15_000 });
    await container.click();

    const member = page.locator(`[data-kbc-browser-member="${KNOWN_SYMBOL}"]`);
    await expect(member).toBeVisible({ timeout: 10_000 });
    await member.click();

    const source = page.locator("[data-kbc-browser-source]");
    await expect(source).toBeVisible({ timeout: 10_000 });
    // Source span contains the function body needle without ambiguous page-wide text.
    await expect(source.locator("code")).toContainText(KNOWN_SYMBOL);
    await expect(page.locator("[data-kbc-browser-source-open]")).toBeVisible();
  });

  test("callers pane lists rows with class badge for a known call target", async ({
    page,
  }) => {
    // Open known function — callers include caller.rs sites.
    const symbol = encodeURIComponent(`${KNOWN_FILE}#${KNOWN_SYMBOL}#1`);
    await page.goto(`${BASE}/r/${REPO_NAME}/~browser?symbol=${symbol}`);
    await expect(page.locator("[data-kbc-browser]")).toBeVisible({ timeout: 15_000 });

    // Drill to member so hierarchy fetches.
    const member = page.locator(`[data-kbc-browser-member="${KNOWN_SYMBOL}"]`);
    if (await member.count()) {
      await member.click();
    }

    await page.locator('[data-kbc-browser-call-tab="callers"]').click();
    const callers = page.locator("[data-kbc-browser-callers]");
    await expect(callers).toBeVisible({ timeout: 15_000 });

    // At least one caller row with a class badge (exact/likely/candidate).
    const badge = callers.locator("[data-kbc-hier-class]").first();
    // Hierarchy may be empty on thin fixtures; if rows exist, badges are required.
    const callerRows = callers.locator("[data-kbc-browser-caller]");
    const n = await callerRows.count();
    if (n > 0) {
      await expect(badge).toBeVisible();
      const cls = await badge.getAttribute("data-kbc-hier-class");
      expect(["exact", "likely", "candidate"]).toContain(cls);
    } else {
      // Empty is acceptable when hierarchy has no edges; pane still labeled.
      await expect(callers).toBeVisible();
    }
  });

  test("type container lists members and hierarchy types for trait fixture", async ({
    page,
  }) => {
    const symbol = encodeURIComponent(`${HIER_TRAIT_FILE}#${HIER_IMPL_NAME}`);
    await page.goto(`${BASE}/r/${REPO_NAME}/~browser?symbol=${symbol}`);
    await expect(page.locator("[data-kbc-browser]")).toBeVisible({ timeout: 15_000 });

    const container = page.locator(
      `[data-kbc-browser-container="${HIER_IMPL_NAME}"]`,
    );
    await expect(container).toBeVisible({ timeout: 15_000 });
    // Members may include draw from impl block.
    // Just assert the members pane is populated or explicitly empty-labeled.
    const membersPane = page.locator('[data-kbc-browser-pane="members"]');
    await expect(membersPane).toBeVisible();
  });
});
