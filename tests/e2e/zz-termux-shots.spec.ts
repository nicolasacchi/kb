// TEMPORARY (TERMUX analysis, CI only, never merge): screenshots of the web
// terminal panel over the real stack. Asserts only that the panel mounted.
// Shots land in tests/e2e/test-results/termux-shots/ (uploaded by the term-e2e job).
import { test, expect, devices, type Page, type BrowserContext } from "@playwright/test";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { Api, MAIN, bindSession, killLaunched, launch, newSid, repoRoot, scratch, waitFile, type Harness } from "./terminal-stack/stack";

const OUT = join(repoRoot(), "tests", "e2e", "test-results", "termux-shots");

const GEN = `
import os, sys, time
cols, rows = os.get_terminal_size(1)
out = sys.argv[1]
E = "\\x1b"
w = sys.stdout.write
# scrollback: a few hundred lines, some longer than the grid
for i in range(1, 321):
    pad = "x" * ((i * 7) % (cols + 60))
    w(f"{E}[3{1 + i % 6}m[{i:04d}]{E}[0m scrollback line {i} {pad}\\r\\n")
w(f"{E}[2J{E}[H")
def at(r, text):
    w(f"{E}[{r};1H{text}")
inner = cols - 2
title = " Claude Code 2.1.329 (synthetic) "
at(1, "\\u256d" + title + "\\u2500" * (inner - len(title)) + "\\u256e")
body = 0
for r in range(2, rows - 4):
    body += 1
    txt = f" {E}[1;3{1 + body % 6}m>{E}[0m row {r} of {rows}, {cols} cols: " + "0123456789 " * 30
    plain_len = len(f" > row {r} of {rows}, {cols} cols: ") + len("0123456789 " * 30)
    shown = txt[: len(txt)]
    # crude clip: cut the visible text to the inner width
    vis = f" > row {r} of {rows}, {cols} cols: " + "0123456789 " * 30
    vis = vis[:inner]
    at(r, "\\u2502" + f"{E}[3{1 + body % 6}m" + vis + f"{E}[0m" + " " * (inner - len(vis)) + "\\u2502")
at(rows - 4, "\\u251c" + "\\u2500" * inner + "\\u2524")
at(rows - 3, "\\u2502" + (" > Try \\"fix lint errors\\"" + " " * inner)[:inner] + "\\u2502")
at(rows - 2, "\\u2570" + "\\u2500" * inner + "\\u256f")
last = f" ? for shortcuts   {cols}x{rows}   ctx 42%   opus   main*"
w(f"{E}[{rows};1H{E}[7m" + (last + " " * cols)[:cols] + f"{E}[0m")
open(os.path.join(out, "ready"), "w").write("1")
time.sleep(3600)
`;

function harness(): Harness {
  const out = scratch("termux");
  writeFileSync(join(out, "gen.py"), GEN);
  return { out, shell: `exec python3 -I '${join(out, "gen.py")}' '${out}'\n` };
}

test.afterEach(async ({ request }) => {
  await killLaunched(new Api(request));
});

const GRIDS = [
  { cols: 80, rows: 24 },
  { cols: 120, rows: 40 },
  { cols: 201, rows: 48 },
];

async function setup(request: import("@playwright/test").APIRequestContext, grid: { cols: number; rows: number }) {
  const api = new Api(request);
  const h = harness();
  const term = await launch(api, h, grid);
  const sid = newSid();
  await bindSession(api, sid, term);
  await waitFile(join(h.out, "ready"), 60_000);
  return { sid };
}

async function shot(page: Page, name: string, panelSel: string, notes: string[]): Promise<void> {
  mkdirSync(OUT, { recursive: true });
  await page.waitForTimeout(1500);
  const vp = page.viewportSize();
  await page.screenshot({ path: join(OUT, `${name}__full.png`), fullPage: true });
  await page.screenshot({ path: join(OUT, `${name}__viewport.png`) });
  try {
    await page.locator(panelSel).first().screenshot({ path: join(OUT, `${name}__panel.png`), timeout: 10_000 });
  } catch (e) {
    notes.push(`${name}: panel shot failed ${String(e).slice(0, 120)}`);
  }
  const geo = await page.evaluate((sel) => {
    const el = document.querySelector(sel) as HTMLElement | null;
    const scr = document.querySelector(".xterm-screen") as HTMLElement | null;
    const r = (e: Element | null) => (e ? (({ x, y, width, height }) => ({ x, y, w: width, h: height }))(e.getBoundingClientRect()) : null);
    return {
      panel: r(el), screen: r(scr),
      panelScroll: el ? { sw: el.scrollWidth, cw: el.clientWidth, sh: el.scrollHeight, ch: el.clientHeight } : null,
      font: scr ? getComputedStyle(scr).fontSize : null,
      docScroll: { sw: document.documentElement.scrollWidth, cw: document.documentElement.clientWidth, sh: document.documentElement.scrollHeight, ch: window.innerHeight },
    };
  }, panelSel);
  writeFileSync(join(OUT, `${name}__geometry.json`), JSON.stringify({ viewport: vp, ...geo }, null, 1));
}

