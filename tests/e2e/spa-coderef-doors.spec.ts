import {
  test,
  expect,
  type BrowserContext,
  type FrameLocator,
  type Page,
  type Route,
} from "@playwright/test";
import type { APIRequestContext } from "@playwright/test";
import { BASE } from "./helpers";

// OL IDEA6 - the citation is the door. A <code> citation inside an artifact
// becomes a link into kb-code at that ref: the parent SPA posts the doors into
// the frame, the artifact runtime decorates the matching elements and opens a
// door from the click itself. The fixture is `code-refs-demo-doors.html`,
// written by global-setup.ts from the golden's reader-fixture case, so the
// ordinals asserted here are the ones the Rust extractor mints:
//   0 config.rs:120   1 config.rs:120   2 coderefs.rs#L42 (declared)
//   3 linked.rs:5 (inside an <a>)   4 listing token   5 config.rs:120
// and the decorated set is [0, 1, 2, 5].
//
// kb-code is never run: its doc-lens answers are page.route() fixtures (as in
// spa-coderefs.spec.ts) and its reader/search destinations are context.route()
// stub pages, so the spec reads the TAB URL a click opens.

const CODE_URL = "http://127.0.0.1:4747";
const CFG = "crates/kb-core/src/config.rs";
const HINT_Q = "%23crates%2Fkb-core%2Fsrc%2Fconfig.rs";

function codeOrigin(url: URL): boolean {
  return url.hostname === "127.0.0.1" && url.port === "4747";
}

async function fulfillJson(route: Route, json: unknown) {
  await route.fulfill({
    status: 200,
    headers: {
      "content-type": "application/json",
      "access-control-allow-origin": BASE,
      "access-control-allow-credentials": "true",
    },
    body: JSON.stringify(json),
  });
}

/// The doors' destinations: only the reader and search paths of kb-code are
/// answered (with a stub page); every other request to it fails naturally.
async function stubDestinations(context: BrowserContext) {
  await context.route(
    (url) => codeOrigin(url) && (url.pathname === "/search" || url.pathname.startsWith("/r/")),
    (route) =>
      route.fulfill({ status: 200, contentType: "text/html", body: "<!doctype html><title>kb-code</title>" }),
  );
}

async function docRel(request: APIRequestContext, kb: string, filename: string) {
  const r = await request.get(`${BASE}/api/kb/${kb}/docs?limit=50`);
  const docs = (await r.json()) as { id: string; source_relative: string }[];
  const hit = docs.find((d) => d.source_relative.endsWith(filename));
  expect(hit, `${filename} indexed in ${kb}`).toBeTruthy();
  return { id: hit!.id, rel: hit!.source_relative };
}

function resolvedRef(o: Record<string, unknown>) {
  return {
    ordinal: 0,
    group: null,
    kind: "path_line",
    raw: "",
    declared: false,
    path_hint: null,
    line_hint: null,
    line_hint_end: null,
    symbol_container: null,
    symbol_member: null,
    context: "ctx",
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
    file_lines: 400,
    line_reason: null,
    spans: [],
    symbol_state: "no_symbol",
    symbol_hit_count: 0,
    symbol_hits: [],
    reader: null,
    search: null,
    note: null,
    ...o,
  };
}

