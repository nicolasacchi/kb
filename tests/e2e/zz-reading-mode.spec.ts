import { test, expect, type Frame, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
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
// Not covered here, on purpose: scroll-position stability. The runtime pins
// the block under (20px, 8px) across the toggle, and on a normal desktop
// viewport the reading column is centred, so that point is margin and there is
// no block to pin - asserting a 2px bound would test the fixture, not the
// feature.

const CORPUS = process.env.KB_E2E_CORPUS;
const DIR = "reading-e2e";
const FIXTURES = resolve(__dirname, "fixtures", "reading");
const FILES = ["ugly.html", "ugly-b.html", "canvas-heavy.html", "opt-out.html", "plain.html", "note.md"];
// ugly-b.html is a second artifact (a second origin) cut from ugly.html.
const SRC = (f: string) => (f === "ugly-b.html" ? "ugly.html" : f);
const KB_BIN = resolve(__dirname, "..", "..", "target", "fast", "kb");

test.beforeAll(async () => {
  expect(CORPUS, "KB_E2E_CORPUS must be exported by global-setup.ts").toBeTruthy();
  const target = join(CORPUS as string, DIR);
  rmSync(target, { recursive: true, force: true });
  const seed = () => {
    mkdirSync(target, { recursive: true });
    for (const f of FILES) cpSync(join(FIXTURES, SRC(f)), join(target, f));
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
  let f: Frame | null = null;
  await expect
    .poll(async () => {
      const h = await page.locator("iframe.detail__frame").first().elementHandle();
      f = h ? await h.contentFrame() : null;
      return f !== null;
    })
    .toBe(true);
  return f as unknown as Frame;
}

const bodyFont = (f: Frame) => f.evaluate(() => getComputedStyle(document.body).fontFamily);
const styleCount = (f: Frame) => f.evaluate(() => document.querySelectorAll("#kb-read-style").length);
// NOT /serif/: the fixtures' own stack ends in "sans-serif".
const SERIF = /Source Serif 4|Charter/;

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

  test("spike (recorded): an artifact's prefers-color-scheme follows the OS, not the SPA theme", async ({ page }) => {
    // The SPA sets `color-scheme` on <html> (ui/src/themes/apply.ts) and the
    // iframe element inherits it. CI result on Chromium: that does NOT reach
    // the frame's media queries - an artifact honouring prefers-color-scheme
    // follows the OS. A `color-scheme` rule on the iframe from data-theme (the
    // brief's fallback) would compute to the very value the element already
    // inherits, and the embedder's `color-scheme` never feeds the frame's
    // media queries, so it is deliberately NOT added. That is why `kb:read` carries the SPA's scheme
    // explicitly, and why no copy may say an artifact "follows the theme"
    // on its own. This pins the observed behaviour so a Chromium change shows.
    await page.addInitScript(() => {
      localStorage.setItem("kb:prefs", JSON.stringify({ theme: "dark" }));
    });
    await page.emulateMedia({ colorScheme: "light" });
    const f = await open(page, "opt-out.html");
    const dark = () => f.evaluate(() => matchMedia("(prefers-color-scheme: dark)").matches);
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    expect(await dark()).toBe(false); // SPA dark, OS light -> the frame sees light
    await page.emulateMedia({ colorScheme: "dark" });
    await expect.poll(dark).toBe(true); // ... and it tracks the OS
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

  test("a page the daemon styled at serve time starts ON: the row is pressed and the first r turns it off", async ({ page }) => {
    const f = await open(page, "plain.html");
    expect(await styleCount(f)).toBe(1); // applied at serve time, no keypress
    await expect(await readingRow(page)).toHaveAttribute("aria-pressed", "true");
    await closeMenu(page);
    await page.keyboard.press("r");
    await expect.poll(() => styleCount(f)).toBe(0);
    await expect(await readingRow(page)).toHaveAttribute("aria-pressed", "false");
    await closeMenu(page);
    await page.keyboard.press("r");
    await expect.poll(() => styleCount(f)).toBe(1);
  });

  test("a Markdown page already has the house voice: r says so and adds no second sheet", async ({ page }) => {
    const f = await open(page, "note.md");
    await page.waitForTimeout(300);
    await page.keyboard.press("r");
    await expect(page.locator(".kb-toast")).toContainText(/already uses kb's reading style/);
    await expect(await readingRow(page)).toBeDisabled();
    expect(await styleCount(f)).toBe(0);
  });

  test("a default set for ANOTHER kb leaves this kb's pages as authored", async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem("kb:prefs", JSON.stringify({ readingDefault: { elsewhere: true } }));
    });
    const f = await open(page, "ugly.html");
    await page.waitForTimeout(400);
    expect(await bodyFont(f)).toMatch(/Verdana/);
    expect(await styleCount(f)).toBe(0);
  });

  test("two panes: r from inside one frame toggles only that pane", async ({ page }) => {
    await page.goto(`${url("ugly.html")}?pane2=${encodeURIComponent(`canon:${DIR}/ugly-b.html`)}`);
    await expect(page.locator("iframe.detail__frame")).toHaveCount(2);
    const frames: Frame[] = [];
    await expect
      .poll(async () => {
        frames.length = 0;
        for (const h of await page.locator("iframe.detail__frame").elementHandles()) {
          const fr = await h.contentFrame();
          if (fr) frames.push(fr);
        }
        return frames.length;
      })
      .toBe(2);
    for (const fr of frames) await expect(fr.locator("h1").first()).toBeVisible();
    await page.waitForTimeout(300);
    await frames[1].locator("h1").click();
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(frames[1])).toMatch(SERIF);
    expect(await bodyFont(frames[0])).toMatch(/Verdana/);
    expect(await styleCount(frames[0])).toBe(0);
    await frames[0].locator("h1").click();
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(frames[0])).toMatch(SERIF);
    expect(await bodyFont(frames[1])).toMatch(SERIF);
  });

  test("annotate: a selection comment keeps its anchor and is not stale across a toggle on and off", async ({ page, request }) => {
    const needle = "Quartz anchor sentence";
    const doc = await (await request.get(`${BASE}/api/kb/canon/docs/by-path/${DIR}/ugly.html`)).json();
    await page.goto(url("ugly.html"));
    const fl = page.frameLocator(".detail__frame");
    await expect.poll(async () => {
      try { return await fl.locator(":root").evaluate(() => !!(window as any).__KB_COMMENTS); } catch { return false; }
    }, { timeout: 15_000 }).toBe(true);
    const panel = page.getByRole("complementary", { name: "comments" });
    const floater = page.locator(".kb-selact");
    const commentBtn = page.locator('[data-kb-act="selection-comment"]');
    await expect(async () => {
      await fl.locator(":root").evaluate((_, n) => {
        const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
        let t: Text | null = null;
        let at = -1;
        for (let x = w.nextNode(); x; x = w.nextNode()) {
          const i = (x as Text).data.indexOf(n);
          if (i >= 0) { t = x as Text; at = i; break; }
        }
        if (!t) throw new Error("needle not found");
        const r = document.createRange();
        r.setStart(t, at);
        r.setEnd(t, at + n.length);
        const s = window.getSelection()!;
        s.removeAllRanges();
        s.addRange(r);
      }, needle);
      await expect(floater).toBeVisible({ timeout: 3_000 });
      await commentBtn.click({ timeout: 3_000 });
      await expect(panel).toBeVisible({ timeout: 3_000 });
    }).toPass({ timeout: 20_000 });
    const body = `reading-anchor-${Date.now()}`;
    const compose = panel.locator(".cp__compose");
    await compose.getByRole("textbox", { name: "new comment body" }).fill(body);
    await compose.getByRole("button", { name: "add comment" }).click();

    const anchorOf = async () => {
      const r = await request.get(`${BASE}/api/kb/canon/review/${doc.id}`);
      const file = (await r.json()) as { comments: { body: string; anchor: unknown }[] };
      return file.comments.find((c) => c.body === body)?.anchor ?? null;
    };
    await expect.poll(anchorOf).not.toBeNull();
    const before = JSON.stringify(await anchorOf());
    const marked = () =>
      fl.locator(":root").evaluate((_, b) => document.querySelectorAll(`[title*="${b}"]`).length, body);
    await expect.poll(marked).toBeGreaterThan(0);

    const f = await frame(page);
    await page.locator("body").click({ position: { x: 2, y: 2 } });
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(SERIF);
    await expect.poll(marked).toBeGreaterThan(0); // repainted over the reflowed text
    await page.keyboard.press("r");
    await expect.poll(() => bodyFont(f)).toMatch(/Verdana/);
    await expect.poll(marked).toBeGreaterThan(0);

    expect(JSON.stringify(await anchorOf())).toBe(before);
    const rows = await (await request.get(`${BASE}/api/kb/canon/reviews?status=all`)).json();
    const list = (Array.isArray(rows) ? rows : rows.items ?? rows.comments ?? []) as { body?: string; excerpt?: string; stale?: boolean }[];
    const row = list.find((c) => (c.body ?? c.excerpt ?? "").includes(body));
    expect(row, "the new comment is listed").toBeTruthy();
    expect(row!.stale ?? false).toBe(false);
  });

  test("kb share --local of the fixture folder carries no reading-mode trace", async () => {
    test.skip(!existsSync(KB_BIN), "fast-profile kb CLI not built");
    const out = mkdtempSync(join(tmpdir(), "kb-read-share-"));
    execFileSync(KB_BIN, ["share", DIR, "--local", out, "--kb", "canon", "--daemon", BASE], { encoding: "utf-8" });
    const walk = (d: string): string[] =>
      readdirSync(d).flatMap((n) => {
        const p = join(d, n);
        return statSync(p).isDirectory() ? walk(p) : [p];
      });
    const files = walk(out).filter((p) => /\.(html|md)$/.test(p));
    expect(files.length).toBeGreaterThan(0);
    for (const p of files) {
      const text = readFileSync(p, "utf-8");
      for (const bad of ["kb-read", "reading.css", "data-kb-read"]) {
        expect(text, `${p} contains ${bad}`).not.toContain(bad);
      }
    }
    const plain = files.find((p) => p.endsWith("plain.html"));
    expect(plain, "plain.html exported").toBeTruthy();
    expect(readFileSync(plain!, "utf-8")).toBe(readFileSync(join(FIXTURES, "plain.html"), "utf-8"));
  });
});
