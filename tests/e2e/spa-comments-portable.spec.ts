import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
import { PORT } from "./helpers";

// v0.19 — portable comment round-trip (borrowed from redline). The panel can
// bake the review state into a standalone copy of the artifact HTML
// ("Portable HTML" in the export modal) and read it back ("import…"). This
// spec exercises the full UI round-trip against a real daemon:
//
//   add ALPHA → export portable HTML (assert the embedded kb-review-state
//   block carries ALPHA) → add BETA → import the ALPHA-only file (overwrite,
//   confirmed) → panel shows ALPHA, BETA is gone.
//
// Uses cost-of-abstraction.html — a canon artifact no other comment spec
// touches — so the destructive overwrite-import can't perturb shared review
// state (the suite runs serially: workers:1, fullyParallel:false).

async function costDoc(
  request: import("@playwright/test").APIRequestContext,
): Promise<{ id: string; rel: string }> {
  const r = await request.get(
    `http://127.0.0.1:${PORT}/api/kb/canon/docs?limit=50`,
  );
  expect(r.status()).toBe(200);
  const docs = (await r.json()) as {
    id: string;
    path: string;
    source_relative: string;
  }[];
  const hit = docs.find((d) => d.path.endsWith("cost-of-abstraction.html"));
  expect(hit).toBeTruthy();
  return { id: hit!.id, rel: hit!.source_relative };
}

test.describe("portable comment export / import (v0.19)", () => {
  test("round-trips: export embeds the review, import restores it (replacing newer comments)", async ({
    page,
    request,
  }) => {
    const { rel } = await costDoc(request);
    const alpha = `alpha-${Date.now()}`;
    const beta = `beta-${Date.now()}`;

    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    // Open the comments panel (without arming the pencil) + add ALPHA.
    await page.locator('[data-kb-act="dock-comments"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();
    await panel.getByRole("textbox", { name: "file-scope comment" }).fill(alpha);
    await panel.getByRole("button", { name: "add file-scope" }).click();
    await expect(panel.getByText(alpha)).toBeVisible({ timeout: 5_000 });

    // Open the export modal → download the Portable HTML copy.
    await panel.locator(".cp__export-btn").first().click(); // "⤓ export…"
    const dialog = page.locator("dialog.cp__export");
    await expect(dialog).toBeVisible();

    const downloadPromise = page.waitForEvent("download");
    await dialog.getByRole("button", { name: /download \.html/ }).click();
    const download = await downloadPromise;
    expect(download.suggestedFilename()).toMatch(/\.review\.html$/);

    // The standalone copy carries the inert review block + ALPHA's body.
    const path = await download.path();
    const html = readFileSync(path, "utf-8");
    expect(html).toContain("kb-review-state");
    expect(html).toContain(alpha);

    // Close the modal, then add BETA so the live review now has ALPHA + BETA
    // while the exported file still holds only ALPHA.
    await dialog.getByRole("button", { name: "close" }).click();
    await expect(dialog).toBeHidden();
    await panel.getByRole("textbox", { name: "file-scope comment" }).fill(beta);
    await panel.getByRole("button", { name: "add file-scope" }).click();
    await expect(panel.getByText(beta)).toBeVisible({ timeout: 5_000 });

    // Import the ALPHA-only file. The artifact already has comments, so the
    // panel asks to confirm the overwrite via the ConfirmModal — accept it.
    await page
      .locator('[data-testid="comments-import-input"]')
      .setInputFiles(path);
    await page.locator("dialog.confirm .confirm__go").click();

    // After the import (comments.updated SSE → panel refetch), ALPHA is back
    // and BETA is gone — the file replaced the live review wholesale.
    await expect(panel.getByText(alpha)).toBeVisible({ timeout: 5_000 });
    await expect(panel.getByText(beta)).toHaveCount(0, { timeout: 5_000 });

    // Confirm against the API too: exactly the imported comment survives.
    const after = await request.get(
      `http://127.0.0.1:${PORT}/api/kb/canon/review/${(await costDoc(request)).id}`,
    );
    const file = (await after.json()) as { comments: { body: string }[] };
    expect(file.comments.some((c) => c.body === alpha)).toBe(true);
    expect(file.comments.some((c) => c.body === beta)).toBe(false);
  });
});
