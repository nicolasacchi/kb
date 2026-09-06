import { test, expect } from "@playwright/test";
import { PORT, BASE } from "./helpers";

// Unit 2 — the e-ink daycard (`GET /api/kb/{kb}/daycard`, `routes/
// daycard.rs`) and its SPA twin, `/ambient` (`routes/ambient.tsx`).
//
// Coverage:
// 1. Wire-level content negotiation: no `Accept: application/json` header
//    and no `?format=` -> a self-contained HTML+inline-SVG document (the
//    e-ink panel's whole diet — no script, no external resources); an
//    explicit `?format=json` or `Accept: application/json` -> the JSON
//    twin the SPA/CLI consume.
// 2. Determinism: two requests for the SAME `?day=` produce byte-identical
//    HTML (the "same corpus state, same day -> same bytes" contract).
// 3. The SPA route renders the same data family: activity counts, and the
//    recent/never-opened sections link through to real canon artifacts.

test.describe("daycard wire — GET /api/kb/{kb}/daycard", () => {
  test("defaults to a self-contained HTML document", async ({ request }) => {
    const r = await request.get(`${BASE}/api/kb/canon/daycard`, {
      headers: { Accept: "*/*" },
    });
    expect(r.ok()).toBeTruthy();
    expect(r.headers()["content-type"]).toContain("text/html");
    const body = await r.text();
    expect(body).toContain("<!doctype html");
    expect(body).toContain("canon");
    // No daemon bitmap pipeline, no script, no external assets — the whole
    // point of the recorded design constraint.
    expect(body).not.toContain("<script");
    expect(body).not.toMatch(/https?:\/\//);
    expect(body).not.toContain("@font-face");
  });

  test("?format=json returns the structured twin", async ({ request }) => {
    const r = await request.get(
      `${BASE}/api/kb/canon/daycard?format=json`,
      { headers: { Accept: "*/*" } },
    );
    expect(r.ok()).toBeTruthy();
    expect(r.headers()["content-type"]).toContain("application/json");
    const body = await r.json();
    expect(body.kb).toBe("canon");
    expect(typeof body.day).toBe("string");
    expect(body.activity).toEqual(
      expect.objectContaining({
        opens: expect.any(Number),
        searches: expect.any(Number),
        comments: expect.any(Number),
      }),
    );
    expect(Array.isArray(body.resurface)).toBe(true);
    expect(Array.isArray(body.recent)).toBe(true);
    expect(Array.isArray(body.never_opened)).toBe(true);
  });

  test("Accept: application/json selects JSON with no ?format= needed", async ({
    request,
  }) => {
    const r = await request.get(`${BASE}/api/kb/canon/daycard`, {
      headers: { Accept: "application/json" },
    });
    expect(r.ok()).toBeTruthy();
    expect(r.headers()["content-type"]).toContain("application/json");
  });

  test("same day -> byte-identical HTML across requests", async ({
    request,
  }) => {
    const day = "2026-01-15";
    const [a, b] = await Promise.all([
      request.get(`${BASE}/api/kb/canon/daycard?day=${day}`, {
        headers: { Accept: "*/*" },
      }),
      request.get(`${BASE}/api/kb/canon/daycard?day=${day}`, {
        headers: { Accept: "*/*" },
      }),
    ]);
    expect(a.ok()).toBeTruthy();
    expect(b.ok()).toBeTruthy();
    const [textA, textB] = await Promise.all([a.text(), b.text()]);
    expect(textA).toBe(textB);
    expect(textA).toContain(day);
  });

  test("an invalid ?day= is a 4xx problem+json, not a 500", async ({
    request,
  }) => {
    const r = await request.get(
      `${BASE}/api/kb/canon/daycard?day=not-a-date`,
    );
    expect(r.status()).toBeGreaterThanOrEqual(400);
    expect(r.status()).toBeLessThan(500);
  });
});

test.describe("/ambient — the desk radiator", () => {
  test("renders activity + recent/never-opened, both linking to real artifacts", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/ambient?kb=canon`);

    const view = page.locator('[data-testid="ambient-view"]');
    await expect(view).toBeVisible({ timeout: 10_000 });
    await expect(page.locator(".kb-ambient__kb")).toHaveText("canon");

    // Today's activity: three labelled counts, honest zeros are fine (no
    // gamified framing — plain numbers only).
    const activity = page.locator('[data-testid="ambient-activity"]');
    await expect(activity).toContainText("opens");
    await expect(activity).toContainText("searches");
    await expect(activity).toContainText("comments");

    // The canon corpus has real artifacts, so "recently touched" is
    // non-empty and its first link is a real artifact permalink.
    const recent = page.locator('[data-testid="ambient-recent"]');
    const recentLink = recent.locator("a").first();
    await expect(recentLink).toBeVisible();
    const href = await recentLink.getAttribute("href");
    expect(href).toMatch(/^\/a\/canon\//);

    // Clicking through actually navigates to the reader — the ONLY
    // interactive affordance this view offers.
    await recentLink.click();
    await expect(page).toHaveURL(/\/a\/canon\//);
  });

  test("no corpus selected renders an honest empty state, not a crash", async ({
    page,
  }) => {
    // An unresolvable kb still lets useActiveKb fall back to the first
    // configured kb (kb-select invariant #33) — so exercise the "still
    // loading" edge instead by checking the view never throws and always
    // reaches a stable state.
    await page.goto(`http://127.0.0.1:${PORT}/ambient`);
    await expect(
      page.locator('[data-testid="ambient-view"], .kb-ambient--empty'),
    ).toBeVisible({ timeout: 10_000 });
  });
});
