import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, TERM_ID } from "./fixtures/fake-tap.mjs";

// HUBDOOR — the Terminals door (sixth Spine door, admitted callers only), the
// /terminals hub list and the /term/:id redirect. Transport is the fake tap;
// the fixture's identity is terminal-admitted, and the term list is re-routed
// here to carry a second, blocked terminal. CHROMIUM desktop only.

const OTHER_ID = "t-e2e-2";

function row(id: string, o: Record<string, unknown> = {}) {
  return {
    id,
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
    state: "working",
    blocked: false,
    ...o,
  };
}

async function twoTerms(page: Page, blockedId: string | null): Promise<void> {
  await page.route("**/api/terms", (route) => {
    if (route.request().method() !== "GET") return route.continue();
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        terms: [row(TERM_ID, { blocked: blockedId === TERM_ID }), row(OTHER_ID, { blocked: blockedId === OTHER_ID })],
      }),
    });
  });
}

async function notAdmitted(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: false } });
  });
}

const DOOR = '[data-testid="header-terminals-tab"]';

test.describe("HUBDOOR Terminals door and hub", () => {
  test("door with a pill for the one blocked terminal; the hub lists it first and rows open /terminals/<id>", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await twoTerms(page, OTHER_ID);
    await page.goto(`${BASE}/`);
    const door = page.locator(DOOR);
    await expect(door).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('[data-testid="door-badge-terminals"]')).toHaveText("1");
    await expect(page.getByRole("link", { name: /Terminals\s*,\s*1 needs attention/ })).toBeVisible();
    await door.click();
    await expect(page).toHaveURL(/\/terminals$/);
    const rows = page.locator('[data-testid="hub-row"]');
    await expect(rows).toHaveCount(2, { timeout: 15_000 });
    await expect(rows.first()).toHaveAttribute("data-term", OTHER_ID);
    await expect(rows.first()).toHaveAttribute("data-tier", "needs");
    await rows.nth(1).click();
    await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`));
    await expect(page.locator("[data-kb-term]")).toHaveAttribute("data-kb-term-phase", "live", { timeout: 30_000 });
  });

  test("/term/<id> lands on /terminals/<id> with the viewport live", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await page.goto(`${BASE}/term/${TERM_ID}`);
    await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`));
    await expect(page.locator("[data-kb-term]")).toHaveAttribute("data-kb-term-phase", "live", { timeout: 30_000 });
  });

  test("a caller without the terminal tier sees no door and a refused card on /terminals", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await notAdmitted(page);
    await page.goto(`${BASE}/`);
    await expect(page.locator('[data-testid="header-search-tab"]')).toBeVisible({ timeout: 20_000 });
    await expect(page.locator(DOOR)).toHaveCount(0);
    await page.goto(`${BASE}/terminals`);
    await expect(page.getByText("Terminals are not enabled")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-testid="hub-new"]')).toHaveCount(0);
  });

  test("at 1000px the label collapses and the pill stays", async ({ page }) => {
    await page.setViewportSize({ width: 1000, height: 720 });
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await twoTerms(page, TERM_ID);
    await page.goto(`${BASE}/`);
    await expect(page.locator('[data-testid="door-badge-terminals"]')).toBeVisible({ timeout: 20_000 });
    const w = await page.locator(`${DOOR} .kb-door__label`).evaluate((el) => el.getBoundingClientRect().width);
    expect(w).toBeLessThanOrEqual(1);
  });

  for (const width of [861, 915, 1000]) {
    test(`six doors fit at ${width}px: no sideways page scroll, bar inside the viewport`, async ({ page }) => {
      await page.setViewportSize({ width, height: 720 });
      await installFakeTerminal(page, { total: 300, seedLines: 100 });
      await twoTerms(page, TERM_ID);
      await page.goto(`${BASE}/`);
      await expect(page.locator(DOOR)).toBeVisible({ timeout: 20_000 });
      const fit = await page.evaluate(() => ({
        inner: window.innerWidth,
        doc: document.documentElement.scrollWidth,
        right: Math.max(...[...document.querySelectorAll(".kb-doors .kb-door")].map((e) => e.getBoundingClientRect().right)),
      }));
      expect(fit.doc, "document scrollWidth").toBeLessThanOrEqual(fit.inner);
      expect(fit.right, "last door right edge").toBeLessThanOrEqual(fit.inner);
    });
  }
});

