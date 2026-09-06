import { test, expect, type Page, type FrameLocator } from "@playwright/test";
import { BASE } from "./helpers";

// link-flow — the reader's link PEEK + reading FLOW.
//
// What this file guards, end to end (the runtime relay is a Rust `format!`
// string in `crates/kb-server/src/routes/artifact.rs`, so nothing below can
// be faked from the SPA side):
//
//   * the daemon runtime relays a hover inside the cross-origin artifact
//     iframe (`kb:link-hover`) and the SPA turns it into ONE interactive
//     preview card — the same `.kb-peek` the Alt-hover trigger mounts (#30),
//     showing the TARGET artifact's own metadata (so the resolution
//     `lib/artifactLinks.ts` performs really did land on the right doc).
//   * descending into a link records the artifact you came from, at the
//     scroll offset you left it at, and the ContextBar chip (`flow-back`)
//     + the `u` keybind both bring you back THERE — not to the top.
//   * "Open beside" from the card reuses the existing `?pane2=` split
//     (invariant #30 v0.29 — no new grammar, no new URL params, #35).
//
// Uses the canon `pm/` fixture (four individually-indexed, cross-linked
// pages) — the same harness `spa-backbutton.spec.ts` / `spa-multipage.spec.ts`
// use, whose relative `next →` links are exactly the in-artifact
// cross-artifact links this feature is about.

const SUMMARY = "/a/canon/pm/00-summary.html";
const TIMELINE_RE = /\/a\/canon\/pm\/01-timeline\.html(\?|$)/;
const SUMMARY_RE = /\/a\/canon\/pm\/00-summary\.html(\?|$)/;

const peek = (page: Page) => page.locator(".kb-peek");
const chip = (page: Page) => page.locator('[data-kb-act="flow-back"]');
const frames = (page: Page) => page.locator("iframe.detail__frame");

async function openSummary(page: Page): Promise<FrameLocator> {
  await page.goto(`${BASE}${SUMMARY}`);
  await expect(
    page.getByRole("navigation", { name: "artifact context" }),
  ).toBeVisible();
  const frame = page.frameLocator(".detail__frame");
  await expect(frame.locator("h1").first()).toBeVisible();
  return frame;
}

/// Scroll the artifact iframe and wait past the runtime's 500ms scroll
/// debounce, so the pane has published a non-zero live offset for the flow
/// capture to read when we navigate away.
async function scrollFrame(frame: FrameLocator, y: number) {
  await frame.locator("body").evaluate((_el, top) => {
    window.scrollTo(0, top as number);
  }, y);
  await expect
    .poll(() => frame.locator("body").evaluate(() => window.scrollY))
    .toBeGreaterThan(0);
  // The runtime debounces `kb:scroll` by 500ms; there is no observable
  // parent-side signal for "the beacon landed", so this one wait is real.
  await new Promise((r) => setTimeout(r, 900));
}

/// Hover the fixture's `next →` link and wait for the dwell-delayed card.
async function hoverNextLink(page: Page, frame: FrameLocator) {
  await frame.getByRole("link", { name: /next/i }).hover();
  await expect(peek(page)).toBeVisible({ timeout: 10_000 });
}

test.describe("link peek (in-artifact hover)", () => {
  test("hovering a cross-artifact link previews the TARGET, with actions", async ({
    page,
  }) => {
    const frame = await openSummary(page);
    await hoverNextLink(page, frame);

    // The card describes the artifact the link points AT (01-timeline), not
    // the one being read — i.e. the relative href really resolved through
    // the pane's own source directory.
    await expect(peek(page)).toContainText("Timeline");
    await expect(peek(page).locator('[data-kb-act="peek-open"]')).toBeVisible();
  });

  test("Escape dismisses the card", async ({ page }) => {
    const frame = await openSummary(page);
    await hoverNextLink(page, frame);
    await page.keyboard.press("Escape");
    await expect(peek(page)).toHaveCount(0);
  });
});

