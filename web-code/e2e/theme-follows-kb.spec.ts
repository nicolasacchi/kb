import { expect, test } from "@playwright/test";
import { DOCLENS_FIXTURE_PORT } from "./doclens-fixture";
import { BASE } from "./helpers";

// OL S1 — the palette family travels kb -> kb-code, one way. The harness's
// mock kb (`doclens-fixture.ts`) reports no family until this spec flips it,
// so every other spec keeps kb-code's built-in palette.

const HOOK = `http://127.0.0.1:${DOCLENS_FIXTURE_PORT}/__test__/theme-family`;

async function kbFamily(family: string | null) {
  const r = await fetch(HOOK, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ family }),
  });
  expect(r.ok).toBe(true);
}

test.describe("theme family follows kb", () => {
  test.afterEach(async () => {
    await kbFamily(null);
  });

  test("a browser that has chosen nothing here follows kb, and the picker says so", async ({
    page,
  }) => {
    await kbFamily("catppuccin");
    await page.goto(`${BASE}/`);
    await expect(page.locator("html")).toHaveAttribute("data-kbc-theme", /^catppuccin/, {
      timeout: 10_000,
    });

    await page.locator("[data-kbc-theme-toggle]").click();
    await expect(page.locator("[data-kbc-theme-source]")).toHaveText("Following kb");

    // Following is not storing: kb-code's own prefs hold no family choice.
    const blob = await page.evaluate(() => JSON.parse(localStorage.getItem("kbc:prefs") ?? "{}"));
    expect(blob.themeFamilyChosen).toBeUndefined();
  });

  test("a family chosen here wins over kb and flips the caption to 'Chosen here'", async ({
    page,
  }) => {
    await kbFamily("catppuccin");
    await page.goto(`${BASE}/`);
    await expect(page.locator("html")).toHaveAttribute("data-kbc-theme", /^catppuccin/, {
      timeout: 10_000,
    });

    await page.locator("[data-kbc-theme-toggle]").click();
    // Choosing a row commits it AND closes the popover.
    await page.locator('[data-kbc-theme-family="nord"]').click();
    await expect(page.locator("html")).toHaveAttribute("data-kbc-theme", /^nord/);
    await expect(page.locator("[data-kbc-themepicker]")).toHaveCount(0);

    await page.locator("[data-kbc-theme-toggle]").click();
    await expect(page.locator("[data-kbc-theme-source]")).toHaveText("Chosen here");

    // Survives a reload even though kb still says catppuccin.
    await page.reload();
    await expect(page.locator("html")).toHaveAttribute("data-kbc-theme", /^nord/, {
      timeout: 10_000,
    });
  });

  test("kb with no family leaves kb-code on its built-in palette, with no caption", async ({
    page,
  }) => {
    await kbFamily(null);
    await page.goto(`${BASE}/`);
    await page.locator("[data-kbc-theme-toggle]").click();
    await expect(page.locator("[data-kbc-themepicker]")).toBeVisible();
    await expect(page.locator("[data-kbc-theme-source]")).toHaveCount(0);
    await expect(page.locator("html")).not.toHaveAttribute("data-kbc-theme", /.+/);
  });
});
