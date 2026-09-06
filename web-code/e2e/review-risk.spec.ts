import { expect, test, type APIRequestContext } from "@playwright/test";
import { FEATURE_BRANCH, KNOWN_FILE } from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V3.2-B3 — Review risk: default sort is diff order; risk-first reorders;
/// null-risk files show "—". Degrades silently when risk endpoint is 404.

async function createReview(request: APIRequestContext): Promise<number | null> {
  const res = await request.post(`${BASE}/api/reviews`, {
    data: {
      repo: REPO_NAME,
      head_ref: FEATURE_BRANCH,
      base_ref: "main",
      title: "e2e risk-order review",
    },
  });
  if (res.status() === 404) return null; // non-loopback / mutations unavailable
  if (!res.ok()) {
    const body = await res.text();
    throw new Error(`create review failed: ${res.status()} ${body}`);
  }
  const json = (await res.json()) as { id: number };
  return json.id;
}

async function riskAvailable(request: APIRequestContext, id: number): Promise<boolean> {
  const res = await request.get(`${BASE}/api/reviews/${id}/risk`);
  return res.ok();
}

test.describe("review risk order + badges", () => {
  test("default order is diff order; risk-first reorders; null shows em dash", async ({
    page,
    request,
  }) => {
    const id = await createReview(request);
    test.skip(id == null, "review create is loopback-only; skip on non-loopback");

    const hasRisk = await riskAvailable(request, id!);
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${id}`);
    await expect(page.locator("[data-kbc-review]")).toBeVisible({ timeout: 15_000 });

    // Wait for files list.
    await expect(page.locator("[data-kbc-review-files]")).toBeVisible({ timeout: 15_000 });
    const fileRows = page.locator("[data-kbc-review-file-row]");
    await expect(fileRows.first()).toBeVisible({ timeout: 10_000 });

    if (!hasRisk) {
      // B2 surface absent — sort control and badges must not render.
      await expect(page.locator("[data-kbc-review-sort]")).toHaveCount(0);
      await expect(page.locator("[data-kbc-review-risk]")).toHaveCount(0);
      return;
    }

    // Default: sort select present and set to diff order.
    const sort = page.locator("[data-kbc-review-sort]");
    await expect(sort).toBeVisible();
    await expect(sort).toHaveValue("diff");

    const pathsDiff = await fileRows.evaluateAll((els) =>
      els.map((e) => e.getAttribute("data-kbc-review-file-row") ?? ""),
    );

    // Switch to risk first.
    await sort.selectOption("risk");
    await expect(sort).toHaveValue("risk");
    await page.waitForTimeout(200);

    const pathsRisk = await fileRows.evaluateAll((els) =>
      els.map((e) => e.getAttribute("data-kbc-review-file-row") ?? ""),
    );
    // Same set of paths (reordered or identical if ties).
    expect([...pathsRisk].sort()).toEqual([...pathsDiff].sort());

    // Scores non-increasing among rows with a numeric badge (nulls last).
    const badges = page.locator("[data-kbc-review-risk]");
    const badgeCount = await badges.count();
    expect(badgeCount).toBe(pathsRisk.length);

    const scores: Array<number | null> = [];
    for (let i = 0; i < badgeCount; i++) {
      const el = badges.nth(i);
      const isNull = (await el.getAttribute("data-kbc-review-risk-null")) !== null;
      if (isNull) {
        await expect(el).toHaveText("—");
        scores.push(null);
      } else {
        const s = await el.getAttribute("data-kbc-review-risk-score");
        scores.push(s != null ? Number(s) : null);
      }
    }
    // Once a null appears, all subsequent should be null.
    let sawNull = false;
    for (const s of scores) {
      if (s == null) {
        sawNull = true;
      } else if (sawNull) {
        throw new Error("null-risk file appeared before a scored file under risk-first sort");
      }
    }
    // Among finite scores, non-increasing.
    const finite = scores.filter((s): s is number => s != null);
    for (let i = 1; i < finite.length; i++) {
      expect(finite[i]).toBeLessThanOrEqual(finite[i - 1] + 1e-9);
    }

    // At least one null badge is ideal when signals are sparse; if every file
    // has a score, the "—" contract is still covered by the isNull branch above
    // when applicable. Force a sanity check that badges never show a bare "0"
    // for a null risk attr.
    for (let i = 0; i < badgeCount; i++) {
      const el = badges.nth(i);
      if ((await el.getAttribute("data-kbc-review-risk-null")) !== null) {
        await expect(el).not.toHaveText(/^0(\.0+)?$/);
        await expect(el).toHaveText("—");
      }
    }

    // Restore diff order still available.
    await sort.selectOption("diff");
    await expect(sort).toHaveValue("diff");

    // Touch KNOWN_FILE path if present (file list includes it on feature-x).
    const known = page.locator(`[data-kbc-review-file-row="${KNOWN_FILE}"]`);
    if ((await known.count()) > 0) {
      await expect(known).toBeVisible();
    }
  });
});
