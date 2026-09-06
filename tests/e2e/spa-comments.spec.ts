import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// Annotator round-trip specs — exercise the Track A + B pipeline:
//   iframe loads with ?cm=on whenever the kb has comments enabled
//   (review-active artifact loads carry the flag from initial mount,
//   regardless of whether the pen is currently toggled — see the
//   `artifactUrl` derivation in routes/detail.tsx). The pen click
//   then drives annotate-on/off purely via the bridge's `cm:mode`
//   postMessage path, so the iframe never remounts on toggle.
//
//   click pen icon → CommentsPanel mounts + bridge posts cm:mode:on →
//   annotator script (already injected at first load) flips body
//   class → cm:compose postMessage from inside the iframe → bridge opens
//   the panel's inline composer → user writes + adds → POST to
//   /api/kb/{kb}/review/{id}/comments (R7 fine-grained add) → the comment
//   persists + shows in the panel.
//
// Three load-bearing specs:
//   1. compose round-trip — dispatch cm:compose from inside the iframe,
//      write + add in the panel; assert POST + persistence + reload
//   2. origin rejection — fake cm:compose from the parent's own origin
//      must be dropped by the bridge's origin check (no composer opens)
//   3. resolve toggle — write a file-scope comment via the panel
//      composer, resolve it, assert filtering + persistence
//
// The full in-iframe click → popover → submit chain proved fragile
// under headless Chromium even with all preconditions green; the
// bridge spec is the load-bearing assertion of the same path. The
// in-iframe smoke is left out of v0.2 — manual click-through is the
// confidence check (kept in the verification plan).

// Track U — returns both the artifact id (still needed for the review
// API: the `file` field + `/review/{id}` path are keyed on the id) and
// its source-relative path (for the path-based `/a/<kb>/<path>` permalink).
async function pickArtifact(
  request: import("@playwright/test").APIRequestContext,
): Promise<{ id: string; rel: string }> {
  // kitchen-sink has the deepest heading tree among the canon set, so
  // the Chapter / Section anchors have somewhere real to attach — and
  // every caller below is written against ITS content (SELECTION_NEEDLE,
  // `#bench-table`, the `]` sibling hop to multi-page.html). So resolve
  // it BY PATH; never page `/docs` and fall back to whatever is first.
  //
  // That list is recency-sorted (`SortKey::Recent`) and capped, and canon
  // grows well past any cap as the suite seeds fixtures, so
  // kitchen-sink.html — written once by global-setup, hence the oldest
  // row — is the first to fall off page one. Worse, the list carries rows
  // for files a spec has already deleted from disk: a directory-level
  // `rmSync` emits no per-file notify event, so those rows survive until
  // the next 60s reconcile delete pass. A `docs[0]` fallback could
  // therefore hand back an artifact whose bytes are gone; its iframe
  // 404s, `window.__KB_COMMENTS` never appears, and the caller dies in
  // `waitForAnnotatorReady` with an opaque predicate timeout instead of a
  // legible "no such artifact". That is exactly how this file's first
  // firefox spec failed in CI run 31600285599 (the reconcile 4s later
  // reported `deletes=9`, and every later spec — picking after the sweep,
  // with the list back under the cap — passed).
  const r = await request.get(
    `http://127.0.0.1:${PORT}/api/kb/canon/docs/by-path/kitchen-sink.html`,
  );
  expect(
    r.status(),
    "canon must index kitchen-sink.html (seeded by global-setup's CANON_FILES)",
  ).toBe(200);
  const doc = (await r.json()) as { id: string; source_relative: string };
  return { id: doc.id, rel: doc.source_relative };
}

