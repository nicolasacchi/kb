import { test, expect, type Page } from "@playwright/test";
import { PORT } from "./helpers";

// W3.C-b — the multi-facet reflection canvas (`?view=canvas`): four
// synchronized UTC-day tracks (created · read · sessions · comments) sharing
// ONE brush, whose brushed set pivots into the gallery through the
// already-shipped `?ids=` atom (invariant #35 — the canvas adds no filter
// grammar of its own).
//
// The journey below drives the KEYBOARD brush, never a pointer drag: the two
// `role="slider"` handles are the accessible path AND the deterministic one
// (a synthetic drag over a 90-column grid is flaky in Playwright, and a
// one-pixel rounding difference silently changes the brushed day).
//
// The fixture daemon is shared with the other specs, so this file asserts
// only things it owns: the canon corpus's own artifacts (copied at
// global-setup, hence a TODAY mtime → the `created` lane's last day), the
// lane frame, and the pivot URL. It never asserts an absolute count on the
// read/comment lanes, which other specs write into.

const base = () => `http://127.0.0.1:${PORT}`;

/// The canvas's UTC day axis ends on "today" (the component pins its window
/// at mount off `Date.now()`), which is also the day the fixture files were
/// copied — so the last column is where the canon artifacts live.
async function gotoCanvas(page: Page) {
  await page.goto(`${base()}/?view=canvas&kb=canon`);
  await expect(page.locator(".rcanvas")).toBeVisible();
  // Wait for the fetched lanes rather than the loading frame.
  await expect(page.locator(".rcanvas__lane")).toHaveCount(4);
  await expect(page.locator(".rcanvas__status")).toHaveCount(0);
}

// PF-F1 — poll the shared daemon's index-run state to "idle" through the
// SPA's own debug/e2e surface (`window.__KB_SSE__`, web/src/api/sse.ts),
// which derives `phase` from the SAME index.start/index.file/artifact.
// indexed/index.complete balance queryClient.ts's SSE→invalidation bridge
// reacts to. Same pattern as multidaemon.spec.ts / sse-shared.spec.ts's
// `expect.poll(...).toEqual("idle")` — reused here to drain a still-running
// canon reindex (kicked off by an earlier spec in this single-worker suite)
// before counting requests a test expects to be zero.
async function waitForDaemonIdle(page: Page) {
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            (
              window as unknown as {
                __KB_SSE__?: { status(): { phase: string } };
              }
            ).__KB_SSE__?.status().phase,
        ),
      { timeout: 20_000 },
    )
    .toBe("idle");
}

