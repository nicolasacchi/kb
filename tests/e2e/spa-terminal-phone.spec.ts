import { test, expect, type Page } from "@playwright/test";
import { BASE, installCloseWatcherStub } from "./helpers";
import { installFakeTerminal, SID } from "./fixtures/fake-tap.mjs";
import { fixtureCard, TERM_ID } from "./fixtures/fake-tap.mjs";

// T17 — the phone terminal view. Runs ONLY on the three phone projects
// (chromium-pixel7, chromium-iphone15, webkit-iphone15; see
// playwright.config.ts). Transport is T14's `fixtures/fake-tap.mjs`; every
// byte is the synthetic numbered-line stream `L000001..LNNNNNN`.
// Gestures are synthetic PointerEvents dispatched on the Watch viewport
// (`pointerType: "touch"`); every assertion goes through the stable
// `data-kb-term-*` attributes, never a window global.

// The fake relay's recorder plus the T17 controls (fixtures/fake-tap.mjs).
type Stats = {
  tickets: number;
  sockets: number;
  froms: (number | null)[];
  inputs: string[];
  closes: number[];
  dropNow(code?: number): void;
  refuse(status: number | null): void;
  release(): void;
};

const VP = "[data-kb-term-viewport]";
const LINE = (i: number) => `L${String(i).padStart(6, "0")}`;
// "L000001\r\n": the stream offset after N lines. (The fake's SEED is sliced at
// 8 bytes per line, so seed sizes below are in its own unit.)
const LINE_BYTES = 9;

async function num(page: Page, name: string): Promise<number> {
  const v = await page.locator(VP).getAttribute(`data-kb-term-${name}`);
  return Number(v);
}

async function openPhone(page: Page, opts: Record<string, unknown>): Promise<Stats> {
  const stats = (await installFakeTerminal(page, opts)) as Stats;
  await page.goto(`${BASE}/sessions/${SID}`);
  // Under the 860 px breakpoint the session sections start collapsed.
  await page.locator('[data-testid="session-sec-terminal"] > summary').click();
  await expect(page.locator('[data-testid="kb-terminal-slot"]')).toBeVisible({ timeout: 15_000 });
  await page.locator('[data-testid="kb-term-open"]').click();
  await expect(page.locator(VP)).toBeVisible({ timeout: 15_000 });
  await expect(page.locator(VP)).toHaveAttribute("data-kb-term-phase", "live", { timeout: 30_000 });
  // The seed is applied before it is parsed: wait for the buffer to stop growing
  // so a test never measures a position that is still moving under it. The
  // viewport position is part of the signature: on WebKit CI the bottom position
  // was seen to move by one line (1961 -> 1960) after a test had taken its
  // baseline `vy`, with a sub-lock-distance touch as the only gesture in between.
  let last = "";
  let since = Date.now();
  for (let i = 0; i < 200; i += 1) {
    await page.waitForTimeout(50);
    const len = `${await num(page, "len")}/${await num(page, "vy")}/${await num(page, "base")}`;
    if (len !== last) {
      last = len;
      since = Date.now();
    } else if (Date.now() - since >= 400) break;
  }
  return stats;
}

/** Pixel height of one terminal row, measured from the rendered screen. */
async function cellHeight(page: Page): Promise<number> {
  return page.evaluate(() => {
    const screen = document.querySelector(".xterm-screen") as HTMLElement;
    const rows = Number(document.querySelector("[data-kb-term-viewport]")!.getAttribute("data-kb-term-rows"));
    return screen.getBoundingClientRect().height / rows;
  });
}

type Drag = { x0: number; y0: number; x1: number; y1: number; steps?: number; stepMs?: number; holdMs?: number; wobble?: number };

async function drag(page: Page, d: Drag): Promise<void> {
  await page.evaluate(async (a) => {
    const vp = document.querySelector("[data-kb-term-viewport]") as HTMLElement;
    const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
    const fire = (type: string, x: number, y: number) =>
      vp.dispatchEvent(
        new PointerEvent(type, {
          pointerId: 1,
          pointerType: "touch",
          isPrimary: true,
          bubbles: true,
          cancelable: true,
          clientX: x,
          clientY: y,
          buttons: type === "pointerup" ? 0 : 1,
        }),
      );
    const steps = a.steps ?? 10;
    const stepMs = a.stepMs ?? 10;
    fire("pointerdown", a.x0, a.y0);
    for (let i = 1; i <= steps; i += 1) {
      await sleep(stepMs);
      const wob = a.wobble ? (i % 2 === 0 ? a.wobble : -a.wobble) : 0;
      fire("pointermove", a.x0 + ((a.x1 - a.x0) * i) / steps + wob, a.y0 + ((a.y1 - a.y0) * i) / steps);
    }
    if (a.holdMs) await sleep(a.holdMs);
    fire("pointerup", a.x1, a.y1);
  }, d);
}

async function pinch(page: Page, d0: number, d1: number): Promise<void> {
  await page.evaluate(
    async ({ d0, d1 }) => {
      const vp = document.querySelector("[data-kb-term-viewport]") as HTMLElement;
      const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
      const fire = (type: string, id: number, x: number, y: number) =>
        vp.dispatchEvent(
          new PointerEvent(type, {
            pointerId: id,
            pointerType: "touch",
            isPrimary: id === 1,
            bubbles: true,
            cancelable: true,
            clientX: x,
            clientY: y,
            buttons: type === "pointerup" ? 0 : 1,
          }),
        );
      const cx = 190;
      const cy = 300;
      fire("pointerdown", 1, cx - d0 / 2, cy);
      fire("pointerdown", 2, cx + d0 / 2, cy);
      for (let i = 1; i <= 10; i += 1) {
        await sleep(12);
        const d = d0 + ((d1 - d0) * i) / 10;
        fire("pointermove", 1, cx - d / 2, cy);
        fire("pointermove", 2, cx + d / 2, cy);
      }
      fire("pointerup", 1, cx - d1 / 2, cy);
      fire("pointerup", 2, cx + d1 / 2, cy);
    },
    { d0, d1 },
  );
}

/** Wait until the buffer position has been quiet for `quietMs`. */
async function settled(page: Page, quietMs = 300): Promise<number> {
  let last = await num(page, "vy");
  let since = Date.now();
  for (let i = 0; i < 100; i += 1) {
    await page.waitForTimeout(50);
    const y = await num(page, "vy");
    if (y !== last) {
      last = y;
      since = Date.now();
    } else if (Date.now() - since >= quietMs) break;
  }
  return last;
}

async function readText(page: Page): Promise<string> {
  await page.locator('[data-testid="kb-term-phone-mode"]').click();
  const el = page.locator('[data-testid="kb-term-read-text"]');
  await expect(el).toBeVisible();
  const t = (await el.textContent()) ?? "";
  await page.locator('[data-testid="kb-term-read-watch"]').click();
  return t;
}

