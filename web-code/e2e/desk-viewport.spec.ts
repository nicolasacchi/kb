import { expect, test, type Page } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";
import { KNOWN_FILE } from "./fixture-repo";

/// V70-A4 — the VIEWPORT golden.
///
/// docs/research/kb-code-v7-continuum-2026-09.html §P1: "a viewport
/// golden (at 1280×720 with the default desk the code region is ≥80
/// columns × 28 lines at the default font; the drawer opens as an
/// overlay when it would violate that)."
///
/// Why this test exists at all: the failure mode it guards is the one
/// the layout recon measured on the shipped reader — "On a 1280px laptop
/// the reader permanently spends 480px of 1280 on chrome"
/// (docs/research/kb-code-v7-evidence/recon/layout-rails-panels.md §5.4).
/// A shell that can be resized is a shell whose defaults can drift, so
/// the floor is a TEST, not a comment on a constant.
///
/// The measurement is taken from CodeMirror's own metrics — the real
/// character advance of `.cm-content` and the real line height — rather
/// than from an assumed px-per-char. That way a font change, a token
/// change or a zoom change fails here honestly instead of passing
/// against stale arithmetic.

const MIN_COLS = 80;
const MIN_ROWS = 28;

interface CodeMetrics {
  charWidth: number;
  lineHeight: number;
  contentWidth: number;
  contentHeight: number;
  cols: number;
  rows: number;
}

async function measureCode(page: Page): Promise<CodeMetrics> {
  return page.evaluate(() => {
    const content = document.querySelector(".cm-content") as HTMLElement | null;
    const scroller = document.querySelector(".cm-scroller") as HTMLElement | null;
    if (!content || !scroller) throw new Error("no CodeMirror content/scroller on the page");

    // Character advance: measure a real run of monospace glyphs inside
    // the live editor (same font stack, same size, same letter-spacing)
    // rather than trusting a ratio.
    const probe = document.createElement("span");
    probe.textContent = "0".repeat(100);
    probe.style.cssText = "position:absolute;visibility:hidden;white-space:pre;";
    content.appendChild(probe);
    const charWidth = probe.getBoundingClientRect().width / 100;
    probe.remove();

    const line = content.querySelector(".cm-line") as HTMLElement | null;
    const lineHeight = line ? line.getBoundingClientRect().height : parseFloat(getComputedStyle(content).lineHeight);

    const contentRect = content.getBoundingClientRect();
    const scrollerRect = scroller.getBoundingClientRect();
    return {
      charWidth,
      lineHeight,
      contentWidth: contentRect.width,
      contentHeight: scrollerRect.height,
      cols: Math.floor(contentRect.width / charWidth),
      rows: Math.floor(scrollerRect.height / lineHeight),
    };
  });
}

test.describe("Desk viewport floor (V70-A4)", () => {
  test.use({ viewport: { width: 1280, height: 720 } });

  test.beforeEach(() => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
  });

  test("at 1280×720 the default (Read) desk leaves the code region ≥80 cols × 28 lines", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?desk=read`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".cm-content")).toBeVisible();

    const m = await measureCode(page);
    console.log(
      `desk-viewport: ${m.cols} cols × ${m.rows} rows ` +
        `(char ${m.charWidth.toFixed(2)}px, line ${m.lineHeight.toFixed(2)}px, ` +
        `content ${m.contentWidth.toFixed(0)}×${m.contentHeight.toFixed(0)})`,
    );
    expect(m.cols, "code region is narrower than 80 columns at the default font").toBeGreaterThanOrEqual(
      MIN_COLS,
    );
    expect(m.rows, "code region is shorter than 28 lines at the default font").toBeGreaterThanOrEqual(
      MIN_ROWS,
    );
  });

  test("opening the drawer never pushes the code region below the floor — it overlays instead", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?desk=read`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });

    const drawer = page.locator('[data-region="drawer"]');
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "1");

    await page.locator('[data-desk-stripe-btn="drawer"]').click();
    await expect(drawer).toHaveAttribute("data-desk-drawer-collapsed", "0");

    const m = await measureCode(page);
    const overlay = await drawer.getAttribute("data-desk-drawer-overlay");
    console.log(
      `desk-viewport (drawer open, overlay=${overlay}): ${m.cols} cols × ${m.rows} rows`,
    );

    // The contract is on the CODE, not on the mechanism: whichever branch
    // the shell took — docking because there was room, or floating
    // because there wasn't — the floor holds.
    expect(m.cols).toBeGreaterThanOrEqual(MIN_COLS);
    expect(m.rows).toBeGreaterThanOrEqual(MIN_ROWS);
  });

  test("a short viewport forces the overlay branch, and says so", async ({ page }) => {
    // 560px of height cannot hold 28 lines of code AND a docked drawer.
    await page.setViewportSize({ width: 1280, height: 560 });
    await page.goto(`${BASE}/r/${REPO_NAME}/${KNOWN_FILE}?desk=review`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 15_000 });

    const drawer = page.locator('[data-region="drawer"]');
    await expect(drawer).toHaveAttribute("data-desk-drawer-overlay", "1");
    // Honest, not silent: the drawer explains why it is floating.
    await expect(drawer).toContainText(/floating over the code/i);
  });
});
