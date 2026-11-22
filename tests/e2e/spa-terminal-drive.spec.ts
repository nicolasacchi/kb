import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

// T18 — phone Drive mode: composer, key bar, choice bar, Claude guards,
// explicit "Fit to phone" and the image upload. CHROMIUM project only (the
// firefox project runs `@selection` titles, the phone projects run
// spa-terminal-phone.spec.ts only); a 390x844 touch context gives the phone
// Sheet. Transport is `fixtures/fake-tap.mjs` with `drive: true` (page.route
// fakes for the Drive endpoints and live-status). Every picture is a
// synthetic magic header plus filler generated here.

test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

type DriveCall = {
  op: string;
  headers: Record<string, string>;
  json: Record<string, unknown> | null;
  bytes: number | null;
  contentType: string;
};
type Stats = {
  drive: DriveCall[];
  replyDrive(op: string, status: number, body?: unknown): void;
  setBlocked(on: boolean, tool?: string): void;
};

const VIEWER = "ws-e2eviewer01";
const VP = "[data-kb-term-viewport]";
const id = (t: string) => `[data-testid="${t}"]`;

async function openDrive(page: Page, opts: Record<string, unknown> = {}): Promise<Stats> {
  const stats = (await installFakeTerminal(page, { total: 200, seedLines: 200, drive: true, viewer: VIEWER, ...opts })) as Stats;
  await page.goto(`${BASE}/sessions/${SID}`);
  // Under the 860 px breakpoint the session sections start collapsed.
  await page.locator('[data-testid="session-sec-terminal"] > summary').click();
  await expect(page.locator('[data-testid="kb-terminal-slot"]')).toBeVisible({ timeout: 15_000 });
  await page.locator('[data-testid="kb-term-open"]').click();
  await expect(page.locator(VP)).toBeVisible({ timeout: 15_000 });
  await expect(page.locator(VP)).toHaveAttribute("data-kb-term-phase", "live", { timeout: 30_000 });
  await expect(page.locator(id("kb-drive"))).toBeVisible();
  return stats;
}

const ops = (s: Stats): string[] => s.drive.map((c) => c.op);

async function typeAndSend(page: Page, text: string): Promise<void> {
  await page.locator(id("kb-drive-input")).fill(text);
  await page.locator(id("kb-drive-send")).click();
}

/** A tiny synthetic PNG: the 8-byte signature plus filler. */
function png(size: number): Buffer {
  const b = Buffer.alloc(size, 0x5a);
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).copy(b, 0);
  return b;
}

test.describe("T18 phone Drive: composer", () => {
  test("Send pastes then enters; a leading / pastes only; every request names the viewer and carries the intent header", async ({ page }) => {
    const stats = await openDrive(page, { bracketPaste: true });
    await typeAndSend(page, "fix the failing test");
    // FUPHONESEND: Send is ONE request (paste with enter:true), not paste then enter.
    await expect.poll(() => ops(stats)).toEqual(["paste"]);
    await expect(page.locator(id("kb-drive-input"))).toHaveValue("");

    await typeAndSend(page, "/help");
    await expect.poll(() => ops(stats)).toEqual(["paste", "paste"]);
    await expect(page.locator(id("kb-drive-caption"))).toContainText("Pasted, not sent");

    expect(stats.drive[0].json).toEqual({ text: "fix the failing test", enter: true, viewer: VIEWER });
    expect(stats.drive[1].json).toEqual({ text: "/help", viewer: VIEWER });
    for (const c of stats.drive) {
      expect(c.headers["x-kb-intent"]).toBe("terminal");
      expect(c.json?.viewer).toBe(VIEWER);
    }
  });

  test("a multi-line message is refused client-side when the pane did not enable bracketed paste", async ({ page }) => {
    const stats = await openDrive(page, { bracketPaste: false });
    await typeAndSend(page, "line one\nline two");
    await expect(page.locator(id("kb-drive-caption"))).toContainText("did not enable bracketed paste");
    await page.waitForTimeout(400);
    expect(stats.drive).toHaveLength(0);
    await expect(page.locator(id("kb-drive-input"))).toHaveValue("line one\nline two");
  });

  test("focusing the composer and typing never resizes the pty, and a key-bar tap leaves focus on the textarea", async ({ page }) => {
    const stats = await openDrive(page, { bracketPaste: true });
    const resizes: string[] = [];
    page.on("request", (r) => {
      if (/\/api\/terms\/[^/]+\/(take-size|resize)$/.test(r.url())) resizes.push(r.url());
    });
    const ta = page.locator(id("kb-drive-input"));
    await ta.click();
    await ta.pressSequentially("hello");
    await page.locator(id("kb-drive-key-Tab")).click();
    expect(await page.evaluate(() => document.activeElement?.getAttribute("data-testid"))).toBe("kb-drive-input");
    await page.waitForTimeout(500);
    expect(resizes).toEqual([]);
    expect(ops(stats)).toEqual(["keys"]);
    expect(stats.drive[0].json).toEqual({ keys: ["Tab"], viewer: VIEWER });
  });
});

