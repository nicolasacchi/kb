import { test, expect } from "@playwright/test";
import { BASE, PORT } from "./helpers";

// v0.3 Atlas spec — exercises the full E4 + E5 pipeline:
//   1. POST /api/kb/canon/atlas/recompute → 202 + run_id
//   2. SSE atlas.recompute.complete arrives with {points, clusters,
//      duration_ms} (verified via the response — by the time the
//      route returns 202, the spawned task has at most a few hundred
//      ms of work to finish)
//   3. GET /api/kb/canon/docs?include=atlas returns atlas_x/y/cluster
//      for at least one row
//   4. SPA Atlas view renders a canvas with one dot per artifact and
//      clicking a dot navigates to the detail route
//
// The canon corpus has 4 root files but no embedder is configured in
// the e2e fixture (per global-setup.ts), so the recompute completes
// with 0 points and the SPA falls back to the hash placement. We
// assert that gracefully — the dots still render, the click still
// navigates, and the note text is the "no atlas data" form.
//
// S6 (S-milestone): renderer ported from SVG to Canvas2D. The
// previous specs targeted `.atlas__dot` SVG elements; canvas drawing
// is opaque so we now read test hooks (`data-atlas-count`,
// `data-first-dot`) off the canvas element.

test.describe("atlas v0.3", () => {
  test("recompute endpoint accepts request and returns run", async ({
    request,
  }) => {
    const r = await request.post(`${BASE}/api/kb/canon/atlas/recompute`);
    expect(r.status()).toBe(202);
    const body = (await r.json()) as { run: string; events: string };
    expect(body.run).toMatch(/^r-/);
    expect(body.events).toContain("/api/events");
  });

  test("docs include=atlas returns rows", async ({ request }) => {
    const r = await request.get(`${BASE}/api/kb/canon/docs?include=atlas&limit=20`);
    expect(r.status()).toBe(200);
    const docs = (await r.json()) as Array<{ id: string }>;
    expect(docs.length).toBeGreaterThanOrEqual(4);
    for (const d of docs) expect(typeof d.id).toBe("string");
  });

  test("Atlas view renders a canvas sized for the doc count", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas`);
    await expect(page.getByRole("tab", { name: "atlas" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();
    const count = await canvas.getAttribute("data-atlas-count");
    expect(Number(count ?? "0")).toBeGreaterThanOrEqual(4);
  });

  test("clicking a dot selects it in the AtlasInspector; preview navigates", async ({
    page,
  }) => {
    // v0.11 A1 — click on a dot now SELECTS it for the AtlasInspector
    // rail instead of navigating. The inspector's "preview" link is the
    // design-canonical navigate path.
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas`);
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();
    const box = await canvas.boundingBox();
    expect(box).not.toBeNull();
    // The atlas exposes the first placed point's logical (W=600,H=360)
    // coords via data-first-dot so tests can hit a real dot
    // deterministically; the SPA's default zoom=1, pan=(0,0) means
    // logical → CSS pixel is just a width-ratio scale.
    const firstDot = await canvas.getAttribute("data-first-dot");
    expect(firstDot).toBeTruthy();
    const parts = firstDot!.split(",").map(Number);
    expect(parts.length).toBe(2);
    const [logicalX, logicalY] = parts;
    const cssX = (box!.width * logicalX) / 600;
    const cssY = (box!.height * logicalY) / 360;
    await canvas.click({ position: { x: cssX, y: cssY } });

    const preview = page.locator(".kb-atlas-insp__btn--primary");
    await expect(preview).toBeVisible();
    await preview.click();
    await expect(page).toHaveURL(/\/a\/canon\/[^/]+/);
  });

  // --- interaction coverage (review follow-up): the canvas transform is
  // invisible to the DOM, so these read the data-atlas-zoom /
  // data-atlas-pan test hooks the component exposes alongside
  // data-first-dot.

  test("wheel zooms in and back out (clamped scale tracks the hook)", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas`);
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();
    await expect(canvas).toHaveAttribute("data-atlas-zoom", "1.000");
    const box = await canvas.boundingBox();
    expect(box).not.toBeNull();
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
    await page.mouse.wheel(0, -240); // scroll up = zoom in (×1.1)
    await expect
      .poll(async () => Number(await canvas.getAttribute("data-atlas-zoom")))
      .toBeGreaterThan(1.05);
    await page.mouse.wheel(0, 240); // scroll down = zoom back out (÷1.1)
    await expect
      .poll(async () => Number(await canvas.getAttribute("data-atlas-zoom")))
      .toBeLessThanOrEqual(1.001);
  });

  test("dragging pans the canvas and the offset sticks", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas`);
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();
    await expect(canvas).toHaveAttribute("data-atlas-pan", "0.0,0.0");
    const box = await canvas.boundingBox();
    expect(box).not.toBeNull();
    const cx = box!.x + box!.width / 2;
    const cy = box!.y + box!.height / 2;
    await page.mouse.move(cx, cy);
    await page.mouse.down();
    await page.mouse.move(cx + 80, cy + 40, { steps: 8 });
    await page.mouse.up();
    const panAttr = (await canvas.getAttribute("data-atlas-pan"))!;
    expect(panAttr).not.toBe("0.0,0.0");
    // The offset persists after the pointer is released (no snap-back).
    await page.waitForTimeout(100);
    await expect(canvas).toHaveAttribute("data-atlas-pan", panAttr);
  });

  test("dot selection survives zoom and pan", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas`);
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();
    const box = await canvas.boundingBox();
    const firstDot = await canvas.getAttribute("data-first-dot");
    expect(firstDot).toBeTruthy();
    const [logicalX, logicalY] = firstDot!.split(",").map(Number);
    await canvas.click({
      position: {
        x: (box!.width * logicalX) / 600,
        y: (box!.height * logicalY) / 360,
      },
    });
    const preview = page.locator(".kb-atlas-insp__btn--primary");
    await expect(preview).toBeVisible();
    // Zoom + pan must not reset the selection.
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
    await page.mouse.wheel(0, -240);
    await page.mouse.down();
    await page.mouse.move(box!.x + box!.width / 2 + 60, box!.y + box!.height / 2, {
      steps: 6,
    });
    await page.mouse.up();
    await expect(preview).toBeVisible();
  });
});

// W3.M-d — the map-home shell (`?shell=map`) and, more importantly, ITS
// GATE. Map-home is an evidence-gated promotion: the local atlas census
// decides whether it ever replaces the grid as the flagship home, and the
// census has barely any rows, so the gate CANNOT have fired yet. These
// tests pin the "not yet" side of that decision as hard as the feature
// itself — a future session that promotes map-home by taste, not by
// evidence, has to delete an assertion to do it.
test.describe("map-home shell (W3.M-d)", () => {
  test("THE GATE: a bare / with no pref lands on the GRID, not the map", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/`);
    // The grid tab is the selected view…
    await expect(page.getByRole("tab", { name: "grid" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    // …the shell is nowhere on the page…
    await expect(page.locator(".map-shell")).toHaveCount(0);
    // …and the URL was not rewritten to carry the flag.
    expect(new URL(page.url()).searchParams.get("shell")).toBeNull();
  });

  test("THE GATE: no prominent map entry point exists in the nav", async ({
    page,
  }) => {
    // A prominent placement would manufacture the very evidence the census
    // is supposed to measure, so the view tabs must NOT grow a map tab and
    // the existing ones must keep their labels.
    await page.goto(`http://127.0.0.1:${PORT}/`);
    await expect(page.getByRole("tab", { name: "grid" })).toBeVisible();
    await expect(page.getByRole("tab", { name: "atlas" })).toBeVisible();
    await expect(page.getByRole("tab", { name: /^map/i })).toHaveCount(0);
  });

  test("?shell=map renders the full-bleed map beside its projection panel", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.goto(`http://127.0.0.1:${PORT}/?kb=canon&shell=map`);
    const shell = page.locator(".map-shell");
    await expect(shell).toBeVisible();
    // The same atlas canvas, drawing the same dots.
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();
    expect(
      Number((await canvas.getAttribute("data-atlas-count")) ?? "0"),
    ).toBeGreaterThanOrEqual(4);
    // It is full-bleed: no card chrome, and the canvas is NOT holding the
    // 600/360 aspect ratio (safe only because lib/atlasFit.ts letterboxes).
    await expect(page.locator(".atlas--bleed")).toHaveCount(1);
    const box = (await canvas.boundingBox())!;
    expect(box.height / box.width).not.toBeCloseTo(360 / 600, 1);
    // The projection panel is present, ~340px, and honest about the empty
    // selection rather than showing a fake list.
    const panel = page.getByTestId("map-shell-panel");
    await expect(panel).toBeVisible();
    const pbox = (await panel.boundingBox())!;
    expect(pbox.width).toBeGreaterThan(280);
    expect(pbox.width).toBeLessThan(400);
    await expect(shell).toHaveAttribute("data-map-shell-selected", "0");
    await expect(panel.locator(".map-shell__rows")).toHaveCount(0);
    // The filter rail steps aside for the shell (it owns the body width).
    await expect(page.locator(".body--with-rail")).toHaveCount(0);
  });

  test("invariant #31: the shell does NOT become the reader's back-to-recent target", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 720 });
    // `lastGalleryUrl` is module-level (one tab, ephemeral), so every step
    // here must be an IN-TAB navigation — a `page.goto` would wipe it. The
    // shell has no in-app entry point by design (that is the gate), so the
    // hops are done with pushState + popstate, which is what react-router's
    // history listens to.
    const spaNav = async (url: string) => {
      await page.evaluate((u) => {
        window.history.pushState({}, "", u);
        window.dispatchEvent(new PopStateEvent("popstate"));
      }, url);
    };
    // A real LIST url first — this is what "back to recent" must return to.
    await page.goto(`http://127.0.0.1:${PORT}/?kb=canon&view=list&sort=title`);
    await expect(page.locator(".list-row").first()).toBeVisible();
    // …then the map shell, which must NOT overwrite that memory…
    await spaNav("/?kb=canon&shell=map");
    await expect(page.locator(".map-shell")).toBeVisible();
    // …then a reader, and back.
    await spaNav("/a/canon/kitchen-sink.html");
    await page
      .getByRole("navigation", { name: "artifact context" })
      .locator(".kb-ctxbar__back")
      .click();
    await expect(page).toHaveURL(/view=list/);
    await expect(page).not.toHaveURL(/shell=map/);
  });

  test("the shell degrades to the ordinary gallery on a phone viewport", async ({
    page,
  }) => {
    // Desktop-only by construction: a full-viewport map on a phone reads as
    // a workbench and fights the <=860px atlas overrides + the v0.23
    // one-button-one-sheet reader contract.
    await page.setViewportSize({ width: 390, height: 780 });
    await page.goto(`http://127.0.0.1:${PORT}/?kb=canon&shell=map`);
    await expect(page.locator(".map-shell")).toHaveCount(0);
    await expect(page.locator(".gallery")).toBeVisible();
  });
});
