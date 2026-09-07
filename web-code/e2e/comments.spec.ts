import { expect, test } from "@playwright/test";
import { COMMENTS_DOC_METHOD, COMMENTS_TODO_TEXT, RUBY_ORDER_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

// V72-J2 (D8) — comments/1 in the SPA: the per-file comment gutter (kind +
// state markers, the three display modes), the `~comments` dashboard's
// actionable default, and one claim -> annotation bridge round trip.
// `RUBY_ORDER_FILE` (`shop_order.rb`) now carries, additively (see
// `fixture-repo.ts`'s own doc): a `doc` block above `refund`, and an
// `annotation` (TODO) + an UNREASONED `directive` (rubocop:disable, no
// reason) pair above `apply_discount`.

async function openOrderFile(page: import("@playwright/test").Page) {
  await page.goto(`${BASE}/r/${REPO_NAME}`);
  await page.locator(".kbc-tree__row", { hasText: RUBY_ORDER_FILE }).click();
  await expect(page.locator(".kbc-codeview")).toContainText(COMMENTS_DOC_METHOD, { timeout: 10_000 });
}

test.describe("comments/1 — the per-file gutter", () => {
  test("renders doc/annotation/directive markers, honouring the quiet/doc-only modes", async ({ page }) => {
    await openOrderFile(page);

    // `all` (the default) shows every kind comments/1 classified for this
    // file — the doc block, the TODO annotation, and the unreasoned
    // rubocop:disable directive.
    await expect(page.locator(".kbc-comment-dot--doc").first()).toBeVisible({ timeout: 10_000 });
    await expect(page.locator(".kbc-comment-dot--annotation").first()).toBeVisible();
    const unreasonedDirective = page.locator(".kbc-comment-dot--directive.kbc-comment-dot--state-unreasoned");
    await expect(unreasonedDirective.first()).toBeVisible();

    // The mode chip names the active mode, and `Space C c` cycles it —
    // `quiet` hides the doc block (a `none`-state row) but keeps the
    // annotation/directive rows, both of which carry a state.
    const modeChip = page.locator("[data-kbc-comment-gutter-mode]");
    await expect(modeChip).toHaveAttribute("data-kbc-comment-gutter-mode", "all");
    await page.keyboard.press("Space");
    await page.keyboard.press("C");
    await page.keyboard.press("c");
    await expect(modeChip).toHaveAttribute("data-kbc-comment-gutter-mode", "quiet");
    await expect(page.locator(".kbc-comment-dot--doc")).toHaveCount(0);
    await expect(unreasonedDirective.first()).toBeVisible();

    // `doc-only` hides the annotation/directive rows and keeps only doc.
    await page.keyboard.press("Space");
    await page.keyboard.press("C");
    await page.keyboard.press("c");
    await expect(modeChip).toHaveAttribute("data-kbc-comment-gutter-mode", "doc-only");
    await expect(page.locator(".kbc-comment-dot--doc").first()).toBeVisible();
    await expect(page.locator(".kbc-comment-dot--annotation")).toHaveCount(0);
    await expect(page.locator(".kbc-comment-dot--directive")).toHaveCount(0);

    // Back to `all` (cycle wraps).
    await page.keyboard.press("Space");
    await page.keyboard.press("C");
    await page.keyboard.press("c");
    await expect(modeChip).toHaveAttribute("data-kbc-comment-gutter-mode", "all");
  });

  test("clicking a marker opens the rail's Comments tab at that block", async ({ page }) => {
    await openOrderFile(page);
    // The rail defaults to "All" (every section stacked), so the Comments
    // section is already present; open the dedicated tab via its own key
    // (`Space R c`) so this also exercises `rail.tab.comments`.
    await page.keyboard.press("Space");
    await page.keyboard.press("R");
    await page.keyboard.press("c");
    await expect(page.locator('[data-kbc-itab="comments"]')).toHaveAttribute("aria-selected", "true");

    const gutter = page.locator(".cm-gutter.kbc-comment-gutter");
    await expect(gutter).toBeVisible();
    const marker = page.locator(".kbc-comment-dot--annotation").first();
    await marker.scrollIntoViewIfNeeded();
    await marker.click();
    await expect(page.locator("[data-kbc-comments-panel] [data-kbc-comment-active]")).toBeVisible();
  });
});

test.describe("comments/1 — the claim -> annotation bridge", () => {
  test("tracks a TODO-family comment as an annotation, then shows it as tracked", async ({ page }) => {
    await openOrderFile(page);

    // Position the cursor on the TODO comment's own line by clicking it —
    // no pinned line number (the fixture doc deliberately avoids one).
    const todoLine = page.locator(".kbc-codeview .cm-line", { hasText: COMMENTS_TODO_TEXT });
    await expect(todoLine).toBeVisible({ timeout: 10_000 });
    await todoLine.click();

    const openBadge = page.locator('[data-kbc-bridge="open"]');
    await expect(openBadge.first()).toBeVisible();

    // `Space C t` — comments.track-as-annotation.
    await page.keyboard.press("Space");
    await page.keyboard.press("C");
    await page.keyboard.press("t");

    const trackedBadge = page.locator('[data-kbc-bridge="tracked"]');
    await expect(trackedBadge.first()).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('[data-kbc-bridge="open"]')).toHaveCount(0);
  });

  test("the ~comments dashboard defaults to the actionable slice", async ({ page }) => {
    // `Space g m` — nav.comments.
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.keyboard.press("Space");
    await page.keyboard.press("g");
    await page.keyboard.press("m");
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/~comments$`));

    const unreasonedSection = page.locator('[data-kbc-comments-dash-section="unreasoned"]');
    await expect(unreasonedSection).toBeVisible({ timeout: 10_000 });
    await expect(unreasonedSection).toContainText(RUBY_ORDER_FILE);
    await expect(page.locator('[data-kbc-comments-dash-chip="kind-directive"]').first()).toBeVisible();

    // "Show everything" swaps to the unfiltered, still server-paged list.
    await page.locator("[data-kbc-comments-dash-show-everything]").check();
    await expect(page.locator("[data-kbc-comments-dash-everything]")).toBeVisible();
  });
});
