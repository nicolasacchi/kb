import { test, expect, type Page } from "@playwright/test";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { PORT } from "./helpers";

// U6 (v0.25 quick capture) — end-to-end SPA capture flow: palette → the
// "Capture file…" command → CaptureSheet (picker/kb/sanitize) → multipart
// POST → success toast → watcher/SSE-driven indexing → the artifact lands
// in the gallery carrying its capture-time provenance (`kb-category:
// capture`, `kb-tags: source:upload, from:spa, …`).
//
// Mirrors zz-live-fs.spec.ts's "mutate the watched corpus, poll without a
// reload" pattern, but drives the write through the SPA's own upload path
// (multipart POST) rather than touching the filesystem directly. Fixtures
// are written to an OS tmp dir at test time (deterministic, no repo
// fixtures to keep in sync) and never cleaned up — same non-cleanup stance
// as zz-live-fs's own corpus writes; the daemon + its tmp KB_HOME are torn
// down wholesale by global-teardown.ts at the end of the run.

const BASE = `http://127.0.0.1:${PORT}`;
const FIXTURE_DIR = mkdtempSync(join(tmpdir(), "kb-e2e-capture-"));

async function openCmdk(page: Page): Promise<void> {
  // Click the topbar button rather than press Ctrl+K — Playwright's
  // synthetic keystroke doesn't reliably reach window-level keydown
  // handlers in headless chromium (spa-cmdk.spec.ts precedent).
  await page.getByRole("button", { name: /open search/i }).click();
  await expect(page.getByRole("dialog", { name: "search" })).toBeVisible();
}

async function openCaptureSheet(page: Page): Promise<void> {
  await openCmdk(page);
  // Command palette shows all built-in commands when the query is empty.
  await page.getByRole("option", { name: /Capture file/ }).click();
  await expect(
    page.getByRole("dialog", { name: "capture files" }),
  ).toBeVisible();
}