// HUBHOST - launching is an action of the hub: the New session button, the
// ?launch=1&resume=<sid> deep link (opens, never launches), the strip-once rule.
const SID = "sid-e2e-resume";

async function mockLaunch(page: Page): Promise<Array<Record<string, unknown>>> {
  const posted: Array<Record<string, unknown>> = [];
  await page.route("**/api/terms", (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    posted.push(JSON.parse(route.request().postData() ?? "{}") as Record<string, unknown>);
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ id: TERM_ID }) });
  });
  return posted;
}

async function mockSession(page: Page): Promise<void> {
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
        display_name: "Synthetic resume session",
        harness: "codex",
        cwd: "/synthetic/resume",
        checks: [],
        unnamed_edits: [],
        unnamed_edits_total: 0,
        memory_ids: [],
      }),
    }),
  );
}

const VIEWPORTS = [
  { name: "desktop", width: 1280, height: 720 },
  { name: "phone", width: 390, height: 844 },
];

test.describe("HUBHOST launcher in the hub", () => {
  for (const vp of VIEWPORTS) {
    test(`New session at ${vp.name}: confirm shows cwd and harness, one POST, lands on /terminals/<id>`, async ({ page }) => {
      await page.setViewportSize({ width: vp.width, height: vp.height });
      await installFakeTerminal(page, { total: 300, seedLines: 100 });
      const posted = await mockLaunch(page);
      await page.goto(`${BASE}/terminals`);
      const btn = page.locator('[data-testid="hub-new"]');
      await expect(btn).toBeVisible({ timeout: 20_000 });
      expect((await btn.boundingBox())!.height).toBeGreaterThanOrEqual(vp.name === "phone" ? 43 : 30);
      await btn.click();
      await expect(page.locator('[data-testid="launch-dialog"]')).toBeVisible();
      if (vp.name === "phone") await expect(page.locator("[data-kb-sheet]")).toHaveAttribute("data-detent", "full");
      await page.locator('[data-testid="launch-cwd"]').fill("/synthetic/new");
      await page.locator('[data-testid="launch-harness-claude"]').click();
      await page.locator('[data-testid="launch-review"]').click();
      await expect(page.locator('[data-testid="launch-confirm-cwd"]')).toContainText("/synthetic/new");
      await expect(page.locator('[data-testid="launch-confirm-harness"]')).toContainText("Claude");
      expect(posted).toEqual([]);
      await page.locator('[data-testid="launch-confirm"]').click();
      await expect.poll(() => posted.length).toBe(1);
      expect(posted[0]).toEqual({ harness: "claude", cwd: "/synthetic/new" });
      await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`), { timeout: 15_000 });
    });

    test(`deep link at ${vp.name}: opens prefilled from the session row, strips the URL, never launches, reload does not reopen`, async ({ page }) => {
      await page.setViewportSize({ width: vp.width, height: vp.height });
      await installFakeTerminal(page, { total: 300, seedLines: 100 });
      const posted = await mockLaunch(page);
      await mockSession(page);
      await page.goto(`${BASE}/terminals?launch=1&resume=${SID}&harness=grok&cwd=/evil`);
      await expect(page.locator('[data-testid="launch-dialog"]')).toBeVisible({ timeout: 20_000 });
      // launch/resume are consumed and `cwd` is not a hub param; `harness` is a
      // hub FILTER param so it is kept (and never reaches the dialog).
      await expect(page).toHaveURL(/\/terminals\?harness=grok$/);
      await expect(page.locator('[data-testid="launch-review"]')).toBeEnabled({ timeout: 15_000 });
      await page.locator('[data-testid="launch-review"]').click();
      await expect(page.locator('[data-testid="launch-confirm-cwd"]')).toContainText("/synthetic/resume");
      await expect(page.locator('[data-testid="launch-confirm-harness"]')).toContainText("Codex");
      expect(posted).toEqual([]);
      await page.reload();
      await expect(page.locator('[data-testid="hub-new"]')).toBeVisible({ timeout: 20_000 });
      await expect(page.locator('[data-testid="launch-dialog"]')).toHaveCount(0);
      expect(posted).toEqual([]);
    });
  }

  test("deep link with an unknown session shows the error and no launch path", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    const posted = await mockLaunch(page);
    await page.route("**/api/sessions/sid-e2e-unknown", (route) =>
      route.fulfill({ status: 404, contentType: "application/json", body: "{}" }),
    );
    await page.goto(`${BASE}/terminals?launch=1&resume=sid-e2e-unknown`);
    await expect(page.locator('[data-testid="launch-prefill-error"]')).toContainText("Unknown session", { timeout: 20_000 });
    await expect(page.locator('[data-testid="launch-review"]')).toBeDisabled();
    expect(posted).toEqual([]);
  });

  test("deep link with a failing session read is an error with Retry, not Unknown", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await page.route("**/api/sessions/sid-e2e-500", (route) =>
      route.fulfill({ status: 500, contentType: "application/json", body: "{}" }),
    );
    await page.goto(`${BASE}/terminals?launch=1&resume=sid-e2e-500`);
    const err = page.locator('[data-testid="launch-prefill-error"]');
    await expect(err).toContainText("Could not load that session", { timeout: 20_000 });
    await expect(err).not.toContainText("Unknown");
    await expect(page.locator('[data-testid="launch-prefill-retry"]')).toBeVisible();
  });

  test("a problem+json refusal from the launch route is shown by its title and detail", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await page.route("**/api/terms", (route) => {
      if (route.request().method() !== "POST") return route.fallback();
      return route.fulfill({
        status: 429,
        contentType: "application/problem+json",
        body: JSON.stringify({ type: "urn:example:refused", title: "Too many terminals", detail: "limit is 3" }),
      });
    });
    await page.goto(`${BASE}/terminals`);
    await page.locator('[data-testid="hub-new"]').click();
    await page.locator('[data-testid="launch-cwd"]').fill("/synthetic/new");
    await page.locator('[data-testid="launch-review"]').click();
    await page.locator('[data-testid="launch-confirm"]').click();
    await expect(page.locator('[data-testid="launch-error"]')).toHaveText("Too many terminals: limit is 3");
    await expect(page.locator('[data-testid="launch-confirm-view"]')).toBeVisible();
  });
});

// HUBPROMPT - the optional first prompt: confirm-view field, one-line rule, and
// the text never leaves the page (not in the POST body, not in the URL).
test.describe("HUBPROMPT first prompt in the launch flow", () => {
  for (const vp of VIEWPORTS) {
    test(`at ${vp.name}: a typed prompt is not sent to the server and the URL stays clean`, async ({ page }) => {
      await page.setViewportSize({ width: vp.width, height: vp.height });
      await installFakeTerminal(page, { total: 300, seedLines: 100 });
      const posted = await mockLaunch(page);
      const urls: string[] = [];
      page.on("request", (r) => urls.push(r.url() + (r.postData() ?? "")));
      await page.goto(`${BASE}/terminals`);
      await page.locator('[data-testid="hub-new"]').click();
      await page.locator('[data-testid="launch-cwd"]').fill("/synthetic/new");
      await page.locator('[data-testid="launch-review"]').click();
      await page.locator('[data-testid="launch-first-prompt"]').fill("echo synthetic-prompt-text");
      await expect(page.locator('[data-testid="launch-confirm-prompt"]')).toHaveText("First prompt: 26 characters");
      await expect(page.locator('[data-testid="launch-confirm-view"]')).not.toContainText("synthetic-prompt-text");
      await page.locator('[data-testid="launch-confirm"]').click();
      await expect.poll(() => posted.length).toBe(1);
      expect(Object.keys(posted[0]).sort()).toEqual(["cwd", "harness"]);
      await expect(page).toHaveURL(new RegExp(`/terminals/${TERM_ID}$`), { timeout: 15_000 });
      expect(urls.filter((u) => u.includes("synthetic-prompt-text"))).toEqual([]);
    });
  }

  test("a multi-line prompt shows the caption and disables Launch", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    const posted = await mockLaunch(page);
    await page.goto(`${BASE}/terminals`);
    await page.locator('[data-testid="hub-new"]').click();
    await page.locator('[data-testid="launch-cwd"]').fill("/synthetic/new");
    await page.locator('[data-testid="launch-review"]').click();
    await page.locator('[data-testid="launch-first-prompt"]').fill("first\nsecond");
    await expect(page.locator('[data-testid="launch-first-prompt-issue"]')).toContainText("One line only");
    await expect(page.locator('[data-testid="launch-confirm"]')).toBeDisabled();
    expect(posted).toEqual([]);
  });
});
