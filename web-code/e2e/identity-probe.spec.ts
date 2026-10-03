import { expect, test } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// v0.44 X2 (A9.f2) - the review-write pre-probe flips WITHOUT a reload.
///
/// `GET /api/identity` carries `review_mutations_admitted`, computed per
/// request from the caller's peer classification. The SPA caches it with
/// `staleTime: Infinity`, so the only thing that can refresh it is the SSE
/// reconnect (`queryClient.ts`: `source.onopen` invalidates everything,
/// `["identity"]` included). This drives exactly that: the probe answers
/// `false` until the event stream has been (re)opened twice, the first stream
/// is cut short, and the verdict controls must go from disabled to enabled
/// with no navigation in between.
test.describe("identity pre-probe flips false -> true without a reload (A9.f2)", () => {
  test("verdict buttons enable after the SSE stream reconnects", async ({ page, request }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set - global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: "feature-x",
        base_ref: "main",
        title: "e2e identity probe",
      },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const { id: reviewId } = (await createRes.json()) as { id: number };

    let streams = 0;
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route("**/api/events", async (route) => {
      streams += 1;
      if (streams === 1) {
        // Held until the test says so (the page sees a stream that never
        // opens), then a stream that opens and ends at once; `retry:` keeps
        // the browser's automatic reconnect quick.
        await gate;
        await route.fulfill({
          status: 200,
          headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
          body: "retry: 200\n\n",
        });
        return;
      }
      await route.continue();
    });
    await page.route("**/api/identity", async (route) => {
      const upstream = await route.fetch();
      const body = (await upstream.json()) as Record<string, unknown>;
      await route.fulfill({
        response: upstream,
        json: { ...body, review_mutations_admitted: streams >= 2 },
      });
    });

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
    await expect(page.locator("[data-kbc-review-verdict-bar]")).toBeVisible({ timeout: 15_000 });

    // Refused while the daemon says so ...
    await expect(page.locator('[data-kbc-review-verdict="approve"]')).toBeDisabled();
    await expect(page.locator("[data-kbc-review-verdict-loopback]")).toBeVisible();

    // ... enabled once the reconnect re-asked - same page, no goto between.
    release();
    await expect(page.locator('[data-kbc-review-verdict="approve"]')).toBeEnabled({
      timeout: 20_000,
    });
    expect(streams).toBeGreaterThanOrEqual(2);
    await expect(page.locator("[data-kbc-review-verdict-loopback]")).toHaveCount(0);
  });
});