test.describe("quick capture (U6)", () => {
  test.beforeAll(async () => {
    // The poll below deliberately waits out the 60s reconcile backstop —
    // but Playwright's DEFAULT hook budget is also 60s, so the one case
    // the wait exists for (live watch missed the write) timed the hook
    // out before its own 75s deadline could fire. Give the hook headroom.
    test.setTimeout(120_000);
    // Prime the kb's `capture/` folder once, before either timed assertion
    // below. The FIRST write into a not-yet-existing subdirectory can race
    // notify's recursive watch registration on Linux (a well-known inotify
    // limitation — a file created in the same instant as its new parent
    // directory can be missed by the live watch); kb-core's periodic
    // reconcile pass is the documented backstop for exactly this case
    // (config.rs `DEFAULT_RECONCILE_SECS` = 60s, "reconcile is the
    // correctness backstop regardless of live delivery"). Eating that
    // worst-case wait ONCE here, off the UI, means both real assertions
    // below land in an already-watched folder and pick up via the fast
    // live path — matching the plan's called-out "e2e flake on index
    // latency → poll with generous timeout" risk without paying it twice.
    // Title-Case to match every other canon fixture's title casing — the
    // server's title sort is byte-order (case-sensitive: uppercase sorts
    // before lowercase), while spa-views.spec.ts's sort assertions verify
    // against JS's locale-aware `localeCompare`. A leading-lowercase title
    // is the one shape where those two orderings can disagree, which would
    // otherwise leak this fixture into an unrelated spec's ordering check
    // (both algorithms agree once the leading case matches the corpus).
    const form = new FormData();
    form.append(
      "files",
      new Blob(
        ["# Capture Warmup\n\nPrimes the capture/ folder's watch.\n"],
        { type: "text/markdown" },
      ),
      "capture-warmup.md",
    );
    const resp = await fetch(`${BASE}/api/kb/canon/capture`, {
      method: "POST",
      body: form,
    });
    if (!resp.ok) {
      throw new Error(`capture warmup failed: ${resp.status} ${resp.statusText}`);
    }
    const body = (await resp.json()) as { items: { id: string }[] };
    const id = body.items[0]?.id;
    if (!id) throw new Error("capture warmup returned no item id");

    const deadline = Date.now() + 75_000;
    for (;;) {
      const r = await fetch(`${BASE}/api/kb/canon/artifact/${id}`);
      if (r.status === 200) return;
      if (Date.now() > deadline) {
        throw new Error(
          "capture/ warmup file never indexed (exceeded the reconcile backstop)",
        );
      }
      await new Promise((res) => setTimeout(res, 1000));
    }
  });

  test("capturing a markdown file toasts success and indexes into the canon gallery with the capture category + provenance tag", async ({
    page,
  }) => {
    const stamp = Date.now();
    const title = `Capture E2E Markdown ${stamp}`;
    const fixture = join(FIXTURE_DIR, `capture-md-${stamp}.md`);
    writeFileSync(
      fixture,
      `# ${title}\n\nBody text written by capture.spec.ts to exercise the quick-capture upload path.\n`,
      "utf-8",
    );

    await page.goto(`${BASE}/`);
    // Gallery is live (a known canon card is visible), and give the SSE
    // stream a beat to subscribe before triggering an indexing event —
    // otherwise the artifact.indexed frame can fire before the browser is
    // listening (zz-live-fs precedent).
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();
    await page.waitForTimeout(1500);

    await openCaptureSheet(page);
    await page
      .locator('[data-testid="capture-file-input"]')
      .setInputFiles(fixture);
    await page.locator('[data-testid="capture-kb"]').selectOption("canon");
    await page.locator('[data-testid="capture-tags"]').fill("e2e-capture");
    await page.locator('[data-testid="capture-submit"]').click();

    // Success toast names the RESOLVED title — the fixture's own H1, not
    // the (left-blank) sheet title field; capture preserves the uploaded
    // content's authored title and only steers the filename with it.
    await expect(
      page.locator('[data-kb-toast="ok"]').filter({ hasText: title }),
    ).toBeVisible({ timeout: 10_000 });
    // The sheet closes itself on a successful submit.
    await expect(
      page.getByRole("dialog", { name: "capture files" }),
    ).toHaveCount(0);

    // Poll the gallery — no reload, the watcher/SSE path populates the
    // card once indexing completes (generous timeout).
    const card = page.getByRole("link", { name: new RegExp(title) });
    await expect(card).toBeVisible({ timeout: 20_000 });

    await card.click();
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();

    // Passport — the category KV is a `capture` deep-link, stamped at
    // write time by kb-core's capture engine (U1).
    const categoryRow = page.locator(".kb-pinsp__kv", { hasText: "category" });
    await expect(categoryRow).toBeVisible();
    await expect(categoryRow.locator(".kb-pinsp__kv-link")).toHaveText(
      "capture",
    );

    // Provenance tag `from:spa` is slugified to `from-spa` at index time
    // (parser::slugify_tag) and rendered as a tag chip.
    await expect(
      page.locator(".kb-pinsp__tagchip-link", { hasText: "from-spa" }),
    ).toBeVisible();
  });

  test("the sanitize toggle strips <script>/onclick from the stored + served HTML source", async ({
    page,
    request,
  }) => {
    const stamp = Date.now();
    const title = `Capture E2E Sanitize ${stamp}`;
    const fixture = join(FIXTURE_DIR, `capture-html-${stamp}.html`);
    writeFileSync(
      fixture,
      `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title></head><body><h1>${title}</h1><script>window.__kbCaptureXssProbe = 1;</script><p onclick="window.__kbCaptureXssProbe = 2">click me</p></body></html>`,
      "utf-8",
    );

    await page.goto(`${BASE}/`);
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();

    await openCaptureSheet(page);
    await page
      .locator('[data-testid="capture-file-input"]')
      .setInputFiles(fixture);
    await page.locator('[data-testid="capture-kb"]').selectOption("canon");
    await page.locator('[data-testid="capture-sanitize"]').check();

    const respPromise = page.waitForResponse(
      (r) =>
        r.url().endsWith("/api/kb/canon/capture") &&
        r.request().method() === "POST",
    );
    await page.locator('[data-testid="capture-submit"]').click();
    const resp = await respPromise;
    const body = (await resp.json()) as { items: { id: string }[] };
    const id = body.items[0]?.id;
    expect(id, "capture response should include an item id").toBeTruthy();

    await expect(
      page.locator('[data-kb-toast="ok"]').filter({ hasText: title }),
    ).toBeVisible({ timeout: 10_000 });

    // The response id is valid pre-index (#27 — same derivation the
    // indexer uses), but the artifact-bytes endpoint 404s until the
    // watcher has actually indexed the file. Poll it directly rather
    // than round-tripping through the gallery — this is exactly the
    // endpoint the SPA's own download control reads (lib/download.ts),
    // so it's "served", not just "stored on disk".
    await expect(async () => {
      const raw = await request.get(`${BASE}/api/kb/canon/artifact/${id}`);
      expect(raw.status()).toBe(200);
    }).toPass({ timeout: 20_000, intervals: [500] });

    const raw = await request.get(`${BASE}/api/kb/canon/artifact/${id}`);
    const text = await raw.text();
    // Sanitize is a CAPTURE-TIME transform (Decision 3) — the stored
    // source IS the sanitized output, so the served bytes never carried
    // the script/handler in the first place.
    expect(text).not.toContain("<script");
    expect(text).not.toContain("onclick=");
    // The sanitize profile keeps ordinary content — the H1 text survives.
    expect(text).toContain(title);
  });
});

