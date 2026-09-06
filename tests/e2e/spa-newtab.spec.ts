import { test, expect } from "@playwright/test";
import { PORT, BASE, artifactUrlRe } from "./helpers";

// "Open in a new tab — inside kb." Two mechanisms:
//   1. Gallery browse surfaces (Cmd+K results, PreviewInspector folder
//      rows, atlas dots) open the artifact in a new kb tab on
//      ctrl/⌘-click.
//   2. A top-level artifact-subdomain load that was navigated to *from*
//      another artifact (the ctrl/middle-click case) is bounced by the
//      daemon-injected runtime to the kb SPA wrapper; a directly-typed /
//      externally-shared raw link is left raw.

async function firstDoc(
  request: import("@playwright/test").APIRequestContext,
): Promise<{ id: string; source_relative: string }> {
  const r = await request.get(`${BASE}/api/kb/canon/docs?limit=1`);
  expect(r.status()).toBe(200);
  const docs = (await r.json()) as Array<{
    id: string;
    source_relative: string;
  }>;
  return docs[0];
}

async function pmSiblings(
  request: import("@playwright/test").APIRequestContext,
): Promise<Array<{ source_relative: string; path: string }>> {
  const r = await request.get(`${BASE}/api/kb/canon/docs?limit=50`);
  expect(r.status()).toBe(200);
  const docs = (await r.json()) as Array<{
    folder: string;
    source_relative: string;
    path: string;
  }>;
  return docs.filter((d) => d.folder === "pm");
}

test.describe("open in a new tab (inside kb)", () => {
  test("Ctrl+click on a Cmd+K result opens the artifact in a new kb tab", async ({
    page,
    context,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/`);
    await page.getByRole("button", { name: /open search/i }).click();
    const dialog = page.getByRole("dialog", { name: "search" });
    await expect(dialog).toBeVisible();
    await page.keyboard.press("Tab"); // hybrid → keyword (no embedder 400)
    await page.getByRole("textbox", { name: "search query" }).fill("borrow");
    const opt = dialog.getByRole("option", { name: /Borrow Checker/ });
    await expect(opt).toBeVisible({ timeout: 5_000 });

    const [newPage] = await Promise.all([
      context.waitForEvent("page"),
      opt.click({ modifiers: ["ControlOrMeta"] }),
    ]);
    await newPage.waitForLoadState();
    await expect(newPage).toHaveURL(/\/a\/canon\/[^/]+\.html(\?|$)/);
    // The modified click must NOT close the palette in the original tab.
    await expect(dialog).toBeVisible();
  });

  test("Ctrl+click on a PreviewInspector folder row opens it in a new kb tab", async ({
    page,
    context,
    request,
  }) => {
    // Folder siblings moved from the retired FloatingPill popover into
    // PreviewInspector's Folder section (.kb-pinsp__folder-row). The
    // rows are `<Link>` anchors, so ctrl/⌘-click still opens a new tab.
    const pm = await pmSiblings(request);
    expect(pm.length).toBeGreaterThanOrEqual(2);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${pm[0].source_relative}`);
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();
    await expect(page.locator(".kb-pinsp__folder-list")).toBeVisible();

    const targetFilename = pm[1].path.split("/").pop()!;
    const row = page
      .locator(".kb-pinsp__folder-row")
      .filter({ hasText: targetFilename });
    const [newPage] = await Promise.all([
      context.waitForEvent("page"),
      row.click({ modifiers: ["ControlOrMeta"] }),
    ]);
    await newPage.waitForLoadState();
    await expect(newPage).toHaveURL(
      artifactUrlRe("canon", pm[1].source_relative),
    );
    // Original tab stays on pm[0].
    await expect(page).toHaveURL(artifactUrlRe("canon", pm[0].source_relative));
  });

  test("Ctrl+click on an atlas dot opens it in a new kb tab", async ({
    page,
    context,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/?view=atlas`);
    const canvas = page.locator(".atlas__canvas");
    await expect(canvas).toBeVisible();
    const box = await canvas.boundingBox();
    expect(box).not.toBeNull();
    const firstDot = await canvas.getAttribute("data-first-dot");
    expect(firstDot).toBeTruthy();
    const [logicalX, logicalY] = firstDot!.split(",").map(Number);
    const cssX = (box!.width * logicalX) / 600;
    const cssY = (box!.height * logicalY) / 360;

    const [newPage] = await Promise.all([
      context.waitForEvent("page"),
      canvas.click({
        position: { x: cssX, y: cssY },
        modifiers: ["ControlOrMeta"],
      }),
    ]);
    await newPage.waitForLoadState();
    await expect(newPage).toHaveURL(/\/a\/canon\/[^/]+/);
  });

  test("a top-level artifact load referred from an artifact bounces to the kb wrapper", async ({
    page,
    request,
  }) => {
    const doc = await firstDoc(request);
    const subOrigin = `http://${doc.id}.artifacts.localhost:${PORT}`;
    // Simulates ctrl/middle-click inside an artifact: the new tab loads
    // the subdomain URL with an artifact-subdomain referrer.
    await page.goto(`${subOrigin}/`, { referer: `${subOrigin}/` });
    await expect(page).toHaveURL(
      artifactUrlRe("canon", doc.source_relative),
    );
  });

  test("a directly-typed artifact subdomain stays raw (no bounce)", async ({
    page,
    request,
  }) => {
    const doc = await firstDoc(request);
    const subOrigin = `http://${doc.id}.artifacts.localhost:${PORT}`;
    // No referer → directly-typed / externally-shared link → left raw.
    await page.goto(`${subOrigin}/`);
    await expect(page).toHaveURL(
      new RegExp(`${doc.id}\\.artifacts\\.localhost`),
    );
    // The artifact body rendered in place (not the SPA shell).
    await expect(page.locator("h1, h2").first()).toBeVisible();
  });
});
