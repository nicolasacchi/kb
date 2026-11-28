import { test, expect, type BrowserContext, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

// PQUI - the prompt queue in the Now tab. The daemon's /api/term-prompts store
// is a route-level fake shared by every page of the test (synthetic prompts
// only); the terminal is the fake tap with its Drive routes. The proof points:
// Send is ONE POST /paste with no newline and no `enter`, never a /enter, the
// item moves to Done; the phone sheet has no horizontal overflow and 44 px
// controls; a second page learns of a change through the `term.prompts` event.
// CHROMIUM only.

interface Item {
  id: string;
  text: string;
  created_at: number;
  done_at: number | null;
}
interface Srv {
  rev: number;
  items: Item[];
  n: number;
  gets: number;
}
const newSrv = (): Srv => ({ rev: 0, items: [], n: 1, gets: 0 });
const view = (s: Srv) => ({ project: "demo", rev: s.rev, items: s.items.map((i, order) => ({ ...i, order })) });

async function admit(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
  });
}

async function mockPrompts(page: Page, s: Srv): Promise<void> {
  await page.route(new RegExp(`/api/sessions/${SID}$`), (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ session_id: SID, kb: "mem", source_relative: "sessions/synthetic-queue.html", project_slug: "demo" }),
    }),
  );
  await page.route(/\/api\/term-prompts\/demo(\/q_[0-9a-f]+)?$/, async (route) => {
    const req = route.request();
    const method = req.method();
    const reply = (status: number, body: unknown) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
    if (method === "GET") {
      s.gets += 1;
      return reply(200, view(s));
    }
    const body = (req.postDataJSON() ?? {}) as Record<string, unknown>;
    if (method === "POST") {
      s.items.push({ id: `q_${String(s.n++).padStart(12, "0")}`, text: String(body.text).trim(), created_at: 1, done_at: null });
      s.rev += 1;
      return reply(201, view(s));
    }
    if (req.headers()["if-match"] !== `"${s.rev}"`) return reply(412, { type: "urn:kb:errors:prompts-stale", title: "Precondition Failed", detail: "changed", rev: s.rev });
    const id = new URL(req.url()).pathname.split("/").pop();
    const idx = s.items.findIndex((i) => i.id === id);
    if (idx < 0) return reply(404, { title: "Not Found", detail: "no such prompt" });
    if (method === "DELETE") s.items.splice(idx, 1);
    else {
      const it = s.items[idx];
      if (typeof body.text === "string") it.text = body.text;
      if (typeof body.done === "boolean") it.done_at = body.done ? 9 : null;
      if (typeof body.position === "number") {
        s.items.splice(idx, 1);
        s.items.splice(body.position, 0, it);
      }
    }
    s.rev += 1;
    return reply(200, view(s));
  });
}

/** Hold this page's SSE connect until `emit` fires one `term.prompts` frame (direct transport, so page.route sees it). */
async function holdEvents(page: Page): Promise<(rev: number) => void> {
  await page.addInitScript(() => localStorage.setItem("kb:sse:transport", "direct"));
  let fire: (rev: number) => void = () => undefined;
  const first = new Promise<number>((resolve) => {
    fire = resolve;
  });
  let served = false;
  await page.route("**/api/events**", async (route) => {
    if (!served) {
      served = true;
      const rev = await first;
      return route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: `id: 1\nevent: term.prompts\ndata: ${JSON.stringify({ payload: { project: "demo", rev } })}\n\n`,
      });
    }
    // Later reconnects: park briefly, then answer with an empty comment frame.
    await new Promise((r) => setTimeout(r, 4000));
    return route.fulfill({ status: 200, contentType: "text/event-stream", body: ": idle\n\n" }).catch(() => undefined);
  });
  return fire;
}

async function openNow(page: Page, phone: boolean): Promise<void> {
  await page.goto(`${BASE}/terminals/${TERM_ID}`);
  if (phone) await page.locator('[data-testid="term-sheet-handle"]').click();
  await page.locator('[data-testid="term-tab-now"]').click();
  await expect(page.locator('[data-testid="now-queue-body"]')).toBeVisible({ timeout: 20_000 });
}

