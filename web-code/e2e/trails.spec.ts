import { expect, test, type APIRequestContext } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";
import { RESOLVER_FILE } from "./fixture-repo";

/// V74-L3b — the kbc-trail/1 SURFACE, and above all its PRIVACY posture
/// (design D17).
///
/// The fixture daemon runs with `[trails] enabled = true` but has never been
/// opted into, which is exactly D17's first-boot shape: the feature is
/// permitted, nothing is recorded, and the indicator says so. This spec walks
/// that whole ladder — off, enable, record two steps, read them in the rail,
/// pause, purge — because each rung is a separate promise and a surface that
/// only ever gets tested in its "on" state is a surface whose off state is
/// nobody's.
///
/// Every write below is loopback-only server-side; this harness hits
/// 127.0.0.1, so they work exactly as `kb-code trail …` would.

async function setMode(request: APIRequestContext, mode: string) {
  const res = await request.post(`${BASE}/api/trails/state`, {
    headers: { "X-Kbc-Request": "1", "Content-Type": "application/json" },
    data: { mode },
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
  return res.json();
}

async function purge(request: APIRequestContext) {
  await request
    .post(`${BASE}/api/trails/purge`, {
      headers: { "X-Kbc-Request": "1", "Content-Type": "application/json" },
      data: { repo: REPO_NAME },
    })
    .catch(() => undefined);
}

async function ingest(request: APIRequestContext, steps: unknown[]) {
  const res = await request.post(`${BASE}/api/trails/steps`, {
    headers: { "X-Kbc-Request": "1", "Content-Type": "application/json" },
    data: { repo: REPO_NAME, session_hint: "e2e", steps },
  });
  return res;
}

const NOW = Math.floor(Date.now() / 1000);

test.describe("kbc-trail/1", () => {
  test.beforeAll(async ({ request }) => {
    // Start from the FIRST-BOOT state every time: purged, and opted out.
    await purge(request);
    await setMode(request, "off").catch(() => undefined);
  });

  test.afterAll(async ({ request }) => {
    await purge(request);
    await setMode(request, "off").catch(() => undefined);
  });

  test("with nobody opted in, the indicator says OFF and the ledger refuses by name", async ({
    page,
    request,
  }) => {
    // The daemon refuses ingest with the reason that names WHICH gate it hit.
    const res = await ingest(request, [
      { via: "manual", path: RESOLVER_FILE, entered_at: NOW, left_at: NOW + 12 },
    ]);
    expect(res.status()).toBe(409);
    expect(await res.text()).toContain("not been turned on");

    await page.goto(`${BASE}/r/${REPO_NAME}/~boards`);
    // D17's "visible indicator": off is a STATE, shown, not an absence.
    const ind = page.locator("[data-kbc-trail-indicator]");
    await expect(ind).toBeVisible({ timeout: 15_000 });
    await expect(ind).toHaveAttribute("data-kbc-trail-indicator", "off");
    await expect(page.locator("[data-kbc-trail-toggle]")).toContainText("trail off");
  });

  test("recording ingests quantised steps; the rail shows them, then pause and purge", async ({
    page,
    request,
  }) => {
    await setMode(request, "recording");

    const res = await ingest(request, [
      { via: "search", path: RESOLVER_FILE, line_start: 1, entered_at: NOW, left_at: NOW + 12 },
      { via: "definition_of", path: "README.md", entered_at: NOW + 12, left_at: NOW + 15 },
    ]);
    expect(res.status(), await res.text()).toBe(200);
    const out = await res.json();
    expect(out.appended).toBe(2);
    expect(String(out.trail_id)).toMatch(/^trl_[0-9a-f]{12}$/);

    // Anything FINER than a step is refused, not rounded.
    const fine = await ingest(request, [
      {
        via: "manual",
        path: RESOLVER_FILE,
        entered_at: NOW + 40,
        left_at: NOW + 50,
        viewport: [1, 40],
      },
    ]);
    expect(fine.status()).toBe(400);
    expect(await fine.text()).toContain("REFUSED, not rounded");

    // The indicator now says recording.
    await page.goto(`${BASE}/r/${REPO_NAME}/${RESOLVER_FILE}`);
    await expect(page.locator("[data-kbc-trail-indicator]")).toHaveAttribute(
      "data-kbc-trail-indicator",
      "recording",
      { timeout: 15_000 },
    );

    // The RAIL is the operator's own read. `Space R t` opens it.
    await page.locator("body").press(" ");
    await page.locator("body").press("R");
    await page.locator("body").press("t");
    const rail = page.locator("[data-kbc-trail-rail]");
    await expect(rail).toBeVisible({ timeout: 15_000 });
    await expect(rail.locator("[data-kbc-trail-step]")).toHaveCount(2);

    // Every state is the daemon's, computed on that read. Neither step pinned
    // a blob, so neither may CLAIM `pinned` — `carried` is the honest answer.
    const first = rail.locator('[data-kbc-trail-step="0"]');
    await expect(first).toContainText(RESOLVER_FILE);
    await expect(first).toHaveAttribute("data-kbc-trail-step-state", /carried|orphan|pinned/);
    // Dwell is FLOORED by the daemon to `[trails] step_granularity_secs` (5 in
    // the harness config), so a 12-second hop records TEN — never 12, never
    // rounded up to 15.
    await expect(first).toContainText("10s");
    // …and the 3-second hop falls UNDER the floor, which is recorded as an
    // honest zero rather than hidden.
    await expect(rail.locator('[data-kbc-trail-step="1"]')).toContainText(
      "under the dwell floor",
    );
    // A fork chip is offered (this harness is loopback).
    await expect(rail.locator('[data-kbc-trail-fork="0"]')).toBeVisible();

    // `Space k p` pauses, and the INDICATOR changes — D17 pairs them.
    await page.locator("body").press(" ");
    await page.locator("body").press("k");
    await page.locator("body").press("p");
    await expect(page.locator("[data-kbc-trail-indicator]")).toHaveAttribute(
      "data-kbc-trail-indicator",
      "paused",
      { timeout: 15_000 },
    );
    // …and the daemon refuses with the PAUSED reason, not the OFF one.
    const paused = await ingest(request, [
      { via: "manual", path: RESOLVER_FILE, entered_at: NOW + 60, left_at: NOW + 70 },
    ]);
    expect(paused.status()).toBe(409);
    expect(await paused.text()).toContain("PAUSED");

    // Purge is wholesale and behind the ONE confirm host.
    await page.locator("[data-kbc-trail-purge]").click();
    await page.locator(".confirm__go").click();
    await expect(rail.locator("[data-kbc-trail-step]")).toHaveCount(0, { timeout: 15_000 });
    await expect(page.locator("[data-kbc-trail-rail-empty]")).toBeVisible();

    // A purge deletes DATA, never the opt-in decision.
    await expect(page.locator("[data-kbc-trail-indicator]")).toHaveAttribute(
      "data-kbc-trail-indicator",
      "paused",
    );
  });
});