test.describe("reading flow", () => {
  test("peek → Open descends, the chip appears, and clicking it returns to the offset you left", async ({
    page,
  }) => {
    const frame = await openSummary(page);
    // No descent yet ⇒ no chip (the affordance only exists when there IS
    // somewhere to go back to).
    await expect(chip(page)).toHaveCount(0);

    await scrollFrame(frame, 500);
    const leftAt = await frame
      .locator("body")
      .evaluate(() => window.scrollY);
    expect(leftAt).toBeGreaterThan(0);

    await hoverNextLink(page, frame);
    await peek(page).locator('[data-kb-act="peek-open"]').click();

    await expect(page).toHaveURL(TIMELINE_RE);
    await expect(
      page.frameLocator(".detail__frame").getByRole("heading", {
        name: /Minute-by-minute/i,
      }),
    ).toBeVisible();
    // The descent is recorded, and the chip names where it came from.
    await expect(chip(page)).toBeVisible();
    await expect(chip(page)).toContainText(/Summary/i);

    await chip(page).click();
    await expect(page).toHaveURL(SUMMARY_RE);
    const back = page.frameLocator(".detail__frame");
    await expect(back.locator("h1").first()).toBeVisible();
    // THE POINT: we came back to where we were reading, not to the top.
    await expect
      .poll(() => back.locator("body").evaluate(() => window.scrollY), {
        timeout: 10_000,
      })
      .toBeGreaterThan(100);
    // The stack is spent — one descent, one return.
    await expect(chip(page)).toHaveCount(0);
  });

  test("`u` is the chip's keybind — same return, same restored offset", async ({
    page,
  }) => {
    const frame = await openSummary(page);
    await scrollFrame(frame, 500);

    // Descend the ordinary way (a plain in-artifact link click, which the
    // runtime routes through the trampoline — invariant #20's one-history-
    // entry path). The flow records it the same as the peek's Open does.
    await frame.getByRole("link", { name: /next/i }).click();
    await expect(page).toHaveURL(TIMELINE_RE);
    await expect(chip(page)).toBeVisible();

    // The descent was a click INSIDE the cross-origin iframe, so the frame
    // holds focus and keystrokes would never reach the parent's handler.
    // Hand focus back to the parent document programmatically — clicking a
    // bit of chrome is layout-dependent (the crumb's inner <b> is
    // ellipsis-collapsed to zero width at 1280px and Playwright refuses
    // invisible click targets).
    await page.evaluate(() => {
      const el = document.activeElement;
      if (el instanceof HTMLElement) el.blur();
      window.focus();
    });
    await page.keyboard.press("u");
    await expect(page).toHaveURL(SUMMARY_RE);
    await expect
      .poll(
        () =>
          page
            .frameLocator(".detail__frame")
            .locator("body")
            .evaluate(() => window.scrollY),
        { timeout: 10_000 },
      )
      .toBeGreaterThan(100);
    await expect(chip(page)).toHaveCount(0);
  });

  test("browser Back is recognised as the same return (and stays one entry per artifact)", async ({
    page,
  }) => {
    const frame = await openSummary(page);
    await frame.getByRole("link", { name: /next/i }).click();
    await expect(page).toHaveURL(TIMELINE_RE);
    await expect(chip(page)).toBeVisible();

    // invariant:20 — one Back press returns to the entry artifact, and the
    // flow stack pops with it rather than stranding a stale chip.
    await page.goBack();
    await expect(page).toHaveURL(SUMMARY_RE);
    await expect(chip(page)).toHaveCount(0);
  });
});

test.describe("peek → open beside", () => {
  test("the card's split action opens the target in pane 2", async ({ page }) => {
    const frame = await openSummary(page);
    await expect(frames(page)).toHaveCount(1);
    await hoverNextLink(page, frame);

    const split = peek(page).locator('[data-kb-act="peek-split"]');
    await expect(split).toBeVisible();
    await split.click();

    await expect(page).toHaveURL(/[?&]pane2=/);
    await expect(frames(page)).toHaveCount(2);
    // Still exactly ONE inspector rail for the whole reader (#30).
    await expect(page.locator(".kb-pinsp")).toHaveCount(1);
    // Two DIFFERENT artifact origins — the attribution invariant (#8/#19).
    const origins = await frames(page).evaluateAll((els) =>
      els.map((e) => new URL((e as HTMLIFrameElement).src).origin),
    );
    expect(origins[0]).not.toBe(origins[1]);
  });
});