// C3 (v0.26 capture ergonomics) — paste-text capture rides the SAME
// files/watcher/SSE pipeline as a picked file (Decision 1: the
// client-synthesized File goes through the `files` multipart field, never
// the share-sheet `text` stub field), plus the two new C2 chrome entry
// points (Header iconbtn on desktop, BottomTabBar button on mobile) both
// raise the identical CaptureSheet the palette command does.
test.describe("capture ergonomics — paste text + chrome entry points (C3)", () => {
  test("pasting markdown text captures it through the file pipeline: provenance tags, no url-stub tag, body round-trips", async ({
    page,
    request,
  }) => {
    const stamp = Date.now();
    const title = `Capture E2E Paste ${stamp}`;
    const pasteBody = `# ${title}\n\nBody text pasted by capture.spec.ts to exercise the paste-text capture path.\n`;

    await page.goto(`${BASE}/`);
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();

    await openCaptureSheet(page);
    // Title left blank — the synthesized filename falls back to
    // pasteFile.ts's firstLineStem (the pasted H1 with its `#` stripped),
    // and the server resolves that same H1 as the artifact's title — the
    // same "content wins over filename" shape the file-upload test above
    // exercises. Format stays at its "md" default (no radio click).
    await page.locator('[data-testid="capture-text"]').fill(pasteBody);
    await page.locator('[data-testid="capture-kb"]').selectOption("canon");

    const respPromise = page.waitForResponse(
      (r) =>
        r.url().endsWith("/api/kb/canon/capture") &&
        r.request().method() === "POST",
    );
    await page.locator('[data-testid="capture-submit"]').click();
    const resp = await respPromise;
    const body = (await resp.json()) as { items: { id: string }[] };
    const id = body.items[0]?.id;
    expect(id, "capture response should include an item id").toBeTruthy();

    await expect(
      page.locator('[data-kb-toast="ok"]').filter({ hasText: title }),
    ).toBeVisible({ timeout: 10_000 });
    await expect(
      page.getByRole("dialog", { name: "capture files" }),
    ).toHaveCount(0);

    // Poll the gallery — paste rides the same watcher/SSE path a picked
    // file does.
    const card = page.getByRole("link", { name: new RegExp(title) });
    await expect(card).toBeVisible({ timeout: 20_000 });

    await card.click();
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();

    const categoryRow = page.locator(".kb-pinsp__kv", { hasText: "category" });
    await expect(categoryRow).toBeVisible();
    await expect(categoryRow.locator(".kb-pinsp__kv-link")).toHaveText(
      "capture",
    );
    // Both provenance tags the capture engine stamps on every batch
    // (`build_tag_list`) — this is the one test asserting the paste path
    // stamps the SAME tags a picked file does, not just `from:spa`.
    await expect(
      page.locator(".kb-pinsp__tagchip-link", { hasText: "source-upload" }),
    ).toBeVisible();
    await expect(
      page.locator(".kb-pinsp__tagchip-link", { hasText: "from-spa" }),
    ).toBeVisible();

    // The stored source IS the pasted markdown (through `files` /
    // `stamp_markdown`, not the share-sheet `text` field's
    // `capture_url_stub` snippet path) — the body round-trips and there is
    // no `kind:url-stub` tag anywhere in the stamped output.
    const raw = await request.get(`${BASE}/api/kb/canon/artifact/${id}`);
    const text = await raw.text();
    expect(text).toContain(
      "Body text pasted by capture.spec.ts to exercise the paste-text capture path.",
    );
    expect(text).not.toContain("kind:url-stub");
  });

  test("desktop: header capture button opens the sheet", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await page.locator('.kb-head [data-kb-act="capture"]').click();
    await expect(
      page.getByRole("dialog", { name: "capture files" }),
    ).toBeVisible();
    await page.getByRole("button", { name: "close" }).click();
    await expect(
      page.getByRole("dialog", { name: "capture files" }),
    ).toHaveCount(0);
  });

  test.describe("mobile (≤860px)", () => {
    test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

    test("bottom tab bar capture button opens the sheet; the drawer no longer carries a capture row", async ({
      page,
    }) => {
      await page.goto(`${BASE}/`);
      await page.locator('.kb-tabbar [data-kb-act="capture"]').click();
      await expect(
        page.getByRole("dialog", { name: "capture files" }),
      ).toBeVisible();
      await page.getByRole("button", { name: "close" }).click();
      await expect(
        page.getByRole("dialog", { name: "capture files" }),
      ).toHaveCount(0);

      // C2 retired the NavList drawer capture row — the tab-bar button
      // above is mobile's only chrome entry point now (Decision 3; the
      // palette row is the other survivor, exercised via openCaptureSheet
      // in the "quick capture (U6)" describe above).
      await page.locator(".kb-burger").click();
      const drawer = page.locator(".kb-drawer");
      await expect(drawer).toBeVisible();
      await expect(drawer.locator('[data-kb-act="capture"]')).toHaveCount(0);
    });
  });
});
