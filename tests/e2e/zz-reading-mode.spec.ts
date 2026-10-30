import { test, expect, type Frame, type Page } from "@playwright/test";
import { cpSync, mkdirSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { BASE } from "./helpers";

// OL READ2 - reading mode, SPA half, end to end (Chromium).
//
// The runtime half (READ1/READ1b) is a Rust format! string in
// crates/kb-server/src/routes/artifact.rs, so none of this can be faked from
// the SPA side: the page below is served by the real daemon, in a real
// cross-origin iframe, and `r` goes through the real `kb:read` / `kb:read-state`
// / `kb:key` messages.
//
// Named zz-* because it seeds files into the shared canon corpus (it removes
// them afterwards) and must not perturb specs that count that corpus.
//
// Not covered here, on purpose: annotate repaint with `kb comments list` (the
// SPA half is unit-pinned in hooks/useReadingPane.test.tsx against a fake
// bridge), the two-pane `r` (same hook, unit-pinned per frame), the
// `kb share --local` byte-equality (server-side, crates/kb-server tests), and
// scroll-position stability (the runtime pins the block at x=20, which on a
// wide viewport is margin).

const CORPUS = process.env.KB_E2E_CORPUS;
const DIR = "reading-e2e";
const FIXTURES = resolve(__dirname, "fixtures", "reading");
const FILES = ["ugly.html", "canvas-heavy.html", "opt-out.html"];

test.beforeAll(async () => {
  expect(CORPUS, "KB_E2E_CORPUS must be exported by global-setup.ts").toBeTruthy();
  const target = join(CORPUS as string, DIR);
  rmSync(target, { recursive: true, force: true });
  const seed = () => {
    mkdirSync(target, { recursive: true });
    for (const f of FILES) cpSync(join(FIXTURES, f), join(target, f));
  };
  seed();
  const missing = async () => {
    const r = await fetch(`${BASE}/api/kb/canon/docs?limit=500`);
    if (!r.ok) return FILES;
    const docs = (await r.json()) as { source_relative: string }[];
    return FILES.filter((f) => !docs.some((d) => d.source_relative === `${DIR}/${f}`));
  };
  // A file written into a brand-new directory can land in the watcher's
  // dir-create blind window; rewrite it once the watch is established.
  for (let round = 0; round < 3; round++) {
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline) {
      if ((await missing()).length === 0) return;
      await new Promise((r) => setTimeout(r, 500));
    }
    seed();
  }
  expect(await missing(), "fixtures never indexed").toEqual([]);
});

test.afterAll(() => {
  if (CORPUS) rmSync(join(CORPUS, DIR), { recursive: true, force: true });
});

const url = (f: string) => `${BASE}/a/canon/${DIR}/${f}`;

async function open(page: Page, f: string): Promise<Frame> {
  await page.goto(url(f));
  await expect(page.getByRole("navigation", { name: "artifact context" })).toBeVisible();
  await expect(page.frameLocator(".detail__frame").locator("h1").first()).toBeVisible();
  return frame(page);
}

async function frame(page: Page): Promise<Frame> {
  const h = await page.locator("iframe.detail__frame").first().elementHandle();
  const f = await h!.contentFrame();
  expect(f).not.toBeNull();
  return f as Frame;
}

const bodyFont = (f: Frame) => f.evaluate(() => getComputedStyle(document.body).fontFamily);
const styleCount = (f: Frame) => f.evaluate(() => document.querySelectorAll("#kb-read-style").length);
const SERIF = /Source Serif 4|Charter|Georgia|serif/i;

async function readingRow(page: Page) {
  const bar = page.getByRole("navigation", { name: "artifact context" });
  if ((await page.locator('[role="menu"] [data-kb-act="reading"]').count()) === 0) {
    await bar.locator('[data-kb-act="more"]').click();
  }
  return page.locator('[role="menu"] [data-kb-act="reading"]');
}

async function closeMenu(page: Page) {
  await page.keyboard.press("Escape");
  await expect(page.locator('[role="menu"]')).toHaveCount(0);
}

