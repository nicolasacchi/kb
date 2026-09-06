import { expect, test } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";

/// F1/F3 — the repo pill (replacing the old `RepoPicker` box) + the route
/// ErrorBoundary/API-error path, smoke-tested end to end. Deterministic: no
/// reliance on timing beyond the existing daemon-response waits every other
/// spec in this suite already uses.
test.describe("chrome — repo pill + error resilience", () => {
  test("the pill shows the active repo on the reader route", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    const pill = page.locator("[data-kbc-repopill]");
    await expect(pill).toBeVisible();
    await expect(pill).toHaveAttribute("data-scope", "one");
    await expect(pill).toContainText(REPO_NAME);
  });

  test("the pill dims (unscoped) on the bare Home route", async ({ page }) => {
    await page.goto(`${BASE}/`);
    const pill = page.locator("[data-kbc-repopill]");
    await expect(pill).toBeVisible();
    await expect(pill).toHaveAttribute("data-scope", "unscoped");
  });

  test("the pill's dropdown lists the configured repo and navigates into it", async ({ page }) => {
    await page.goto(`${BASE}/`);
    const pill = page.locator("[data-kbc-repopill]");
    await pill.click();

    const popover = page.locator("[data-kbc-repopopover]");
    await expect(popover).toBeVisible();
    const item = popover.locator(`[data-kbc-repopill-item="${REPO_NAME}"]`);
    await expect(item).toBeVisible();
    await item.click();

    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}$`));
    await expect(popover).toBeHidden();
    const pillAfter = page.locator("[data-kbc-repopill]");
    await expect(pillAfter).toHaveAttribute("data-scope", "one");
  });

  test("a nonexistent repo/file renders the honest API-error UI, not a white screen", async ({ page }) => {
    await page.goto(`${BASE}/r/does-not-exist/nope.rs`);
    const errorHint = page.locator(".kbc-reader__hint--error");
    await expect(errorHint).toBeVisible({ timeout: 10_000 });
    // The chrome (TopBar) survives around the error — no white screen, the
    // app shell stayed mounted.
    await expect(page.locator("[data-kbc-repopill]")).toBeVisible();
  });
});

/// V4.U2 / V70-A7 — the theme control. It no longer CYCLES: a catalogue of
/// families is not a three-item cycle, and reaching `light` from `system`
/// used to cost two blind clicks. The button now opens the ThemePicker, and
/// the appearance segmented control inside it is the direct pick. Both axes
/// persist in the same `kbc:prefs` blob, and picking a family must actually
/// flip the painted tokens, not merely the attribute.
test.describe("chrome — theme picker", () => {
  const openPicker = async (page: import("@playwright/test").Page) => {
    await page.locator("[data-kbc-theme-toggle]").click();
    await expect(page.locator("[data-kbc-themepicker]")).toBeVisible();
  };

  test("the appearance control picks light / dark / system directly", async ({ page }) => {
    await page.goto(`${BASE}/`);
    const toggle = page.locator("[data-kbc-theme-toggle]");
    await expect(toggle).toBeVisible();
    await expect(toggle).toHaveAttribute("aria-expanded", "false");
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

    await openPicker(page);
    await expect(toggle).toHaveAttribute("aria-expanded", "true");

    await page.locator('[data-kbc-theme-appearance="light"]').click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
    await expect(page.locator('[data-kbc-theme-appearance="light"]')).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    await page.locator('[data-kbc-theme-appearance="system"]').click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "system");

    await page.locator('[data-kbc-theme-appearance="dark"]').click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  });

  test("the chosen appearance persists across reload (localStorage)", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await openPicker(page);
    await page.locator('[data-kbc-theme-appearance="light"]').click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");

    await page.reload();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  });

  test("light theme paints body with the light --bg token", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await openPicker(page);
    await page.locator('[data-kbc-theme-appearance="light"]').click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
    // tokens.css :root[data-theme="light"] --bg: #f3f1ec
    await expect(page.locator("body")).toHaveCSS(
      "background-color",
      "rgb(243, 241, 236)",
    );
  });

  /// The zero-diff guarantee: with no theme chosen there is NO
  /// `data-kbc-theme` attribute at all, so tokens.css alone paints and the
  /// DOM is byte-identical to pre-V70-A7. Choosing a family adds it; choosing
  /// the built-in back removes it again.
  test("no family chosen ⇒ no data-kbc-theme attribute; choosing one adds it", async ({
    page,
  }) => {
    await page.goto(`${BASE}/`);
    await expect(page.locator("html")).not.toHaveAttribute("data-kbc-theme", /.*/);

    await openPicker(page);
    await page.locator('[data-kbc-theme-family="catppuccin"]').click();
    await expect(page.locator("html")).toHaveAttribute("data-kbc-theme", "catppuccin-mocha");
    // The registry's own `base` anchor for Catppuccin Mocha — the attribute
    // flip really does repaint, it is not just a marker.
    await expect(page.locator("body")).toHaveCSS("background-color", "rgb(30, 30, 46)");

    await page.reload();
    await expect(page.locator("html")).toHaveAttribute("data-kbc-theme", "catppuccin-mocha");

    await openPicker(page);
    await page.locator('[data-kbc-theme-family="kbc"]').click();
    await expect(page.locator("html")).not.toHaveAttribute("data-kbc-theme", /.*/);
    await expect(page.locator("body")).toHaveCSS("background-color", "rgb(14, 14, 16)");
  });

  /// Escape reverts a STAGED preview — arrowing through the catalogue must
  /// not commit anything until Enter or a click says so.
  test("Escape reverts a previewed family", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await openPicker(page);
    await page.locator('[data-kbc-theme-family="gruvbox"]').hover();
    await expect(page.locator("html")).toHaveAttribute("data-kbc-theme", "gruvbox-dark");
    await page.keyboard.press("Escape");
    await expect(page.locator("[data-kbc-themepicker]")).toHaveCount(0);
    await expect(page.locator("html")).not.toHaveAttribute("data-kbc-theme", /.*/);
  });

  /// R11 — the theme-color meta follows the CHOSEN theme, not the OS.
  test("the theme-color meta follows the chosen theme", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await expect(page.locator('meta[name="theme-color"]')).toHaveCount(1);
    await openPicker(page);
    await page.locator('[data-kbc-theme-family="catppuccin"]').click();
    await expect(page.locator('meta[name="theme-color"]')).toHaveAttribute(
      "content",
      "#1e1e2e",
    );
  });
});

const NAV_KEYS = [
  "branches",
  "prs",
  "reviews",
  "browser",
  "sets",
  "hotspots",
  "todos",
  "recipes",
  "stacks",
  "canvas",
] as const;

/// V4.U5 — mobile nav sheet (390×844) + desktop review chips / Explore.
test.describe("chrome — mobile nav sheet (390×844)", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("nav toggle opens the sheet; every destination is a row; tap navigates and closes", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    const toggle = page.locator("[data-kbc-nav-toggle]");
    await expect(toggle).toBeVisible();
    await expect(page.locator("[data-kbc-navsheet]")).toHaveCount(0);

    await toggle.click();
    const sheet = page.locator("[data-kbc-navsheet]");
    await expect(sheet).toBeVisible();
    for (const key of NAV_KEYS) {
      await expect(sheet.locator(`[data-kbc-topbar-${key}]`)).toBeVisible();
    }

    await sheet.locator("[data-kbc-topbar-todos]").click();
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/~todos`));
    await expect(page.locator("[data-kbc-navsheet]")).toHaveCount(0);
  });

  test("scrim tap and Escape both dismiss the nav sheet", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator("[data-kbc-nav-toggle]").click();
    await expect(page.locator("[data-kbc-navsheet]")).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(page.locator("[data-kbc-navsheet]")).toHaveCount(0);

    await page.locator("[data-kbc-nav-toggle]").click();
    await expect(page.locator("[data-kbc-navsheet]")).toBeVisible();
    // Sheet sits at the bottom; tap the scrim near the top, clear of the
    // panel (same trick as mobile.spec.ts's drawer-scrim click).
    await page.locator("[data-kbc-navsheet-scrim]").click({ position: { x: 200, y: 40 } });
    await expect(page.locator("[data-kbc-navsheet]")).toHaveCount(0);
  });

  test("theme row cycles data-theme", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await page.locator("[data-kbc-nav-toggle]").click();
    await expect(page.locator("[data-kbc-navsheet]")).toBeVisible();
    await page.locator("[data-kbc-nav-theme]").click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  });

  /// V70-A7 — the sheet keeps the appearance-cycling row (one thumb, no
  /// popover) AND gains a row that opens the same picker as a bottom sheet.
  test("colour-scheme row opens the picker as a sheet", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator("[data-kbc-nav-toggle]").click();
    await expect(page.locator("[data-kbc-navsheet]")).toBeVisible();
    await page.locator("[data-kbc-nav-theme-picker]").click();
    const picker = page.locator("[data-kbc-themepicker]");
    await expect(picker).toBeVisible();
    await expect(picker).toHaveClass(/kbc-themepicker--sheet/);
    await page.locator('[data-kbc-theme-family="nord"]').click();
    await expect(page.locator("html")).toHaveAttribute("data-kbc-theme", "nord");
  });
});