test.describe("T17 phone terminal: layout and gestures @phone", () => {
  test("layout: opens in a Sheet at the real 120-column grid, pannable; widths attached", async ({ page }, info) => {
    await openPhone(page, { total: 2000, seedLines: 2000 });
    await expect(page.locator("[data-kb-sheet]")).toBeVisible();
    await expect(page.locator(VP)).toHaveAttribute("data-kb-term-font", "12");
    expect(await num(page, "cols")).toBe(120);
    const m = await page.evaluate(() => {
      const vp = document.querySelector("[data-kb-term-viewport]") as HTMLElement;
      const wrap = vp.parentElement as HTMLElement;
      const screen = document.querySelector(".xterm-screen") as HTMLElement;
      const cellW = screen.getBoundingClientRect().width / 120;
      return {
        innerWidth: window.innerWidth,
        clientWidth: wrap.clientWidth,
        scrollWidth: wrap.scrollWidth,
        cellW,
        visibleCols: Math.floor(wrap.clientWidth / cellW),
      };
    });
    expect(m.scrollWidth).toBeGreaterThan(m.clientWidth);
    await info.attach("phone-widths", { body: JSON.stringify(m, null, 2), contentType: "application/json" });
    await info.attach("phone-12px", { body: await page.screenshot(), contentType: "image/png" });
  });

  test("vertical drag of 10 lines scrolls 10 lines; a fling coasts on momentum without panning", async ({ page }) => {
    await openPhone(page, { total: 3000, seedLines: 3000 });
    const cellH = await cellHeight(page);
    const start = await num(page, "vy");
    expect(start).toBe(await num(page, "base"));
    await drag(page, { x0: 200, y0: 100, x1: 200, y1: 100 + 10 * cellH, holdMs: 200 });
    const afterDrag = await settled(page);
    expect(Math.abs(start - afterDrag - 10)).toBeLessThanOrEqual(1);

    // Fling: 240 px in about 100 ms, released while still moving.
    const before = await num(page, "vy");
    await drag(page, { x0: 200, y0: 100, x1: 200, y1: 340, steps: 10, stepMs: 10 });
    const justAfter = await num(page, "vy");
    await expect.poll(() => num(page, "vy"), { timeout: 5_000 }).not.toBe(justAfter);
    const end = await settled(page, 400);
    const scrolled = before - end;
    expect(scrolled).toBeGreaterThan(Math.floor(240 / cellH));
    expect(await num(page, "sl")).toBe(0);
  });

  test("horizontal drag pans; a 4 px touch locks nothing; a wobble never pans", async ({ page }) => {
    await openPhone(page, { total: 2000, seedLines: 2000 });
    const vy = await num(page, "vy");
    await drag(page, { x0: 300, y0: 200, x1: 100, y1: 200 });
    expect(await num(page, "sl")).toBeGreaterThan(100);
    expect(await num(page, "vy")).toBe(vy);

    const sl = await num(page, "sl");
    await drag(page, { x0: 200, y0: 200, x1: 204, y1: 203, steps: 3 });
    expect(await num(page, "sl")).toBe(sl);
    expect(await num(page, "vy")).toBe(vy);

    // Back to the left edge, then a vertical drag with a 6 px cross-axis wobble.
    await page.evaluate(() => {
      (document.querySelector("[data-kb-term-viewport]")!.parentElement as HTMLElement).scrollLeft = 0;
    });
    await expect.poll(() => num(page, "sl")).toBe(0);
    const cellH = await cellHeight(page);
    await drag(page, { x0: 200, y0: 100, x1: 200, y1: 100 + 10 * cellH, wobble: 6, holdMs: 200 });
    await settled(page);
    expect(await num(page, "sl")).toBe(0);
    expect(await num(page, "vy")).toBeLessThan(vy);
  });

  test("pinch snaps to discrete font steps, never changes the grid, persists, and uses no transform", async ({
    page,
  }) => {
    await openPhone(page, { total: 1000, seedLines: 1000 });
    const rows = await num(page, "rows");
    await pinch(page, 100, 160);
    await expect(page.locator(VP)).toHaveAttribute("data-kb-term-font", "20");
    await pinch(page, 200, 60);
    await expect(page.locator(VP)).toHaveAttribute("data-kb-term-font", "8");
    expect(await num(page, "cols")).toBe(120);
    expect(await num(page, "rows")).toBe(rows);
    const bad = await page.evaluate(() => {
      const found: string[] = [];
      const host = document.querySelector(".xterm") as HTMLElement;
      for (let el: HTMLElement | null = host; el && el !== document.documentElement; el = el.parentElement) {
        const t = getComputedStyle(el).transform;
        if (t && t !== "none") found.push(`ancestor:${el.className || el.tagName}`);
      }
      for (const el of host.querySelectorAll<HTMLElement>("*")) {
        if (el.closest(".scrollbar")) continue;
        const t = getComputedStyle(el).transform;
        if (t && t !== "none") found.push(`inner:${el.className || el.tagName}`);
      }
      return found;
    });
    expect(bad).toEqual([]);
    expect(await page.evaluate(() => localStorage.getItem("kb-term-phone-font"))).toBe("8");
    // The persisted step survives a reload.
    // /term/:id reopens the panel by itself after a reload.
    await page.reload();
    await expect(page.locator(VP)).toHaveAttribute("data-kb-term-font", "8", { timeout: 30_000 });
  });

  test("real touch (CDP): a swipe scrolls the buffer and never the page @phone", async ({ page, browserName }) => {
    test.skip(browserName !== "chromium", "CDP Input.dispatchTouchEvent is Chromium-only");
    await openPhone(page, { total: 3000, seedLines: 3000 });
    const cellH = await cellHeight(page);
    const start = await num(page, "vy");
    // The Watch area is bottom-anchored: aim inside the part that is visible.
    const box = (await page.locator(".kb-term-phone__watch").boundingBox())!;
    const x = Math.round(box.x + 40);
    const y0 = Math.round(box.y + 20);
    const cdp = await page.context().newCDPSession(page);
    const touch = (type: string, y?: number) =>
      cdp.send("Input.dispatchTouchEvent", {
        type,
        touchPoints: y === undefined ? [] : [{ x, y }],
      });
    await touch("touchStart", y0);
    for (let i = 1; i <= 12; i += 1) {
      await touch("touchMove", y0 + Math.round(i * cellH));
      await page.waitForTimeout(16);
    }
    await page.waitForTimeout(150);
    await touch("touchEnd");
    const end = await settled(page, 400);
    expect(start - end).toBeGreaterThanOrEqual(8);
    expect(await page.evaluate(() => document.scrollingElement?.scrollTop ?? 0)).toBe(0);
  });
});

test.describe("T17 phone terminal: Read view @phone", () => {
  test("a long press opens Read; a 12 px move first keeps Watch; Watch resumes in place", async ({ page }) => {
    await openPhone(page, { total: 2000, seedLines: 2000 });
    // Scroll up a little so "Watch resumes at the same position" is meaningful.
    const cellH = await cellHeight(page);
    await drag(page, { x0: 200, y0: 100, x1: 200, y1: 100 + 20 * cellH, holdMs: 200 });
    const vy = await settled(page);

    await drag(page, { x0: 200, y0: 300, x1: 200, y1: 312, steps: 4, holdMs: 600 });
    await expect(page.locator('[data-testid="kb-term-read"]')).toHaveCount(0);

    await drag(page, { x0: 200, y0: 300, x1: 200, y1: 300, steps: 1, stepMs: 600 });
    const read = page.locator('[data-testid="kb-term-read-text"]');
    await expect(read).toBeVisible();
    expect(await read.textContent()).toContain(LINE(2000));
    expect(await read.evaluate((el) => getComputedStyle(el).userSelect)).toBe("text");

    await page.locator('[data-testid="kb-term-read-watch"]').click();
    await expect(page.locator(VP)).toBeVisible();
    expect(await num(page, "vy")).toBe(vy);
  });
});

