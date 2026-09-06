import { test, expect, type Page } from "@playwright/test";
import { PORT } from "./helpers";

// Obsidian-class markdown editor (CodeMirror 6). The shared MarkdownEditor keeps
// a hidden mirror <textarea> (carrying the per-surface aria-label + class +
// value) behind the CM6 contenteditable, so these tests read/assert the value
// via `.cp__file-scope-input` while driving the visible CM6 surface.

async function openPanel(page: Page): Promise<{ panel: ReturnType<Page["locator"]> }> {
  const r = await page.request.get(
    `http://127.0.0.1:${PORT}/api/kb/canon/docs?limit=20`,
  );
  const docs = (await r.json()) as { path: string; source_relative: string }[];
  const ks = docs.find((d) => d.path.endsWith("kitchen-sink.html")) ?? docs[0];
  await page.goto(`http://127.0.0.1:${PORT}/a/canon/${ks.source_relative}`);
  await page.locator('[data-kb-act="dock-comments"]').click();
  const panel = page.getByRole("complementary", { name: "comments" });
  await expect(panel).toBeVisible();
  await expect(panel.locator(".cp__file-scope .cm-editor")).toBeVisible();
  return { panel };
}

const cm = ".cp__file-scope .cm-content";
const mirror = ".cp__file-scope .cp__file-scope-input";

test.describe("markdown editor — CodeMirror 6", () => {
  // invariant:22
  test("toolbar Bold wraps the selection and reflects aria-pressed", async ({
    page,
  }) => {
    const { panel } = await openPanel(page);
    await page.locator(cm).click();
    await page.keyboard.type("hello world");
    await page.keyboard.press("Control+a");
    await panel.locator(".cme__tb-b").click();
    await expect(panel.locator(mirror)).toHaveValue("**hello world**");
    await expect(panel.locator(".cme__tb-b")).toHaveAttribute("aria-pressed", "true");
  });

  test("⌘/Ctrl-Enter submits the file-scope comment", async ({ page }) => {
    const { panel } = await openPanel(page);
    await page.locator(cm).click();
    await page.keyboard.type("**submitted via shortcut**");
    await page.keyboard.press("Control+Enter");
    // The posted comment renders (CommentBody bolds it) — composer clears.
    await expect(
      panel.locator(".cp__row strong", { hasText: "submitted via shortcut" }),
    ).toBeVisible({ timeout: 8_000 });
    await expect(panel.locator(mirror)).toHaveValue("");
  });

  test("slash menu opens and applies a block command", async ({ page }) => {
    const { panel } = await openPanel(page);
    await page.locator(cm).click();
    await page.keyboard.press("/");
    await expect(page.locator(".cm-tooltip-autocomplete")).toBeVisible();
    await page.keyboard.type("quote");
    // Wait for the typing-debounced filter to settle on "Quote" before Enter,
    // else acceptCompletion fires with no active selection (a newline instead).
    await expect(
      page.getByRole("option", { name: /Quote/ }),
    ).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(panel.locator(mirror)).toHaveValue("> ");
  });

  test("live preview hides markup on inactive lines", async ({ page }) => {
    const { panel } = await openPanel(page);
    await page.locator(cm).click();
    // Caret ends on line 2 → line 1 is inactive and renders without `## `.
    await page.keyboard.type("## Heading\nplain line");
    const firstLine = panel.locator(`${cm} .cm-line`).first();
    await expect(firstLine).toHaveText("Heading");
    // The source is untouched — the mirror still holds the raw markdown.
    await expect(panel.locator(mirror)).toHaveValue("## Heading\nplain line");
  });

  test("live-preview checkbox toggles the markdown source", async ({ page }) => {
    const { panel } = await openPanel(page);
    await page.locator(cm).click();
    // Enter after a task auto-continues `- [ ] ` (smart continuation), so we
    // only type the content of the second item.
    await page.keyboard.type("- [ ] task one\ntask two");
    // Line 1 is inactive → its `[ ]` renders as a real checkbox widget.
    const box = panel.locator(`${cm} input.cm-kb-task`).first();
    await expect(box).toBeVisible();
    await box.click();
    await expect(panel.locator(mirror)).toHaveValue(/- \[x\] task one/);
  });
});