test.describe("reflection canvas", () => {
  test("renders four synchronized lanes, labeling the empty sessions lane honestly", async ({
    page,
  }) => {
    await gotoCanvas(page);

    const tracks = await page
      .locator(".rcanvas__lane")
      .evaluateAll((els) => els.map((e) => e.getAttribute("data-track")));
    expect(tracks).toEqual(["created", "read", "session", "comment"]);

    // Every lane draws the SAME number of day cells — the "synchronized"
    // contract (one shared axis, zero-filled).
    const cellCounts = await page
      .locator(".rcanvas__track")
      .evaluateAll((els) => els.map((e) => e.childElementCount));
    expect(new Set(cellCounts).size).toBe(1);
    expect(cellCounts[0]).toBeGreaterThan(1);

    // Non-goal check, in UI form: the sessions lane is EMPTY on this content
    // corpus (session rows live in the kb holding the transcript) and says so
    // instead of being hidden.
    const session = page.locator('.rcanvas__lane[data-track="session"]');
    await expect(session).toBeVisible();
    await expect(session.locator(".rcanvas__lane-label")).toContainText(
      /empty outside a sessions corpus/,
    );

    // Calm-computing contract: density + evidence only.
    const text = (await page.locator(".rcanvas").innerText()).toLowerCase();
    for (const forbidden of ["streak", "best day", "goal", "complete", "%"]) {
      expect(text).not.toContain(forbidden);
    }
  });

  test("the KEYBOARD brush narrows the window and pivots to a gallery ids= filter", async ({
    page,
    request,
  }) => {
    // The id the pivot URL must carry: a canon artifact created (copied) today.
    const docs = await (
      await request.get(`${base()}/api/kb/canon/docs?limit=200`)
    ).json();
    const borrow = (docs as Array<{ id: string; title: string }>).find((d) =>
      /Borrow Checker/.test(d.title),
    );
    expect(borrow, "canon fixture must expose the borrow-checker artifact").toBeTruthy();

    await gotoCanvas(page);

    // PF-F1 — settings-dashboard.spec.ts's "Pipeline reindex button" test
    // (runs earlier in this single-worker suite, alphabetically before
    // this file) POSTs /api/kb/canon/reindex and returns as soon as the
    // request lands, NOT once the daemon's walk finishes. Under CI runner
    // load that walk can still be emitting `index.file`/`artifact.indexed`
    // when this test's page opens its own fresh SSE connection —
    // queryClient.ts's `docsGate` burst-invalidates `["timeline", kb]` on
    // every one of those, which the mounted ReflectionCanvas refetches.
    // That's a legitimate background refetch, not the brush fetching
    // (invariant #23 — dragging refetches nothing); it was previously
    // miscounted as a brush-caused call. Drain it deterministically before
    // trusting the zero-fetch count: the SPA already tracks active index
    // runs client-side (web/src/sse/core.ts's index.start/index.complete
    // balance) and exposes the aggregated phase at window.__KB_SSE__ for
    // exactly this — same helper shape as multidaemon.spec.ts /
    // sse-shared.spec.ts's `expect.poll(...).toEqual("idle")`.
    await waitForDaemonIdle(page);

    const start = page.getByRole("slider", { name: "brush start" });
    const end = page.getByRole("slider", { name: "brush end" });
    await expect(start).toBeVisible();
    const wholeWindowStart = await start.getAttribute("aria-valuetext");
    const lastDay = await end.getAttribute("aria-valuetext");

    // Runs the five keyboard brush moves + their intra-sequence
    // assertions, counting /timeline requests observed during it. `End`
    // always jumps to an absolute position first, so the sequence is
    // idempotent and safe to replay from any prior brush state (needed
    // for the retry below — no re-navigation required).
    async function runBrushSequence(): Promise<number> {
      let calls = 0;
      const onRequest = (r: import("@playwright/test").Request) => {
        if (r.url().includes("/timeline")) calls += 1;
      };
      page.on("request", onRequest);
      try {
        // End on the START handle collapses the brush onto the window's
        // last UTC day — the day the fixture corpus was written.
        await start.focus();
        await start.press("End");
        await expect(start).toHaveAttribute("aria-valuetext", lastDay!);
        expect(await start.getAttribute("aria-valuetext")).not.toBe(
          wholeWindowStart,
        );
        await expect(page.locator(".rcanvas__summary")).toContainText("1 day");

        // Widen it again by a week from the start handle (arrows + PageDown
        // are the same handler; PageDown is the 7-day step).
        await start.press("PageDown");
        await expect(page.locator(".rcanvas__summary")).toContainText("8 days");
        await start.press("ArrowRight");
        await expect(page.locator(".rcanvas__summary")).toContainText("7 days");

        // The accessible date fallback mirrors the same brush.
        const fromInput = page
          .locator('.rcanvas__date input[type="date"]')
          .first();
        await expect(fromInput).toHaveValue(
          (await start.getAttribute("aria-valuetext"))!,
        );
      } finally {
        page.off("request", onRequest);
      }
      return calls;
    }

    let timelineCalls = await runBrushSequence();
    if (timelineCalls > 0) {
      // PF-F1 — a nonzero count here is ambiguous: it's either the
      // background-churn refetch described above landing mid-sequence
      // despite the up-front idle wait (SSE delivery isn't instantaneous),
      // or a real regression. Re-settle and replay ONCE — a bounded,
      // documented retry for the diagnosed orthogonal cause, not a blind
      // test retry. A second nonzero count is treated as real and fails
      // the assertion below.
      await waitForDaemonIdle(page);
      timelineCalls = await runBrushSequence();
    }

    // invariant:23 — the brush is pure client state over already-fetched
    // buckets. Five brush moves, zero refetches.
    expect(timelineCalls).toBe(0);

    // The pivot resolves the brushed window's id set and lands on the
    // gallery through galleryUrl's shipped `ids=` atom.
    await page.locator('[data-kb-act="canvas-pivot"]').click();
    await expect(page).toHaveURL(/[?&]ids=/);
    await expect(page).toHaveURL(new RegExp(borrow!.id));
    await expect(page).toHaveURL(/kb=canon/);
    // …and the gallery actually renders that id-filtered set.
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();
  });

  test("history view cross-links to the canvas, keeping the active kb", async ({
    page,
  }) => {
    await page.goto(`${base()}/?view=history&kb=canon`);
    await page.locator(".rcanvas__crosslink").click();
    await expect(page).toHaveURL(/view=canvas/);
    await expect(page).toHaveURL(/kb=canon/);
    await expect(page.locator(".rcanvas")).toBeVisible();
  });

  test("deselecting every track disables the pivot with a plain sentence", async ({
    page,
  }) => {
    await gotoCanvas(page);
    for (const track of ["created", "read", "session", "comment"]) {
      await page
        .locator(`.rcanvas__lane[data-track="${track}"] input[type="checkbox"]`)
        .uncheck();
    }
    const pivot = page.locator('[data-kb-act="canvas-pivot"]');
    await expect(pivot).toBeDisabled();
    await expect(page.locator(".rcanvas__pivot-note")).toContainText(
      /select at least one track/,
    );
  });

  // W3 C-c — scenes: a named, restorable canvas brush riding the EXISTING
  // saved-query store. Drives the KEYBOARD brush only (same rationale as the
  // pivot test above); the save step's name entry is the one native
  // `window.prompt` this journey touches — the DELETE step must go through
  // useConfirm()'s `<dialog class="confirm">` (invariant #32), never a
  // second native dialog.
  test("save a scene, restore it exactly, then delete it", async ({ page }) => {
    const sceneName = `e2e scene ${Date.now()}`;
    page.on("dialog", (d) => {
      if (d.type() === "prompt") void d.accept(sceneName);
    });

    await gotoCanvas(page);

    const start = page.getByRole("slider", { name: "brush start" });
    const end = page.getByRole("slider", { name: "brush end" });
    await start.focus();
    await start.press("End"); // collapse onto the window's last UTC day
    await start.press("PageDown"); // widen 7 days back from there
    await expect(page.locator(".rcanvas__summary")).toContainText("8 days");
    const fromDay = await start.getAttribute("aria-valuetext");
    const toDay = await end.getAttribute("aria-valuetext");

    // Deselect a track too, so the saved scene carries a non-default
    // selection and restoring it is a genuine round trip, not a no-op.
    const commentToggle = page.locator(
      '.rcanvas__lane[data-track="comment"] input[type="checkbox"]',
    );
    await commentToggle.uncheck();

    await page.locator('[data-kb-act="scene-save"]').click();
    const chip = page.locator(`[data-kb-scene="${sceneName}"]`);
    await expect(chip).toBeVisible();

    // Perturb the view so restoring the scene is provably doing something:
    // back to the whole window, and the deselected track re-checked.
    await page.locator(".rcanvas__reset").click();
    await commentToggle.check();
    await expect(page.locator(".rcanvas__summary")).not.toContainText("8 days");

    await chip.click();
    await expect(page).toHaveURL(/[?&]view=canvas/);
    await expect(page).toHaveURL(/[?&]kb=canon/);
    await expect(page.locator(".rcanvas__summary")).toContainText("8 days");
    await expect(start).toHaveAttribute("aria-valuetext", fromDay!);
    await expect(end).toHaveAttribute("aria-valuetext", toDay!);
    await expect(commentToggle).not.toBeChecked();

    // Delete — the ONLY destructive prompt is useConfirm()'s modal.
    await page
      .locator(`[aria-label="delete scene ${sceneName}"]`)
      .click();
    await page.locator("dialog.confirm .confirm__go").click();
    await expect(chip).toHaveCount(0);
  });
});