test.describe("T17 phone terminal: reconnect @phone", () => {
  test("resume after four drops: no gap, strictly increasing from, Read text equals the stream", async ({ page }) => {
    const TOTAL = 2000;
    const stats = await openPhone(page, { total: TOTAL, seedLines: 500, drops: 4, dropEvery: 1024, rewind: 37 });
    const len = TOTAL * LINE_BYTES;
    await expect(page.locator(VP)).toHaveAttribute("data-kb-term-offset", String(len), { timeout: 60_000 });
    expect(await page.locator(VP).getAttribute("data-kb-term-gaps")).toBe("0");
    expect(await page.locator(VP).getAttribute("data-kb-term-resets")).toBe("1");
    const froms = stats.froms.filter((f) => f !== null) as number[];
    expect(froms.length).toBeGreaterThanOrEqual(4);
    for (let i = 1; i < froms.length; i += 1) expect(froms[i]).toBeGreaterThan(froms[i - 1]);
    // The offset counts bytes handed to xterm; wait for them to be parsed.
    let text = "";
    await expect
      .poll(
        async () => {
          text = await readText(page);
          return text.includes(`${LINE(TOTAL)}\n`);
        },
        { timeout: 30_000 },
      )
      .toBe(true);
    const expected = Array.from({ length: TOTAL }, (_, i) => LINE(i + 1));
    // The Read view hard-breaks at the grid width and right-trims, exactly like
    // the numbered stream (8 columns per line), so the lines are the stream.
    expect(text.replace(/\n$/, "").split("\n")).toEqual(expected);
  });

  test("backoff: a refused network makes few attempts, and `online` bypasses the pending delay", async ({
    page,
    browserName,
  }) => {
    test.skip(browserName !== "chromium", "backoff timing is asserted on Chromium only");
    const stats = await openPhone(page, { total: 1200, seedLines: 400, holdAt: 400 * 8 + 800 });
    const base = stats.tickets;
    stats.refuse(503);
    stats.dropNow();
    await page.waitForTimeout(3000);
    // 250/500/1000/2000 ms backoff: at most about 5 attempts in 3 s (a fixed 100 ms loop made 30).
    expect(stats.tickets - base).toBeLessThanOrEqual(6);
    // Let the delay grow past 4 s, then restore the network.
    await expect.poll(() => stats.tickets - base, { timeout: 30_000 }).toBeGreaterThanOrEqual(5);
    const sockets = stats.sockets;
    stats.refuse(null);
    stats.release();
    await page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect.poll(() => stats.sockets, { timeout: 3_000 }).toBeGreaterThan(sockets);
    await expect(page.locator(VP)).toHaveAttribute("data-kb-term-offset", String(1200 * LINE_BYTES), { timeout: 10_000 });
  });

  test("a resume while scrolled up leaves the viewport where it was and offers Jump to latest", async ({ page }) => {
    const TOTAL = 3000;
    const hold = 1000 * 8 + 800 * 8;
    const stats = await openPhone(page, { total: TOTAL, seedLines: 1000, holdAt: hold });
    await expect(page.locator(VP)).toHaveAttribute("data-kb-term-offset", String(hold), { timeout: 30_000 });
    const cellH = await cellHeight(page);
    await drag(page, { x0: 200, y0: 100, x1: 200, y1: 100 + 50 * cellH, holdMs: 200 });
    const vy = await settled(page);
    expect(vy).toBeLessThan(await num(page, "base"));
    await expect(page.locator('[data-testid="kb-term-phone-jump"]')).toBeVisible();
    const sockets = stats.sockets;
    stats.dropNow();
    await expect.poll(() => stats.sockets, { timeout: 15_000 }).toBeGreaterThan(sockets);
    stats.release();
    await expect(page.locator(VP)).toHaveAttribute("data-kb-term-offset", String(TOTAL * LINE_BYTES), { timeout: 30_000 });
    expect(await num(page, "vy")).toBe(vy);
    expect(await page.locator(VP).getAttribute("data-kb-term-resets")).toBe("1");
    await expect(page.locator('[data-testid="kb-term-phone-jump"]')).toBeVisible();
    await page.locator('[data-testid="kb-term-phone-jump"]').click();
    expect(await num(page, "vy")).toBe(await num(page, "base"));
  });
});

test.describe("T17 phone terminal: host and lifecycle @phone", () => {
  test("Back closes the sheet through the platform close request, no history entry", async ({ page }) => {
    await installCloseWatcherStub(page);
    await openPhone(page, { total: 500, seedLines: 500 });
    const len = await page.evaluate(() => history.length);
    await page.evaluate(() =>
      (window as unknown as { __cw: { instances: { alive: boolean; requestClose(): void }[] } }).__cw.instances
        .filter((w) => w.alive)
        .at(-1)!
        .requestClose(),
    );
    await expect(page.locator("[data-kb-sheet]")).toHaveCount(0);
    expect(await page.evaluate(() => history.length)).toBe(len);
  });

  test("keyboard-less: a tap never focuses an editable and nothing is sent", async ({ page }) => {
    const stats = await openPhone(page, { total: 500, seedLines: 500 });
    const vis = (await page.locator(".kb-term-phone__watch").boundingBox())!;
    await page.touchscreen.tap(vis.x + 60, vis.y + 60);
    const focus = await page.evaluate(() => {
      const el = document.activeElement as HTMLTextAreaElement | HTMLInputElement | null;
      return {
        tag: el?.tagName ?? "",
        xterm: el?.classList.contains("xterm-helper-textarea") ?? false,
        readOnly: (el as HTMLTextAreaElement | null)?.readOnly ?? false,
        inputMode: (el as HTMLTextAreaElement | null)?.inputMode ?? "",
      };
    });
    if (focus.xterm) {
      expect(focus.readOnly).toBe(true);
      expect(focus.inputMode).toBe("none");
    } else {
      expect(["TEXTAREA", "INPUT"]).not.toContain(focus.tag);
    }
    expect(stats.inputs).toEqual([]);
  });

  test("rotation across the breakpoint keeps one socket and never re-seeds", async ({ page }) => {
    const stats = await openPhone(page, { total: 1500, seedLines: 800, holdAt: 800 * 8 + 400 * 8 });
    const size = page.viewportSize()!;
    await page.setViewportSize({ width: 1000, height: 700 });
    // Desktop view: the same xterm element is re-parented into the desktop host.
    await expect(page.locator("[data-kb-term]")).toBeVisible();
    await expect(page.locator("[data-kb-term] .xterm")).toHaveCount(1);
    await page.setViewportSize(size);
    // Back below the breakpoint the shell is on its phone layout again, and the
    // sheet (portaled) stays visible without anyone reopening it.
    await expect(page.locator('[data-testid="term-shell"]')).toHaveAttribute("data-kb-term-layout", "phone");
    await expect(page.locator("[data-kb-sheet]")).toBeAttached();
    await expect(page.locator(VP)).toBeVisible();
    await expect(page.locator(`${VP} .xterm`)).toHaveCount(1);
    stats.release();
    await expect(page.locator(VP)).toHaveAttribute("data-kb-term-offset", String(1500 * LINE_BYTES), { timeout: 30_000 });
    expect(stats.sockets).toBe(1);
    expect(stats.tickets).toBe(1);
    expect(await page.locator(VP).getAttribute("data-kb-term-resets")).toBe("1");
    expect(await num(page, "len")).toBeGreaterThanOrEqual(1500);
  });
});

// FUTERMFIT — grid ownership from the phone. The web outranks a local attach,
// so "Fit to phone" never asks about a `local:*` owner; "Release grid" shows
// while the web owns the grid; and the page never scrolls sideways. The fake
// relay's Drive controls are on (`drive: true`); the list row's owner is
// `gridOwner`.
type DriveStats = Stats & { drive: Array<{ op: string; json: Record<string, unknown> | null }> };

test.describe("FUTERMFIT phone grid ownership @phone", () => {
  const ops = (s: DriveStats): string[] => s.drive.map((c) => c.op);

  test("Fit to phone with a local-attach owner shows no confirm and says it took the grid", async ({ page }) => {
    const stats = (await openPhone(page, { total: 300, seedLines: 300, drive: true, gridOwner: "local:9" })) as DriveStats;
    await expect(page.locator('[data-testid="kb-drive-release"]')).toHaveCount(0);
    await page.locator('[data-testid="kb-drive-fit"]').click();
    await expect.poll(() => ops(stats)).toEqual(["take-size"]);
    await expect(page.locator('[data-testid="kb-drive-caption"]')).toContainText("Took the grid from the local attach");
    // No confirm was ever raised.
    await expect(page.locator(".confirm__go")).toHaveCount(0);
  });

  test("Release grid is visible while the web owns the grid, and posts release-size", async ({ page }) => {
    const stats = (await openPhone(page, { total: 300, seedLines: 300, drive: true, gridOwner: "kb-term" })) as DriveStats;
    const release = page.locator('[data-testid="kb-drive-release"]');
    await expect(release).toBeVisible();
    await release.click();
    await expect.poll(() => ops(stats)).toEqual(["release-size"]);
    expect(stats.drive[0].json).toEqual({});
    await expect(page.locator('[data-testid="kb-drive-caption"]')).toHaveText("Released the grid.");
  });

  // FUTERMFULL — the full detent is the whole dynamic viewport, the title lives in the phone
  // bar only, and the full-screen button follows the browser's own feature detect.
  test("FUTERMFULL: the sheet covers the viewport, one visible title, the bar still works, the full-screen button follows the feature detect", async ({ page }) => {
    await openPhone(page, { total: 300, seedLines: 300 });
    const m = await page.evaluate(() => {
      const sheet = document.querySelector("[data-kb-sheet]") as HTMLElement;
      const r = sheet.getBoundingClientRect();
      const h2 = sheet.querySelector(".kb-sheet__title") as HTMLElement;
      const hr = h2.getBoundingClientRect();
      const bar = sheet.querySelector(".kb-term-phone__title") as HTMLElement;
      return {
        top: r.top,
        height: r.height,
        innerHeight: window.innerHeight,
        sheetTitleW: hr.width,
        sheetTitleH: hr.height,
        barTitle: bar.textContent,
        barTitleW: bar.getBoundingClientRect().width,
        fsAvailable: document.fullscreenEnabled === true && typeof document.documentElement.requestFullscreen === "function",
      };
    });
    expect(Math.abs(m.height - m.innerHeight), `sheet ${m.height} vs viewport ${m.innerHeight}`).toBeLessThanOrEqual(2);
    expect(Math.abs(m.top)).toBeLessThanOrEqual(2);
    expect(m.sheetTitleW).toBeLessThanOrEqual(1);
    expect(m.sheetTitleH).toBeLessThanOrEqual(1);
    // FUTERMPOLISH: the title row names the harness (the fake relay's is omp).
    expect(m.barTitle).toBe("omp");
    expect(m.barTitleW).toBeGreaterThan(8);
    await expect(page.locator('[data-testid="kb-term-phone-fullscreen"]')).toHaveCount(m.fsAvailable ? 1 : 0);
    // the bar still works: Read and back, a font step
    await page.locator('[data-testid="kb-term-phone-mode"]').click();
    await expect(page.locator('[data-testid="kb-term-read-text"]')).toBeVisible();
    await page.locator('[data-testid="kb-term-read-watch"]').click();
    const before = await num(page, "font");
    await page.locator('[data-testid="kb-term-phone-font-up"]').click();
    await expect.poll(() => num(page, "font")).toBeGreaterThan(before);
  });

  test("the page has no horizontal overflow with the sheet open, and the dock does not overflow its sheet", async ({ page }) => {
    await openPhone(page, { total: 300, seedLines: 300, drive: true, gridOwner: "kb-term" });
    const m = await page.evaluate(() => {
      const dock = document.querySelector('[data-testid="kb-drive"]') as HTMLElement;
      return {
        docW: document.documentElement.scrollWidth,
        vw: window.innerWidth,
        dockScroll: dock.scrollWidth,
        dockClient: dock.clientWidth,
      };
    });
    expect(m.docW).toBeLessThanOrEqual(m.vw);
    expect(m.dockScroll).toBeLessThanOrEqual(m.dockClient);
  });
});

