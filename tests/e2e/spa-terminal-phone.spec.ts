import { test, expect, type Page } from "@playwright/test";
import { BASE, installCloseWatcherStub } from "./helpers";
import { installFakeTerminal, SID } from "./fixtures/fake-tap.mjs";

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
  // so a test never measures a position that is still moving under it.
  let last = -1;
  let since = Date.now();
  for (let i = 0; i < 200; i += 1) {
    await page.waitForTimeout(50);
    const len = await num(page, "len");
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
    await page.reload();
    await page.locator('[data-testid="session-sec-terminal"] > summary').click();
    await page.locator('[data-testid="kb-term-open"]').click();
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
    // The session page closes its collapsible section below the breakpoint; the
    // sheet is portaled, so it stays visible without anyone reopening it.
    await expect
      .poll(() =>
        page.evaluate(
          () => (document.querySelector('[data-testid="session-sec-terminal"]') as HTMLDetailsElement).open,
        ),
      )
      .toBe(false);
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