async function mockVerified(page: Page, docId: string) {
  const now = Math.floor(Date.now() / 1000);
  await page.route(
    (url) => codeOrigin(url) && url.pathname === "/api/doc-lens/repos",
    (route) =>
      fulfillJson(route, {
        schema: "codelens-scorecard/1",
        kb: "canon",
        doc_id: docId,
        doc_hash: "h",
        doc_title: "Code refs demo doors",
        never_scanned: false,
        resolved_unix: now,
        pinned_repo: "app",
        counted_refs: 6,
        truncated: false,
        repos: [
          {
            name: "app",
            root: "/tmp/app",
            state: "ready",
            head_sha: "abc1234567",
            head_branch: "main",
            dirty: false,
            present: 3,
            ambiguous: 0,
            absent: 1,
            external: 0,
            partial: false,
            reason: null,
          },
        ],
        note: "n",
      }),
  );
  await page.route(
    (url) => codeOrigin(url) && url.pathname === "/api/doc-lens",
    (route) =>
      fulfillJson(route, {
        schema: "codelens/1",
        kb: "canon",
        doc_id: docId,
        moved_from: null,
        doc_path: "code-refs-demo-doors.html",
        doc_href: null,
        doc_hash: "h",
        doc_title: "Code refs demo doors",
        doc_extracted_at: now,
        doc_code_rev: null,
        never_scanned: false,
        repo: {
          name: "app",
          root: "/tmp/app",
          state: "ready",
          head_sha: "abc1234567",
          head_branch: "main",
          dirty: false,
          source: "pin",
        },
        resolved_unix: now,
        truncated: false,
        partial: false,
        partial_reason: null,
        counts: {
          total: 3,
          resolved: 3,
          present: 2,
          ambiguous: 0,
          absent: 1,
          external: 0,
          confirmed: 1,
          drifted: 1,
          unverifiable: 0,
          line_absent: 0,
          declared_but_absent: 1,
        },
        ungrouped_count: 3,
        groups: [],
        refs: [
          resolvedRef({
            ordinal: 0,
            raw: `${CFG}:120`,
            path_hint: CFG,
            line_hint: 120,
            resolved_line: 120,
            reader: { repo: "app", path: CFG, line: 120 },
          }),
          resolvedRef({
            ordinal: 1,
            raw: `${CFG}:120`,
            path_hint: CFG,
            line_hint: 120,
            line_state: "drifted",
            resolved_line: 131,
            reader: { repo: "app", path: CFG, line: 131 },
          }),
          resolvedRef({
            ordinal: 2,
            raw: "crates/kb-core/src/coderefs.rs#L42",
            declared: true,
            path_hint: "crates/kb-core/src/coderefs.rs",
            line_hint: 42,
            path_state: "absent",
            line_state: "absent",
            candidate_count: 0,
            search: { q: "coderefs.rs", repo: "app" },
            note: "not found in app",
          }),
        ],
        note: "n",
      }),
  );
}

function frameOf(page: Page): FrameLocator {
  return page.frameLocator(".detail__frame");
}

async function openDoors(page: Page, rel: string, expected = 4) {
  await page.goto(`${BASE}/a/canon/${rel}`);
  await expect(frameOf(page).locator("[data-kb-coderef]")).toHaveCount(expected, { timeout: 20_000 });
}

/// Click a door and return the URL of the tab it opened.
async function clickDoor(page: Page, ordinal: number): Promise<string> {
  const popupP = page.context().waitForEvent("page");
  await frameOf(page).locator(`[data-kb-coderef="${ordinal}"]`).click();
  const popup = await popupP;
  await expect(popup).not.toHaveURL("about:blank");
  const url = popup.url();
  await popup.close();
  return url;
}