async function openDesktop(page: Page, sid: string): Promise<void> {
  await page.goto(`${MAIN}/sessions/${sid}`);
  const open = page.locator('[data-testid="kb-term-open"]');
  await expect(open).toBeVisible({ timeout: 30_000 });
  await open.click();
  await expect(page.locator("[data-kb-term]")).toBeVisible({ timeout: 30_000 });
  await expect(page.locator("[data-kb-term]")).toHaveAttribute("data-kb-term-phase", "live", { timeout: 45_000 });
}

async function openPhoneView(page: Page, sid: string): Promise<void> {
  await page.goto(`${MAIN}/sessions/${sid}`);
  await page.locator('[data-testid="kb-term-open"]').waitFor({ state: "attached", timeout: 30_000 });
  await page.evaluate(() => {
    document.querySelectorAll("details").forEach((d) => {
      if (d.querySelector('[data-testid="kb-term-open"]')) d.open = true;
    });
  });
  const open = page.locator('[data-testid="kb-term-open"]');
  await expect(open).toBeVisible({ timeout: 30_000 });
  await open.click();
  await expect(page.locator("[data-kb-term-viewport]")).toBeVisible({ timeout: 30_000 });
  await expect(page.locator("[data-kb-term-viewport]")).toHaveAttribute("data-kb-term-phase", "live", { timeout: 45_000 });
}

const DESKTOP = [
  { n: "d1920x1080", w: 1920, h: 1080 },
  { n: "d1440x900", w: 1440, h: 900 },
  { n: "d1280x720", w: 1280, h: 720 },
  { n: "t1024x768", w: 1024, h: 768 },
];

for (const v of DESKTOP) {
  for (const g of GRIDS) {
    test(`shots ${v.n} grid ${g.cols}x${g.rows}`, async ({ browser, request }) => {
      test.setTimeout(180_000);
      const { sid } = await setup(request, g);
      const ctx: BrowserContext = await browser.newContext({ viewport: { width: v.w, height: v.h } });
      const page = await ctx.newPage();
      const notes: string[] = [];
      try {
        await openDesktop(page, sid);
        await shot(page, `${v.n}_g${g.cols}x${g.rows}`, "[data-kb-term]", notes);
        if (v.n === "d1440x900" && g.cols === 201) {
          for (const [w, h] of [[1100, 700], [1920, 1080], [800, 600]]) {
            await page.setViewportSize({ width: w, height: h });
            await shot(page, `resize_${v.n}_to_${w}x${h}_g${g.cols}x${g.rows}`, "[data-kb-term]", notes);
          }
        }
      } finally {
        if (notes.length) writeFileSync(join(OUT, `${v.n}_g${g.cols}x${g.rows}__notes.txt`), notes.join("\n"));
        await ctx.close();
      }
    });
  }
}

const iphone = (() => {
  const { defaultBrowserType: _d, ...d } = devices["iPhone 15"];
  void _d;
  return { ...d, hasTouch: true };
})();
const PHONES = [
  { n: "pixel7", use: { ...devices["Pixel 7"], hasTouch: true } },
  { n: "iphone15", use: iphone },
];

for (const p of PHONES) {
  for (const g of GRIDS) {
    test(`shots phone ${p.n} grid ${g.cols}x${g.rows}`, async ({ browser, request }) => {
      test.setTimeout(180_000);
      const { sid } = await setup(request, g);
      const ctx = await browser.newContext(p.use as never);
      const page = await ctx.newPage();
      const notes: string[] = [];
      const name = `p_${p.n}_g${g.cols}x${g.rows}`;
      const sel = '[data-testid="kb-terminal"]';
      try {
        await openPhoneView(page, sid);
        await shot(page, `${name}_watch`, sel, notes);
        const font = page.locator('[data-testid="kb-term-phone-font-up"]');
        if (await font.count()) {
          await font.click();
          await font.click();
          await shot(page, `${name}_watch_fontup2`, sel, notes);
        }
        const mode = page.locator('[data-testid="kb-term-phone-mode"]');
        if (await mode.count()) {
          await mode.click();
          await shot(page, `${name}_read`, sel, notes);
          await mode.click();
        }
        const input = page.locator('[data-testid="kb-drive-input"]');
        if (await input.count()) {
          await input.click().catch(() => {});
          await shot(page, `${name}_drive_focus`, sel, notes);
        } else notes.push(`${name}: no drive composer present`);
        // landscape
        const vp = page.viewportSize()!;
        await page.setViewportSize({ width: vp.height, height: vp.width });
        await shot(page, `${name}_landscape`, sel, notes);
      } finally {
        if (notes.length) writeFileSync(join(OUT, `${name}__notes.txt`), notes.join("\n"));
        await ctx.close();
      }
    });
  }
}
