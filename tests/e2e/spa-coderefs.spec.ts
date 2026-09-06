import { test, expect, type Page, type Route } from "@playwright/test";
import type { APIRequestContext } from "@playwright/test";
import { BASE } from "./helpers";

// DCB W1.D — the Code section (CodeRefsSection.tsx) inside PreviewInspector's
// Links tab: the coderef/1 same-origin fetch, kb-code's cross-origin
// codelens/1 + codelens-scorecard/1 surface, the pin write, tiered rows, and
// the five-state degrade matrix (13-w1d-kb-spa.md §6/§12.3).
//
// Fixture strategy: `page.route()` interception (13-w1d-kb-spa.md §12.1) —
// NOT a real kb-code-server process. This track's job is to verify the
// SPA's rendering of a given codelens/1 response, not kb-code's resolution
// correctness (that's W1.C's own fixture-repo gate). `route.fulfill()`
// intercepts before the real network request, but Chromium still enforces
// CORS on the fulfilled response, so every handler below sets an
// exact-origin `Access-Control-Allow-Origin` (+ `-Credentials`) — verified
// locally that omitting the header makes the mocked fetch fail (sanity
// check per §12.3's closing note; not re-run every CI pass).
//
// CODE_URL (`http://127.0.0.1:4747`) is an ordinary config VALUE
// (global-setup.ts's `[kb.canon] code_url`) — nothing needs to actually
// listen there, since every request to it is intercepted.

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
  const docs = (await r.json()) as {
    id: string;
    path: string;
    source_relative: string;
  }[];
  const hit = docs.find((d) => d.source_relative.endsWith(filename));
  expect(hit, `${filename} indexed in ${kb}`).toBeTruthy();
  return { id: hit!.id, rel: hit!.source_relative };
}

