import { expect, test, type APIRequestContext } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";
import { LOCAL_TARGET_FN, RESOLVER_FILE } from "./fixture-repo";

/// V74-L3b — the kbc-tour/1 SURFACE: a tour applied through the loopback
/// route, then listed, walked, and re-entered through its own linked-tab chip.
///
/// The tour deliberately carries one of each interesting shape: a resolvable
/// `code` step authored through the `ref` STRING sugar, a second authored
/// through the STRUCTURED fields (so both authoring forms are proved on one
/// document), an ORPHAN pointing at a file that does not exist (which must be
/// a VISIBLE step saying the code is gone), and a prose-only step. A camera on
/// step one proves the hint is rendered rather than applied as geometry.
///
/// Mutations are loopback-only server-side; this harness hits 127.0.0.1, so
/// `apply` works exactly as `kb-code tour apply` would.

const SLUG = "e2e-tour";

function tourDoc() {
  return {
    schema: "kbc-tour/1",
    repo: REPO_NAME,
    slug: SLUG,
    title: "E2E tour",
    description_md: "the fixture's own walk",
    ref: "main",
    steps: [
      {
        id: "entry",
        title: "the definition",
        body_md: "everything starts here",
        // The `ref` STRING sugar — lowered server-side by the ONE ref parser.
        ref: `code:${RESOLVER_FILE}:1-4`,
        camera: { fold: false, context: 3 },
      },
      {
        id: "second",
        title: "and its neighbour",
        // The STRUCTURED form, byte-identical to a board node's reference.
        kind: "code",
        path: RESOLVER_FILE,
        range: [1, 2],
      },
      {
        id: "gone",
        title: "a file that is gone",
        ref: "code:no-such-file.rs:1-2",
      },
      { id: "why", title: "why it matters", body_md: "**because** the fixture says so" },
    ],
  };
}

async function applyTour(request: APIRequestContext) {
  const res = await request.post(`${BASE}/api/tours/apply`, {
    headers: { "X-Kbc-Request": "1", "Content-Type": "application/json" },
    data: tourDoc(),
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
  return res.json();
}

test.describe("kbc-tour/1 tours", () => {
  test.beforeAll(async ({ request }) => {
    await applyTour(request);
  });

  test.afterAll(async ({ request }) => {
    await request
      .delete(`${BASE}/api/tours/${SLUG}?repo=${REPO_NAME}`, {
        headers: { "X-Kbc-Request": "1" },
      })
      .catch(() => undefined);
  });

  test("the list shows the tour with ONE count, and links Boards as its sibling", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~tours`);
    const row = page.locator(`[data-kbc-tours-row="${SLUG}"]`);
    await expect(row).toBeVisible({ timeout: 15_000 });
    // D21: an agent-authored document is PENDING until a human accepts it.
    await expect(row.locator("[data-kbc-tours-row-status]")).toHaveText("pending");
    // ONE count, not two: a tour's nodes ARE its steps.
    await expect(row).toContainText("4 steps");
    await expect(row).not.toContainText("node");
    await expect(page.locator("[data-kbc-tours-boards-link]")).toHaveAttribute(
      "href",
      `/r/${REPO_NAME}/~boards`,
    );
  });

  test("playback walks the steps, keeps `?step=` in the URL, and shows the orphan", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~tours/${SLUG}`);
    await expect(page.locator(`[data-kbc-tour="${SLUG}"]`)).toBeVisible({ timeout: 15_000 });

    // A tour is ALWAYS being walked: an absent `?step=` is step one.
    await expect(page.locator("[data-kbc-tour-step-counter]")).toHaveText("step 1 of 4");
    await expect(page.locator("[data-kbc-tour-step-title]")).toHaveText("the definition");
    // The `ref` sugar resolved, and the K1 projection came back WITH the blob
    // the daemon captured (which the document never supplied).
    await expect(page.locator("[data-kbc-tour-step-ref]")).toContainText(
      `code:${RESOLVER_FILE}:1-4@`,
    );
    // The camera is RENDERED as a hint, never applied as geometry.
    await expect(page.locator("[data-kbc-tour-camera]")).toContainText("±3 lines of context");
    // The live code card is the BOARD card, which is the review document's.
    await expect(page.locator(".kbc-refcard")).toContainText(LOCAL_TARGET_FN);

    // `n` walks forward and the URL follows (1-based on the wire).
    await page.locator("body").press("n");
    await expect(page).toHaveURL(/[?&]step=2\b/);
    await expect(page.locator("[data-kbc-tour-step-counter]")).toHaveText("step 2 of 4");
    // `p` toggles play; press it twice so the run does not race the asserts.
    await page.locator("body").press("p");
    await expect(page.locator("[data-kbc-tour-playing]")).toBeVisible();
    await page.locator("body").press("p");
    // …and `k` walks back.
    await page.locator("body").press("k");
    await expect(page.locator("[data-kbc-tour-step-counter]")).toHaveText("step 1 of 4");

    // The ORPHAN step is a VISIBLE stop that says the code is gone.
    await page.locator('[data-kbc-tour-strip-item="2"]').click();
    await expect(page.locator("[data-kbc-tour-step-counter]")).toHaveText("step 3 of 4");
    await expect(page.locator('[data-kbc-tour-strip-item="2"]')).toHaveAttribute(
      "data-kbc-tour-strip-state",
      "orphan",
    );
    await expect(page.locator(".kbc-refcard")).toContainText("the code is gone");

    // The census is the daemon's, and it names the orphan out loud.
    await expect(page.locator("[data-kbc-tour-census]")).toContainText("1 orphan");
    await expect(page.locator("[data-kbc-tour-census]")).toContainText("shown, never dropped");
    // A tour's census reports STEPS and never a node/edge count.
    await expect(page.locator("[data-kbc-tour-census]")).toContainText("4 steps");
    await expect(page.locator("[data-kbc-tour-census]")).not.toContainText("edge");
  });

  test("a step's reader link carries the tour back, and the chip re-focuses that step", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~tours/${SLUG}?step=2`);
    await expect(page.locator("[data-kbc-tour-step-counter]")).toHaveText("step 2 of 4");

    // The link carries `?trail=<slug>&step=<ordinal>&src=tour` — the SAME
    // linkage grammar the browser-local trail uses, discriminated by `src`.
    const href = await page.locator("[data-kbc-tour-open-reader]").getAttribute("href");
    expect(href).toContain(`trail=${SLUG}`);
    expect(href).toContain("step=1");
    expect(href).toContain("src=tour");

    await page.locator("[data-kbc-tour-open-reader]").click();
    // The reader wears the chip, and it names the tour and the stop.
    const chip = page.locator('[data-kbc-linked-chip="tour"]');
    await expect(chip).toBeVisible({ timeout: 15_000 });
    await expect(chip).toContainText("E2E tour");
    await expect(chip).toContainText("step 2 of 4");

    // Following it lands back on THAT step, not merely on the tour.
    await chip.locator("[data-kbc-linked-return]").click();
    await expect(page).toHaveURL(new RegExp(`~tours/${SLUG}\\?step=2`));
    await expect(page.locator("[data-kbc-tour-step-counter]")).toHaveText("step 2 of 4");
  });

  test("`Space g T` is the tour destination, and `Space t` is still the theme", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~boards`);
    await expect(page.locator("[data-kbc-boards]")).toBeVisible({ timeout: 15_000 });
    await page.locator("body").press(" ");
    await page.locator("body").press("g");
    await page.locator("body").press("T");
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/~tours$`));
    await expect(page.locator("[data-kbc-tours]")).toBeVisible();
  });
});
