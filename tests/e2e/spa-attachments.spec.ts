import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// Y-track — comment attachment round-trip in the SPA:
//   open the comments panel → stage an image in the file-scope composer
//   (the hidden file input) → the markdown ref token lands in the draft +
//   a staged chip shows → post → the new comment renders the inline embed
//   (CommentBody's attachment: → <img class=cp__att-embed>) AND the strip
//   thumbnail (AttachmentStrip), and the served bytes decode as an image
//   (naturalWidth > 0 — i.e. GET …/attachments/{aid} returned the PNG).
//
// A 1×1 transparent PNG: valid magic bytes, so the daemon's sniff_allowed
// classifies it image/png and serves it inline.
const PNG_1x1 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

async function pickArtifact(
  request: import("@playwright/test").APIRequestContext,
): Promise<{ rel: string }> {
  const r = await request.get(
    `http://127.0.0.1:${PORT}/api/kb/canon/docs?limit=20`,
  );
  expect(r.status()).toBe(200);
  const docs = (await r.json()) as { path: string; source_relative: string }[];
  const ks = docs.find((d) => d.path.endsWith("kitchen-sink.html"));
  return { rel: (ks ?? docs[0]).source_relative };
}

test.describe("comment attachments", () => {
  test("stage an image at compose → token + chip → strip + inline embed → bytes load", async ({
    page,
    request,
  }) => {
    const { rel } = await pickArtifact(request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await page.locator('[data-kb-act="dock-comments"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();

    const name = `shot-${Date.now()}.png`;
    // The file-scope composer's (hidden) multi-file picker.
    await panel
      .locator('input[data-testid="attach-input"]')
      .first()
      .setInputFiles({
        name,
        mimeType: "image/png",
        buffer: Buffer.from(PNG_1x1, "base64"),
      });

    // On stage: the inline ref token lands in the draft (read via the value
    // mirror — CM6's contenteditable has no `.value`) + a staged chip shows.
    const ta = panel.locator(".cp__file-scope-input");
    await expect(ta).toHaveValue(
      new RegExp(`!\\[${name}\\]\\(attachment:a_`),
      { timeout: 10_000 },
    );
    await expect(panel.locator(".cp__attach-chip.is-staged")).toBeVisible();

    // Post the comment (the daemon adopts the staged blob).
    await panel.getByRole("button", { name: "add file-scope" }).click();

    // The inline embed + the strip thumbnail both render…
    const embed = panel.locator("img.cp__att-embed").first();
    await expect(embed).toBeVisible({ timeout: 10_000 });
    await expect(panel.locator("img.cp__att-thumb").first()).toBeVisible();

    // …and the served bytes decode as an image (proves the attachment serve
    // route returned the PNG, not a 404).
    await expect
      .poll(
        async () =>
          embed.evaluate((el) => (el as HTMLImageElement).naturalWidth),
        { timeout: 10_000 },
      )
      .toBeGreaterThan(0);
  });
});
