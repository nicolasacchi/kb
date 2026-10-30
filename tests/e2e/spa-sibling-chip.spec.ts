import { test, expect, type Page, type Route } from "@playwright/test";
import type { APIRequestContext } from "@playwright/test";
import { BASE } from "./helpers";

// OL-SIBL2 - the Header Code chip (`[data-kb-spine-slot=crossing]`) on the
// reader route, against the REAL kb daemon, SPA and artifact runtime (the
// live `kb:section` postMessage is the runtime's own). kb-code's doc-lens
// responses are `page.route()` fixtures with the exact-origin CORS headers
// Chromium enforces, same strategy as spa-coderefs.spec.ts; the 127.0.0.1:4747
// origin from global-setup's `[kb.canon] code_url` is never actually listened
// on. The inspector rail is collapsed in every test (the chip is hidden while
// the rail shows the Code section - one home).
//
// Not covered here: the kb -> kb-code -> Cited-by -> kb round trip (needs both
// SPAs in one harness; SIBL1's Cited-by href is pinned in kb-code's own tests).

const CODE_URL = "http://127.0.0.1:4747";

function codeLensOrigin(url: URL): boolean {
  return url.hostname === "127.0.0.1" && url.port === "4747";
}

async function fulfillJson(route: Route, json: unknown, status = 200) {
  await route.fulfill({
    status,
    headers: {
      "content-type": "application/json",
      "access-control-allow-origin": BASE,
      "access-control-allow-credentials": "true",
    },
    body: JSON.stringify(json),
  });
}

async function docByFilename(
  request: APIRequestContext,
  kb: string,
  filename: string,
): Promise<{ id: string; rel: string }> {
  const r = await request.get(`${BASE}/api/kb/${kb}/docs?limit=50`);
  expect(r.status(), `GET /api/kb/${kb}/docs`).toBe(200);
  const docs = (await r.json()) as { id: string; source_relative: string }[];
  const hit = docs.find((d) => d.source_relative.endsWith(filename));
  expect(hit, `${filename} indexed in ${kb}`).toBeTruthy();
  return { id: hit!.id, rel: hit!.source_relative };
}

function baseScorecard(overrides: Record<string, unknown> = {}) {
  return {
    schema: "codelens-scorecard/1",
    kb: "canon",
    doc_id: "placeholder",
    doc_hash: "abc123",
    doc_title: "Code refs demo",
    never_scanned: false,
    resolved_unix: Math.floor(Date.now() / 1000),
    pinned_repo: null,
    counted_refs: 2,
    truncated: false,
    repos: [
      {
        name: "app",
        root: "/tmp/app",
        state: "ready",
        head_sha: "abc1234567",
        head_branch: "main",
        dirty: false,
        present: 2,
        ambiguous: 0,
        absent: 0,
        external: 0,
        partial: false,
        reason: null,
      },
    ],
    note: "scorecard note",
    ...overrides,
  };
}

function baseLens(overrides: Record<string, unknown> = {}) {
  return {
    schema: "codelens/1",
    kb: "canon",
    doc_id: "placeholder",
    moved_from: null,
    doc_path: "code-refs-demo.html",
    doc_href: null,
    doc_hash: "abc123",
    doc_title: "Code refs demo",
    doc_extracted_at: Math.floor(Date.now() / 1000),
    doc_code_rev: null,
    never_scanned: false,
    repo: {
      name: "app",
      root: "/tmp/app",
      state: "ready",
      head_sha: "abc1234567",
      head_branch: "main",
      dirty: false,
      source: "param",
    },
    resolved_unix: Math.floor(Date.now() / 1000),
    truncated: false,
    partial: false,
    partial_reason: null,
    counts: {
      total: 2,
      resolved: 2,
      present: 2,
      ambiguous: 0,
      absent: 0,
      external: 0,
      confirmed: 2,
      drifted: 0,
      unverifiable: 0,
      line_absent: 0,
      declared_but_absent: 0,
    },
    ungrouped_count: 2,
    groups: [],
    refs: [
      resolvedRef({
        ordinal: 0,
        raw: "coderefs.rs",
        declared: true,
        path_hint: "crates/kb-core/src/coderefs.rs",
        line_hint: 42,
        resolved_path: "crates/kb-core/src/coderefs.rs",
        candidates: ["crates/kb-core/src/coderefs.rs"],
        resolved_line: 42,
        reader: { repo: "app", path: "crates/kb-core/src/coderefs.rs", line: 42 },
      }),
      resolvedRef({
        ordinal: 1,
        raw: "config.rs:120",
        path_hint: "crates/kb-core/src/config.rs",
        line_hint: 120,
        resolved_path: "crates/kb-core/src/config.rs",
        candidates: ["crates/kb-core/src/config.rs"],
        resolved_line: 120,
        reader: { repo: "app", path: "crates/kb-core/src/config.rs", line: 120 },
      }),
    ],
    note: "lens note",
    ...overrides,
  };
}

