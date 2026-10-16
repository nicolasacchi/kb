import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// OL P6 — the solid accent fill (`--accent-fill`, behind the reader rail's
// count badges and every other AA-repaired fill) must paint. The values are
// logged so a failure names the computed state it saw.
const BASE = `http://127.0.0.1:${PORT}`;

for (const theme of ["dark", "light"] as const) {
  test(`the rail count badge has a solid fill (${theme})`, async ({ page }) => {
    await page.addInitScript((t) => {
      localStorage.setItem("kb:prefs", JSON.stringify({ theme: t }));
    }, theme);
    await page.goto(`${BASE}/a/canon/kitchen-sink.html`);
    await expect(page.locator("iframe").first()).toBeVisible();
    const badge = page.locator(".kb-pinsp__itab-badge").first();
    await expect(badge).toBeVisible();
    const seen = await page.evaluate(() => {
      const root = getComputedStyle(document.documentElement);
      const b = getComputedStyle(document.querySelector(".kb-pinsp__itab-badge")!);
      return {
        accentFill: root.getPropertyValue("--accent-fill"),
        accent: root.getPropertyValue("--accent"),
        colorScheme: root.colorScheme,
        dataTheme: document.documentElement.getAttribute("data-theme"),
        kbcTheme: document.documentElement.getAttribute("data-kbc-theme"),
        badgeBg: b.backgroundColor,
        badgeBgImage: b.backgroundImage,
        badgeColor: b.color,
      };
    });
    console.log(`accent-fill ${theme}: ${JSON.stringify(seen)}`);
    expect(seen.badgeBg, JSON.stringify(seen)).not.toBe("rgba(0, 0, 0, 0)");
    expect(seen.badgeBg, JSON.stringify(seen)).not.toBe("transparent");
  });
}
