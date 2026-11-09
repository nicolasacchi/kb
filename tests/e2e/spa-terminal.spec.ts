import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID } from "./fixtures/fake-tap.mjs";

// T14 — desktop terminal panel on /sessions/:sid (CHROMIUM ONLY; the
// firefox project runs only /@selection/ titles and this spec carries no
// marker). Transport is `fixtures/fake-tap.mjs` (page.route for the card,
// list and ticket endpoints, page.routeWebSocket for the stream). All
// content is synthetic (numbered lines L000001..LNNNNNN).

type BufInfo = {
  lines: string[];
  baseY: number;
  rows: number;
  length: number;
  phase: string;
  offset: string;
  resets: string;
  gaps: string;
  width: string | null;
  cellWidths: number[];
};

async function bufferInfo(page: Page): Promise<BufInfo> {
  return page.evaluate(() => {
    const el = document.querySelector("[data-kb-term]") as unknown as {
      __kbTerm: {
        buffer: {
          active: {
            length: number;
            baseY: number;
            getLine(y: number): {
              translateToString(t: boolean): string;
              getCell(x: number): { getWidth(): number } | undefined;
            } | null;
          };
        };
        rows: number;
      };
    } | null;
    const term = el?.__kbTerm;
    const host = document.querySelector("[data-kb-term]") as unknown as {
      getAttribute(n: string): string | null;
    };
    const buf = term?.buffer.active;
    const lines: string[] = [];
    if (buf) {
      for (let i = 0; i < buf.length; i += 1) {
        const line = buf.getLine(i);
        const text = line ? line.translateToString(true) : "";
        if (text) lines.push(text);
      }
    }
    const cellWidths: number[] = [];
    if (buf) {
      for (let x = 0; x < 40; x += 1) {
        cellWidths.push(buf.getLine(0)?.getCell(x)?.getWidth() ?? -1);
      }
    }
    return {
      lines,
      baseY: buf?.baseY ?? -1,
      rows: term?.rows ?? -1,
      length: buf?.length ?? -1,
      phase: host?.getAttribute("data-kb-term-phase") ?? "?",
      offset: host?.getAttribute("data-kb-term-offset") ?? "?",
      resets: host?.getAttribute("data-kb-term-resets") ?? "?",
      gaps: host?.getAttribute("data-kb-term-gaps") ?? "?",
      width: host?.getAttribute("data-kb-term-width") ?? null,
      cellWidths,
    };
  });
}

async function openTerminal(page: Page): Promise<void> {
  const errors: unknown[] = [];
  page.on("pageerror", (e) => errors.push(e));
  // Surface uncaught page errors in the CI log even when the panel mounts:
  // a createTerminal throw currently shows up only as missing scrollback.
  page.on("pageerror", (e) => console.log(`[t14-pageerror] ${String(e).slice(0, 900)}`));
  page.on("console", (m) => errors.push(`${m.type()}: ${m.text()}`));
  page.on("requestfailed", (r) => errors.push(`reqfail: ${r.url()} ${r.failure()?.errorText ?? ""}`));
  page.on("response", (r) => {
    if (r.status() >= 400) errors.push(`http-${r.status()}: ${r.url()}`);
  });
  await page.goto(`${BASE}/sessions/${SID}`);
  await expect(page.locator('[data-testid="kb-terminal-slot"]')).toBeVisible({ timeout: 15_000 });
  await page.locator('[data-testid="kb-term-open"]').click();
  try {
    await expect(page.locator('[data-kb-term]')).toBeVisible({ timeout: 15_000 });
  } catch (e) {
    const slots = await page.evaluate(() => document.body.innerHTML.length);
    // eslint-disable-next-line no-console
    console.log(`[t14-open] bodyLen=${slots} errors=${JSON.stringify(errors.map(String)).slice(0, 4000)}`);
    throw e;
  }
}

