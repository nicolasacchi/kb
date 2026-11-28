import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

// RAILKNOW — the Knowledge tab of the terminal's right panel, with the session
// readers mocked at the network edge. Desktop rail and the 390x844 sheet.

async function admit(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
  });
}

async function mockKnowledge(page: Page, opts: { recallsStatus?: number } = {}): Promise<string[]> {
  const pastes: string[] = [];
  const json = (body: unknown, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(new RegExp(`/api/sessions/${SID}/recalls`), (r) =>
    r.fulfill(
      opts.recallsStatus && opts.recallsStatus !== 200
        ? json({ detail: "boom" }, opts.recallsStatus)
        : json({
            recalls: [
              { kb: "mem", id: "a", title: "ranked memory", source_relative: "m/a.html", used: false, pos: 2 },
              { kb: "mem", id: "b", title: "unranked memory", source_relative: "m/b.html", used: false },
            ],
          }),
    ),
  );
  await page.route(new RegExp(`/api/sessions/${SID}/memories`), (r) =>
    r.fulfill(json({ memories: [{ id: "w", kb: "mem", title: "written", path: "/x", source_relative: "m/w.html" }] })),
  );
  await page.route(new RegExp(`/api/sessions/${SID}/research`), (r) =>
    r.fulfill(json({ research: [{ kind: "web", query: "a query" }] })),
  );
  await page.route(/\/api\/memory\/recall/, (r) =>
    r.fulfill(
      json({
        ms: 1,
        hits: [{ id: "h1", kb: "mem", title: "line one\nline two", source_relative: "m/h1.html", score: 1, salience: 1, pinned: false, path: "/p" }],
      }),
    ),
  );
  await page.route(new RegExp(`/api/terms/${TERM_ID}/paste`), async (r) => {
    pastes.push(r.request().postData() ?? "");
    await r.fulfill(json({}));
  });
  return pastes;
}

async function openKnowledge(page: Page, phone: boolean): Promise<void> {
  await page.goto(`${BASE}/terminals/${TERM_ID}`);
  if (phone) await page.locator('[data-testid="term-sheet-handle"]').click();
  await page.locator('[data-testid="term-tab-knowledge"]').click();
  await expect(page.locator('[data-testid="knowledge-rail"]')).toBeVisible({ timeout: 15_000 });
}

for (const phone of [false, true]) {
  test.describe(`RAILKNOW Knowledge tab (${phone ? "phone 390x844" : "desktop"})`, () => {
    test.use(phone ? { viewport: { width: 390, height: 844 } } : {});

    test("rows show rank or rank unknown, captions, and one failing lane stays alone", async ({ page }) => {
      await installFakeTerminal(page, { total: 300, seedLines: 100 });
      await admit(page);
      await mockKnowledge(page, { recallsStatus: 500 });
      await openKnowledge(page, phone);
      await expect(page.locator('[data-testid="kn-state-injected-error"]')).toBeVisible();
      await expect(page.locator('[data-testid="kn-lane-written"] [data-testid="kn-row"]')).toHaveCount(1);
      await expect(page.locator('[data-testid="kn-fresh-written"]')).toContainText("From last capture");
      await expect(page.locator('[data-testid="kn-lane-research"] [data-testid="kn-research-row"]')).toHaveCount(1);
    });

    test("injected lane ranks, and paste reference sends a newline-free body", async ({ page }) => {
      await installFakeTerminal(page, { total: 300, seedLines: 100 });
      await admit(page);
      const pastes = await mockKnowledge(page);
      await openKnowledge(page, phone);
      const ranks = page.locator('[data-testid="kn-lane-injected"] [data-testid="kn-rank"]');
      await expect(ranks).toHaveText(["#2", "rank unknown"]);
      await page.locator('[data-testid="kn-lane-search"] [data-testid="kn-paste"]').first().click();
      await expect.poll(() => pastes.length).toBe(1);
      const body = JSON.parse(pastes[0]) as Record<string, unknown>;
      expect(body.enter).toBeUndefined();
      expect(String(body.text)).not.toMatch(/[\n\r]/);
      expect(body.text).toBe("[line one line two] (mem/m/h1.html)");
    });
  });
}