test.describe("the citation is the door", () => {
  test("(a) kb-code unreachable: hint doors, dashed, opening kb-code's #file search", async ({
    page,
    request,
  }) => {
    await stubDestinations(page.context());
    const { rel } = await docRel(request, "canon", "code-refs-demo-doors.html");
    await openDoors(page, rel);
    const f = frameOf(page);
    const ords = await f
      .locator("[data-kb-coderef]")
      .evaluateAll((els) => els.map((e) => Number(e.getAttribute("data-kb-coderef"))));
    expect(ords).toEqual([0, 1, 2, 5]);
    const cls = await f
      .locator("[data-kb-coderef]")
      .evaluateAll((els) => els.map((e) => e.getAttribute("data-kb-coderef-cls")));
    expect(cls).toEqual(["hint", "hint", "hint", "hint"]);
    // The linked citation and the listing token are not doors.
    await expect(f.locator("a code[data-kb-coderef]")).toHaveCount(0);
    await expect(f.locator("pre code[data-kb-coderef]")).toHaveCount(0);
    await expect(f.locator('[data-kb-coderef="0"]')).toHaveCSS("text-decoration-style", "dashed");

    const before = { url: page.url(), len: await page.evaluate(() => history.length) };
    const url = await clickDoor(page, 0);
    expect(url).toBe(`${CODE_URL}/search?q=${HINT_Q}`);
    expect(page.url()).toBe(before.url);
    expect(await page.evaluate(() => history.length)).toBe(before.len);
  });

  test("(b) verified by kb-code: exact, moved, unsure and hint doors open at the ref", async ({
    page,
    request,
  }) => {
    await stubDestinations(page.context());
    const { id, rel } = await docRel(request, "canon", "code-refs-demo-doors.html");
    await mockVerified(page, id);
    await openDoors(page, rel);
    const f = frameOf(page);
    await expect(f.locator('[data-kb-coderef="0"]')).toHaveAttribute("data-kb-coderef-cls", "exact", {
      timeout: 15_000,
    });
    await expect(f.locator('[data-kb-coderef="1"]')).toHaveAttribute("data-kb-coderef-cls", "moved");
    await expect(f.locator('[data-kb-coderef="2"]')).toHaveAttribute("data-kb-coderef-cls", "unsure");
    await expect(f.locator('[data-kb-coderef="5"]')).toHaveAttribute("data-kb-coderef-cls", "hint");
    await expect(f.locator('[data-kb-coderef="0"]')).toHaveCSS("text-decoration-style", "solid");
    await expect(f.locator('[data-kb-coderef="2"]')).toHaveCSS("text-decoration-style", "dotted");

    expect(await clickDoor(page, 0)).toBe(`${CODE_URL}/r/app/${CFG}?line=120`);
    expect(await clickDoor(page, 1)).toBe(`${CODE_URL}/r/app/${CFG}?line=131`);
    expect(await clickDoor(page, 2)).toBe(`${CODE_URL}/search?q=coderefs.rs&repo=app`);
    expect(await clickDoor(page, 5)).toBe(`${CODE_URL}/search?q=${HINT_Q}&repo=app`);
  });

  test("(c) annotate mode: the annotator owns the click, no tab opens", async ({ page, request }) => {
    await stubDestinations(page.context());
    const { rel } = await docRel(request, "canon", "code-refs-demo-doors.html");
    await openDoors(page, rel);
    let opened = 0;
    page.context().on("page", () => {
      opened += 1;
    });
    const pen = page.locator('[data-kb-act="annotate"]');
    await pen.click();
    await expect(pen).toHaveAttribute("aria-pressed", "true");
    const f = frameOf(page);
    await expect
      .poll(async () => await f.locator(":root").evaluate(() => !!(window as unknown as { __KB_COMMENTS?: unknown }).__KB_COMMENTS), {
        timeout: 15_000,
      })
      .toBe(true);
    await expect(f.locator("[data-kb-coderef]")).toHaveCount(4, { timeout: 15_000 });
    await f.locator('[data-kb-coderef="0"]').click();
    await expect(page.getByRole("complementary", { name: "comments" }).locator(".cp__compose")).toBeVisible();
    expect(opened).toBe(0);
  });

  test("(d) a kb with no code_url: no doors and no request to kb-code", async ({ page, request }) => {
    const hits: string[] = [];
    page.on("request", (r) => {
      if (codeOrigin(new URL(r.url()))) hits.push(r.url());
    });
    const { rel } = await docRel(request, "nocode", "no-code-url.html");
    await page.goto(`${BASE}/a/nocode/${rel}`);
    const f = frameOf(page);
    await expect(f.getByRole("heading", { name: "No code url" })).toBeVisible();
    await page.waitForTimeout(1500);
    await expect(f.locator("[data-kb-coderef]")).toHaveCount(0);
    expect(hits).toEqual([]);
  });

  test("(e) a native note: the citation is an anchor into kb-code", async ({ page, request }) => {
    const created = await request.post(`${BASE}/api/kb/canon/notes`, {
      data: { title: "Doors note", body_md: `Look at \`${CFG}:120\` first.\n` },
    });
    expect(created.ok()).toBeTruthy();
    const { id, path } = (await created.json()) as { id: string; path: string };
    await expect
      .poll(
        async () => {
          const r = await request.get(`${BASE}/api/kb/canon/docs/${id}/code-refs`);
          if (!r.ok()) return 0;
          return ((await r.json()) as { ref_count: number }).ref_count;
        },
        { timeout: 15_000 },
      )
      .toBeGreaterThan(0);
    await page.goto(`${BASE}/a/canon/${path.split("/").map(encodeURIComponent).join("/")}`);
    const door = page.locator(".kb-note-md a.kb-coderef-door");
    await expect(door).toHaveCount(1, { timeout: 15_000 });
    await expect(door).toHaveAttribute("href", `${CODE_URL}/search?q=${HINT_Q}`);
    await expect(door).toHaveAttribute("target", "_blank");
    await expect(door).toHaveAttribute("rel", /noopener/);
  });
});