function luminance([r, g, b]: number[]): number {
  const c = [r, g, b].map((v) => {
    const s = v / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
}
const rgb = (s: string) => (s.match(/[\d.]+/g) ?? []).slice(0, 3).map(Number);
async function contrast(f: Frame): Promise<number> {
  const { fg, bg } = await f.evaluate(() => ({
    fg: getComputedStyle(document.body).color,
    bg: getComputedStyle(document.documentElement).backgroundColor,
  }));
  const [a, b] = [luminance(rgb(fg)), luminance(rgb(bg))];
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

test.describe("reading mode", () => {
  test("default: the page keeps its own styling and nothing fetches the reading sheet", async ({ page }) => {
    const sheet: string[] = [];
    page.on("request", (r) => {
      if (r.url().includes("/_kb/reading.css")) sheet.push(r.url());
    });
    const f = await open(page, "ugly.html");
    expect(await bodyFont(f)).toMatch(/Verdana/);
    expect(await styleCount(f)).toBe(0);
    await page.waitForTimeout(500);
    expect(sheet).toEqual([]);
  });

  test("r with SPA focus turns it on and off; the row follows the ack; pre stays monospace", async ({ page }) => {
    const f = await open(page, "ugly.html");
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(SERIF);
    expect(await styleCount(f)).toBe(1);
    // Body is held to at most 68ch.
    const { width, ch } = await f.evaluate(() => {
      const probe = document.createElement("div");
      probe.style.cssText = "width:68ch;position:absolute;visibility:hidden";
      document.body.appendChild(probe);
      const ch = probe.getBoundingClientRect().width;
      probe.remove();
      return { width: document.body.getBoundingClientRect().width, ch };
    });
    expect(width).toBeLessThanOrEqual(ch + 1);
    expect(await f.evaluate(() => getComputedStyle(document.querySelector("pre")!).fontFamily)).toMatch(/mono|Courier/i);
    await expect(await readingRow(page)).toHaveAttribute("aria-pressed", "true");
    await closeMenu(page);

    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(/Verdana/);
    expect(await styleCount(f)).toBe(0);
    await expect(await readingRow(page)).toHaveAttribute("aria-pressed", "false");
  });

  test("r pressed inside the iframe toggles (key-forward lane); typing r in an input does not", async ({ page }) => {
    const f = await open(page, "ugly.html");
    await f.locator("h1").click();
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(SERIF);
    await f.locator("#probe-input").click();
    await page.keyboard.type("rrr");
    await page.waitForTimeout(400);
    expect(await f.locator("#probe-input").inputValue()).toBe("rrr");
    expect(await bodyFont(f)).toMatch(SERIF);
    await f.locator("h1").click();
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(/Verdana/);
  });

  test("toggling off restores the original computed styles; the kb-prompt template is untouched", async ({ page }) => {
    const f = await open(page, "ugly.html");
    const snap = () =>
      f.evaluate(() => {
        const cs = getComputedStyle(document.body);
        return {
          font: cs.fontFamily,
          size: cs.fontSize,
          width: cs.width,
          bg: cs.backgroundColor,
          tpl: document.getElementById("kb-prompt")!.outerHTML,
        };
      });
    const before = await snap();
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(SERIF);
    expect((await snap()).tpl).toBe(before.tpl);
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(/Verdana/);
    expect(await snap()).toEqual(before);
  });

  test("a theme switch re-themes the sheet live: no reload, no history entry, contrast >= 4.5", async ({ page }) => {
    await page.addInitScript(() => {
      if (!localStorage.getItem("kb:prefs")) {
        localStorage.setItem("kb:prefs", JSON.stringify({ theme: "dark" }));
      }
    });
    const f = await open(page, "ugly.html");
    await page.keyboard.press("r");
    await expect(f.locator("html")).toHaveAttribute("data-kb-read", "dark");
    expect(await contrast(f)).toBeGreaterThanOrEqual(4.5);

    let navs = 0;
    page.on("framenavigated", () => void navs++);
    const history = await page.evaluate(() => window.history.length);
    await page.getByRole("button", { name: "cycle theme" }).click(); // dark -> light
    await expect(f.locator("html")).toHaveAttribute("data-kb-read", "light");
    expect(await contrast(f)).toBeGreaterThanOrEqual(4.5);
    expect(navs).toBe(0);
    expect(await page.evaluate(() => window.history.length)).toBe(history);
  });

  test("spike: the SPA's colour-scheme reaches the iframe as prefers-color-scheme", async ({ page }) => {
    // OS says light; the SPA is set to dark. If Chromium propagates the
    // embedding element's used color-scheme into the frame's media queries,
    // every artifact that honours prefers-color-scheme follows the SPA theme
    // with no help from reading mode.
    await page.emulateMedia({ colorScheme: "light" });
    await page.addInitScript(() => {
      localStorage.setItem("kb:prefs", JSON.stringify({ theme: "dark" }));
    });
    const f = await open(page, "opt-out.html");
    const dark = () => f.evaluate(() => matchMedia("(prefers-color-scheme: dark)").matches);
    expect(await dark()).toBe(true);
    await page.getByRole("button", { name: "cycle theme" }).click(); // dark -> light
    await expect.poll(dark).toBe(false);
  });

  test("a canvas-heavy page and an opted-out page each give one toast, a disabled row, and no sheet", async ({ page }) => {
    for (const [file, re] of [
      ["canvas-heavy.html", /images or media/],
      ["opt-out.html", /opts out/],
    ] as const) {
      await page.goto(url(file));
      await expect(page.getByRole("navigation", { name: "artifact context" })).toBeVisible();
      const f = await frame(page);
      await page.waitForTimeout(300);
      await page.keyboard.press("r");
      await expect(page.locator(".kb-toast")).toHaveCount(1);
      await expect(page.locator(".kb-toast")).toContainText(re);
      expect(await styleCount(f)).toBe(0);
      const row = await readingRow(page);
      await expect(row).toBeDisabled();
      await expect(row.locator(".kb-ctxbar__mi-cap")).toContainText(re);
      await closeMenu(page);
    }
  });

  test("reading and folio apply together and folio still toggles on its own", async ({ page }) => {
    const f = await open(page, "ugly.html");
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(SERIF);
    await readingRow(page); // opens the More menu
    await page.locator('[role="menu"] [data-kb-act="folio"]').click();
    await expect(page).toHaveURL(/[?&]view=folio/);
    await expect.poll(() => bodyFont(f)).toMatch(SERIF);
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(/Verdana/);
    await expect(page).toHaveURL(/[?&]view=folio/);
  });

  test("phone viewport: reading mode adds no horizontal overflow", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const f = await open(page, "ugly.html");
    // On a phone the ContextBar's More menu is the entry; the key still works.
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(SERIF);
    const { sw, iw } = await f.evaluate(() => ({
      sw: document.documentElement.scrollWidth,
      iw: window.innerWidth,
    }));
    expect(sw).toBeLessThanOrEqual(iw);
  });

  test("the per-kb default turns reading on at load, with no keypress, for that kb only", async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem("kb:prefs", JSON.stringify({ readingDefault: { canon: true } }));
    });
    const f = await open(page, "ugly.html");
    await expect.poll(() => bodyFont(f)).toMatch(SERIF);
    await expect(await readingRow(page)).toHaveAttribute("aria-pressed", "true");
    await closeMenu(page);
    await page.keyboard.press("r"); // an explicit off beats the default
    await expect.poll(() => bodyFont(f)).toMatch(/Verdana/);
  });

  test("the reading sheet is fetched only once reading mode is used", async ({ page }) => {
    const sheet: string[] = [];
    page.on("request", (r) => {
      if (r.url().includes("/_kb/reading.css")) sheet.push(r.url());
    });
    const f = await open(page, "ugly.html");
    await page.waitForTimeout(400);
    expect(sheet).toEqual([]);
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(SERIF);
    expect(sheet.length).toBeGreaterThan(0);
  });
});