test.describe("T14 — desktop terminal panel", () => {
  test("open + scrollback: seed renders L000001..LTOTAL once each, highest == total", async ({
    page,
  }) => {
    const TOTAL = 2000;
    const stats = await installFakeTerminal(page, { total: TOTAL, seedLines: 500 });
    await openTerminal(page);
    await expect
      .poll(async () => (await bufferInfo(page)).lines.length, { timeout: 30_000 })
      .toBe(TOTAL);
    const info = await bufferInfo(page);
    expect(info.lines).toHaveLength(TOTAL);
    for (let i = 0; i < TOTAL; i += 1) {
      expect(info.lines[i]).toBe(`L${String(i + 1).padStart(6, "0")}`);
    }
    // Highest line == total, zero-padded like every line above.
    expect(info.lines[TOTAL - 1]).toBe(`L${String(TOTAL).padStart(6, "0")}`);
    expect(info.baseY + info.rows).toBeGreaterThanOrEqual(TOTAL);
    expect(info.width).toBe("unicode-graphemes");

    expect(stats.tickets).toBe(1);
    expect(stats.sockets).toBe(1);
    const headers = stats.ticketRequests[0].headers;
    expect(headers["x-kb-intent"]).toBe("terminal");
    expect(headers["origin"]).toContain("127.0.0.1");
    expect(headers["authorization"]).toBeUndefined();
    expect(stats.froms[0]).toBeNull();
  });

  test("resume: 4 abrupt drops, from strictly increasing, gaps=0, resets=1", async ({
    page,
  }) => {
    const TOTAL = 2000;
    const stats = await installFakeTerminal(page, {
      total: TOTAL,
      seedLines: 500,
      drops: 4,
      dropEvery: 1024,
    });
    await openTerminal(page);
    await expect
      .poll(async () => (await bufferInfo(page)).lines.length, { timeout: 60_000 })
      .toBe(TOTAL);
    const info = await bufferInfo(page);
    expect(info.gaps).toBe("0");
    expect(info.resets).toBe("1");
    expect(stats.tickets).toBe(stats.sockets);
    const froms = stats.froms.filter((f) => f !== null) as number[];
    expect(froms.length).toBeGreaterThan(0);
    for (let i = 1; i < froms.length; i += 1) {
      expect(froms[i]).toBeGreaterThan(froms[i - 1]);
    }
    expect(new Set(stats.ticketRequests.map((r) => r.headers["x-kb-intent"]))).toEqual(
      new Set(["terminal"]),
    );
  });

  test("37-byte rewind overlap is absorbed", async ({ page }) => {
    const TOTAL = 1000;
    const stats = await installFakeTerminal(page, {
      total: TOTAL,
      seedLines: 300,
      drops: 1,
      dropEvery: 777,
      rewind: 37,
    });
    await openTerminal(page);
    await expect
      .poll(async () => (await bufferInfo(page)).lines.length, { timeout: 30_000 })
      .toBe(TOTAL);
    const info = await bufferInfo(page);
    for (let i = 0; i < TOTAL; i += 1) {
      expect(info.lines[i]).toBe(`L${String(i + 1).padStart(6, "0")}`);
    }
    expect(stats.froms.filter((f) => f !== null).length).toBeGreaterThan(0);
  });

  test("gap triggers a full rejoin: gaps=1, second socket without from, resets=2", async ({
    page,
  }) => {
    const TOTAL = 1000;
    const stats = await installFakeTerminal(page, {
      total: TOTAL,
      seedLines: 300,
      gapAt: 8 * 300 + 200,
    });
    await openTerminal(page);
    await expect
      .poll(async () => (await bufferInfo(page)).gaps, { timeout: 30_000 })
      .toBe("1");
    const info = await bufferInfo(page);
    // The full rejoin lands after the >=250ms reconnect backoff, so poll
    // for it rather than asserting it immediately after the gap.
    await expect
      .poll(async () => (await bufferInfo(page)).resets, { timeout: 30_000 })
      .toBe("2");
    await expect
      .poll(async () => (await bufferInfo(page)).lines.length, { timeout: 30_000 })
      .toBe(TOTAL);
    expect(stats.froms).toContain(null);
  });

  test("reply guards swallow queries; typing x yields exactly one input frame", async ({
    page,
  }) => {
    const stats = await installFakeTerminal(page, { total: 300, seedLines: 100, injectQueries: true });
    await openTerminal(page);
    await expect
      .poll(async () => (await bufferInfo(page)).phase, { timeout: 30_000 })
      .toBe("live");
    // The injected query storm arrived; the fake saw ZERO client frames.
    expect(stats.inputs).toEqual([]);
    const view = page.locator("[data-kb-term]");
    await view.click();
    await page.keyboard.type("x");
    await expect.poll(() => stats.inputs.length).toBe(1);
    expect(stats.inputs[0]).toContain('"data":"x"');
  });

  test("keys: Shift+Enter, Enter, Ctrl-C encode to the pinned bytes", async ({ page }) => {
    const stats = await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await openTerminal(page);
    await expect
      .poll(async () => (await bufferInfo(page)).phase, { timeout: 30_000 })
      .toBe("live");
    const area = page.locator(".xterm-helper-textarea");
    await area.press("Shift+Enter");
    await expect.poll(() => stats.inputs.length).toBeGreaterThanOrEqual(1);
    expect(stats.inputs.some((f) => f.includes("13;2u"))).toBe(true);
    await area.press("Enter");
    await expect.poll(() => stats.inputs.some((f) => f.endsWith('"data":"\\r"}'))).toBe(true);
    await area.press("Control+c");
    await expect
      .poll(() => stats.inputs.some((f) => f.endsWith('"data":"\\u0003"}')))
      .toBe(true);
  });

  test("hotkeys: typing ? g j / with the terminal focused opens no overlay and all arrive", async ({
    page,
  }) => {
    const stats = await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await openTerminal(page);
    await expect
      .poll(async () => (await bufferInfo(page)).phase, { timeout: 30_000 })
      .toBe("live");
    const view = page.locator("[data-kb-term]");
    await view.click();
    for (const ch of ["?", "g", "j", "/"]) {
      await page.keyboard.type(ch);
    }
    await expect.poll(() => stats.inputs.length).toBeGreaterThanOrEqual(4);
    const joined = stats.inputs.join(" ");
    for (const ch of ["?", "g", "j", "/"]) expect(joined).toContain(`"data":"${ch}"`);
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);
  });

  test("width table: 40 kitty placeholder cells render width 1", async ({ page }) => {
    await installFakeTerminal(page, { total: 40, seedLines: 40, widthTest: true });
    await openTerminal(page);
    await expect
      .poll(async () => (await bufferInfo(page)).width, { timeout: 30_000 })
      .toBe("unicode-graphemes");
    const info = await bufferInfo(page);
    expect(info.cellWidths).toHaveLength(40);
    for (const w of info.cellWidths) expect(w).toBe(1);
  });

  test("exit: caption shows the code and no further tickets arrive", async ({ page }) => {
    const stats = await installFakeTerminal(page, { total: 200, seedLines: 100, exitCode: 3 });
    await openTerminal(page);
    await expect(page.locator('[data-testid="kb-term-caption"]')).toContainText("exited (code 3)", {
      timeout: 30_000,
    });
    const tickets = stats.tickets;
    await page.waitForTimeout(3000);
    expect(stats.tickets).toBe(tickets);
    expect(stats.sockets).toBe(1);
  });

  test("close: socket closes with 1000, no more tickets; reopening mints exactly one", async ({
    page,
  }) => {
    const stats = await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await openTerminal(page);
    await expect
      .poll(async () => (await bufferInfo(page)).phase, { timeout: 30_000 })
      .toBe("live");
    await page.locator('[data-testid="kb-term-open"]').click();
    await expect(page.locator("[data-kb-term]")).toHaveCount(0);
    expect(stats.closes).toContain(1000);
    const tickets = stats.tickets;
    await page.waitForTimeout(1000);
    expect(stats.tickets).toBe(tickets);
    await page.locator('[data-testid="kb-term-open"]').click();
    await expect(page.locator("[data-kb-term]")).toBeVisible({ timeout: 15_000 });
    expect(stats.tickets).toBe(tickets + 1);
  });

  test("disabled: ticket 404 shows 'terminal disabled', one attempt, no WS", async ({ page }) => {
    const stats = await installFakeTerminal(page, { total: 100, ticketStatus: 404 });
    await openTerminal(page);
    await expect(page.locator('[data-testid="kb-term-caption"]')).toContainText("terminal disabled", {
      timeout: 15_000,
    });
    expect(stats.tickets).toBe(1);
    expect(stats.sockets).toBe(0);
  });

  test("1012: reconnects after backoff with a fresh ticket and a from", async ({ page }) => {
    const stats = await installFakeTerminal(page, { total: 500, seedLines: 200, restartOnce: true });
    await openTerminal(page);
    await expect.poll(() => stats.sockets).toBeGreaterThanOrEqual(2);
    expect(stats.tickets).toBeGreaterThanOrEqual(2);
    expect(stats.closes).toContain(1012);
    const froms = stats.froms.filter((f) => f !== null) as number[];
    expect(froms.length).toBeGreaterThan(0);
  });

  test("profile pin mismatch: panel is blocked, no seed applied", async ({ page }) => {
    await installFakeTerminal(page, { total: 100, profilePatch: { xterm: "9.9.9" } });
    await openTerminal(page);
    await expect
      .poll(async () => (await bufferInfo(page)).phase, { timeout: 15_000 })
      .toBe("blocked");
    const info = await bufferInfo(page);
    expect(info.resets).toBe("0");
    expect(info.lines).toEqual([]);
  });
});
