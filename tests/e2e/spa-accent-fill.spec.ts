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

    // Every other role that used to be written as `light-dark()` in a custom
    // property, and the mobile reader-tools count (`.kb-ctxbar__cnt`, which
    // only renders with open comments) probed through a throwaway element.
    const probes = await page.evaluate(() => {
      const out: Record<string, string> = {};
      const probe = (name: string, css: string, cls = "") => {
        const el = document.createElement("div");
        el.className = cls;
        el.style.cssText = css;
        document.body.appendChild(el);
        out[name] = getComputedStyle(el).backgroundColor;
        el.remove();
      };
      for (const t of ["accent-fill", "paper", "paper-ink", "paper-rule", "card-bg-end", "card-ring", "term-bg"]) {
        probe(`--${t}`, `background-color: var(--${t})`);
      }
      probe(".kb-ctxbar__cnt", "", "kb-ctxbar__cnt");
      probe(".kb-hints__chip", "", "kb-hints__chip");
      return out;
    });
    console.log(`probes ${theme}: ${JSON.stringify(probes)}`);
    for (const [name, bg] of Object.entries(probes)) {
      expect(bg, `${name} (${theme}): ${JSON.stringify(probes)}`).not.toBe("rgba(0, 0, 0, 0)");
    }
  });
}