// FUTERMPOLISH — the phone sheet reads cleanly: title, grid chip, button row, key
// row, crop hint and full screen.
test.describe("FUTERMPOLISH phone sheet @phone", () => {
  test("W6a: the title row shows the harness name and the state caption, neither truncated", async ({ page }) => {
    await openPhone(page, { total: 300, seedLines: 300, drive: true, gridOwner: "kb-term" });
    const m = await page.evaluate(() => {
      const t = document.querySelector('[data-testid="kb-term-phone-title"]') as HTMLElement;
      const c = document.querySelector('[data-testid="kb-term-caption"]') as HTMLElement;
      return {
        title: t.textContent,
        titleCut: t.scrollWidth > t.clientWidth + 1,
        caption: c.textContent,
        captionCut: c.scrollWidth > c.clientWidth + 1,
      };
    });
    expect(m.title).toBe("omp");
    expect(m.titleCut).toBe(false);
    expect(m.caption).toBe("live");
    expect(m.captionCut).toBe(false);
  });

  test("W6b: the grid chip names the real grid and its owner, with the desktop chip's words", async ({ page }) => {
    await openPhone(page, { total: 300, seedLines: 300, drive: true, gridOwner: "kb-term" });
    await expect(page.locator('[data-testid="kb-drive-grid-chip"]')).toHaveText("120x40 · web");
    await expect(page.locator('[data-testid="kb-drive-grid-chip"]')).not.toContainText("phone");
  });

  test("W6c: every button of the title bar is one line, the same height", async ({ page }) => {
    await openPhone(page, { total: 300, seedLines: 300, drive: true, gridOwner: "kb-term" });
    const heights = await page.evaluate(() =>
      [...document.querySelectorAll(".kb-term-phone__bar button")].map((b) => ({
        id: (b as HTMLElement).dataset.testid ?? "",
        h: Math.round(b.getBoundingClientRect().height),
      })),
    );
    expect(heights.length).toBeGreaterThanOrEqual(4);
    const min = Math.min(...heights.map((x) => x.h));
    for (const x of heights) expect(x.h, `${x.id}: ${JSON.stringify(heights)}`).toBeLessThanOrEqual(min + 1);
  });

  test("W6d: the key row says it scrolls, and its last key is reachable whole", async ({ page }) => {
    await openPhone(page, { total: 300, seedLines: 300, drive: true, gridOwner: "kb-term" });
    const keys = page.locator('[data-testid="kb-drive-keys"]');
    const scrolls = await keys.evaluate((el) => el.scrollWidth > el.clientWidth + 1);
    test.skip(!scrolls, "everything fits this phone: the key row has nothing to signal");
    await expect(keys).toHaveAttribute("data-more", "right");
    await keys.evaluate((el) => {
      el.scrollLeft = el.scrollWidth;
    });
    await expect(keys).not.toHaveAttribute("data-more", "right");
    const cut = await page.evaluate(() => {
      const row = document.querySelector('[data-testid="kb-drive-keys"]') as HTMLElement;
      const last = row.lastElementChild as HTMLElement;
      return Math.round(last.getBoundingClientRect().right - row.getBoundingClientRect().right);
    });
    expect(cut).toBeLessThanOrEqual(1);
  });

  test("W6e: in Watch, a grid wider than the screen carries the Cropped hint with the way out", async ({ page }) => {
    await openPhone(page, { total: 300, seedLines: 300, drive: true, gridOwner: "kb-term" });
    const hint = page.locator('[data-testid="kb-term-phone-cropped"]');
    await expect(hint).toBeVisible();
    await expect(hint).toContainText("Cropped");
    await expect(hint).toContainText("Fit to phone");
  });

  test("W6f: full screen folds the grid-controls row away; the keys and the composer stay (where the browser has full screen)", async ({ page }) => {
    await openPhone(page, { total: 300, seedLines: 300, drive: true, gridOwner: "kb-term" });
    const btn = page.locator('[data-testid="kb-term-phone-fullscreen"]');
    test.skip((await btn.count()) === 0, "this browser has no element full screen (iPhone Safari)");
    await expect(page.locator('[data-testid="kb-drive-fit"]')).toBeVisible();
    await btn.click();
    await expect(page.locator('[data-testid="kb-drive-fit"]')).toHaveCount(0, { timeout: 10_000 });
    await expect(page.locator('[data-testid="kb-drive-keybar"]')).toBeVisible();
    await expect(page.locator('[data-testid="kb-drive-composer"], [data-testid="kb-drive-input"]').first()).toBeVisible();
  });
});

