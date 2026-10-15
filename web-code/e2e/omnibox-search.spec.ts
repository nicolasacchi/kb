import { expect, test } from "@playwright/test";
import { KNOWN_FILE, KNOWN_SYMBOL, TEXT_NEEDLE } from "./fixture-repo";
import {
  DOCLENS_DOC_PATH,
  DOCLENS_DOC_TITLE,
  DOCLENS_FIXTURE_PORT,
  DOCLENS_KB,
} from "./doclens-fixture";
import { BASE, REPO_NAME } from "./helpers";

test.describe("omnibox search", () => {
  test("Ctrl+K opens the omnibox, a symbol query finds it, Enter opens the reader at the right file", async ({
    page,
  }) => {
    await page.goto(`${BASE}/`);
    await page.keyboard.press("Control+k");

    const dialog = page.getByRole("dialog", { name: "search" });
    await expect(dialog).toBeVisible();

    const input = dialog.getByRole("textbox", { name: "search query" });
    await input.fill(`@${KNOWN_SYMBOL}`);

    const symbolsSection = dialog.locator('[data-kbc-lane="symbols"]');
    await expect(symbolsSection).toContainText(KNOWN_SYMBOL, { timeout: 10_000 });

    // The prefix-scoped query narrows to a single lane section — its
    // header is the initial cursor, one ArrowDown reaches the first
    // (only) hit row.
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("Enter");

    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${KNOWN_FILE}`));
    await expect(dialog).toBeHidden();
    await expect(page.locator(".kbc-codeview")).toContainText(KNOWN_SYMBOL);
  });

  test("a plain (unprefixed) query renders every fixed lane section, and Esc closes the omnibox", async ({
    page,
  }) => {
    await page.goto(`${BASE}/`);
    await page.keyboard.press("Control+k");
    const dialog = page.getByRole("dialog", { name: "search" });
    const input = dialog.getByRole("textbox", { name: "search query" });
    await input.fill(TEXT_NEEDLE);

    // The server always returns the full six-lane section set for an
    // unprefixed query, even when some lanes report `unavailable_reason`
    // (semantic is disabled by default; sessions gets a `BadStatus` —
    // W2.B.R fix 14: since DCB W2.B, `[kb_daemon]` IS enabled and reachable
    // in this harness, pointed at `doclens-fixture.ts`'s mock, but that
    // mock implements only the doc-lens `code-refs`/`by-path` routes, not
    // `/api/sessions/recollect` — the mock's plain 404 for that path is
    // what degrades the sessions lane here, not an unreachable daemon) —
    // see `search::unified::run`'s doc.
    await expect(dialog.locator("[data-kbc-lane]")).toHaveCount(6, { timeout: 10_000 });
    await expect(dialog.locator('[data-kbc-lane="text"]')).toContainText(TEXT_NEEDLE);

    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
  });

  // OL S1 — the "Documents" lane: kb's own search, relayed by kb-code-server.
  // The harness's mock kb (`doclens-fixture.ts`) answers `/api/search` for one
  // query only, so the same suite proves a hit row and the honest unavailable
  // line.
  test("the Documents lane lists a kb hit after the code lanes, and Enter opens its permalink in a new tab", async ({
    page,
  }) => {
    await page.goto(`${BASE}/`);
    await page.keyboard.press("Control+k");
    const dialog = page.getByRole("dialog", { name: "search" });
    await dialog.getByRole("textbox", { name: "search query" }).fill("fixture-doc");

    const row = dialog.locator("[data-kbc-docs-row]");
    await expect(row).toHaveCount(1, { timeout: 10_000 });
    await expect(row).toContainText(DOCLENS_DOC_TITLE);
    await expect(row).toHaveAttribute(
      "href",
      `http://127.0.0.1:${DOCLENS_FIXTURE_PORT}/a/${DOCLENS_KB}/${DOCLENS_DOC_PATH}`,
    );
    await expect(row).toHaveAttribute("target", "_blank");
    // after the six code lanes (which the existing specs count by [data-kbc-lane])
    await expect(dialog.locator("[data-kbc-lane]")).toHaveCount(6);
    const order = await dialog.locator("[data-kbc-lane], [data-kbc-docs-lane]").evaluateAll((els) =>
      els.map((e) => (e.hasAttribute("data-kbc-docs-lane") ? "documents" : e.getAttribute("data-kbc-lane"))),
    );
    expect(order[order.length - 1]).toBe("documents");

    // Tab walks the six code lanes, then lands on Documents; ArrowDown reaches
    // its first row; Enter opens the permalink in a new tab.
    for (let i = 0; i < 6; i++) await page.keyboard.press("Tab");
    await page.keyboard.press("ArrowDown");
    await expect(row).toHaveAttribute("aria-selected", "true");
    const [popup] = await Promise.all([
      page.waitForEvent("popup"),
      page.keyboard.press("Enter"),
    ]);
    expect(popup.url()).toBe(
      `http://127.0.0.1:${DOCLENS_FIXTURE_PORT}/a/${DOCLENS_KB}/${DOCLENS_DOC_PATH}`,
    );
    await popup.close();
  });

  test("a query kb cannot answer renders one muted unavailable line, never an error", async ({
    page,
  }) => {
    await page.goto(`${BASE}/`);
    await page.keyboard.press("Control+k");
    const dialog = page.getByRole("dialog", { name: "search" });
    await dialog.getByRole("textbox", { name: "search query" }).fill(TEXT_NEEDLE);

    const note = dialog.locator("[data-kbc-docs-unavailable]");
    await expect(note).toBeVisible({ timeout: 10_000 });
    await expect(note).toContainText(/kb/i);
    await expect(dialog.locator("[data-kbc-docs-row]")).toHaveCount(0);
    // the code lanes are untouched and nothing raised an alert
    await expect(dialog.locator("[data-kbc-lane]")).toHaveCount(6);
    await expect(dialog.locator('[role="alert"]')).toHaveCount(0);
  });

  test("a prefix-scoped (code) query does not search documents", async ({ page }) => {
    await page.goto(`${BASE}/`);
    await page.keyboard.press("Control+k");
    const dialog = page.getByRole("dialog", { name: "search" });
    await dialog.getByRole("textbox", { name: "search query" }).fill(`@${KNOWN_SYMBOL}`);
    await expect(dialog.locator('[data-kbc-lane="symbols"]')).toContainText(KNOWN_SYMBOL, {
      timeout: 10_000,
    });
    await expect(dialog.locator("[data-kbc-docs-lane]")).toHaveCount(0);
  });
});

