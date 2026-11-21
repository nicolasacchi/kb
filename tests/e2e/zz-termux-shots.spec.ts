// TEMPORARY (TERMSHOTS final pass, CI only, never merge): screenshots of the web
// terminal panel over the real stack. Asserts only that the panel mounted.
// Shots land in tests/e2e/test-results/termux-shots/ (uploaded by the term-e2e job).
import { test, expect, devices, type Page, type BrowserContext } from "@playwright/test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { Api, MAIN, bindSession, killLaunched, launch, newSid, repoRoot, runnerRpc, scratch, waitFile, type Harness } from "./terminal-stack/stack";

const OUT_DIR = (): string => { mkdirSync(OUT, { recursive: true }); return OUT; };
const OUT = join(repoRoot(), "tests", "e2e", "test-results", "termux-shots");

const GEN = `
import os, sys, time
out = sys.argv[1]
while not os.path.exists(os.path.join(out, "go")):
    time.sleep(0.1)
time.sleep(0.5)
cols, rows = os.get_terminal_size(1)
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
sys.stdout.flush()
open(os.path.join(out, "ready"), "w").write(f"{cols}x{rows}")
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
  const ts = await api.json("POST", `/api/terms/${term.id}/take-size`, { data: grid });
  writeFileSync(join(OUT_DIR(), `setup_${grid.cols}x${grid.rows}_${sid}.txt`), `take-size ${ts.status} ${JSON.stringify(ts.body)}\n`);
  await new Promise((r) => setTimeout(r, 1000));
  writeFileSync(join(h.out, "go"), "1");
  await waitFile(join(h.out, "ready"), 60_000);
  const g2 = await api.json("GET", `/api/terms/${term.id}/grid`);
  writeFileSync(join(OUT_DIR(), `setup_${grid.cols}x${grid.rows}_${sid}.txt`), `take-size ${ts.status} ${JSON.stringify(ts.body)}\ngrid ${JSON.stringify(g2.body).slice(0, 300)}\npane saw ${readFileSync(join(h.out, "ready"), "utf8")}\n`);
  return { sid };
}

const gridOf = (name: string): string | null => /_g(\d+x\d+)/.exec(name)?.[1] ?? null;

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
    const host = (document.querySelector("[data-kb-term]") ?? document.querySelector("[data-kb-term-viewport]")) as (HTMLElement & { __kbTerm?: { cols: number; rows: number; options: { fontSize: number } } }) | null;
    const r = (e: Element | null) => (e ? (({ x, y, width, height, right, bottom }) => ({ x, y, w: width, h: height, right, bottom }))(e.getBoundingClientRect()) : null);
    const hostRect = host ? host.getBoundingClientRect() : null;
    const scrRect = scr ? scr.getBoundingClientRect() : null;
    const vw = document.documentElement.clientWidth;
    const term = host?.__kbTerm ?? null;
    return {
      panel: r(el), screen: r(scr), host: r(host),
      xterm: term ? { cols: term.cols, rows: term.rows, fontSize: term.options.fontSize } : null,
      panelScroll: el ? { sw: el.scrollWidth, cw: el.clientWidth, sh: el.scrollHeight, ch: el.clientHeight } : null,
      font: scr ? getComputedStyle(scr).fontSize : null,
      lastColVisible: scrRect && hostRect ? scrRect.right <= hostRect.right + 1 && scrRect.right <= vw + 1 && scrRect.left >= -1 : null,
      docScroll: { sw: document.documentElement.scrollWidth, cw: vw, sh: document.documentElement.scrollHeight, ch: window.innerHeight },
      chip: document.querySelector('[data-testid="kb-term-grid"]')?.textContent ?? null,
      status: document.querySelector('[data-testid="kb-term-status"]')?.textContent ?? null,
      overlay: !!document.querySelector(".kb-term--max"),
      nativeFullscreen: document.fullscreenElement !== null,
      theme: document.documentElement.getAttribute("data-theme"),
    };
  }, panelSel);
  writeFileSync(join(OUT, `${name}__geometry.json`), JSON.stringify({ viewport: vp, grid: gridOf(name), ...geo }, null, 1));
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

async function fit(page: Page): Promise<void> {
  await page.locator('[data-testid="kb-term-fit"]').click();
  const go = page.locator(".confirm__go");
  await go.waitFor({ state: "visible", timeout: 3_000 }).then(() => go.click(), () => undefined);
  await page.waitForTimeout(3000);
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
        await shot(page, `${v.n}_g${g.cols}x${g.rows}_inline`, "[data-kb-term]", notes);
        await fit(page);
        await shot(page, `${v.n}_g${g.cols}x${g.rows}_fit`, "[data-kb-term]", notes);
        const fs = page.locator('[data-testid="kb-term-fullscreen"]');
        if (await fs.count()) {
          await fs.click();
          await page.waitForTimeout(1500);
          await shot(page, `${v.n}_g${g.cols}x${g.rows}_fullscreen`, '[data-testid="kb-terminal"]', notes);
          await fs.click().catch(() => {});
        } else notes.push("no kb-term-fullscreen button");
      } finally {
        if (notes.length) writeFileSync(join(OUT, `${v.n}_g${g.cols}x${g.rows}__notes.txt`), notes.join("\n"));
        await ctx.close();
      }
    });
  }
}

test("shots light theme 1440x900 grid 120x40", async ({ browser, request }) => {
  test.setTimeout(180_000);
  const g = { cols: 120, rows: 40 };
  const { sid } = await setup(request, g);
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await ctx.addInitScript(() => localStorage.setItem("kb:prefs", JSON.stringify({ theme: "light" })));
  const page = await ctx.newPage();
  const notes: string[] = [];
  try {
    await openDesktop(page, sid);
    await shot(page, "light_d1440x900_g120x40_inline", "[data-kb-term]", notes);
    await fit(page);
    await shot(page, "light_d1440x900_g120x40_fit", "[data-kb-term]", notes);
  } finally {
    if (notes.length) writeFileSync(join(OUT_DIR(), "light__notes.txt"), notes.join("\n"));
    await ctx.close();
  }
});

test("shots local owner local:9 then Fit at 1440x900", async ({ browser, request }) => {
  test.setTimeout(180_000);
  const api = new Api(request);
  const h = harness();
  const term = await launch(api, h, { cols: 120, rows: 40 });
  const sid = newSid();
  await bindSession(api, sid, term);
  const seed = await runnerRpc("take_size", { id: term.id, cols: 120, rows: 40, owner: "local:9", force: false });
  writeFileSync(join(OUT_DIR(), "localowner__seed.txt"), JSON.stringify(seed));
  writeFileSync(join(h.out, "go"), "1");
  await waitFile(join(h.out, "ready"), 60_000);
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  const notes: string[] = [];
  try {
    await openDesktop(page, sid);
    await shot(page, "localowner_d1440x900_g120x40_before", "[data-kb-term]", notes);
    await fit(page);
    const grid = await api.json("GET", `/api/terms/${term.id}/grid`);
    writeFileSync(join(OUT_DIR(), "localowner__grid_after_fit.txt"), JSON.stringify(grid.body));
    await shot(page, "localowner_d1440x900_g120x40_after_fit", "[data-kb-term]", notes);
  } finally {
    if (notes.length) writeFileSync(join(OUT_DIR(), "localowner__notes.txt"), notes.join("\n"));
    await ctx.close();
  }
});

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
        const pfs = page.locator('[data-testid="kb-term-phone-fullscreen"]');
        if (await pfs.count()) {
          await pfs.click().catch(() => {});
          await shot(page, `${name}_fullscreen`, sel, notes);
        } else notes.push(`${name}: no phone fullscreen button`);
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