// FUWEBLAUNCH — the two launch controls on a phone: "New terminal" on
// /sessions and "Resume in a kb terminal" on the session page. The daemon's
// launch route and identity are mocked (the harness daemon has the terminal
// tier off); the stream is the same synthetic fake relay as above.
test.describe("FUWEBLAUNCH phone launch controls @phone", () => {
  type Post = Record<string, unknown>;

  async function admitAndMockLaunch(page: Page): Promise<Post[]> {
    await page.route("**/api/identity", async (route) => {
      const res = await route.fetch();
      const j = (await res.json()) as Record<string, unknown>;
      await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
    });
    const posted: Post[] = [];
    // Registered after installFakeTerminal's own /api/terms route, so it is
    // consulted first; everything but the POST falls through to the fake list.
    await page.route("**/api/terms", (route) => {
      if (route.request().method() === "POST") {
        posted.push(JSON.parse(route.request().postData() ?? "{}") as Post);
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ id: TERM_ID }),
        });
      }
      return route.fallback();
    });
    return posted;
  }

  async function noHorizontalScroll(page: Page): Promise<void> {
    const over = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(over).toBeLessThanOrEqual(1);
  }

  test("New terminal: form fits the phone, a bad path sends nothing, a good one launches and links the session", async ({
    page,
  }) => {
    await installFakeTerminal(page, {});
    const posted = await admitAndMockLaunch(page);
    await page.goto(`${BASE}/sessions`);
    const open = page.locator('[data-testid="new-terminal-open"]');
    await expect(open).toBeVisible({ timeout: 20_000 });
    expect((await open.boundingBox())!.height).toBeGreaterThanOrEqual(43);
    await open.click();
    await expect(page.locator('[data-testid="new-terminal-form"]')).toBeVisible();
    await noHorizontalScroll(page);
    const vw = page.viewportSize()!.width;
    const field = (await page.locator('[data-testid="new-terminal-cwd"]').boundingBox())!;
    expect(field.x + field.width).toBeLessThanOrEqual(vw + 1);
    expect(field.height).toBeGreaterThanOrEqual(43);

    await page.locator('[data-testid="new-terminal-cwd"]').fill("relative/dir");
    await page.locator('[data-testid="new-terminal-launch"]').click();
    await expect(page.locator('[data-testid="new-terminal-invalid"]')).toBeVisible();
    expect(posted).toEqual([]);

    await page.locator('[data-testid="new-terminal-harness"]').selectOption("codex");
    await page.locator('[data-testid="new-terminal-cwd"]').fill("/synthetic");
    await page.locator('[data-testid="new-terminal-launch"]').click();
    await expect.poll(() => posted.length).toBe(1);
    expect(posted[0]).toEqual({ harness: "codex", cwd: "/synthetic" });
    // BSHELL: a launch lands on the terminal's own page.
    await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`), { timeout: 15_000 });
    await expect(page.locator(VP)).toBeVisible({ timeout: 30_000 });
    await noHorizontalScroll(page);
  });

  test("Resume in a kb terminal: launches with resume_sid and shows the panel", async ({ page }) => {
    await installFakeTerminal(page, {});
    const posted = await admitAndMockLaunch(page);
    const card: Record<string, unknown> = fixtureCard();
    card.terminal = { available: false, reason: "no live terminal", admitted: true };
    await page.route("**/api/sessions/*/card", (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(card) }),
    );
    await page.route(`**/api/sessions/${SID}`, (route) =>
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          id: "d1",
          kb: "mem",
          artifact_id: "a1",
          session_id: SID,
          started_at: 1760000000,
          ended_at: 1760000010,
          duration_ms: 10000,
          message_count: 1,
          memory_count: 0,
          source_relative: "t.html",
          display_name: "Synthetic T14 session",
          harness: "omp",
          cwd: "/synthetic",
          checks: [],
          unnamed_edits: [],
          unnamed_edits_total: 0,
          memory_ids: [],
        }),
      }),
    );
    await page.goto(`${BASE}/sessions/${SID}`);
    // Under the 860 px breakpoint the sections start collapsed.
    await page.locator('[data-testid="session-sec-resume"] > summary').click();
    const go = page.locator('[data-testid="session-resume-term-go"]');
    await expect(go).toBeVisible({ timeout: 20_000 });
    expect((await go.boundingBox())!.height).toBeGreaterThanOrEqual(43);
    await go.click();
    await expect.poll(() => posted.length).toBe(1);
    expect(posted[0]).toEqual({ harness: "omp", cwd: "/synthetic", resume_sid: SID });
    await expect(page.locator(VP)).toBeVisible({ timeout: 30_000 });
    await noHorizontalScroll(page);
  });
});

// FUPHONEMODE - the panel's own phone predicate (a coarse pointer with a short
// viewport is a phone even past the 860 px width breakpoint) and iOS Back.
async function openAt(page: Page, size: { width: number; height: number }, opts: Record<string, unknown> = {}): Promise<Stats> {
  await page.setViewportSize(size);
  const stats = (await installFakeTerminal(page, { total: 300, seedLines: 300, drive: true, ...opts })) as Stats;
  await page.goto(`${BASE}/sessions/${SID}`);
  // Collapsed below the breakpoint, open above it: click only when closed.
  const sec = page.locator('[data-testid="session-sec-terminal"]');
  await expect(sec).toBeAttached({ timeout: 15_000 });
  if (!(await sec.evaluate((d) => (d as HTMLDetailsElement).open))) {
    await sec.locator(":scope > summary").click();
  }
  await expect(page.locator('[data-testid="kb-terminal-slot"]')).toBeVisible({ timeout: 15_000 });
  await page.locator('[data-testid="kb-term-open"]').click();
  return stats;
}

test.describe("FUPHONEMODE landscape phone and iOS Back @phone", () => {
  test("PHONE-2: a touch phone in landscape (915x412) gets the Sheet and Drive, with the grid controls folded away", async ({ page }) => {
    await openAt(page, { width: 915, height: 412 });
    await expect(page.locator("[data-kb-sheet]")).toBeAttached({ timeout: 15_000 });
    await expect(page.locator('[data-testid="kb-drive"]')).toBeVisible();
    await expect(page.locator('[data-testid="kb-drive-keybar"]')).toBeVisible();
    await expect(page.locator('[data-testid="kb-drive-composer"]')).toBeVisible();
    // The dock keeps the key bar and the composer; the head row is folded.
    await expect(page.locator('[data-testid="kb-drive-fit"]')).toBeHidden();
    await expect(page.locator(VP)).toBeVisible({ timeout: 15_000 });
  });

  test("PHONE-2 negative control: a touch screen with a tall landscape viewport (1280x720) keeps the desktop panel", async ({ page }) => {
    await openAt(page, { width: 1280, height: 720 });
    await expect(page.locator('[data-testid="kb-terminal"]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator("[data-kb-sheet]")).toHaveCount(0);
    await expect(page.locator('[data-testid="kb-drive"]')).toHaveCount(0);
  });

  test("PHONE-4: without CloseWatcher the sheet owns one history entry; Back closes it and the page stays", async ({ page }) => {
    await page.addInitScript(() => {
      delete (window as unknown as { CloseWatcher?: unknown }).CloseWatcher;
    });
    await installFakeTerminal(page, { total: 300, seedLines: 300 });
    await page.goto(`${BASE}/sessions/${SID}`);
    await page.locator('[data-testid="session-sec-terminal"] > summary').click();
    await expect(page.locator('[data-testid="kb-terminal-slot"]')).toBeVisible({ timeout: 15_000 });
    const before = await page.evaluate(() => history.length);
    await page.locator('[data-testid="kb-term-open"]').click();
    await expect(page.locator("[data-kb-sheet]")).toBeAttached({ timeout: 15_000 });
    // One entry for the /term/:id page, one for the sheet.
    expect(await page.evaluate(() => history.length)).toBe(before + 2);
    await page.evaluate(() => history.back());
    await expect(page.locator("[data-kb-sheet]")).toHaveCount(0);
    // Back closes the sheet and the terminal page hands over to the session page.
    await expect(page).toHaveURL(new RegExp(`/sessions/${SID}$`));
    await expect(page.locator('[data-testid="kb-terminal-slot"]')).toBeAttached();
  });

  test("PHONE-4: closing with the sheet's own button pops its entry, so Back later leaves nothing behind", async ({ page }) => {
    await page.addInitScript(() => {
      delete (window as unknown as { CloseWatcher?: unknown }).CloseWatcher;
    });
    await installFakeTerminal(page, { total: 300, seedLines: 300 });
    await page.goto(`${BASE}/sessions/${SID}`);
    await page.locator('[data-testid="session-sec-terminal"] > summary').click();
    await expect(page.locator('[data-testid="kb-terminal-slot"]')).toBeVisible({ timeout: 15_000 });
    await page.locator('[data-testid="kb-term-open"]').click();
    await expect(page.locator("[data-kb-sheet]")).toBeAttached({ timeout: 15_000 });
    await page.locator('[data-testid="kb-term-close"]').click();
    await expect(page.locator("[data-kb-sheet]")).toHaveCount(0);
    await expect.poll(() => page.evaluate(() => (history.state as { kbSheet?: string } | null)?.kbSheet ?? null)).toBeNull();
  });
});

test.describe("FUTERMLIST terminals list @phone", () => {
  async function admitWithUnboundRow(page: Page): Promise<string[]> {
    await page.route("**/api/identity", async (route) => {
      const res = await route.fetch();
      const j = (await res.json()) as Record<string, unknown>;
      await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
    });
    const kills: string[] = [];
    // Registered after installFakeTerminal's routes, so it is consulted first.
    await page.route("**/api/terms", (route) => {
      if (route.request().method() !== "GET") return route.fallback();
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          terms: [
            {
              id: TERM_ID,
              harness: "omp",
              cwd: "/synthetic",
              started_unix: 1760000000,
              resume_sid: null,
              pane_pid: 1,
              cols: 120,
              rows: 40,
              bracket_paste_flag: false,
              pane_dead: false,
              exit_code: null,
              grid_owner: null,
              conf_version: 1,
              anchor_pid: null,
              stale: false,
              session_id: null,
              state: null,
              blocked: false,
            },
          ],
        }),
      });
    });
    await page.route("**/api/terms/*/kill", (route) => {
      kills.push(`${route.request().method()} ${new URL(route.request().url()).pathname} ${route.request().headers()["x-kb-intent"] ?? ""}`);
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ exit_code: null }) });
    });
    return kills;
  }

  test("from /sessions, tap a running unbound terminal: the sheet opens at /term/<id>, and a reload reopens it", async ({ page }) => {
    await installFakeTerminal(page, {});
    await admitWithUnboundRow(page);
    await page.goto(`${BASE}/sessions`);
    const row = page.locator(`[data-testid="term-row"][data-term="${TERM_ID}"]`);
    await expect(row).toBeVisible({ timeout: 20_000 });
    await expect(row.locator('[data-testid="term-row-unbound"]')).toBeVisible();
    await row.locator('[data-testid="term-row-open"]').click();
    await expect(page.locator(VP)).toBeVisible({ timeout: 30_000 });
    await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`));
    await page.reload();
    await expect(page.locator(VP)).toBeVisible({ timeout: 30_000 });
    await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`));
  });

  test("End in the sheet: Cancel sends nothing, Confirm sends exactly one kill with the intent header, and the sheet stays until the stream ends it", async ({ page }) => {
    await installFakeTerminal(page, {});
    const kills = await admitWithUnboundRow(page);
    await page.goto(`${BASE}/sessions?term=${TERM_ID}`);
    await expect(page.locator(VP)).toBeVisible({ timeout: 30_000 });
    const end = page.locator('[data-testid="kb-term-phone-extra"] [data-testid="kb-term-end"]');
    await expect(end).toBeVisible();
    await end.click();
    await page.locator(".confirm__cancel").click();
    await page.waitForTimeout(300);
    expect(kills).toEqual([]);
    await end.click();
    await page.locator(".confirm__go").click();
    await expect.poll(() => kills.length).toBe(1);
    expect(kills[0]).toBe(`POST /api/terms/${TERM_ID}/kill terminal`);
    // The client never ends the panel itself.
    await expect(page.locator(VP)).toBeVisible();
  });
});

// FUPHONELINKS — a tap on a URL in the Watch grid opens it (the scroller owns every
// gesture, so xterm's own link click never fires on a phone). Real touch input:
// page.touchscreen.tap produces trusted touch + pointer events.
test.describe("FUPHONELINKS tap to open @phone", () => {
  const URL_TEXT = "https://example.com/fuphonelinks-e2e";

  /** Viewport coordinates of a cell inside the rendered URL, from the DOM renderer's row. */
  async function urlPoint(page: Page): Promise<{ x: number; y: number }> {
    await expect
      .poll(() => page.evaluate((t) => [...document.querySelectorAll(".xterm-rows > div")].some((r) => r.textContent?.includes(t)), URL_TEXT))
      .toBe(true);
    return page.evaluate((t) => {
      const row = [...document.querySelectorAll(".xterm-rows > div")].find((r) => r.textContent?.includes(t)) as HTMLElement;
      const cols = Number(document.querySelector("[data-kb-term-viewport]")!.getAttribute("data-kb-term-cols"));
      const screen = document.querySelector(".xterm-screen") as HTMLElement;
      const cellW = screen.getBoundingClientRect().width / cols;
      const idx = (row.textContent ?? "").indexOf("https://");
      const r = row.getBoundingClientRect();
      return { x: screen.getBoundingClientRect().left + (idx + 4) * cellW, y: r.top + r.height / 2 };
    }, URL_TEXT);
  }

  test("a real tap on a rendered URL opens it in a new tab; a pan across it does not", async ({ page }) => {
    await page.context().route("https://example.com/**", (r) => r.fulfill({ status: 200, contentType: "text/html", body: "<title>ok</title>" }));
    await openPhone(page, { total: 200, seedLines: 200, tailText: `open ${URL_TEXT} now\r\n` });
    const p = await urlPoint(page);

    const before = page.context().pages().length;
    // A vertical drag that starts on the URL scrolls; it must not open anything.
    await drag(page, { x0: p.x, y0: p.y, x1: p.x, y1: p.y + 30, holdMs: 100 });
    // A link opens synchronously in the pointerup handler, so a pop-up would already exist by the
    // time a round trip through the page returns (same CDP channel, in order): wait for that, not a timer.
    await page.evaluate(() => new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r()))));
    expect(page.context().pages().length).toBe(before);

    const p2 = await urlPoint(page);
    const popupP = page.context().waitForEvent("page");
    await page.touchscreen.tap(p2.x, p2.y);
    const popup = await popupP;
    await expect.poll(() => popup.url(), { timeout: 15_000 }).toBe(URL_TEXT);
    await popup.close();
    // The phone view is still the Watch grid: the tap was not sent as a click to the page.
    await expect(page.locator(VP)).toBeVisible();
  });

  // FUMINPHONE: a still tap on a URL opens it and nothing else - no byte reaches the pty.
  test("a still tap on a URL sends nothing to the pty", async ({ page }) => {
    await page.context().route("https://example.com/**", (r) => r.fulfill({ status: 200, contentType: "text/html", body: "<title>ok</title>" }));
    const stats = await openPhone(page, { total: 200, seedLines: 200, tailText: `open ${URL_TEXT} now\r\n` });
    const p = await urlPoint(page);
    const popupP = page.context().waitForEvent("page");
    await page.touchscreen.tap(p.x, p.y);
    const popup = await popupP;
    await popup.close();
    expect(stats.inputs).toEqual([]);
  });
});

// FUMINPHONE (FUTERMFULL follow-up) - the Full screen button really enters element
// full screen on an Android-class phone and the same button leaves it.
test.describe("FUMINPHONE full screen button @phone", () => {
  test("a real tap on Full enters full screen on the terminal root; Exit leaves it", async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "chromium-pixel7", "only Android Chrome has element full screen on the phone view");
    await openPhone(page, { total: 300, seedLines: 300, drive: true, gridOwner: "kb-term" });
    const btn = page.locator('[data-testid="kb-term-phone-fullscreen"]');
    await expect(btn).toHaveAttribute("aria-label", "Full screen");
    await btn.tap();
    await expect
      .poll(() => page.evaluate(() => document.fullscreenElement?.hasAttribute("data-kb-term-root") ?? false), { timeout: 10_000 })
      .toBe(true);
    await expect(btn).toHaveAttribute("aria-label", "Exit full screen");
    // The grid-controls row folds away in full screen (W6f) and nothing was sent to the pty.
    await expect(page.locator('[data-testid="kb-drive-fit"]')).toHaveCount(0);
    await btn.tap();
    await expect.poll(() => page.evaluate(() => document.fullscreenElement === null), { timeout: 10_000 }).toBe(true);
    await expect(btn).toHaveAttribute("aria-label", "Full screen");
    await expect(page.locator('[data-testid="kb-drive-fit"]')).toBeVisible();
  });
});

// POLISH1 — the /sessions Terminals list keeps the page's horizontal inset on a
// phone: its heading and rows must not touch the screen's left edge, and sit no
// further left than the New terminal control above them.
test.describe("POLISH1 terminals list inset @phone", () => {
  test("the Terminals list and its rows start at the same inset as the New terminal button", async ({ page }) => {
    await installFakeTerminal(page, {});
    await page.route("**/api/identity", async (route) => {
      const res = await route.fetch();
      const j = (await res.json()) as Record<string, unknown>;
      await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
    });
    await page.route("**/api/terms", (route) => {
      if (route.request().method() !== "GET") return route.fallback();
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          terms: [
            {
              id: TERM_ID,
              harness: "omp",
              cwd: "/synthetic",
              started_unix: 1760000000,
              resume_sid: null,
              pane_pid: 1,
              cols: 120,
              rows: 40,
              bracket_paste_flag: false,
              pane_dead: false,
              exit_code: null,
              grid_owner: null,
              conf_version: 1,
              anchor_pid: null,
              stale: false,
              session_id: null,
              state: null,
              blocked: false,
            },
          ],
        }),
      });
    });
    await page.goto(`${BASE}/sessions`);
    const list = page.locator('[data-testid="term-list"]');
    const row = page.locator(`[data-testid="term-row"][data-term="${TERM_ID}"]`);
    const open = page.locator('[data-testid="new-terminal-open"]');
    await expect(row).toBeVisible({ timeout: 20_000 });
    await expect(open).toBeVisible();
    const heading = list.locator(".kb-ses__terms-h");
    const left = async (l: import("@playwright/test").Locator) => (await l.evaluate((el) => el.getBoundingClientRect().left));
    const openLeft = await left(open);
    // The list's content (heading text and row) starts at the page inset, not at x=0.
    const contentLeft = await list.evaluate((el) => {
      const cs = getComputedStyle(el);
      return el.getBoundingClientRect().left + parseFloat(cs.paddingLeft);
    });
    expect(openLeft).toBeGreaterThan(0);
    expect(contentLeft).toBeGreaterThanOrEqual(openLeft - 1);
    expect(await left(row)).toBeGreaterThanOrEqual(openLeft - 1);
    expect(await left(heading)).toBeGreaterThanOrEqual(openLeft - 1);
  });
});

// POLISH2 - a landscape phone keeps at least half the sheet for the terminal:
// the key row and the quick replies share one scrolling row, and the composer
// is one line with the attach button and Send beside it.
test.describe("POLISH2 landscape phone dock @phone", () => {
  const box = async (page: Page, sel: string) => {
    const b = await page.locator(sel).first().boundingBox();
    expect(b, sel).not.toBeNull();
    return b!;
  };
  const centreY = (b: { y: number; height: number }) => b.y + b.height / 2;

  test("POLISH2: at 915x412 the Watch pane is at least half the sheet and every control stays reachable", async ({ page }) => {
    await openAt(page, { width: 915, height: 412 });
    await expect(page.locator(VP)).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-testid="kb-drive-composer"]')).toBeVisible();
    // The last quick reply sits at the end of the key row: scroll it into view.
    await page.locator('[data-testid="kb-drive-keys"]').evaluate((el) => {
      el.scrollLeft = el.scrollWidth;
    });
    const sheet = await box(page, "[data-kb-sheet]");
    const watch = await box(page, ".kb-term-phone__watch");
    const share = watch.height / sheet.height;
    expect(share, `watch ${watch.height} of sheet ${sheet.height}`).toBeGreaterThanOrEqual(0.5);
    const vp = page.viewportSize()!;
    for (const id of ["kb-drive-keybar", "kb-drive-reply-0", "kb-drive-input", "kb-drive-send"]) {
      await expect(page.locator(`[data-testid="${id}"]`)).toBeVisible();
      const b = await box(page, `[data-testid="${id}"]`);
      expect(b.y, id).toBeGreaterThanOrEqual(0);
      expect(b.y + b.height, id).toBeLessThanOrEqual(vp.height + 1);
      expect(b.x + b.width, id).toBeLessThanOrEqual(vp.width + 1);
    }
    // A reply chip shares the key row; the textarea and Send share the composer row.
    const key = await box(page, '[data-testid="kb-drive-key-Enter"]');
    const reply = await box(page, '[data-testid="kb-drive-reply-0"]');
    expect(Math.abs(centreY(key) - centreY(reply))).toBeLessThanOrEqual(3);
    const input = await box(page, '[data-testid="kb-drive-input"]');
    const send = await box(page, '[data-testid="kb-drive-send"]');
    expect(Math.abs(centreY(input) - centreY(send))).toBeLessThanOrEqual(4);
  });

  test("POLISH2 negative control: portrait 412x839 keeps the replies row above the textarea and Send below it", async ({ page }) => {
    await openAt(page, { width: 412, height: 839 });
    await expect(page.locator(VP)).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-testid="kb-drive-input"]')).toBeVisible();
    const key = await box(page, '[data-testid="kb-drive-key-Enter"]');
    const reply = await box(page, '[data-testid="kb-drive-reply-0"]');
    const input = await box(page, '[data-testid="kb-drive-input"]');
    const send = await box(page, '[data-testid="kb-drive-send"]');
    expect(reply.y).toBeGreaterThan(key.y + key.height - 1);
    expect(input.y).toBeGreaterThan(reply.y + reply.height - 1);
    expect(send.y).toBeGreaterThan(input.y + input.height - 1);
    // Two lines of text, as on main (rows=2).
    expect(input.height).toBeGreaterThan(50);
  });
});

// FUMINLIST - the phone sheet's terminal switcher with two listed terminals:
// switching in the sheet moves /term/<id> to the other id. Switching PUSHES a
// history entry, so Back first returns to the previous terminal and a second
// Back leaves the terminal page.
test.describe("FUMINLIST two-terminal switcher @phone", () => {
  const OTHER_ID = "t-e2f";
  const termRow = (id: string, cwd: string) => ({
    id,
    harness: "omp",
    cwd,
    started_unix: 1760000000,
    resume_sid: null,
    pane_pid: 1,
    cols: 120,
    rows: 40,
    bracket_paste_flag: false,
    pane_dead: false,
    exit_code: null,
    grid_owner: null,
    conf_version: 1,
    anchor_pid: null,
    stale: false,
    session_id: null,
    state: null,
    blocked: false,
  });

  test("switching terminals in the sheet makes /term/<id> follow", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 300 });
    await page.route("**/api/identity", async (route) => {
      const res = await route.fetch();
      const j = (await res.json()) as Record<string, unknown>;
      await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
    });
    // Registered after installFakeTerminal's routes, so it is consulted first.
    await page.route("**/api/terms", (route) => {
      if (route.request().method() !== "GET") return route.fallback();
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ terms: [termRow(TERM_ID, "/synthetic/one"), termRow(OTHER_ID, "/synthetic/two")] }),
      });
    });
    await page.goto(`${BASE}/sessions`);
    const first = page.locator(`[data-testid="term-row"][data-term="${TERM_ID}"] [data-testid="term-row-open"]`);
    await expect(first).toBeVisible({ timeout: 20_000 });
    await first.click();
    await expect(page.locator("[data-kb-sheet]")).toBeAttached({ timeout: 15_000 });
    const at = () => new URL(page.url()).pathname;
    await expect.poll(at).toBe(`/terminals/${TERM_ID}`);

    const sw = page.locator('[data-testid="kb-term-switch"]');
    await expect(sw).toBeVisible({ timeout: 15_000 });
    // The switcher offers only the OTHER live terminal.
    await expect(sw.locator("option")).toHaveCount(2);
    await sw.selectOption(OTHER_ID);
    await expect.poll(at).toBe(`/terminals/${OTHER_ID}`);
    await expect(page.locator("[data-kb-sheet]")).toBeAttached();
    await expect(page.locator('[data-testid="term-shell"]')).toHaveAttribute("data-term", OTHER_ID);

    // Back #1 undoes the switch (the switch pushed an entry): same sheet, first terminal.
    await page.goBack();
    await expect.poll(at).toBe(`/terminals/${TERM_ID}`);
    await expect(page.locator("[data-kb-sheet]")).toBeAttached();
    await expect(page.locator('[data-testid="term-shell"]')).toHaveAttribute("data-term", TERM_ID);
    // Back #2 leaves the terminal page.
    await page.goBack();
    await expect.poll(at).toBe("/sessions");
    await expect(page.locator("[data-kb-sheet]")).toHaveCount(0);
    await expect(page.locator('[data-testid="term-shell"]')).toHaveCount(0);
  });
});

// BSHELL — the phone layer of /term/:id: the panel keeps its own full-bleed
// sheet, the shell adds ONE non-modal bottom layer whose tabs mount only while
// it is expanded, and leaving the page goes back to the session.
test.describe("BSHELL phone tab sheet @phone", () => {
  test("the handle expands to tabs and collapses; Back leaves the pane", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 300 });
    await page.route("**/api/identity", async (route) => {
      const res = await route.fetch();
      const j = (await res.json()) as Record<string, unknown>;
      await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
    });
    await page.goto(`${BASE}/sessions/${SID}`);
    await page.locator('[data-testid="session-sec-terminal"] > summary').click();
    await page.locator('[data-testid="kb-term-open"]').click();
    await expect(page.locator('[data-testid="term-shell"]')).toHaveAttribute("data-kb-term-layout", "phone", { timeout: 15_000 });
    await expect(page.locator("[data-kb-sheet]")).toBeAttached({ timeout: 15_000 });
    const handle = page.locator('[data-testid="term-sheet-handle"]');
    await expect(handle).toHaveAttribute("aria-expanded", "false");
    await expect(page.locator('[data-testid="rail-placeholder-conversation"]')).toHaveCount(0);
    await handle.click();
    await expect(handle).toHaveAttribute("aria-expanded", "true");
    await expect(page.locator('[data-testid="term-sheet"] [role="tab"]')).toHaveCount(3);
    await expect(page.locator('[data-testid="rail-placeholder-conversation"]')).toBeVisible();
    await handle.click();
    await expect(page.locator('[data-testid="rail-placeholder-conversation"]')).toHaveCount(0);
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`/sessions/${SID}$`));
    await expect(page.locator('[data-testid="term-shell"]')).toHaveCount(0);
  });
});

// PHIN0 - direct typing behind the hidden pref (`kb.term.phoneDirect`). The pref is
// seeded by an init script (the same storage `?direct=1` writes). Android Chrome only:
// the IME behaviour of Android Firefox is measured by hand with `?inputdebug=1`.
test.describe("PHIN0 phone direct typing @phone", () => {
  const PROXY = "[data-kb-term-input]";
  const typed = (stats: Stats): string =>
    stats.inputs
      .map((f) => JSON.parse(f) as { t: string; data?: string })
      .filter((m) => m.t === "input")
      .map((m) => m.data ?? "")
      .join("");

  async function direct(page: Page, on: boolean): Promise<Stats> {
    if (on) await page.addInitScript(() => window.localStorage.setItem("kb.term.phoneDirect", "1"));
    return openPhone(page, { total: 500, seedLines: 500 });
  }
  async function tapGrid(page: Page): Promise<void> {
    const vis = (await page.locator(".kb-term-phone__watch").boundingBox())!;
    await page.touchscreen.tap(vis.x + 60, vis.y + 60);
  }

  test.beforeEach(({}, info) => {
    test.skip(info.project.name !== "chromium-pixel7", "Android Chrome only");
  });

  test("a real tap on the grid focuses the input proxy (the touch sequence is not cancelled)", async ({ page }) => {
    const stats = await direct(page, true);
    await expect(page.locator(PROXY)).toHaveCount(1);
    await tapGrid(page);
    await expect.poll(() => page.evaluate(() => document.activeElement?.hasAttribute("data-kb-term-input") ?? false)).toBe(true);
    expect(stats.inputs).toEqual([]);
  });

  test("inserted text reaches the pty as input frames", async ({ page }) => {
    const stats = await direct(page, true);
    await tapGrid(page);
    await expect(page.locator(PROXY)).toBeFocused();
    await page.keyboard.insertText("hello");
    await expect.poll(() => typed(stats)).toBe("hello");
  });

  test("a composition wor -> world is sent without duplicated text", async ({ page }) => {
    const stats = await direct(page, true);
    await tapGrid(page);
    await expect(page.locator(PROXY)).toBeFocused();
    await page.evaluate(() => {
      const el = document.querySelector("[data-kb-term-input]") as HTMLTextAreaElement;
      const input = (v: string) => {
        el.value = v;
        el.dispatchEvent(new InputEvent("input", { inputType: "insertCompositionText", isComposing: true, bubbles: true }));
      };
      el.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
      input("wor");
      input("world");
      el.dispatchEvent(new CompositionEvent("compositionend", { data: "world", bubbles: true }));
    });
    await expect.poll(() => typed(stats)).toBe("world");
    // and nothing more arrives afterwards
    await page.waitForTimeout(300);
    expect(typed(stats)).toBe("world");
  });

  test("Backspace is a DEL byte", async ({ page }) => {
    const stats = await direct(page, true);
    await tapGrid(page);
    await expect(page.locator(PROXY)).toBeFocused();
    await page.keyboard.press("Backspace");
    await expect.poll(() => typed(stats)).toBe("\x7f");
  });

  test("flag off: a tap leaves no proxy and sends nothing", async ({ page }) => {
    const stats = await direct(page, false);
    await tapGrid(page);
    await page.waitForTimeout(300);
    await expect(page.locator(PROXY)).toHaveCount(0);
    expect(stats.inputs).toEqual([]);
  });
});

// PHIN1 - cursor-follow for direct typing. Android Chrome only (Playwright cannot raise a real
// soft keyboard; a shorter viewport is the layout-resize variant Android Firefox produces, and the
// visual-resize variant is covered by the jsdom geometry tables). Real timers throughout: the
// fixture's stream and the sheet run on timers a paused page.clock would freeze.
test.describe("PHIN1 phone cursor-follow @phone", () => {
  const PROXY = "[data-kb-term-input]";
  const WATCH = ".kb-term-phone__watch";

  async function directWith(page: Page, tailText: string): Promise<Stats> {
    await page.addInitScript(() => window.localStorage.setItem("kb.term.phoneDirect", "1"));
    return openPhone(page, { total: 500, seedLines: 500, tailText });
  }
  async function tapGrid(page: Page): Promise<void> {
    const vis = (await page.locator(WATCH).boundingBox())!;
    await page.touchscreen.tap(vis.x + 60, vis.y + 60);
    await expect(page.locator(PROXY)).toBeFocused();
  }
  const visualViewportResize = (page: Page): Promise<void> =>
    page.evaluate(() => void window.visualViewport?.dispatchEvent(new Event("resize")));

  test.beforeEach(({}, info) => {
    test.skip(info.project.name !== "chromium-pixel7", "Android Chrome only");
  });

  test("a shorter viewport keeps the typing cursor inside the Watch box", async ({ page }) => {
    // The cursor is parked on screen row 5, which a short viewport clips off the top.
    await directWith(page, "\x1b[5;1Hab");
    await tapGrid(page);
    await page.setViewportSize({ width: 412, height: 640 }) // the page chrome alone needs ~420: shorter collapses the Watch box to 0;
    await page.keyboard.insertText("x");
    await expect
      .poll(async () => {
        const p = await page.locator(PROXY).boundingBox();
        const w = await page.locator(WATCH).boundingBox();
        const diag = await page.evaluate(() => ({
          mb: (document.querySelector(".kb-term-phone__wrap") as HTMLElement | null)?.style.marginBottom ?? null,
          wrapH: document.querySelector(".kb-term-phone__wrap")?.getBoundingClientRect().height ?? null,
          vv: window.visualViewport ? [window.visualViewport.offsetTop, window.visualViewport.height] : null,
          inner: window.innerHeight,
          focused: document.activeElement?.hasAttribute("data-kb-term-input") ?? false,
        }));
        if (!p || !w) return `missing p=${JSON.stringify(p)} w=${JSON.stringify(w)} ${JSON.stringify(diag)}`;
        const ok = w.height > 40 && p.y >= w.y - 1 && p.y + p.height <= w.y + w.height + 1;
        return ok ? "inside" : `outside p=${JSON.stringify(p)} w=${JSON.stringify(w)} ${JSON.stringify(diag)}`;
      })
      .toBe("inside");
  });

  test("a cursor at a far column pans the grid to it", async ({ page }) => {
    await directWith(page, "x".repeat(110));
    expect(await num(page, "sl")).toBe(0);
    await tapGrid(page);
    await expect.poll(() => num(page, "sl")).toBeGreaterThan(100);
  });

  test("a manual pan suspends follow for 2 s", async ({ page }) => {
    await directWith(page, "x".repeat(110));
    await tapGrid(page);
    await expect.poll(() => num(page, "sl")).toBeGreaterThan(300);
    const followed = await num(page, "sl");
    await drag(page, { x0: 100, y0: 300, x1: 300, y1: 300 });
    const panned = await num(page, "sl");
    expect(panned).toBeLessThan(followed);
    // Right after the pan: a viewport event must not pull the grid back to the cursor.
    await visualViewportResize(page);
    await page.waitForTimeout(150);
    expect(await num(page, "sl")).toBe(panned);
    // After the window the same event follows again.
    await page.waitForTimeout(2100);
    await visualViewportResize(page);
    await expect.poll(() => num(page, "sl")).toBeGreaterThan(panned);
  });
});