/// Test 1's "any canon doc with zero declared refs" — verified via the
/// coderef/1 route itself rather than guessed from the canon README, so
/// this test doesn't silently start failing if a future canon fixture adds
/// a code-shaped `<code>` block to one of the original design-handoff
/// files.
async function docWithNoCodeRefs(
  request: APIRequestContext,
): Promise<{ id: string; rel: string }> {
  const r = await request.get(`${BASE}/api/kb/canon/docs?limit=50`);
  const docs = (await r.json()) as { id: string; source_relative: string }[];
  for (const d of docs) {
    if (
      d.source_relative.startsWith("code-refs-demo") ||
      d.source_relative.includes("/")
    ) {
      continue; // skip the coderefs fixture + nested pm/ files
    }
    const cr = await request.get(
      `${BASE}/api/kb/canon/docs/${d.id}/code-refs`,
    );
    if (!cr.ok()) continue;
    const body = (await cr.json()) as { never_scanned: boolean; ref_count: number };
    if (!body.never_scanned && body.ref_count === 0) {
      return { id: d.id, rel: d.source_relative };
    }
  }
  throw new Error("no zero-code-ref canon doc found (needed by test 1)");
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

async function mockScorecard(page: Page, json: unknown) {
  await page.route(
    (url) => codeLensOrigin(url) && url.pathname === "/api/doc-lens/repos",
    (route) => fulfillJson(route, json),
  );
}

async function mockLens(page: Page, json: unknown) {
  await page.route(
    (url) => codeLensOrigin(url) && url.pathname === "/api/doc-lens",
    (route) => fulfillJson(route, json),
  );
}

async function openLinksTab(page: Page, kb: string, rel: string) {
  await page.goto(`${BASE}/a/${kb}/${rel}`);
  await page.locator('[data-kb-itab="links"]').click();
}

test.describe("code refs (desktop)", () => {
  test("hidden when the doc has none", async ({ page, request }) => {
    const { rel } = await docWithNoCodeRefs(request);
    await openLinksTab(page, "canon", rel);
    await expect(page.locator(".kb-pinsp__coderef-head")).toHaveCount(0);
    await expect(
      page.locator(".kb-pinsp__hint", { hasText: "No links to or from" }),
    ).toBeVisible();
  });

  test("declared + inferred rows render with correct tiers", async ({
    page,
    request,
  }) => {
    const { id, rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    await mockScorecard(page, baseScorecard({ doc_id: id }));
    await mockLens(page, baseLens({ doc_id: id }));

    await openLinksTab(page, "canon", rel);
    await expect(page.locator(".kb-pinsp__coderef-head")).toContainText("Code · 2");

    const repoBtn = page.locator('.kb-pinsp__coderef-repo[title*="present"]');
    await expect(repoBtn).toBeVisible();
    await repoBtn.click();
    await expect(page).toHaveURL(/[?&]repo=app\b/);

    const rows = page.locator(".kb-pinsp__coderef-row.is-present");
    await expect(rows).toHaveCount(2);
    const hrefs = await rows.locator("a").evaluateAll((as) =>
      as.map((a) => (a as HTMLAnchorElement).href),
    );
    expect(hrefs).toContain(
      `${CODE_URL}/r/app/crates/kb-core/src/coderefs.rs?line=42`,
    );
    expect(hrefs).toContain(`${CODE_URL}/r/app/crates/kb-core/src/config.rs?line=120`);
  });

  test("dirty checkout renders the uncommitted-working-tree banner", async ({
    page,
    request,
  }) => {
    const { id, rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    await mockScorecard(page, baseScorecard({ doc_id: id }));
    await mockLens(
      page,
      baseLens({
        doc_id: id,
        repo: {
          name: "app",
          root: "/tmp/app",
          state: "ready",
          head_sha: "abc1234567",
          head_branch: "main",
          dirty: true,
          source: "param",
        },
      }),
    );

    await page.goto(`${BASE}/a/canon/${rel}?repo=app`);
    await page.locator('[data-kb-itab="links"]').click();

    await expect(page.locator(".kb-pinsp__coderef-dirty-banner")).toContainText(
      "uncommitted working tree",
    );
  });

  test("unreachable kb-code renders the honest degrade state, never \"kb-code down\"", async ({
    page,
    request,
  }) => {
    const { rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    // Deliberately NO page.route() handler for **/api/doc-lens* — the real
    // request against the unlistened 127.0.0.1:4747 fails naturally.
    await openLinksTab(page, "canon", rel);
    const hint = page.locator(".kb-pinsp__hint", {
      hasText: "Code bridge unavailable from this origin",
    });
    await expect(hint).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".kb-pinsp__body")).not.toContainText("kb-code down");
  });

  test("kb with no code_url renders \"not linked to a code repo\" with zero doc-lens requests", async ({
    page,
    request,
  }) => {
    const { rel } = await docByFilename(request, "nocode", "no-code-url.html");
    let hit = false;
    await page.route(
      (url) => codeLensOrigin(url),
      () => {
        hit = true;
        throw new Error("doc-lens must not be called when code_url is unset");
      },
    );
    await openLinksTab(page, "nocode", rel);
    await expect(
      page.locator(".kb-pinsp__hint", { hasText: "Not linked to a code repo" }),
    ).toBeVisible();
    expect(hit).toBe(false);
  });

  test("ambiguous >3 deep-links to search; miss renders inert + search link; issue renders a plain GitHub link", async ({
    page,
    request,
  }) => {
    const { id, rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    await mockScorecard(page, baseScorecard({ doc_id: id, counted_refs: 5 }));
    await mockLens(
      page,
      baseLens({
        doc_id: id,
        ungrouped_count: 5,
        counts: {
          total: 5,
          resolved: 5,
          present: 2,
          ambiguous: 2,
          absent: 1,
          external: 0,
          confirmed: 0,
          drifted: 0,
          unverifiable: 0,
          line_absent: 5,
          declared_but_absent: 0,
        },
        refs: [
          resolvedRef({
            ordinal: 0,
            kind: "path",
            raw: "conversion.rb",
            path_hint: "conversion.rb",
            path_state: "ambiguous",
            candidate_count: 5,
            candidates: [],
            line_state: "absent",
            // Mid-flight correction (post-W1.C-review): `search` is set on
            // EVERY ambiguous ref, both tiers — this one exercises the >3
            // tier, which DOES render it as the escape-hatch link.
            search: { q: "conversion.rb", repo: "app" },
          }),
          // Mid-flight correction — the ≤3 tier ALSO carries a `search`
          // (never null-gated to distinguish tiers); the SPA must tier on
          // `candidate_count`/`candidates.length`, never on `search`
          // presence. This ref would wrongly render as the >3-candidate
          // badge if that regression crept in.
          resolvedRef({
            ordinal: 1,
            kind: "path",
            raw: "helper.rb",
            path_hint: "helper.rb",
            path_state: "ambiguous",
            candidate_count: 2,
            candidates: ["app/a/helper.rb", "app/b/helper.rb"],
            line_state: "absent",
            search: { q: "helper.rb", repo: "app" },
          }),
          resolvedRef({
            ordinal: 2,
            kind: "path",
            raw: "renamed_file.rb",
            path_hint: "renamed_file.rb",
            path_state: "absent",
            candidate_count: 0,
            candidates: [],
            line_state: "absent",
            search: { q: "renamed_file.rb", repo: "app" },
            note: "renamed?",
          }),
          resolvedRef({
            ordinal: 3,
            kind: "issue",
            raw: "acme/shopfront#15357",
            path_hint: null,
            path_state: null,
            candidate_count: 0,
            line_state: "absent",
            issue: {
              owner: "acme",
              repo: "shopfront",
              number: 15357,
              href: "https://github.com/acme/shopfront/issues/15357",
            },
          }),
          // Mid-flight correction — `reader.line` is nullable on codelens/1
          // (a `present` path with NO line hint at all, e.g. a bare `kind:
          // "path"` citation, still carries a `reader`, just `line: null`).
          // The deep-link builder must omit `?line=` entirely here, never
          // render `?line=null`/`?line=0`.
          resolvedRef({
            ordinal: 4,
            kind: "path",
            raw: "app/config/routes.rb",
            path_hint: "app/config/routes.rb",
            path_state: "present",
            resolved_path: "app/config/routes.rb",
            candidate_count: 1,
            candidates: ["app/config/routes.rb"],
            line_state: "absent",
            reader: { repo: "app", path: "app/config/routes.rb", line: null },
          }),
        ],
      }),
    );

    await page.goto(`${BASE}/a/canon/${rel}?repo=app`);
    await page.locator('[data-kb-itab="links"]').click();

    const ambiguousRow = page.locator(".kb-pinsp__coderef-row.is-ambiguous", {
      hasText: "conversion.rb",
    });
    await expect(ambiguousRow.locator(".kb-pinsp__coderef-search")).toHaveAttribute(
      "href",
      `${CODE_URL}/search?q=conversion.rb&repo=app`,
    );

    // The ≤3 tier renders inline candidate links, NOT the >3-candidate
    // search badge — even though `search` is present on the wire.
    const inlineRow = page.locator(".kb-pinsp__coderef-row.is-ambiguous", {
      hasText: "helper.rb",
    });
    await expect(inlineRow.locator(".kb-pinsp__coderef-candidates li")).toHaveCount(2);
    await expect(inlineRow.locator(".kb-pinsp__coderef-search")).toHaveCount(0);

    const noLineRow = page.locator(".kb-pinsp__coderef-row.is-present", {
      hasText: "app/config/routes.rb",
    });
    await expect(noLineRow.locator("a")).toHaveAttribute(
      "href",
      `${CODE_URL}/r/app/app/config/routes.rb`,
    );

    const missRow = page.locator(".kb-pinsp__coderef-row.is-absent", {
      hasText: "renamed_file.rb",
    });
    await expect(missRow.locator("a")).toHaveCount(1); // only the search link, no reader link
    await expect(missRow.locator(".kb-pinsp__coderef-search")).toHaveAttribute(
      "href",
      `${CODE_URL}/search?q=renamed_file.rb&repo=app`,
    );
    await expect(missRow).toContainText("renamed?");

    const issueRow = page.locator(".kb-pinsp__coderef-row", {
      hasText: "acme/shopfront#15357",
    });
    await expect(issueRow.locator("a")).toHaveAttribute(
      "href",
      "https://github.com/acme/shopfront/issues/15357",
    );
  });

  test("picking a repo updates ?repo=, writes the pin, and pre-selects on a fresh entry", async ({
    page,
    request,
  }) => {
    const { id, rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    const crResp = await request.get(`${BASE}/api/kb/canon/docs/${id}/code-refs`);
    const crBody = (await crResp.json()) as { doc_hash: string | null };

    const puts: Record<string, unknown>[] = [];
    await page.route(
      (url) => codeLensOrigin(url) && url.pathname === "/api/doc-lens/pin",
      async (route) => {
        const req = route.request();
        if (req.method() === "OPTIONS") {
          await route.fulfill({
            status: 204,
            headers: {
              "access-control-allow-origin": BASE,
              "access-control-allow-credentials": "true",
              "access-control-allow-methods": "PUT, DELETE",
              "access-control-allow-headers": "content-type",
            },
          });
          return;
        }
        if (req.method() === "PUT") {
          const body = req.postDataJSON() as Record<string, unknown>;
          puts.push(body);
          await fulfillJson(route, {
            schema: "codelens-pin/1",
            kb: body.kb,
            doc_id: body.doc,
            repo: body.repo,
            repo_root: "/tmp/app",
            doc_hash: body.doc_hash,
            pinned_at: Math.floor(Date.now() / 1000),
          });
          return;
        }
        await route.continue();
      },
    );
    await mockScorecard(page, baseScorecard({ doc_id: id }));
    await mockLens(page, baseLens({ doc_id: id }));

    await openLinksTab(page, "canon", rel);
    const repoBtn = page.locator('.kb-pinsp__coderef-repo[title*="present"]');
    await repoBtn.click();
    await expect(page).toHaveURL(/[?&]repo=app\b/);

    await expect.poll(() => puts.length).toBeGreaterThan(0);
    expect(puts[0]).toMatchObject({
      kb: "canon",
      doc: id,
      repo: "app",
      doc_hash: crBody.doc_hash,
    });

    // Fresh entry, no ?repo= — pinned_repo pre-selects the switcher.
    await mockScorecard(page, baseScorecard({ doc_id: id, pinned_repo: "app" }));
    await page.goto(`${BASE}/a/canon/${rel}`);
    await page.locator('[data-kb-itab="links"]').click();
    await expect(page).toHaveURL(/[?&]repo=app\b/);
  });

  // W1.D.R #2 — R5's bug: PreviewInspector stays mounted across reader→
  // reader navigation (no `key=` at either detail.tsx call site), so a
  // pinned_repo seed latch that only ever fires ONCE (a plain boolean, never
  // reset) silently stops pre-selecting past the FIRST doc viewed in a
  // session. This test navigates doc→doc IN-APP — a Folder-tab sibling
  // `<Link>` click, never `page.goto` — and asserts the SECOND doc's own
  // (different) pinned_repo pre-selects too.
  test("pinned_repo re-seeds for a SECOND doc reached via in-app navigation, not just the first", async ({
    page,
    request,
  }) => {
    const docA = await docByFilename(request, "canon", "code-refs-demo.html");
    const docB = await docByFilename(request, "canon", "code-refs-demo-2.html");

    // Per-doc scorecard: docA pins "app", docB pins the DIFFERENT "app-two"
    // — proving the pre-select reflects the doc actually on screen, not a
    // stale first-doc value carried over by a latch that never re-arms.
    await page.route(
      (url) => codeLensOrigin(url) && url.pathname === "/api/doc-lens/repos",
      (route) => {
        const reqUrl = new URL(route.request().url());
        const docId = reqUrl.searchParams.get("doc") ?? "";
        const pinnedRepo = docId === docB.id ? "app-two" : "app";
        return fulfillJson(
          route,
          baseScorecard({
            doc_id: docId,
            pinned_repo: pinnedRepo,
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
              {
                name: "app-two",
                root: "/tmp/app-two",
                state: "ready",
                head_sha: "def7654321",
                head_branch: "main",
                dirty: false,
                present: 1,
                ambiguous: 0,
                absent: 0,
                external: 0,
                partial: false,
                reason: null,
              },
            ],
          }),
        );
      },
    );
    await mockLens(page, baseLens({}));

    // Fresh entry on doc A — pinned_repo "app" pre-selects.
    await openLinksTab(page, "canon", docA.rel);
    await expect(page).toHaveURL(/[?&]repo=app\b/);

    // IN-APP navigation to doc B: the Folder tab's sibling list renders
    // plain react-router `<Link>` rows (PreviewInspector.tsx's
    // `folderRows.map`) — clicking one is an SPA nav, not a page load, so
    // PreviewInspector never unmounts across it.
    await page.locator('[data-kb-itab="folder"]').click();
    await page
      .locator(".kb-pinsp__folder-row", { hasText: "code-refs-demo-2.html" })
      .click();
    await expect(page).toHaveURL(/\/a\/canon\/code-refs-demo-2\.html$/);
    // The nav dropped `?repo=app` (artifactHref doesn't carry it over) —
    // re-opening the Links tab must re-seed from doc B's OWN scorecard.
    await page.locator('[data-kb-itab="links"]').click();
    await expect(page).toHaveURL(/[?&]repo=app-two\b/);
  });

  test("a never-scanned doc says so instead of rendering nothing", async ({
    page,
    request,
  }) => {
    const { id, rel } = await docByFilename(request, "canon", "kitchen-sink.html");
    let doclensHit = false;
    await page.route(
      (url) => codeLensOrigin(url),
      () => {
        doclensHit = true;
        throw new Error("doc-lens must not be called for a never-scanned doc");
      },
    );

    let neverScanned = true;
    await page.route(
      (url) =>
        url.hostname === "127.0.0.1" &&
        url.pathname === `/api/kb/canon/docs/${id}/code-refs`,
      (route) =>
        fulfillJson(route, {
          schema: "coderef/1",
          kb: "canon",
          doc_id: id,
          doc_path: "kitchen-sink.html",
          title: "Kitchen sink",
          doc_hash: neverScanned ? null : "hash",
          extracted_at: neverScanned ? null : Math.floor(Date.now() / 1000),
          never_scanned: neverScanned,
          code_rev: null,
          ref_count: 0,
          ungrouped_count: 0,
          truncated: false,
          groups: [],
          refs: [],
        }),
    );

    await openLinksTab(page, "canon", rel);
    await expect(page.locator(".kb-pinsp__coderef-head")).toContainText("Code");
    // CT-E3 — the honest-staleness line carries the status FACT ("code refs
    // never scanned", the exact tier-(a) wording); the hint below it is
    // remedy-only, so the two never say the same thing twice.
    await expect(
      page.locator('[data-kb-coderef-fresh="never-scanned"]'),
    ).toHaveText("code refs never scanned");
    await expect(
      page.locator(".kb-pinsp__hint", { hasText: "kb reindex" }),
    ).toBeVisible();
    await expect(page.locator(".kb-pinsp__body")).not.toContainText("no code refs");
    expect(doclensHit).toBe(false);

    // Flip to a genuinely-scanned, zero-ref doc — the section must render
    // NOTHING (the two arms of §3.2 are opposite, not aliases) and the
    // "No links…" hint must return.
    neverScanned = false;
    await page.reload();
    await page.locator('[data-kb-itab="links"]').click();
    await expect(page.locator(".kb-pinsp__coderef-head")).toHaveCount(0);
    await expect(
      page.locator(".kb-pinsp__hint", { hasText: "No links to or from" }),
    ).toBeVisible();
  });

  test("\"Open as lens\" deep-links to kb-code's id-addressed ramp", async ({
    page,
    request,
  }) => {
    const { id, rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    await mockScorecard(page, baseScorecard({ doc_id: id }));
    await mockLens(page, baseLens({ doc_id: id }));

    await openLinksTab(page, "canon", rel);
    const lensLink = page.locator(".kb-pinsp__coderef-lens-link");
    await expect(lensLink).toHaveAttribute(
      "href",
      `${CODE_URL}/~lens/canon/${id}`,
    );
    await expect(lensLink).toHaveAttribute("target", "_blank");
    await expect(lensLink).toHaveAttribute("rel", "noopener noreferrer");

    const { rel: nocodeRel } = await docByFilename(request, "nocode", "no-code-url.html");
    await openLinksTab(page, "nocode", nocodeRel);
    await expect(page.locator(".kb-pinsp__coderef-lens-link")).toHaveCount(0);
  });

  // W1.D.R #3 — one canned response combining the three lowest-coverage
  // shapes the review named: a scorecard row still `indexing` (state-4,
  // disabled rendering — a repo genuinely mid-boot-walk, distinct from
  // `ready`/`error`), a `rev_remap`-confirmed line badge with a nonzero
  // delta (R18's "moved +N · git-verified", the strongest evidence class),
  // and a pathless `symbol_method` ref (D5 — no `path_hint` at all) that
  // resolves uniquely against the repo's symbol table and renders as a
  // link, same as a resolved path would.
  test("indexing scorecard row, rev_remap-moved badge, and a pathless symbol hit all render honestly", async ({
    page,
    request,
  }) => {
    const { id, rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    await mockScorecard(
      page,
      baseScorecard({
        doc_id: id,
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
          {
            name: "booting",
            root: "/tmp/booting",
            state: "indexing",
            head_sha: null,
            head_branch: null,
            dirty: null,
            present: null,
            ambiguous: null,
            absent: null,
            external: null,
            partial: false,
            reason: "no indexed files yet",
          },
        ],
      }),
    );
    await mockLens(
      page,
      baseLens({
        doc_id: id,
        ungrouped_count: 2,
        counts: {
          total: 2,
          resolved: 2,
          present: 2,
          ambiguous: 0,
          absent: 0,
          external: 0,
          confirmed: 1,
          drifted: 0,
          unverifiable: 0,
          line_absent: 0,
          declared_but_absent: 0,
        },
        refs: [
          resolvedRef({
            ordinal: 0,
            kind: "path_line",
            raw: "coderefs.rs:42",
            declared: true,
            path_hint: "crates/kb-core/src/coderefs.rs",
            line_hint: 42,
            path_state: "present",
            resolved_path: "crates/kb-core/src/coderefs.rs",
            candidates: ["crates/kb-core/src/coderefs.rs"],
            line_state: "confirmed",
            line_evidence: "rev_remap",
            line_hint_delta: 6,
            resolved_line: 48,
            reader: { repo: "app", path: "crates/kb-core/src/coderefs.rs", line: 48 },
          }),
          resolvedRef({
            ordinal: 1,
            kind: "symbol_method",
            raw: "SearchService#listable_results",
            path_hint: null,
            path_state: null,
            line_state: "absent",
            symbol_container: "SearchService",
            symbol_member: "listable_results",
            symbol_state: "hit_unique",
            symbol_hit_count: 1,
            symbol_hits: [
              {
                path: "app/services/search_service.rb",
                line_start: 88,
                line_end: 96,
                kind: "method",
                container: "SearchService",
              },
            ],
          }),
        ],
      }),
    );

    await page.goto(`${BASE}/a/canon/${rel}?repo=app`);
    await page.locator('[data-kb-itab="links"]').click();

    // State-4 — an `indexing` repo renders disabled with its own label, no
    // present/ambiguous/absent tally (those are `null` on the wire).
    const bootingBtn = page.locator(".kb-pinsp__coderef-repo", { hasText: "booting" });
    await expect(bootingBtn).toBeVisible();
    await expect(bootingBtn).toBeDisabled();
    await expect(bootingBtn.locator(".kb-pinsp__coderef-repo-n")).toHaveText("indexing…");

    // R18 — a rev_remap confirmation that ALSO moved the line renders the
    // honest "moved" badge, not a bare "confirmed" masking the move.
    const movedRow = page.locator(".kb-pinsp__coderef-row", { hasText: "coderefs.rs:42" });
    await expect(movedRow.locator(".kb-pinsp__coderef-line")).toHaveText(
      "✓ moved +6 · git-verified",
    );

    // D5 — a pathless symbol_method ref resolving to a single repo-unique
    // hit renders as a whole-row link, same treatment as a resolved path.
    const symbolRow = page.locator(".kb-pinsp__coderef-row", {
      hasText: "SearchService#listable_results",
    });
    await expect(symbolRow.locator("a")).toHaveAttribute(
      "href",
      `${CODE_URL}/r/app/app/services/search_service.rb?line=88`,
    );
  });
});

test.describe("code refs (mobile)", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test("renders inside the mobile bottom sheet", async ({ page, request }) => {
    const { id, rel } = await docByFilename(request, "canon", "code-refs-demo.html");
    const repos = ["app", "app-two", "app-three", "app-four", "app-five"].map(
      (name) => ({
        name,
        root: `/tmp/${name}`,
        state: "ready" as const,
        head_sha: "abc1234567",
        head_branch: "main",
        dirty: false,
        present: 2,
        ambiguous: 0,
        absent: 0,
        external: 0,
        partial: false,
        reason: null,
      }),
    );
    await mockScorecard(page, baseScorecard({ doc_id: id, repos }));
    await mockLens(page, baseLens({ doc_id: id }));

    await page.goto(`${BASE}/a/canon/${rel}`);
    await page.locator('[data-kb-act="inspect"]').click();
    const sheet = page.locator("#kb-reader-sheet");
    await expect(sheet).toBeVisible();
    await sheet.locator('[data-kb-itab="links"]').click();

    await expect(sheet.locator(".kb-pinsp__coderef-head")).toBeVisible();
    const strip = sheet.locator(".kb-pinsp__coderef-scorecard");
    await expect(strip).toBeVisible();
    await expect(strip.locator(".kb-pinsp__coderef-repo")).toHaveCount(5);
    const overflow = await strip.evaluate(
      (el) => el.scrollWidth - el.clientWidth,
    );
    expect(overflow).toBeGreaterThan(0);
  });
});
