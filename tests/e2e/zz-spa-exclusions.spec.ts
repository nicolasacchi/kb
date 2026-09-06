import { test, expect } from "@playwright/test";
import { writeFileSync, unlinkSync, existsSync } from "node:fs";
import { join } from "node:path";
import { PORT, BASE } from "./helpers";

// v0.24 X4 — per-file exclusion, SPA roundtrip.
//
// Exclude from a gallery card → the card leaves over SSE (no reload) but
// the FILE stays on disk (KeepUserData cascade, decision D3) → the
// Settings → Excluded pane lists it → row-level Include → the doc
// returns to the gallery with its review comment intact. A second test
// drives the reader's About-tab exclude action (the other SPA home).
//
// Named `zz-` so it runs after the doc-count-sensitive specs: it writes
// its own probe artifact into the watched corpus (same pattern as
// zz-live-fs) and restores the baseline on the way out.
test.describe("per-file exclusion (X4)", () => {
  const CORPUS = process.env.KB_E2E_CORPUS;
  const PROBE_NAME = "exclusion-probe.html";
  const PROBE_TITLE = "Exclusion Probe Artifact";
  const COMMENT_MARKER = "survives-exclusion-roundtrip";
  const PROBE_HTML = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>${PROBE_TITLE}</title>
    <meta name="kb-tags" content="e2e, exclusion" />
    <meta name="kb-category" content="notes" />
  </head>
  <body>
    <h1>${PROBE_TITLE}</h1>
    <p>Written by zz-spa-exclusions.spec.ts to exercise the X4 roundtrip.</p>
  </body>
</html>
`;

  const probePath = () => join(CORPUS as string, PROBE_NAME);

  test.afterEach(async ({ request }) => {
    // Restore baseline even on mid-test failure: clear any leftover
    // exclusion row first (a DELETE on a non-excluded path is a cheap
    // idempotent no-op → was_excluded:false), then remove the probe.
    try {
      await request.delete(
        `${BASE}/api/kb/canon/exclusions/${encodeURIComponent(PROBE_NAME)}`,
      );
    } catch {
      // daemon unreachable — nothing more we can do here
    }
    try {
      unlinkSync(probePath());
    } catch {
      // already gone / never written
    }
  });

  /** Resolve the probe's artifact id once it is indexed (id is stable
   *  across exclude/include — invariant #27: ids are source-relative). */
  async function probeDocId(
    request: import("@playwright/test").APIRequestContext,
  ): Promise<string> {
    const r = await request.get(`${BASE}/api/kb/canon/docs?limit=100`);
    expect(r.ok()).toBeTruthy();
    const docs = (await r.json()) as { id: string; source_relative: string }[];
    const hit = docs.find((d) => d.source_relative === PROBE_NAME);
    expect(hit, `probe ${PROBE_NAME} must be indexed`).toBeTruthy();
    return (hit as { id: string }).id;
  }

  test("gallery exclude → SSE removal → Settings pane → include → returns with comment intact", async ({
    page,
    request,
  }) => {
    expect(
      CORPUS,
      "KB_E2E_CORPUS must be exported by global-setup.ts",
    ).toBeTruthy();

    const cardLink = page.getByRole("link", { name: new RegExp(PROBE_TITLE) });
    const card = page.locator(".kb-card").filter({ has: cardLink });

    await page.goto(`http://127.0.0.1:${PORT}/`);
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();
    await expect(card).toHaveCount(0);
    // Let the SSE stream settle before mutating the corpus (zz-live-fs
    // precedent) so no frame lands before the browser subscribes.
    await page.waitForTimeout(1500);

    // Seed the probe artifact + one review comment on it.
    writeFileSync(probePath(), PROBE_HTML, "utf-8");
    await expect(card).toBeVisible({ timeout: 20_000 });
    const docId = await probeDocId(request);
    const posted = await request.post(
      `${BASE}/api/kb/canon/review/${docId}/comments`,
      {
        data: {
          body: COMMENT_MARKER,
          anchor: { kind: "file" },
          author: "you",
        },
      },
    );
    expect(posted.ok()).toBeTruthy();

    // Exclude from the card's hover action; the confirm modal is the one
    // destructive-prompt host (invariant #32 — click .confirm__go).
    await card.hover();
    await card.locator(".kb-card__exclude").click();
    await page.locator(".confirm__go").click();

    // The card leaves via SSE (artifact.removed → docsGate) — NO reload —
    // and the source file is untouched on disk (exclusion ≠ deletion).
    await expect(card).toHaveCount(0, { timeout: 20_000 });
    expect(existsSync(probePath())).toBe(true);

    // Settings → Excluded lists the row.
    await page.goto(`http://127.0.0.1:${PORT}/settings#excluded`);
    const kbSection = page.getByRole("region", {
      name: "exclusions for canon",
    });
    const row = kbSection.locator("tr").filter({ hasText: PROBE_NAME });
    await expect(row).toBeVisible({ timeout: 10_000 });

    // Row-level include → the daemon confirms with artifact.included →
    // the SSE bridge invalidates ["exclusions", canon] and the row leaves.
    await row.getByRole("button", { name: /include/ }).click();
    await expect(row).toHaveCount(0, { timeout: 20_000 });

    // Back in the gallery the doc returns (include = reindex nudge →
    // artifact.indexed → docsGate).
    await page.goto(`http://127.0.0.1:${PORT}/`);
    await expect(card).toBeVisible({ timeout: 20_000 });

    // The comment survived the roundtrip (KeepUserData cascade, D3).
    const review = await request.get(
      `${BASE}/api/kb/canon/review/${docId}`,
    );
    expect(review.ok()).toBeTruthy();
    const file = (await review.json()) as { comments: { body: string }[] };
    expect(
      file.comments.filter((c) => c.body === COMMENT_MARKER),
    ).toHaveLength(1);
  });

  test("reader About-tab exclude leaves the reader and pulls the doc from the gallery", async ({
    page,
    request,
  }) => {
    expect(CORPUS).toBeTruthy();
    const cardLink = page.getByRole("link", { name: new RegExp(PROBE_TITLE) });
    const card = page.locator(".kb-card").filter({ has: cardLink });

    await page.goto(`http://127.0.0.1:${PORT}/`);
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();
    await page.waitForTimeout(1500);
    writeFileSync(probePath(), PROBE_HTML, "utf-8");
    await expect(card).toBeVisible({ timeout: 20_000 });

    // Open the reader; the desktop inspector rail is always docked at the
    // default 1280×720 viewport. Force the About sub-tab (fresh contexts
    // default to it, but the choice persists — don't depend on that).
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${PROBE_NAME}`);
    await page.locator('[data-kb-itab="all"]').click();
    await page.locator('[data-kb-act="exclude"]').click();
    await page.locator(".confirm__go").click();

    // Success navigates back to the kb gallery (the doc is about to 404)…
    await expect(page).toHaveURL(/\/\?kb=canon/, { timeout: 10_000 });
    // …where the probe card is gone but the canon baseline is intact.
    await expect(card).toHaveCount(0, { timeout: 20_000 });
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();

    // Re-include over the API (the CLI/agent path) — the doc comes back
    // live, proving the SPA reacts to non-SPA includes too.
    const r = await request.delete(
      `${BASE}/api/kb/canon/exclusions/${encodeURIComponent(PROBE_NAME)}`,
    );
    expect(r.ok()).toBeTruthy();
    await expect(card).toBeVisible({ timeout: 20_000 });
  });
});