test.describe("T18 phone Drive: choice bar", () => {
  test("shows only while the session is blocked; y sends the key; it goes away when the block clears", async ({ page }) => {
    // The live-status poll is 10 s: flipping the fake's row is picked up on the next poll.
    test.setTimeout(90_000);
    const stats = (await installFakeTerminal(page, { total: 200, seedLines: 200, drive: true, viewer: VIEWER })) as Stats;
    stats.setBlocked(true, "Bash");
    await page.goto(`${BASE}/sessions/${SID}`);
    await page.locator('[data-testid="session-sec-terminal"] > summary').click();
    await page.locator('[data-testid="kb-term-open"]').click();
    await expect(page.locator(VP)).toHaveAttribute("data-kb-term-phase", "live", { timeout: 30_000 });

    const bar = page.locator(id("kb-drive-choicebar"));
    await expect(bar).toBeVisible({ timeout: 20_000 });
    await expect(page.locator(id("kb-drive-choice-caption"))).toHaveText("Waiting on you: Bash");
    await page.locator(id("kb-drive-choice-y")).click();
    await expect.poll(() => ops(stats)).toEqual(["keys"]);
    expect(stats.drive[0].json).toEqual({ keys: ["y"], viewer: VIEWER });

    stats.setBlocked(false);
    await expect(bar).toBeHidden({ timeout: 30_000 });
  });

  test("Enter on a blocked row is refused 409 and the reason shows as the caption", async ({ page }) => {
    const stats = await openDrive(page, { bracketPaste: true });
    stats.replyDrive("enter", 409, {
      type: "urn:kb:errors:term-blocked",
      title: "Conflict",
      status: 409,
      detail: "the bound session is blocked",
    });
    await page.locator(id("kb-drive-key-Enter")).click();
    await expect(page.locator(id("kb-drive-caption"))).toHaveText("Enter not sent: the bound session is blocked");
    expect(ops(stats)).toEqual(["enter"]);
  });
});

test.describe("T18 phone Drive: Claude guards", () => {
  test("Left and a typed /bg each open the confirm; Cancel sends nothing", async ({ page }) => {
    const stats = await openDrive(page, { harness: "claude", bracketPaste: true });

    await page.locator(id("kb-drive-key-Left")).click();
    await expect(page.locator(".confirm__go")).toBeVisible();
    await expect(page.locator(".confirm__title")).toHaveText("Move this conversation to Claude's background?");
    await page.locator(".confirm__cancel").click();
    await expect(page.locator(".confirm__go")).toBeHidden();

    await typeAndSend(page, "/bg");
    await expect(page.locator(".confirm__go")).toBeVisible();
    await page.locator(".confirm__cancel").click();
    await expect(page.locator(".confirm__go")).toBeHidden();

    await page.waitForTimeout(400);
    expect(stats.drive).toHaveLength(0);
    // Declined text stays in the composer.
    await expect(page.locator(id("kb-drive-input"))).toHaveValue("/bg");

    // Confirming Left sends it.
    await page.locator(id("kb-drive-key-Left")).click();
    await page.locator(".confirm__go").click();
    await expect.poll(() => ops(stats)).toEqual(["keys"]);
    expect(stats.drive[0].json).toEqual({ keys: ["Left"], viewer: VIEWER });
  });
});

test.describe("T18 phone Drive: Fit to phone", () => {
  test("sends exactly one take-size with the cols and rows the page measures", async ({ page }) => {
    const stats = await openDrive(page, { bracketPaste: true });
    const want = await page.evaluate(() => {
      const vp = document.querySelector("[data-kb-term-viewport]") as HTMLElement;
      const wrap = vp.parentElement as HTMLElement;
      const watch = wrap.parentElement as HTMLElement;
      const screen = document.querySelector(".xterm-screen") as HTMLElement;
      const box = screen.getBoundingClientRect();
      const cols = Number(vp.getAttribute("data-kb-term-cols"));
      const rows = Number(vp.getAttribute("data-kb-term-rows"));
      return {
        cols: Math.floor(wrap.clientWidth / (box.width / cols)),
        rows: Math.floor(watch.clientHeight / (box.height / rows)),
      };
    });
    await page.locator(id("kb-drive-fit")).click();
    await expect.poll(() => ops(stats)).toEqual(["take-size"]);
    expect(stats.drive[0].json).toEqual({ ...want, viewer: VIEWER });
    await page.waitForTimeout(400);
    expect(ops(stats)).toEqual(["take-size"]);
  });
});

test.describe("T18 phone Drive: image upload", () => {
  test("a picture uploads once, its PATH is pasted (never entered) and a chip names it", async ({ page }) => {
    const stats = await openDrive(page, { bracketPaste: true });
    await page.setInputFiles(id("kb-drive-file"), { name: "p.png", mimeType: "image/png", buffer: png(72) });
    await expect.poll(() => ops(stats)).toEqual(["upload", "paste"]);
    expect(stats.drive[0].bytes).toBe(72);
    expect(stats.drive[0].contentType).toBe("image/png");
    expect(stats.drive[0].headers["x-kb-intent"]).toBe("terminal");
    expect(String(stats.drive[1].json?.text)).toMatch(/^\/run\/kb-term\/s\/synthetic\/up\/[0-9a-f]{64}\.png$/);
    await expect(page.locator(id("kb-drive-chip"))).toHaveText("image-abababab.png");
    await page.waitForTimeout(400);
    expect(ops(stats)).toEqual(["upload", "paste"]);
  });

  test("a 9 MiB picture is refused client-side: no request", async ({ page }) => {
    const stats = await openDrive(page, { bracketPaste: true });
    await page.setInputFiles(id("kb-drive-file"), { name: "big.png", mimeType: "image/png", buffer: png(9 * 1024 * 1024) });
    await page.waitForTimeout(600);
    expect(stats.drive).toHaveLength(0);
    await expect(page.locator(id("kb-drive-chip"))).toHaveCount(0);
  });
});

test.describe("T18 phone Drive: terminal disabled", () => {
  test("the upload route is a bodiless 404 on a daemon with the terminal tier off", async ({ request }) => {
    const resp = await request.post(`${BASE}/api/terms/${TERM_ID}/upload`, {
      headers: { Origin: BASE, "X-Kb-Intent": "terminal", "Content-Type": "image/png" },
      data: png(72),
    });
    expect(resp.status()).toBe(404);
    expect(await resp.text()).toBe("");
  });
});
