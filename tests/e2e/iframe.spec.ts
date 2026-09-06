import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

/**
 * Mirrors spike-iframe's 16/16 PASS pattern: 4 canon artifacts × 4 probe
 * checks each. Validates the cluster-1 sandbox model end-to-end against
 * a real Chromium instance.
 *
 * The probe script is served at `/_kb/probe.js` on each artifact subdomain
 * and tests:
 *   - localStorage write (allow-same-origin via subdomain isolation)
 *   - parent.document access throws SecurityError (cross-origin block)
 *
 * The sandbox-removal-attack and CORS-fetch-block checks from spike-iframe
 * are deferred — they require the parent page to mount the iframe with
 * the production sandbox attribute, which is SPA scope (v0.1+).
 */

const ARTIFACT_IDS = [
  "fullscreen-viz",
  "kitchen-sink",
  "multi-page",
  "cost-of-abstraction",
];

function port(): number {
  return PORT;
}

function parentHtml(port: number): string {
  const iframes = ARTIFACT_IDS.map(
    (id) =>
      `<iframe id="iframe-${id}" src="http://${id}.artifacts.localhost:${port}/" sandbox="allow-scripts allow-same-origin"></iframe>`,
  ).join("\n");
  return `<!doctype html>
<html><head><meta charset="utf-8"><title>kb iframe smoke</title></head>
<body>
  <h1>kb iframe smoke</h1>
  ${iframes}
  <pre id="log"></pre>
  <script>
    window.__probeResults = [];
    window.addEventListener("message", (ev) => {
      if (!ev.data || ev.data.kind !== "kb-probe") return;
      window.__probeResults.push(ev.data);
      const log = document.getElementById("log");
      if (log) log.textContent += JSON.stringify(ev.data) + "\\n";
    });
  </script>
</body></html>`;
}

test.describe("iframe sandbox", () => {
  test("each canon artifact serves its HTML with X-Kb-Artifact-Id header", async ({
    request,
  }) => {
    const p = port();
    for (const id of ARTIFACT_IDS) {
      const resp = await request.get(`http://127.0.0.1:${p}/`, {
        headers: { Host: `${id}.artifacts.localhost:${p}` },
      });
      expect(resp.status(), `serving ${id}`).toBe(200);
      expect(resp.headers()["x-kb-artifact-id"]).toBe(id);
      const ct = resp.headers()["content-type"] ?? "";
      expect(ct).toContain("text/html");
      const body = await resp.text();
      expect(body).toContain(`<script src="/_kb/probe.js"`);
    }
  });

  test("each subdomain serves the probe script", async ({ request }) => {
    const p = port();
    for (const id of ARTIFACT_IDS) {
      const resp = await request.get(`http://127.0.0.1:${p}/_kb/probe.js`, {
        headers: { Host: `${id}.artifacts.localhost:${p}` },
      });
      expect(resp.status(), `probe for ${id}`).toBe(200);
      const body = await resp.text();
      expect(body).toContain("postMessage");
    }
  });

  test("probes run inside iframes and report back to parent", async ({
    page,
  }) => {
    const p = port();

    // Park the page on a kb-served URL so the parent origin matches what
    // CORS / origin-allowlist rules expect. We use an arbitrary 404 path
    // and inject our test HTML via setContent — the URL stays parent-side.
    await page.goto(`http://127.0.0.1:${p}/__e2e_parent_park`, {
      waitUntil: "load",
    }).catch(() => {
      // 404 is fine — we just need the URL to anchor the origin.
    });
    await page.setContent(parentHtml(p), { waitUntil: "load" });

    // Wait for all 4 iframes × 2 probes = 8 messages.
    await page.waitForFunction(
      (expected) =>
        Array.isArray((window as any).__probeResults) &&
        (window as any).__probeResults.length >= expected,
      ARTIFACT_IDS.length * 2,
      { timeout: 15_000 },
    );

    const results: Array<{
      origin: string;
      check: string;
      ok: boolean;
      detail: string;
    }> = await page.evaluate(() => (window as any).__probeResults);

    for (const id of ARTIFACT_IDS) {
      const expectedOrigin = `http://${id}.artifacts.localhost:${p}`;
      const localStorageProbe = results.find(
        (r) =>
          r.origin === expectedOrigin && r.check === "localStorage",
      );
      const crossOriginProbe = results.find(
        (r) =>
          r.origin === expectedOrigin &&
          r.check === "cross_origin_parent_doc_access",
      );
      expect(localStorageProbe, `localStorage for ${id}`).toBeTruthy();
      expect(localStorageProbe!.ok, `localStorage for ${id}`).toBe(true);
      expect(crossOriginProbe, `cross-origin probe for ${id}`).toBeTruthy();
      expect(crossOriginProbe!.ok, `cross-origin block for ${id}`).toBe(true);
    }
  });
});