function resolvedRef(overrides: Record<string, unknown> = {}) {
  return {
    ordinal: 0,
    group: null,
    kind: "path_line",
    raw: "ref",
    declared: false,
    path_hint: null,
    line_hint: null,
    line_hint_end: null,
    symbol_container: null,
    symbol_member: null,
    context: "some context",
    path_state: "present",
    resolved_path: null,
    candidate_count: 1,
    candidates: [],
    issue: null,
    line_state: "confirmed",
    line_evidence: "context_token",
    confirm_token: "tok",
    token_line: 1,
    resolved_line: null,
    line_hint_delta: 0,
    file_lines: 200,
    line_reason: null,
    spans: [],
    symbol_state: "no_symbol",
    symbol_hit_count: 0,
    symbol_hits: [],
    reader: null,
    search: null,
    note: null,
    ...overrides,
  };
}


async function mockCodeLens(page: Page, docId: string) {
  await page.route(
    (url) => codeLensOrigin(url) && url.pathname === "/api/doc-lens/repos",
    (route) => fulfillJson(route, baseScorecard({ doc_id: docId })),
  );
  await page.route(
    (url) => codeLensOrigin(url) && url.pathname === "/api/doc-lens",
    (route) => fulfillJson(route, baseLens({ doc_id: docId })),
  );
}

const chip = (page: Page) =>
  page.locator('[data-kb-spine-slot="crossing"] [data-testid="sibling-code-chip"]');
const kindOf = (page: Page) =>
  page.locator('[data-kb-spine-slot="crossing"] [data-kb-sibling-kind]');

test.describe("kb Code chip (reader Header)", () => {
  test.beforeEach(async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.addInitScript(() => {
      localStorage.setItem("kb:inspector-collapsed.detail", "true");
    });
  });

  test("upgrades to verified and links to the kb-code reader at the ref's line", async ({ page, request }) => {
    const { id, rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    await mockCodeLens(page, id);
    await page.route(
      (url) => codeLensOrigin(url) && url.pathname.startsWith("/r/"),
      (route) => route.fulfill({ status: 200, contentType: "text/html", body: "<html><body>reader</body></html>" }),
    );
    await page.goto(`${BASE}/a/canon/${rel}?repo=app`);
    await expect(kindOf(page)).toHaveAttribute("data-kb-sibling-kind", "verified", { timeout: 15_000 });
    const href = `${CODE_URL}/r/app/crates/kb-core/src/coderefs.rs?line=42`;
    await expect(chip(page)).toHaveAttribute("href", href);
    const popupP = page.waitForEvent("popup");
    await chip(page).click();
    const popup = await popupP;
    await popup.waitForURL(href);
  });

  test("?sec= targets the ref of that group", async ({ page, request }) => {
    const { id, rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    const cr = await request.get(`${BASE}/api/kb/canon/docs/${id}/code-refs`);
    const body = (await cr.json()) as {
      groups: { key: string; anchor: string }[];
      refs: { group: string | null; path_hint: string | null }[];
    };
    expect(body.groups.length).toBeGreaterThan(0);
    const g = body.groups[0];
    const want = body.refs.find((r) => r.group === g.key)?.path_hint;
    expect(want, "a ref under the first group").toBeTruthy();
    await mockCodeLens(page, id);
    await page.goto(`${BASE}/a/canon/${rel}?repo=app&sec=${encodeURIComponent(g.anchor)}`);
    await expect(kindOf(page)).toHaveAttribute("data-kb-sibling-kind", "verified", { timeout: 15_000 });
    const href = await chip(page).getAttribute("href");
    expect(href).toContain(`/r/app/${want}`);
  });

  test("an unreachable kb-code never reads as verified", async ({ page, request }) => {
    const { rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    // No route handler: the request to the unlistened origin fails naturally.
    await page.goto(`${BASE}/a/canon/${rel}?repo=app`);
    await expect(kindOf(page)).toBeVisible({ timeout: 15_000 });
    await expect(kindOf(page)).not.toHaveAttribute("data-kb-sibling-kind", "verified");
    await expect(chip(page)).toHaveAttribute("title", /cannot check them against the code/);
  });
});
