import { test, expect } from "@playwright/test";
import { BASE } from "./helpers";

// W3.P-c — provenance registers, and the SUBSUMPTION of W2.6b marks.
//
// There was no e2e coverage of the letter-prefix keyboard family at all
// (grep: no spec pressed `m`, backtick, or opened the `?` sheet), so this
// spec covers the whole family — marks INCLUDED — since they now share one
// store (`lib/registers.ts`, key `kb:registers`) and one letter-capture
// machine in `components/chrome/HotkeyRoot.tsx`.
//
// Clipboard note: the paste actions write via `navigator.clipboard`, which
// needs a permission grant in headless chromium; each test that reads the
// clipboard grants it explicitly.

const READER = `${BASE}/a/canon/kitchen-sink.html`;

async function clearStore(page: import("@playwright/test").Page) {
  await page.evaluate(() => {
    localStorage.removeItem("kb:registers");
    localStorage.removeItem("kb:marks");
  });
}

test.describe("provenance registers", () => {
  test("`\" a` stores the reader's artifact; the ? sheet lists it and `' a` copies its citation", async ({
    page,
    context,
  }) => {
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
    await page.goto(READER);
    await clearStore(page);
    await page.goto(READER);
    await expect(page.getByLabel("artifact context")).toBeVisible();

    // `"` arms the letter mode (the chord indicator proves the prompt), then
    // `a` stores.
    await page.locator("body").click({ position: { x: 5, y: 5 } });
    await page.keyboard.press('"');
    await expect(page.locator(".kb-chord")).toContainText('"');
    await page.keyboard.press("a");
    await expect(page.locator(".kb-chord")).toHaveCount(0);

    // It landed in the ONE store, as an artifact-kind ref.
    const stored = await page.evaluate(() =>
      JSON.parse(localStorage.getItem("kb:registers") ?? "[]"),
    );
    expect(stored).toHaveLength(1);
    expect(stored[0].letter).toBe("a");
    expect(stored[0].ref.kind).toBe("artifact");
    expect(stored[0].ref.sourceRelative).toBe("kitchen-sink.html");

    // The `?` sheet grows a "Your registers" section beside "Your marks".
    await page.keyboard.press("?");
    const sheet = page.getByRole("dialog", { name: "keyboard shortcuts" });
    await expect(sheet).toBeVisible();
    await expect(sheet).toContainText("Your registers");
    const row = sheet.locator(".kb-keyhelp__row--reg");
    await expect(row).toHaveCount(1);
    await expect(row.locator(".kb-keyhelp__reg-kind")).toHaveText("artifact");
    // ...and the family is advertised in the generated cheat sheet itself
    // (the registry is the only home for a binding — it must not lie).
    await expect(sheet).toContainText("Store this reference in a register");
    await page.keyboard.press("Escape");
    await expect(sheet).toHaveCount(0);

    // `'` + letter pastes the citation payload through lib/quote.ts.
    await page.keyboard.press("'");
    await expect(page.locator(".kb-chord")).toContainText("'");
    await page.keyboard.press("a");
    const clip = await page.evaluate(() => navigator.clipboard.readText());
    expect(clip).toContain("/a/canon/kitchen-sink.html");
    expect(clip.split("\n").length).toBeGreaterThanOrEqual(2);
  });

  test("the sheet's cite button copies the same payload, and ✕ clears the slot", async ({
    page,
    context,
  }) => {
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
    await page.goto(READER);
    await clearStore(page);
    await page.goto(READER);
    await expect(page.getByLabel("artifact context")).toBeVisible();
    await page.locator("body").click({ position: { x: 5, y: 5 } });
    await page.keyboard.press('"');
    await page.keyboard.press("b");

    await page.keyboard.press("?");
    const sheet = page.getByRole("dialog", { name: "keyboard shortcuts" });
    await sheet.locator('[data-kb-act="register-cite"]').click();
    const clip = await page.evaluate(() => navigator.clipboard.readText());
    expect(clip).toContain("/a/canon/kitchen-sink.html");

    // The add-to-list paste reuses AddToListButton verbatim (no fork): the
    // artifact id was captured, so the row offers it.
    await expect(sheet.locator('[data-kb-act="register-list-b"]')).toHaveCount(1);

    await sheet.locator('[data-kb-act="register-clear"]').click();
    await expect(sheet.locator(".kb-keyhelp__row--reg")).toHaveCount(0);
    expect(
      await page.evaluate(() => JSON.parse(localStorage.getItem("kb:registers") ?? "[]")),
    ).toEqual([]);
  });

  test("marks are the artifact lens on the SAME store — `m` writes it, backtick jumps it", async ({
    page,
  }) => {
    await page.goto(READER);
    await clearStore(page);
    await page.goto(READER);
    await expect(page.getByLabel("artifact context")).toBeVisible();
    await page.locator("body").click({ position: { x: 5, y: 5 } });

    await page.keyboard.press("m");
    await expect(page.locator(".kb-chord")).toContainText("m");
    await page.keyboard.press("z");

    // ONE storage key — the old `kb:marks` blob is not resurrected.
    const state = await page.evaluate(() => ({
      registers: JSON.parse(localStorage.getItem("kb:registers") ?? "[]"),
      marks: localStorage.getItem("kb:marks"),
    }));
    expect(state.marks).toBeNull();
    expect(state.registers).toHaveLength(1);
    expect(state.registers[0].ref.kind).toBe("artifact");

    // Navigate away, then backtick-jump back.
    await page.goto(`${BASE}/`);
    await expect(page.locator(".kb-topbar, header").first()).toBeVisible();
    await page.locator("body").click({ position: { x: 5, y: 5 } });
    await page.keyboard.press("`");
    await page.keyboard.press("z");
    await expect(page).toHaveURL(/\/a\/canon\/kitchen-sink\.html/);
  });

  test("a W2.6b mark in the legacy kb:marks blob migrates forward and still jumps", async ({
    page,
  }) => {
    await page.goto(READER);
    await page.evaluate(() => {
      localStorage.removeItem("kb:registers");
      localStorage.setItem(
        "kb:marks",
        JSON.stringify([
          {
            v: 1,
            letter: "q",
            kb: "canon",
            sourceRelative: "kitchen-sink.html",
            title: "Kitchen sink",
            sec: null,
            savedAt: 1234,
          },
        ]),
      );
    });
    await page.goto(`${BASE}/`);
    await expect(page.locator(".kb-topbar, header").first()).toBeVisible();
    await page.locator("body").click({ position: { x: 5, y: 5 } });
    await page.keyboard.press("`");
    await page.keyboard.press("q");
    await expect(page).toHaveURL(/\/a\/canon\/kitchen-sink\.html/);

    const state = await page.evaluate(() => ({
      registers: JSON.parse(localStorage.getItem("kb:registers") ?? "[]"),
      marks: localStorage.getItem("kb:marks"),
    }));
    // Migrated (savedAt preserved, not re-stamped) and the legacy key is
    // consumed so a later clear can't be undone by a re-migration.
    expect(state.marks).toBeNull();
    expect(state.registers).toHaveLength(1);
    expect(state.registers[0].savedAt).toBe(1234);
    expect(state.registers[0].ref).toMatchObject({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "kitchen-sink.html",
      id: null,
    });
  });

  test("typing a quote inside a text field never opens register mode", async ({ page }) => {
    // `isEditableTarget` guards the whole letter family; the add-to-list
    // popover's "New list…" box is a convenient always-present text input
    // on the reader route.
    await page.goto(READER);
    await clearStore(page);
    await page.goto(READER);
    await page
      .getByLabel("artifact context")
      .locator('[data-kb-act="add-to-list"]')
      .click();
    const input = page.getByLabel("new list title");
    await expect(input).toBeVisible();
    await input.click();
    await input.fill("");
    await input.pressSequentially('a "quoted" phrase');
    await expect(page.locator(".kb-chord")).toHaveCount(0);
    await expect(input).toHaveValue('a "quoted" phrase');
    expect(await page.evaluate(() => localStorage.getItem("kb:registers"))).toBeNull();
  });
});