test.describe("annotator round-trip", () => {
  test("click-to-compose routes to the panel composer → persists → panel reflects it", async ({
    page,
    request,
  }) => {
    const { id, rel } = await pickArtifact(request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    // Toggle annotate mode via the pill.
    const pen = page.locator('[data-kb-act="annotate"]');
    await expect(pen).toBeVisible();
    await pen.click();
    await expect(pen).toHaveAttribute("aria-pressed", "true");

    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();

    // Wait for the annotator script (?cm=on) to load + announce readiness.
    await expect(page.locator("iframe.detail__frame")).toHaveAttribute(
      "src",
      /cm=on/,
    );
    const frame = page.frameLocator(".detail__frame");
    await expect
      .poll(
        async () =>
          await frame.locator(":root").evaluate(() => !!window.__KB_COMMENTS),
        { timeout: 10_000 },
      )
      .toBe(true);

    // Dispatch cm:compose from inside the iframe (mirrors a click in
    // annotate mode). The bridge forwards only the anchor; the panel opens
    // its inline composer where the body is written + saved.
    await frame.locator(":root").evaluate((_, args) => {
      window.parent.postMessage(
        {
          type: "cm:compose",
          anchor: { kind: "section", id: "intro", tag: "h2" },
          file: args.id,
          fileLabel: "main",
        },
        "*",
      );
    }, { id });

    const compose = panel.locator(".cp__compose");
    await expect(compose).toBeVisible();
    await compose
      .getByRole("textbox", { name: "new comment body" })
      .fill("first via compose");
    await compose.getByRole("button", { name: "add comment" }).click();

    // Poll the review API until the comment lands.
    await expect
      .poll(
        async () => {
          const r = await request.get(
            `http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`,
          );
          if (r.status() !== 200) return 0;
          const file = (await r.json()) as { comments: { body: string }[] };
          return file.comments.filter((c) => c.body === "first via compose")
            .length;
        },
        { timeout: 5_000 },
      )
      .toBeGreaterThanOrEqual(1);

    // Reload + re-toggle annotate mode; the panel re-renders the comment.
    await page.reload();
    await page.locator('[data-kb-act="annotate"]').click();
    const newPanel = page.getByRole("complementary", { name: "comments" });
    await expect(newPanel.getByText("first via compose")).toBeVisible({
      timeout: 5_000,
    });
  });

  test("ignores cm:compose from a non-artifact origin", async ({ page }) => {
    const { rel } = await pickArtifact(page.request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await page.locator('[data-kb-act="annotate"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();

    // Fire from the parent's own origin (not the artifact subdomain). The
    // bridge's origin check must drop it, so no composer opens.
    await page.evaluate(() => {
      window.postMessage(
        {
          type: "cm:compose",
          anchor: { kind: "file" },
          file: "x",
          fileLabel: "main",
        },
        "*",
      );
    });
    await page.waitForTimeout(500);
    await expect(panel.locator(".cp__compose")).toHaveCount(0);
  });

  test("resolve toggle PATCH-style round-trip", async ({ page, request }) => {
    const { rel } = await pickArtifact(request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await page.locator('[data-kb-act="annotate"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();

    // Add a file-scope comment via the panel composer.
    const draft = panel.getByRole("textbox", { name: "file-scope comment" });
    await draft.fill("toggle-me");
    await panel.getByRole("button", { name: "add file-scope" }).click();

    // Wait for it to render.
    await expect(panel.getByText("toggle-me")).toBeVisible({ timeout: 5_000 });

    // Resolve it.
    const row = panel.locator(".cp__row", { hasText: "toggle-me" });
    await row.getByRole("button", { name: /resolve/ }).click();

    // Filter is "open" by default — it should disappear from the list.
    await expect(panel.getByText("toggle-me")).not.toBeVisible({
      timeout: 5_000,
    });

    // Switch to "all" — it should reappear with reduced opacity (resolved class).
    await panel.getByRole("tab", { name: /^all/ }).click();
    await expect(panel.getByText("toggle-me")).toBeVisible({ timeout: 5_000 });
  });

  test("annotate toggle preserves iframe state (no remount)", async ({
    page,
    request,
  }) => {
    // Regression test for the URL-keyed iframe remount: before the fix
    // the iframe `src` flipped between baseUrl and baseUrl?cm=on on
    // every pen click, which changed the React key and forced React
    // to recreate the <iframe>. With the fix, ?cm=on is appended at
    // first mount and never removed, so toggling annotate mode is a
    // pure postMessage — the iframe element + its DOM stay alive.
    const { rel } = await pickArtifact(request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    // Iframe should already carry ?cm=on at first paint (before any
    // pen click), since canon has comments enabled.
    const iframe = page.locator("iframe.detail__frame");
    await expect(iframe).toHaveAttribute("src", /cm=on/);

    // Stamp a sentinel inside the iframe's window. If the iframe
    // remounts on toggle, the new contentWindow won't have it.
    const frame = page.frameLocator(".detail__frame");
    await expect
      .poll(
        async () =>
          await frame.locator(":root").evaluate(() => !!window.__KB_COMMENTS),
        { timeout: 10_000 },
      )
      .toBe(true);
    await frame.locator(":root").evaluate(() => {
      // @ts-expect-error sentinel marker on the iframe window
      window.__KB_REMOUNT_SENTINEL = "alive";
    });

    // Toggle annotate mode on → off → on.
    const pen = page.locator('[data-kb-act="annotate"]');
    await pen.click();
    await expect(pen).toHaveAttribute("aria-pressed", "true");
    await pen.click();
    await expect(pen).toHaveAttribute("aria-pressed", "false");
    await pen.click();
    await expect(pen).toHaveAttribute("aria-pressed", "true");

    // Sentinel must survive all three toggles — proves the iframe
    // wasn't remounted.
    const survived = await frame.locator(":root").evaluate(
      // @ts-expect-error reading the sentinel back
      () => window.__KB_REMOUNT_SENTINEL,
    );
    expect(survived).toBe("alive");

    // Src is still the same — never lost ?cm=on, never gained a
    // second copy. Stable across toggles.
    await expect(iframe).toHaveAttribute("src", /cm=on/);
  });

  // Track H — in-page highlight band over the anchored text + the
  // bidirectional panel↔page link.
  test("anchored text gets a highlight band; hover glows it; marker click focuses the row", async ({
    page,
    request,
  }) => {
    const { rel } = await pickArtifact(request); // kitchen-sink
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await page.locator('[data-kb-act="annotate"]').click();
    await expect(
      page.getByRole("complementary", { name: "comments" }),
    ).toBeVisible();

    const frame = page.frameLocator(".detail__frame");
    await expect
      .poll(
        async () =>
          await frame.locator(":root").evaluate(() => !!window.__KB_COMMENTS),
        { timeout: 10_000 },
      )
      .toBe(true);

    // Author a section comment on #bench-table (exists in kitchen-sink):
    // dispatch cm:compose from the iframe, then write + add in the panel.
    await frame.locator(":root").evaluate(() => {
      window.parent.postMessage(
        {
          type: "cm:compose",
          anchor: { kind: "section", id: "bench-table", tag: "table" },
          file: location.host.split(".")[0],
          fileLabel: "main",
        },
        "*",
      );
    });
    const panel = page.getByRole("complementary", { name: "comments" });
    const compose = panel.locator(".cp__compose");
    await expect(compose).toBeVisible();
    await compose
      .getByRole("textbox", { name: "new comment body" })
      .fill("highlight me");
    await compose.getByRole("button", { name: "add comment" }).click();

    // A highlight band carrying the comment id is painted over the anchor
    // (the panel save → review.file change → cm:refresh repaints the iframe).
    const band = frame.locator(".kb-annot-hl[data-kb-comment-id]");
    await expect(band.first()).toBeAttached({ timeout: 5_000 });

    // The clickable comment icon is painted alongside the band.
    const icon = frame.locator(".kb-annot-icon[data-kb-comment-id]");
    await expect(icon.first()).toBeAttached({ timeout: 5_000 });

    // Hovering the panel row glows the band (cm:emphasize). `.first()`:
    // earlier specs in this file may have left other comments on the same
    // artifact (shared daemon state), and re-runs append another row.
    const row = page.locator(".cp__row", { hasText: "highlight me" }).first();
    await row.hover();
    await expect(
      frame.locator(".kb-annot-hl.kb-annot-active").first(),
    ).toBeAttached();
    // …and the icon glows too (it carries the same data-kb-comment-id).
    await expect(
      frame.locator(".kb-annot-icon.kb-annot-active").first(),
    ).toBeAttached();

    // Leave annotate mode (the panel now STAYS open — decoupled), then
    // click THIS comment's in-page gutter marker (selected by its title,
    // which carries the body) — the row goes active in the panel.
    await page.locator('[data-kb-act="annotate"]').click();
    await frame
      .locator('.kb-annot-marker[title*="highlight me"]')
      .first()
      .click();
    await expect(
      page.locator(".cp__row--active", { hasText: "highlight me" }).first(),
    ).toBeVisible();
  });
});

// Panel open/close is decoupled from the pencil (the comments toggle drives
// it), persists across in-session navigation, and the in-page comment icon
// links back to the panel without arming annotate mode.
test.describe("panel decoupling + bidirectional link", () => {
  test("comments toggle opens/closes the panel independently of the pencil", async ({
    page,
    request,
  }) => {
    const { rel } = await pickArtifact(request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    const panel = page.getByRole("complementary", { name: "comments" });
    const cmToggle = page.locator('[data-kb-act="dock-comments"]');
    const pen = page.locator('[data-kb-act="annotate"]');
    await expect(cmToggle).toBeVisible();

    // Panel starts closed; the toggle opens it WITHOUT arming the pencil.
    await expect(panel).toHaveCount(0);
    await cmToggle.click();
    await expect(panel).toBeVisible();
    await expect(pen).toHaveAttribute("aria-pressed", "false");

    // Pencil ON arms annotate mode with the panel already open.
    await pen.click();
    await expect(pen).toHaveAttribute("aria-pressed", "true");
    await expect(panel).toBeVisible();

    // Pencil OFF leaves the panel open — the decoupling.
    await pen.click();
    await expect(pen).toHaveAttribute("aria-pressed", "false");
    await expect(panel).toBeVisible();

    // Closing the panel hides it AND disarms the pencil. v0.22 — the panel's
    // own ✕ is gone; the rail's comments icon toggles it closed.
    await cmToggle.click();
    await expect(panel).toHaveCount(0);
    await expect(pen).toHaveAttribute("aria-pressed", "false");
    // U2 — the comments entry is the dock pill (role=tab): aria-selected, not
    // aria-pressed. Closing the panel returns the dock to inspect mode.
    await expect(cmToggle).toHaveAttribute("aria-selected", "false");
  });

  test("comments toggle does not remount the iframe", async ({
    page,
    request,
  }) => {
    const { rel } = await pickArtifact(request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    const iframe = page.locator("iframe.detail__frame");
    await expect(iframe).toHaveAttribute("src", /cm=on/);

    const frame = page.frameLocator(".detail__frame");
    await expect
      .poll(
        async () =>
          await frame.locator(":root").evaluate(() => !!window.__KB_COMMENTS),
        { timeout: 10_000 },
      )
      .toBe(true);
    await frame.locator(":root").evaluate(() => {
      // @ts-expect-error sentinel marker on the iframe window
      window.__KB_REMOUNT_SENTINEL = "alive";
    });

    // Open → close → open the panel via the toggle.
    const cmToggle = page.locator('[data-kb-act="dock-comments"]');
    const panel = page.getByRole("complementary", { name: "comments" });
    await cmToggle.click();
    await expect(panel).toBeVisible();
    await cmToggle.click();
    await expect(panel).toHaveCount(0);
    await cmToggle.click();
    await expect(panel).toBeVisible();

    const survived = await frame.locator(":root").evaluate(
      // @ts-expect-error reading the sentinel back
      () => window.__KB_REMOUNT_SENTINEL,
    );
    expect(survived).toBe("alive");
    await expect(iframe).toHaveAttribute("src", /cm=on/);
  });

  test("the comments toggle shows an open-comment count badge", async ({
    page,
    request,
  }) => {
    const { rel } = await pickArtifact(request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await page.locator('[data-kb-act="dock-comments"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();

    const body = `badge-${Date.now()}`;
    await panel.getByRole("textbox", { name: "file-scope comment" }).fill(body);
    await panel.getByRole("button", { name: "add file-scope" }).click();
    await expect(panel.getByText(body)).toBeVisible({ timeout: 5_000 });

    // The badge reflects open comments — at least the one we just added
    // (the artifact's review state is shared across specs, so assert >= 1
    // rather than an exact count).
    const badge = page.locator('[data-kb-act="comments-badge"]');
    await expect(badge).toBeVisible();
    await expect
      .poll(async () => {
        const t = (await badge.textContent())?.trim() ?? "0";
        return t === "99+" ? 100 : Number(t);
      })
      .toBeGreaterThanOrEqual(1);
  });

  test("clicking the in-page comment icon reopens the panel + activates the row, pencil stays off", async ({
    page,
    request,
  }) => {
    const { rel } = await pickArtifact(request); // kitchen-sink
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    // Pencil opens the panel + arms annotate so we can author a comment.
    await page.locator('[data-kb-act="annotate"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();

    const frame = page.frameLocator(".detail__frame");
    await expect
      .poll(
        async () =>
          await frame.locator(":root").evaluate(() => !!window.__KB_COMMENTS),
        { timeout: 10_000 },
      )
      .toBe(true);

    const body = `icon-focus-${Date.now()}`;
    await frame.locator(":root").evaluate(() => {
      window.parent.postMessage(
        {
          type: "cm:compose",
          anchor: { kind: "section", id: "bench-table", tag: "table" },
          file: location.host.split(".")[0],
          fileLabel: "main",
        },
        "*",
      );
    });
    const compose = panel.locator(".cp__compose");
    await expect(compose).toBeVisible();
    await compose.getByRole("textbox", { name: "new comment body" }).fill(body);
    await compose.getByRole("button", { name: "add comment" }).click();

    // The clickable icon is painted over the anchor (title carries the body).
    const icon = frame.locator(`.kb-annot-icon[title*="${body}"]`);
    await expect(icon.first()).toBeAttached({ timeout: 5_000 });

    // Close the panel; the pencil disarms too. v0.22 — closed via the rail's
    // comments icon (the panel's own ✕ is gone).
    await page.locator('[data-kb-act="dock-comments"]').click();
    await expect(panel).toHaveCount(0);
    const pen = page.locator('[data-kb-act="annotate"]');
    await expect(pen).toHaveAttribute("aria-pressed", "false");

    // Clicking the in-page icon reopens the panel and activates the row,
    // WITHOUT re-arming the pencil — focusing a comment is a read action.
    await icon.first().click();
    await expect(
      page.locator(".cp__row--active", { hasText: body }).first(),
    ).toBeVisible();
    await expect(pen).toHaveAttribute("aria-pressed", "false");
  });

  test("the open panel persists across in-session sibling navigation", async ({
    page,
    request,
  }) => {
    const { rel } = await pickArtifact(request); // kitchen-sink.html
    // Wait for the descendants fetch the `[`/`]` hotkey handler reads
    // from. CommentsPanel hides the PreviewInspector while open, so
    // .kb-pinsp__folder-list can't be the "descendants ready" sentinel —
    // wait on the network response instead. `projection=slim` is the
    // call's unique fingerprint (the gallery uses the default
    // projection, the descendants useEffect uses slim).
    const descendants = page.waitForResponse((r) =>
      /\/api\/kb\/canon\/docs\?.*projection=slim/.test(r.url()) && r.ok(),
    );
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await descendants;

    await page.locator('[data-kb-act="dock-comments"]').click();
    await expect(
      page.getByRole("complementary", { name: "comments" }),
    ).toBeVisible();

    // `]` is a client-side route change (no full reload), so the Detail
    // component stays mounted and `panelOpen` survives.
    await page.keyboard.press("]");

    // kitchen-sink.html → multi-page.html (filename sort order).
    await expect(page).toHaveURL(/multi-page\.html/);
    await expect(
      page.getByRole("complementary", { name: "comments" }),
    ).toBeVisible();
  });
});

// R2 — the expanded comment view (CommentModal). A native <dialog> opened
// with .showModal() (focus trap + backdrop + Escape), supporting read /
// edit-own / quote. Comments are authored via the panel composer (author
// "you", so editable). Bodies are unique per test — the daemon's review
// state is shared across specs on the same artifact.
test.describe("comment modal (R2)", () => {
  async function addFileScope(
    page: import("@playwright/test").Page,
    rel: string,
    body: string,
  ) {
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await page.locator('[data-kb-act="annotate"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();
    await panel.getByRole("textbox", { name: "file-scope comment" }).fill(body);
    await panel.getByRole("button", { name: "add file-scope" }).click();
    await expect(panel.getByText(body)).toBeVisible({ timeout: 5_000 });
    return panel;
  }

  test("⤢ expand opens a modal dialog; Escape closes it", async ({ page }) => {
    const { rel } = await pickArtifact(page.request);
    const panel = await addFileScope(page, rel, "modal-open-me");

    await panel
      .locator(".cp__row", { hasText: "modal-open-me" })
      .first()
      .getByRole("button", { name: /expand/ })
      .click();

    const dlg = page.locator("dialog.cp__modal");
    await expect(dlg).toBeVisible();
    await expect(dlg.getByText("modal-open-me")).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(dlg).toHaveCount(0);
  });

  test("edit own comment body saves, stamps edited, preserves replies", async ({
    page,
    request,
  }) => {
    const { id, rel } = await pickArtifact(page.request);
    const panel = await addFileScope(page, rel, "modal-edit-orig");

    await panel
      .locator(".cp__row", { hasText: "modal-edit-orig" })
      .first()
      .getByRole("button", { name: /expand/ })
      .click();
    const dlg = page.locator("dialog.cp__modal");
    await expect(dlg).toBeVisible();

    // Reply from inside the modal so we can prove the edit preserves it.
    await dlg.getByRole("textbox", { name: "reply body" }).fill("keep-this-reply");
    await dlg.getByRole("button", { name: "send reply" }).click();
    await expect(dlg.getByText("keep-this-reply")).toBeVisible({
      timeout: 5_000,
    });

    // Edit the body.
    await dlg.getByRole("button", { name: /edit/ }).click();
    const edit = dlg.getByRole("textbox", { name: "edit comment body" });
    await edit.fill("modal-edited-body");
    await dlg.getByRole("button", { name: "save" }).click();

    // Read view reflects the new body + an "edited" marker, reply intact.
    await expect(dlg.getByText("modal-edited-body")).toBeVisible({
      timeout: 5_000,
    });
    await expect(dlg.locator(".cp__row-edited")).toBeVisible();
    await expect(dlg.getByText("keep-this-reply")).toBeVisible();

    // Persisted: body changed, editedAt set, the reply survived the edit.
    const r = await request.get(
      `http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`,
    );
    const file = (await r.json()) as {
      comments: {
        body: string;
        editedAt: string | null;
        replies: { body: string }[];
      }[];
    };
    const c = file.comments.find((x) => x.body === "modal-edited-body");
    expect(c).toBeTruthy();
    expect(c!.editedAt).not.toBeNull();
    expect(c!.replies.some((rp) => rp.body === "keep-this-reply")).toBe(true);
  });

  test("quote all seeds the reply composer with a blockquote", async ({
    page,
  }) => {
    const { rel } = await pickArtifact(page.request);
    const panel = await addFileScope(page, rel, "quote-src-text");

    await panel
      .locator(".cp__row", { hasText: "quote-src-text" })
      .first()
      .getByRole("button", { name: /expand/ })
      .click();
    const dlg = page.locator("dialog.cp__modal");
    await expect(dlg).toBeVisible();

    await dlg.getByRole("button", { name: /quote all/ }).click();
    await expect(dlg.locator(".cp__reply-input")).toHaveValue(
      /> quote-src-text/,
    );
  });
});

// R3 — quick-response buttons. Choices are Claude-authored (CLI in real
// use); here the test seeds a Claude comment carrying choices directly via
// the review API, then drives the buttons in the browser. Clicking a
// choice must post a "you" reply (resolving when the choice says so) and
// must NOT strip the comment's choices on the save round-trip.
test.describe("comment choice buttons (R3)", () => {
  // R7 — seed a Claude comment (carrying choices) via the fine-grained
  // POST …/comments endpoint, the same path the SPA now uses. The daemon
  // assigns the id, so we return the created comment's id for the test to
  // assert against (the client no longer mints `c_<hex>` ids).
  async function seedClaudeComment(
    request: import("@playwright/test").APIRequestContext,
    id: string,
    comment: Record<string, unknown>,
  ): Promise<string> {
    const url = `http://127.0.0.1:${PORT}/api/kb/canon/review/${id}/comments`;
    const r = await request.post(url, { data: comment });
    expect(r.status()).toBe(201);
    const created = (await r.json()) as { id: string };
    return created.id;
  }

  test("renders choices on a Claude comment; clicking posts a you-reply + resolve, choices survive", async ({
    page,
    request,
  }) => {
    const { id, rel } = await pickArtifact(request);
    const cid = await seedClaudeComment(request, id, {
      anchor: { kind: "file" },
      author: "claude",
      body: "Apply the fix?",
      choices: [
        { label: "Apply", reply: "yes, apply it", resolve: true },
        { label: "Skip", reply: "skip for now" },
      ],
    });

    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await page.locator('[data-kb-act="annotate"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();

    const row = panel.locator(".cp__row", { hasText: "Apply the fix?" }).first();
    await expect(row.getByRole("button", { name: "Apply" })).toBeVisible();
    await expect(row.getByRole("button", { name: "Skip" })).toBeVisible();

    // Non-resolving choice → posts a "you" reply, comment stays open.
    await row.getByRole("button", { name: "Skip" }).click();
    await expect(row.getByText("skip for now")).toBeVisible({ timeout: 5_000 });

    const after1 = await (
      await request.get(`http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`)
    ).json();
    const c1 = after1.comments.find((c: { id: string }) => c.id === cid);
    expect(c1.status).toBe("open");
    expect(c1.choices).toHaveLength(2); // choices survived the round-trip
    expect(
      c1.replies.some(
        (r: { author: string; body: string }) =>
          r.author === "you" && r.body === "skip for now",
      ),
    ).toBe(true);

    // Resolving choice → posts a reply AND flips the comment to resolved.
    await row.getByRole("button", { name: "Apply" }).click();
    await expect
      .poll(
        async () => {
          const f = await (
            await request.get(
              `http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`,
            )
          ).json();
          const c = f.comments.find((x: { id: string }) => x.id === cid);
          return c?.status;
        },
        { timeout: 5_000 },
      )
      .toBe("resolved");

    const after2 = await (
      await request.get(`http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`)
    ).json();
    const c2 = after2.comments.find((c: { id: string }) => c.id === cid);
    expect(c2.choices).toHaveLength(2); // resolve must not strip choices
    expect(
      c2.replies.some((r: { body: string }) => r.body === "yes, apply it"),
    ).toBe(true);
  });
});

// Regression — the comments panel must FOLLOW a client-side artifact switch.
// Bug: `useReview` kept the PREVIOUS artifact's review file in React state
// until the new review GET resolved, so right after a `]` / in-iframe-link
// navigation the panel still rendered the SOURCE artifact's comments. The fix
// resets the hook's `file` the instant `(kb, artifactId)` changes, so the
// panel blanks to its loading aside and never shows a different artifact's
// comments. We hold the destination review GET open to keep the post-nav
// "stale window" wide enough to assert on deterministically (otherwise the
// fast localhost GET would close it before any assertion could observe it).
test.describe("comments panel follows artifact navigation (regression)", () => {
  async function docByName(
    request: import("@playwright/test").APIRequestContext,
    endsWith: string,
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
    const hit = docs.find((d) => d.path.endsWith(endsWith));
    expect(hit).toBeTruthy();
    return { id: hit!.id, rel: hit!.source_relative };
  }

  async function seedFileScope(
    request: import("@playwright/test").APIRequestContext,
    id: string,
    body: string,
  ): Promise<void> {
    const r = await request.post(
      `http://127.0.0.1:${PORT}/api/kb/canon/review/${id}/comments`,
      { data: { anchor: { kind: "file" }, author: "you", body } },
    );
    expect(r.status()).toBe(201);
  }

  test("`]` to a sibling swaps the panel to the new artifact's comments", async ({
    page,
    request,
  }) => {
    // kitchen-sink.html → multi-page.html is the `]` (filename-sort) hop, per
    // the panel-persistence spec above. Seed a UNIQUE file-scope comment on
    // each so assertions never collide with other specs' comments on the
    // shared (canon) review state.
    const a = await docByName(request, "kitchen-sink.html");
    const b = await docByName(request, "multi-page.html");
    const aBody = `nav-src-${Date.now()}`;
    const bBody = `nav-dst-${Date.now()}`;
    await seedFileScope(request, a.id, aBody);
    await seedFileScope(request, b.id, bBody);

    // Open the source artifact with the panel open; wait for the slim
    // descendants fetch the `]` handler reads from.
    const descendants = page.waitForResponse(
      (r) =>
        /\/api\/kb\/canon\/docs\?.*projection=slim/.test(r.url()) && r.ok(),
    );
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${a.rel}`);
    await descendants;
    await page.locator('[data-kb-act="dock-comments"]').click();
    const panelSrc = page.getByRole("complementary", { name: "comments" });
    await expect(panelSrc).toBeVisible();
    await expect(panelSrc.getByText(aBody)).toBeVisible({ timeout: 5_000 });

    // Hold every review GET issued from here on (the destination's, plus the
    // transient empty-id GET during the id=null nav step) open, so the
    // post-navigation stale window stays observable rather than racing the
    // network. Seeding above went through the `request` fixture, which page
    // routing does not intercept.
    await page.route("**/api/kb/canon/review/**", async (route) => {
      await new Promise((r) => setTimeout(r, 4_000));
      await route.continue().catch(() => {
        /* the client aborted the in-flight fetch when the id changed */
      });
    });

    // Client-side switch to the sibling (no reload — Detail stays mounted, so
    // the in-place useReview is what must re-target).
    await page.keyboard.press("]");
    await expect(page).toHaveURL(/multi-page\.html/);

    // While the destination review is still loading, the panel must NOT show
    // the SOURCE artifact's comment. Pre-fix it lingered for the whole GET;
    // post-fix the hook clears `file` the instant the id changes, so the
    // source comment is gone immediately — well under the 4s hold.
    const panelDst = page.getByRole("complementary", { name: "comments" });
    await expect(panelDst).toBeVisible();
    await expect(panelDst.getByText(aBody)).toHaveCount(0, { timeout: 2_500 });

    // Once the (held) destination review lands, its OWN comment renders and
    // the source comment is still absent.
    await expect(panelDst.getByText(bBody)).toBeVisible({ timeout: 9_000 });
    await expect(panelDst.getByText(aBody)).toHaveCount(0);
  });
});

// E1 — the shared Write | Preview tabbed editor (MarkdownEditor). The Write
// tab is a textarea (its aria-label/class preserved per surface); the
// Preview tab renders the same markdown the panel will, via CommentBody.
test.describe("markdown editor tabs (E1)", () => {
  test("Preview tab renders the Write tab's markdown", async ({ page }) => {
    const { rel } = await pickArtifact(page.request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await page.locator('[data-kb-act="annotate"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();

    const editor = panel.locator(".cp__file-scope .cp__editor");
    await editor
      .getByRole("textbox", { name: "file-scope comment" })
      .fill("**bold-preview**");
    await editor.getByRole("tab", { name: "Preview" }).click();
    await expect(editor.locator(".cp__editor-preview strong")).toHaveText(
      "bold-preview",
    );
    // Back to Write — the editor keeps the source (read via the value mirror).
    await editor.getByRole("tab", { name: "Write" }).click();
    await expect(editor.locator(".cp__file-scope-input")).toHaveValue(
      "**bold-preview**",
    );
  });
});

// W2.16-mobile — selectionchange-driven relay + selection→comment authoring.
//
// annotate.ts's mode-independent selection capture (cite / add-to-list /
// remember, and now comment) used to relay ONLY off `mouseup`. That misses
// any selection made by another means — most importantly the iOS Safari /
// Android Chrome gripper-handle drag, which fires zero mouse/pointer events
// once the long-press has started and refines the range purely via
// `selectionchange`. The fix adds a `selectionchange` listener (debounced
// 150ms, suppressed while a pointer is down so a desktop mouse-drag doesn't
// post dozens of half-made selections).
//
// These specs build the selection with `Range` + `Selection.addRange` and
// dispatch NO mouse/pointer/touch event at all, so Chromium's own
// `selectionchange` event is the ONLY thing that can carry it to the
// parent — a mouseup-only relay would never observe this, making it a
// faithful stand-in for the real gripper-drag gap without the flakiness of
// driving native touch gestures. It's also the first real coverage of the
// selection→comment authoring chain end to end (see this file's header
// comment: real click-through selection was cut as flaky in v0.2; a
// programmatic Range is the robust middle path).

// Distinctive plain-text substring from the paragraph immediately before
// #bench-table in kitchen-sink.html (`<p>Click a column header to sort. …`)
// — a single text node with no nested inline markup, so the Range built
// below is a plain start/end offset pair. Unique in the document (verified
// against the canon fixture).
const SELECTION_NEEDLE = "column header to sort";

// Locate the first text node under the iframe body containing `needle` and
// install a Range over it as the window selection. Deliberately fires no
// mouse/pointer event — Chromium dispatches `selectionchange` on its own
// once `addRange` lands, which is exactly the path this fix added.
async function selectSubstring(
  frame: import("@playwright/test").FrameLocator,
  needle: string,
): Promise<void> {
  await frame.locator(":root").evaluate((_, needle) => {
    const walker = document.createTreeWalker(
      document.body,
      NodeFilter.SHOW_TEXT,
    );
    let node: Text | null = null;
    let at = -1;
    let n: Node | null;
    while ((n = walker.nextNode())) {
      const t = n as Text;
      const i = t.data.indexOf(needle);
      if (i >= 0) {
        node = t;
        at = i;
        break;
      }
    }
    if (!node) throw new Error(`substring not found in iframe: ${needle}`);
    const range = document.createRange();
    range.setStart(node, at);
    range.setEnd(node, at + needle.length);
    const sel = window.getSelection();
    if (!sel) throw new Error("no Selection object in iframe");
    sel.removeAllRanges();
    sel.addRange(range);
  }, needle);
}

// Collapse the live selection — no mouseup, same rationale as above; only
// selectionchange can carry this to the parent as cm:selection-clear.
async function collapseSelection(
  frame: import("@playwright/test").FrameLocator,
): Promise<void> {
  await frame.locator(":root").evaluate(() => {
    window.getSelection()?.removeAllRanges();
  });
}

// `!!window.__KB_COMMENTS` (as the other describes in this file poll for)
// only proves the INLINE data-block script ran — that script is NOT
// `defer`, so it executes almost immediately, well before annotate.js
// (which IS `defer`, injected right after it — see kb-core's
// `iframe::inject_annotator`) has actually run and attached its
// `selectionchange`/`mouseup` listeners. The other specs in this file never
// notice: they simulate the annotator's output by posting `cm:compose`
// directly from the test, which needs no listener on the iframe side at
// all. These specs exercise the REAL listener, so they need a readiness
// signal for it specifically. The HTML spec guarantees every `defer` script
// has finished running before `DOMContentLoaded`, which in turn always
// precedes `readyState` reaching "complete" — so polling for "complete" is
// a sound proxy for "annotate.ts's `init()` has already attached its
// document-level listeners", closing a real (intermittently-observed) race
// where a selection made between navigation and defer-script execution
// fired `selectionchange` before anyone was listening for it.
//
// Evaluated through `frameEval` below, which tolerates the iframe remounting
// underneath the poll.
async function waitForAnnotatorReady(
  frame: import("@playwright/test").FrameLocator,
): Promise<void> {
  await expect
    .poll(async () => await frameEval(frame, () => !!window.__KB_COMMENTS), {
      timeout: 15_000,
    })
    .toBe(true);
  await expect
    .poll(async () => await frameEval(frame, () => document.readyState), {
      timeout: 15_000,
    })
    .toBe("complete");
}

/// Evaluate inside the artifact iframe, tolerating a remount mid-flight.
///
/// The detail route swaps the iframe `src` from `<origin>/` to
/// `<origin>/?cm=on` the moment the by-path doc query resolves (that is what
/// flips `reviewActive`), and the `src` is folded into the React `key`, so the
/// frame is genuinely torn down and rebuilt. Any `evaluate` in flight at that
/// instant dies with "Execution context was destroyed" — a THROWN error, which
/// `expect.poll` propagates instead of retrying, failing the test outright.
///
/// Chromium resolves the doc fast enough that the swap almost always lands
/// before the first poll; Firefox is slower and loses that race regularly,
/// which is how a browser-portability gap in the HARNESS masqueraded as three
/// flaky product specs. Folding the destroyed-context error into a falsy
/// result turns it back into an ordinary retry — the poll's own timeout still
/// fails the test if the condition genuinely never holds.
async function frameEval<T>(
  frame: import("@playwright/test").FrameLocator,
  fn: () => T,
): Promise<T | null> {
  try {
    return await frame.locator(":root").evaluate(fn);
  } catch {
    return null;
  }
}

test.describe("desktop: selectionchange relay + selection→comment @selection", () => {
  test("a programmatic selectionchange (no mouseup) reaches the floater; the comment button opens the composer on a selection anchor without arming annotate mode", async ({
    page,
    request,
  }) => {
    const { id, rel } = await pickArtifact(request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    const frame = page.frameLocator(".detail__frame");
    await waitForAnnotatorReady(frame);

    // The pencil is never touched anywhere in this test — the selection
    // capture is mode-INDEPENDENT (annotate.ts fires cm:selection in view
    // mode too), so this exercises the path with annotate mode off
    // throughout.
    const pen = page.locator('[data-kb-act="annotate"]');
    await expect(pen).toHaveAttribute("aria-pressed", "false");

    await selectSubstring(frame, SELECTION_NEEDLE);

    // Old mouseup-only code would never post this — no mouse event was
    // dispatched, only Chromium's native selectionchange from addRange.
    const floater = page.locator(".kb-selact");
    const commentBtn = page.locator('[data-kb-act="selection-comment"]');
    await expect(floater).toBeVisible({ timeout: 5_000 });
    await expect(commentBtn).toBeVisible();

    await commentBtn.click();

    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();
    const compose = panel.locator(".cp__compose");
    await expect(compose).toBeVisible();
    // The anchor label reflects the quoted selection snippet, not a
    // file/section anchor.
    await expect(compose.locator(".cp__compose-label")).toContainText(
      SELECTION_NEEDLE,
    );

    const body = `selection-comment-desktop-${Date.now()}`;
    await compose
      .getByRole("textbox", { name: "new comment body" })
      .fill(body);
    await compose.getByRole("button", { name: "add comment" }).click();

    await expect
      .poll(
        async () => {
          const r = await request.get(
            `http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`,
          );
          if (r.status() !== 200) return 0;
          const file = (await r.json()) as { comments: { body: string }[] };
          return file.comments.filter((c) => c.body === body).length;
        },
        { timeout: 5_000 },
      )
      .toBeGreaterThanOrEqual(1);

    const after = await (
      await request.get(`http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`)
    ).json();
    const created = after.comments.find((c: { body: string }) => c.body === body);
    expect(created.anchor.kind).toBe("selection");
    expect(created.anchor.snippet).toContain(SELECTION_NEEDLE);

    // Composing from a selection must never arm annotate mode — that
    // pencil turns on tap-anywhere-to-compose, which a selection-initiated
    // compose must not switch on (see detail.tsx's onComposeSelection).
    await expect(pen).toHaveAttribute("aria-pressed", "false");
  });
});

test.describe("mobile: bottom-bar selection→comment (390×844, hasTouch) @selection", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test("bottom-bar variant end to end: selection raises a fixed bottom bar; comment raises the sheet; composer persists the comment", async ({
    page,
    request,
  }) => {
    const { id, rel } = await pickArtifact(request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    const frame = page.frameLocator(".detail__frame");
    await waitForAnnotatorReady(frame);

    await selectSubstring(frame, SELECTION_NEEDLE);

    const bar = page.locator(".kb-selact--bar");
    await expect(bar).toBeVisible({ timeout: 5_000 });
    // Mobile drops the desktop floater's inline top/left entirely — the bar
    // is positioned purely via kb-selact--bar's CSS (fixed to the viewport
    // bottom), not per-selection rect tracking.
    await expect(bar).not.toHaveAttribute("style");

    const box = await bar.boundingBox();
    expect(box, "bar has a layout box").not.toBeNull();
    const viewport = page.viewportSize();
    expect(viewport, "viewport size is known").not.toBeNull();
    // inset: auto 0 0 0 pins the bar's bottom edge to the viewport bottom.
    expect(box!.y + box!.height).toBeGreaterThan(viewport!.height - 5);
    expect(box!.y + box!.height).toBeLessThanOrEqual(viewport!.height + 1);

    const detail = page.locator(".detail");
    await expect(detail).not.toHaveClass(/detail--inspector-open/);

    await page.locator('[data-kb-act="selection-comment"]').click();

    // The mobile reader-tools sheet rises with the composer inside it
    // (invariant #30 — panelMode "comments" rides the SAME `.kb-pinsp`
    // rail/sheet the desktop dock uses).
    await expect(detail).toHaveClass(/detail--inspector-open/);
    const sheet = page.locator(".kb-pinsp");
    await expect(sheet).toBeVisible();
    const compose = sheet.locator(".cp__compose");
    await expect(compose).toBeVisible();
    await expect(compose.locator(".cp__compose-label")).toContainText(
      SELECTION_NEEDLE,
    );

    const body = `selection-comment-mobile-${Date.now()}`;
    await compose
      .getByRole("textbox", { name: "new comment body" })
      .fill(body);
    await compose.getByRole("button", { name: "add comment" }).click();

    await expect
      .poll(
        async () => {
          const r = await request.get(
            `http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`,
          );
          if (r.status() !== 200) return 0;
          const file = (await r.json()) as { comments: { body: string }[] };
          return file.comments.filter((c) => c.body === body).length;
        },
        { timeout: 5_000 },
      )
      .toBeGreaterThanOrEqual(1);

    const after = await (
      await request.get(`http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`)
    ).json();
    const created = after.comments.find((c: { body: string }) => c.body === body);
    expect(created.anchor.kind).toBe("selection");
    expect(created.anchor.snippet).toContain(SELECTION_NEEDLE);
  });

  test("collapsing the selection clears the bottom bar via selectionchange (no mouseup)", async ({
    page,
  }) => {
    const { rel } = await pickArtifact(page.request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    const frame = page.frameLocator(".detail__frame");
    await waitForAnnotatorReady(frame);

    await selectSubstring(frame, SELECTION_NEEDLE);
    const bar = page.locator(".kb-selact--bar");
    await expect(bar).toBeVisible({ timeout: 5_000 });

    // No mouseup anywhere in this test — the clear relay depends entirely
    // on the new selectionchange listener (mouseup-only code would leave
    // the bar stuck open here).
    await collapseSelection(frame);

    await expect(bar).toBeHidden({ timeout: 5_000 });
  });

  // R3 — scrolling must NOT dismiss the mobile bar. The clear that used to
  // fire on every `kb:scroll` beacon existed solely to stop the DESKTOP
  // floater sitting at a stale frozen rect; the mobile variant is
  // `position: fixed` and never reads `rect`, so the clear bought nothing
  // there and cost a great deal — dragging a native selection handle toward
  // either screen edge auto-scrolls the document, which meant the act of
  // refining a selection destroyed the bar you were refining it for.
  test("scrolling the artifact keeps the bottom bar up (native handle drags auto-scroll)", async ({
    page,
  }) => {
    const { rel } = await pickArtifact(page.request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    const frame = page.frameLocator(".detail__frame");
    await waitForAnnotatorReady(frame);

    await selectSubstring(frame, SELECTION_NEEDLE);
    const bar = page.locator(".kb-selact--bar");
    await expect(bar).toBeVisible({ timeout: 5_000 });

    // The iframe runtime debounces its `kb:scroll` beacon ~500ms, so this
    // wait is what makes the assertion meaningful: it outlasts the beacon
    // the old code would have cleared the bar on.
    await frame.locator(":root").evaluate(() => window.scrollTo(0, 400));
    await page.waitForTimeout(1_200);

    await expect(bar).toBeVisible();
  });
});

// R1 — the Gecko wedge, in isolation.
//
// `pointerDown` suppresses the selectionchange relay so a desktop mouse drag
// doesn't post dozens of half-made selections. Before this fix ANY pointer
// type armed it, and the flag was only ever cleared by pointerup /
// pointercancel — events whose arrival at the end of a plain-text long-press
// selection is documented nowhere for Gecko (Firefox gates its touch-
// completion events on its own context menu having opened, Bugzilla 1481923,
// and the AccessibleCaret selection toolbar is a different path again). A
// touch pointerdown with no partner event therefore wedged the flag true for
// the life of the page and killed the relay outright.
//
// The fix makes the SET mouse-only, so touch can never arm what it might not
// be able to disarm. These two specs pin both halves of that contract; both
// run under the `firefox` project too (@selection), which is the whole point
// — Blink and Gecko disagree here and only one of them was ever tested.
test.describe("pointer-type suppression @selection", () => {
  // Dispatch a bare PointerEvent inside the iframe. No Playwright touch
  // gesture: we are testing what the annotator does with the events it
  // RECEIVES, and a synthetic event exercises exactly the listener under
  // test without depending on either engine's gesture recognizer.
  async function firePointer(
    frame: import("@playwright/test").FrameLocator,
    type: string,
    pointerType: string,
  ): Promise<void> {
    await frame.locator(":root").evaluate(
      (_, ev) => {
        document.dispatchEvent(
          new PointerEvent(ev.type, {
            pointerType: ev.pointerType,
            bubbles: true,
          }),
        );
      },
      { type, pointerType },
    );
  }

  test("a TOUCH pointerdown never suppresses the relay (no pointerup needed)", async ({
    page,
  }) => {
    const { rel } = await pickArtifact(page.request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    const frame = page.frameLocator(".detail__frame");
    await waitForAnnotatorReady(frame);

    // Down, and NOTHING else — no pointerup, no pointercancel, no mouseup.
    // This is the exact shape of the gesture we cannot prove Firefox for
    // Android completes.
    await firePointer(frame, "pointerdown", "touch");
    await selectSubstring(frame, SELECTION_NEEDLE);

    // Pre-fix this stayed hidden forever: the flag was armed and nothing
    // was ever going to clear it.
    await expect(page.locator(".kb-selact")).toBeVisible({ timeout: 5_000 });
  });

  test("a MOUSE pointerdown still suppresses the relay until the drag ends", async ({
    page,
  }) => {
    const { rel } = await pickArtifact(page.request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    const frame = page.frameLocator(".detail__frame");
    await waitForAnnotatorReady(frame);

    await firePointer(frame, "pointerdown", "mouse");
    await selectSubstring(frame, SELECTION_NEEDLE);

    // Wait past the 150ms relay debounce, THEN assert — a bare toBeHidden()
    // would pass instantly at t=0 and prove nothing about suppression
    // actually holding.
    const floater = page.locator(".kb-selact");
    await page.waitForTimeout(400);
    await expect(floater).toBeHidden();

    // End the drag the way a real mouse does: pointerup disarms the flag,
    // mouseup is what re-triggers the relay.
    await firePointer(frame, "pointerup", "mouse");
    await frame
      .locator(":root")
      .evaluate(() =>
        document.dispatchEvent(new MouseEvent("mouseup", { bubbles: true })),
      );

    await expect(floater).toBeVisible({ timeout: 5_000 });
  });
});

// R4 — the PULL lane, end to end.
//
// This is the fallback that has to work when every event-timing assumption
// fails. The spec reproduces the real Firefox-for-Android failure mode on
// whatever engine it runs on, deterministically: make a selection, then
// destroy it (which is what Gecko's collapse-before-click does to the user
// mid-tap), confirm the push-driven bar is gone, and then author the comment
// anyway through the sheet — served by annotate.ts's cached anchor.
test.describe("mobile: pull-based comment-on-selection @selection", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test("a collapsed selection still composes via the sheet's pull button", async ({
    page,
    request,
  }) => {
    const { id, rel } = await pickArtifact(request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    const frame = page.frameLocator(".detail__frame");
    await waitForAnnotatorReady(frame);

    await selectSubstring(frame, SELECTION_NEEDLE);
    const bar = page.locator(".kb-selact--bar");
    await expect(bar).toBeVisible({ timeout: 5_000 });

    // Blow the live selection away, then outlast BOTH clear paths: the 150ms
    // fine-pointer debounce and the ~600ms coarse-pointer slow clear. What
    // survives is the cache — which is the entire point.
    await collapseSelection(frame);
    await page.waitForTimeout(900);
    await expect(bar).toBeHidden();

    // Invariant #30 — mobile chrome is ONE button and ONE sheet: the
    // ContextBar `inspect` toggle raises it, the rail switches bodies from
    // inside it. The pull button lives in the comments body, not on a second
    // ContextBar icon.
    await page.locator('[data-kb-act="inspect"]').click();
    await page.locator('[data-kb-act="dock-comments"]').click();

    const pull = page.locator('[data-kb-act="comment-selection-pull"]');
    await expect(pull).toBeVisible();
    await pull.click();

    const compose = page.locator(".kb-pinsp .cp__compose");
    await expect(compose).toBeVisible({ timeout: 5_000 });
    // The anchor came from the CACHE, so it must still name the text the
    // user actually highlighted — not a file/section fallback.
    await expect(compose.locator(".cp__compose-label")).toContainText(
      SELECTION_NEEDLE,
    );

    const body = `selection-pull-mobile-${Date.now()}`;
    await compose.getByRole("textbox", { name: "new comment body" }).fill(body);
    await compose.getByRole("button", { name: "add comment" }).click();

    await expect
      .poll(
        async () => {
          const r = await request.get(
            `http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`,
          );
          if (r.status() !== 200) return 0;
          const file = (await r.json()) as { comments: { body: string }[] };
          return file.comments.filter((c) => c.body === body).length;
        },
        { timeout: 5_000 },
      )
      .toBeGreaterThanOrEqual(1);

    const after = await (
      await request.get(`http://127.0.0.1:${PORT}/api/kb/canon/review/${id}`)
    ).json();
    const created = after.comments.find((c: { body: string }) => c.body === body);
    expect(created.anchor.kind).toBe("selection");
    expect(created.anchor.snippet).toContain(SELECTION_NEEDLE);
  });

  test("pulling with nothing ever selected says so instead of doing nothing", async ({
    page,
  }) => {
    const { rel } = await pickArtifact(page.request);
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);

    const frame = page.frameLocator(".detail__frame");
    await waitForAnnotatorReady(frame);

    await page.locator('[data-kb-act="inspect"]').click();
    await page.locator('[data-kb-act="dock-comments"]').click();
    await page.locator('[data-kb-act="comment-selection-pull"]').click();

    // Invariant #32 — a user action that can't proceed never fails silently.
    // No composer opens, and the reason is on screen.
    await expect(
      page
        .locator('[data-kb-toast="err"]')
        .filter({ hasText: /select some text/i }),
    ).toBeVisible({ timeout: 5_000 });
    await expect(page.locator(".kb-pinsp .cp__compose")).toBeHidden();
  });
});
