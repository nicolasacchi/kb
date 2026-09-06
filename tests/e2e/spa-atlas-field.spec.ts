import { test, expect } from "@playwright/test";
import { BASE, PORT } from "./helpers";

// W3.F-c — the DUAL-FIELD atlas (the operator's own map, onion-skinned
// over the machine's) and the loci walk.
//
// The e2e fixture runs WITHOUT an embedder (see global-setup.ts), so the
// canon corpus draws through the hash placement — which is exactly what
// this spec needs: four dots at stable positions, no vectors required.
// The daemon half (`GET/PUT /api/kb/{kb}/atlas/field` +
// `/field/disagreement`) is live regardless; the sidecar lands in the
// temp corpus dir, so a placement written here never touches a real kb.
//
// These specs are SERIAL: the field is ONE per-kb sidecar and the place
// test writes it, so running them in parallel would race over the same
// file. (Everything else in the suite is read-only against canon.)
test.describe.configure({ mode: "serial" });

test.describe("atlas — the operator field", () => {
  test("the field route answers with an empty JSON Canvas doc", async ({
    request,
  }) => {
    const r = await request.get(`${BASE}/api/kb/canon/atlas/field`);
    expect(r.ok()).toBeTruthy();
    const body = await r.json();
    expect(Array.isArray(body.nodes)).toBeTruthy();

    const d = await request.get(
      `${BASE}/api/kb/canon/atlas/field/disagreement`,
    );
    expect(d.ok()).toBeTruthy();
    const dis = await d.json();
    expect(Array.isArray(dis.disagreements)).toBeTruthy();
    // Nothing joined yet is a NUMBER, not a 404 — the honest empty state.
    expect(typeof dis.matched).toBe("number");
  });

  test("the overlay is off by default and opens to an honest empty state", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas`);
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();
    // Default OFF: no ghosts, no heat, no place mode.
    await expect(canvas).toHaveAttribute("data-atlas-field-ghosts", "0");
    await expect(canvas).toHaveAttribute("data-atlas-field-heat", "0");
    await expect(page.locator(".atlas-field")).toHaveCount(0);

    await page.locator('[data-kb-act="atlas-field"]').click();
    const bar = page.locator(".atlas-field");
    await expect(bar).toBeVisible();
    // A kb nobody has hand-placed says so, rather than drawing nothing in
    // silence.
    await expect(
      page.getByTestId("atlas-field-empty").or(bar.locator(".atlas-field__note")),
    ).toBeVisible();

    // Heat is a sub-mode of the overlay and flips the canvas hook.
    await page.locator('[data-kb-act="atlas-field-heat"]').click();
    await expect(canvas).toHaveAttribute("data-atlas-field-heat", "1");
  });

  test("place mode drags a ghost, writes the sidecar, and never moves the machine layout", async ({
    page,
    request,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas`);
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();
    const before = await canvas.getAttribute("data-first-dot");
    expect(before).toBeTruthy();

    await page.locator('[data-kb-act="atlas-field"]').click();
    await page.locator('[data-kb-act="atlas-field-place"]').click();
    await expect(canvas).toHaveAttribute("data-atlas-field-place", "1");

    // Logical (600×360) → CSS pixels: default zoom 1 / pan 0, same math
    // spa-atlas.spec.ts uses for its dot clicks. `page.mouse` coordinates
    // are VIEWPORT-relative and the field bar pushes the canvas down past
    // 720px, so scroll it into view first and take the box after.
    await canvas.scrollIntoViewIfNeeded();
    const box = (await canvas.boundingBox())!;
    const [lx, ly] = before!.split(",").map(Number);
    const fromX = box.x + (box.width * lx) / 600;
    const fromY = box.y + (box.height * ly) / 360;
    await page.mouse.move(fromX, fromY);
    await page.mouse.down();
    await page.mouse.move(fromX + 90, fromY + 50, { steps: 10 });
    await page.mouse.up();

    // One ghost is drawn for the placement...
    await expect
      .poll(async () =>
        Number(await canvas.getAttribute("data-atlas-field-ghosts")),
      )
      .toBeGreaterThanOrEqual(1);
    // ...the MACHINE coordinate is untouched (read-only, by design)...
    await expect(canvas).toHaveAttribute("data-first-dot", before!);

    // ...and the debounced PUT lands a `file` node in the sidecar.
    await expect
      .poll(
        async () => {
          const r = await request.get(`${BASE}/api/kb/canon/atlas/field`);
          if (!r.ok()) return 0;
          const doc = await r.json();
          return (doc.nodes ?? []).filter(
            (n: { type?: string }) => n.type === "file",
          ).length;
        },
        { timeout: 10_000 },
      )
      .toBeGreaterThanOrEqual(1);

    // Clean up so a re-run starts from the same empty field.
    const put = await request.put(`${BASE}/api/kb/canon/atlas/field`, {
      data: { nodes: [], edges: [] },
    });
    expect(put.ok()).toBeTruthy();
  });

  test("the loci walk offers a list and records nothing", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas`);
    await expect(page.locator(".atlas__canvas")).toBeVisible();
    await page.locator('[data-kb-act="atlas-tour"]').click();
    const tour = page.getByTestId("atlas-tour");
    await expect(tour).toBeVisible();
    // The picker is the whole entry point: a walk IS a reading list.
    await expect(tour.locator('[data-kb-act="atlas-tour-list"]')).toBeVisible();
    // No progress/percentage/completion chrome anywhere in the bar.
    await expect(tour).not.toContainText("%");
    await expect(tour).toContainText("reading list");
  });
});
