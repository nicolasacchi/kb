import { test, expect } from "@playwright/test";
import { BASE } from "./helpers";

// ARTIFACT HOST GRAMMAR v2 (docs/architecture-invariants.md #7) — the
// "hermes-wins" collision regression. global-setup.ts seeds a root
// `index.html` into BOTH the `mem` and `nocode` corpora with distinct
// sentinel bodies; `ArtifactId::from_path` hashes only the source-relative
// path (invariant #27), so the two docs' RAW 12-hex artifact ids collide —
// same id, two different kbs. Pre-v2 the daemon's bare-id subdomain walk
// (alphabetically first kb wins) would have silently served ONE corpus's
// root page to both. The SPA now emits QUALIFIED `{kb_enc}--{id}` iframe
// origins (`artifactOrigin`, `web/src/lib/artifactHost.ts`), so the two
// collide-on-id docs resolve to DISTINCT browser origins and each iframe
// renders its OWN kb's content.

async function docIdByFilename(
  request: import("@playwright/test").APIRequestContext,
  kb: string,
  filename: string,
): Promise<{ id: string; rel: string }> {
  const r = await request.get(`${BASE}/api/kb/${kb}/docs?limit=50`);
  expect(r.status(), `GET /api/kb/${kb}/docs`).toBe(200);
  const docs = (await r.json()) as { id: string; source_relative: string }[];
  const hit = docs.find((d) => d.source_relative === filename);
  expect(hit, `${filename} indexed in ${kb}`).toBeTruthy();
  return { id: hit!.id, rel: hit!.source_relative };
}

function iframeHostname(src: string): string {
  return new URL(src).hostname;
}

test.describe("artifact host grammar v2 — cross-kb id collision", () => {
  test("mem's and nocode's colliding-id root index.html get distinct qualified origins", async ({
    page,
    request,
  }) => {
    const mem = await docIdByFilename(request, "mem", "index.html");
    const nocode = await docIdByFilename(request, "nocode", "index.html");
    // The actual collision this whole grammar exists to disambiguate: same
    // source-relative path ("index.html") in two different kbs hashes to
    // the SAME raw artifact id (invariant #27).
    expect(
      mem.id,
      "mem/index.html and nocode/index.html must collide on id",
    ).toBe(nocode.id);

    // (a) mem — the iframe's CONTENT is mem's own sentinel, and its host
    // label carries the qualified `mem--` prefix (not the bare colliding id).
    await page.goto(`${BASE}/a/mem/index.html`);
    const memFrame = page.frameLocator(".detail__frame");
    await expect(memFrame.getByText("MEM-ROOT-SENTINEL-f3a1c8")).toBeVisible({
      timeout: 10_000,
    });
    const memSrc = await page
      .locator("iframe.detail__frame")
      .getAttribute("src");
    expect(memSrc).toBeTruthy();
    const memHost = iframeHostname(memSrc!);
    expect(memHost.startsWith(`mem--${mem.id}`)).toBe(true);

    // (b) nocode — same shape, the OTHER corpus's sentinel + `nocode--` prefix.
    await page.goto(`${BASE}/a/nocode/index.html`);
    const nocodeFrame = page.frameLocator(".detail__frame");
    await expect(
      nocodeFrame.getByText("NOCODE-ROOT-SENTINEL-9d2e47"),
    ).toBeVisible({ timeout: 10_000 });
    const nocodeSrc = await page
      .locator("iframe.detail__frame")
      .getAttribute("src");
    expect(nocodeSrc).toBeTruthy();
    const nocodeHost = iframeHostname(nocodeSrc!);
    expect(nocodeHost.startsWith(`nocode--${nocode.id}`)).toBe(true);

    // (c) despite the colliding raw id, the two iframes never share an
    // origin — the exact cross-origin-postMessage trust boundary
    // `isOriginOfArtifact` (invariant #7/#20) depends on.
    expect(memHost).not.toBe(nocodeHost);
  });
});