async function addPrompt(page: Page, text: string): Promise<void> {
  await page.locator('[data-testid="queue-input"]').fill(text);
  await page.locator('[data-testid="queue-add"]').click();
  await expect(page.locator('[data-testid="queue-input"]')).toHaveValue("");
}

const texts = (page: Page) => page.locator('[data-testid="queue-text"]').allTextContents();

for (const phone of [false, true]) {
  test.describe(`PQUI prompt queue (${phone ? "phone 360x740" : "desktop 1280x720"})`, () => {
    test.use({ viewport: phone ? { width: 360, height: 740 } : { width: 1280, height: 720 } });

    test("add two, reorder, Send pastes one line without Enter and the item moves to Done", async ({ page }) => {
      const s = newSrv();
      const stats = await installFakeTerminal(page, { total: 300, seedLines: 100, drive: true });
      await admit(page);
      await mockPrompts(page, s);
      await openNow(page, phone);

      await addPrompt(page, "first prompt");
      await addPrompt(page, "second line one\nsecond line two");
      await expect.poll(() => texts(page)).toEqual(["first prompt", "second line one\nsecond line two"]);

      // Reorder: the two-line prompt goes first.
      await page.locator('[data-testid="queue-up"]').nth(1).click();
      await expect.poll(() => texts(page)).toEqual(["second line one\nsecond line two", "first prompt"]);

      // Send the two-line prompt.
      await page.locator('[data-testid="queue-send"]').first().click();
      await expect(page.locator('[data-testid="queue-done"] summary')).toHaveText("Done (1)");
      expect(await texts(page)).toEqual(["first prompt"]);
      const drive = stats.drive as { op: string; json: Record<string, unknown> | null }[];
      const pastes = drive.filter((d) => d.op === "paste");
      expect(pastes).toHaveLength(1);
      expect(pastes[0].json).toEqual({ text: "second line one second line two" });
      expect(drive.filter((d) => d.op === "enter")).toHaveLength(0);
      await expect(page.locator('[data-testid="queue-note"]')).toHaveText("Sent as one line (line breaks removed)");

      if (phone) {
        const width = await page.evaluate(() => document.documentElement.scrollWidth);
        expect(width).toBeLessThanOrEqual(360);
        const sec = page.locator('[data-testid="now-queue"]');
        expect(await sec.evaluate((e) => e.scrollWidth <= e.clientWidth)).toBe(true);
        const buttons = sec.locator("button:visible");
        const n = await buttons.count();
        expect(n).toBeGreaterThan(0);
        for (let i = 0; i < n; i += 1) {
          const box = (await buttons.nth(i).boundingBox())!;
          expect(box.height).toBeGreaterThanOrEqual(44);
          expect(box.width).toBeGreaterThanOrEqual(44);
        }
      }
    });
  });
}

test.describe("PQUI second device", () => {
  test("a second page receives term.prompts and shows the new item without polling", async ({ browser }) => {
    const s = newSrv();
    const ctx: BrowserContext = await browser.newContext({ viewport: { width: 1280, height: 720 } });
    try {
      const a = await ctx.newPage();
      const b = await ctx.newPage();
      for (const p of [a, b]) {
        await installFakeTerminal(p, { total: 300, seedLines: 100, drive: true });
        await admit(p);
        await mockPrompts(p, s);
      }
      const emit = await holdEvents(b);
      await openNow(a, false);
      await openNow(b, false);
      await expect(b.locator('[data-testid="queue-item"]')).toHaveCount(0);

      await addPrompt(a, "from device A");
      // Nothing reaches B until the event does (no interval refetch).
      await b.waitForTimeout(1500);
      expect(await texts(b)).toEqual([]);
      emit(s.rev);
      await expect.poll(() => texts(b), { timeout: 20_000 }).toEqual(["from device A"]);
    } finally {
      await ctx.close();
    }
  });
});