test.describe("chrome — desktop TopBar (1280×720)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });

  test("review chips visible; Explore opens/closes; no sheet in DOM", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await expect(page.locator("[data-kbc-topbar-branches]")).toBeVisible();
    await expect(page.locator("[data-kbc-topbar-prs]")).toBeVisible();
    await expect(page.locator("[data-kbc-topbar-reviews]")).toBeVisible();
    await expect(page.locator("[data-kbc-nav-explore]")).toBeVisible();
    await expect(page.locator("[data-kbc-navsheet]")).toHaveCount(0);
    await expect(page.locator("[data-kbc-nav-toggle]")).toHaveCount(0);

    await page.locator("[data-kbc-nav-explore]").click();
    await expect(page.locator("[data-kbc-navmenu]")).toBeVisible();
    await expect(page.locator("[data-kbc-topbar-todos]")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.locator("[data-kbc-navmenu]")).toHaveCount(0);
  });

  test("active route marks the current chip with aria-current", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~branches`);
    const branches = page.locator("[data-kbc-topbar-branches]");
    await expect(branches).toBeVisible();
    await expect(branches).toHaveAttribute("aria-current", "page");

    await page.goto(`${BASE}/r/${REPO_NAME}/~todos`);
    const explore = page.locator("[data-kbc-nav-explore]");
    await expect(explore).toHaveAttribute("aria-current", "page");
    await expect(explore).toContainText("Explore: TODOs");
  });
});
