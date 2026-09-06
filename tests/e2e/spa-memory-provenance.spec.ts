import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// MI-W4.6 → CT-E4 — the provenance dossier. The seeded `mem` kb
// (global-setup.ts) memories ("Prefers tabs", "Deploy pipeline") carry no
// `kb-session` meta; pre-CT-E4 that DISABLED the action, but the dossier's
// Origin/Currency/Attention sections render fine without a session, so the
// widened gate opens it for ANY row — the hand-written origin class plus an
// honest "no origin session recorded" chain absence IS the honest state
// for this corpus now.

test.describe("provenance dossier — CT-E4 widened gate", () => {
  test("the dossier opens for a memory with no origin session (hand-written origin, honest chain absence)", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${PORT}/memory`);
    const row = page.getByTestId("memory-item").filter({ hasText: "Prefers tabs" });
    await expect(row).toBeVisible({ timeout: 10_000 });

    const btn = row.getByRole("button", { name: "provenance" });
    await expect(btn).toBeEnabled();
    await btn.click();

    const dossier = page.getByTestId("provenance-thread");
    await expect(dossier).toBeVisible();
    await expect(page.getByTestId("provenance-origin")).toContainText("hand-written");
    await expect(page.getByTestId("provenance-no-origin-session")).toBeVisible();
    await expect(page.getByTestId("provenance-no-session")).toHaveCount(0);
    await expect(page.getByTestId("provenance-session")).toHaveCount(0);
  });
});
