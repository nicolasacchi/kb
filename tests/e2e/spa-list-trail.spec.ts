import { test, expect, type Page } from "@playwright/test";
import { PORT } from "./helpers";

// RLs5 — trail navigation. A 3-entry list:
//   1. cost-of-abstraction.html        (whole artifact)
//   2. kitchen-sink.html §<id-a>       (cross-artifact hop lands here)
//   3. kitchen-sink.html §<id-b>       (2→3 is a SAME-artifact hop:
//                                       params-only, no iframe remount)
// The queue bar carries position, prev/next, mark-read; ?list=&entry=
// survive a reload; ✕ exits the trail.

const BASE = `http://127.0.0.1:${PORT}`;

async function discoverHeadingIds(page: Page, n: number): Promise<string[]> {
  await page.goto(`${BASE}/a/canon/kitchen-sink.html`);
  const frame = page.frameLocator(".detail__frame");
  await expect(frame.locator("h1").first()).toBeVisible();
  await expect
    .poll(() =>
      frame
        .locator("h2[id]")
        .count()
        .catch(() => 0),
    )
    .toBeGreaterThanOrEqual(n);
  const ids: string[] = [];
  for (let i = 0; i < n; i++) {
    const id = await frame.locator("h2[id]").nth(i).getAttribute("id");
    ids.push(id as string);
  }
  return ids;
}

test("queue bar walks entries across artifacts and sections", async ({
  page,
  request,
}) => {
  const [secA, secB] = await discoverHeadingIds(page, 2);
  const title = `RT trail ${Date.now()}`;
  const created = await request.post(`${BASE}/api/kb/canon/lists`, {
    data: { title },
  });
  const listId = ((await created.json()) as { id: string }).id;
  const add = (body: Record<string, unknown>) =>
    request
      .post(`${BASE}/api/kb/canon/lists/${listId}/entries`, { data: body })
      .then(async (r) => {
        expect(r.status(), await r.text()).toBe(201);
        return (await r.json()) as { id: string };
      });
  const e1 = await add({ path: "cost-of-abstraction.html" });
  const e2 = await add({
    path: "kitchen-sink.html",
    anchor: { kind: "section", id: secA },
  });
  const e3 = await add({
    path: "kitchen-sink.html",
    anchor: { kind: "section", id: secB },
  });

  // Entry 1 from the list detail → trail params + bar at 1/3.
  await page.goto(`${BASE}/lists/canon/${listId}`);
  await page
    .locator(`[data-entry-id="${e1.id}"] a.kb-listd__title`)
    .click();
  await expect(page).toHaveURL(/cost-of-abstraction\.html/);
  expect(page.url()).toContain(`list=${listId}`);
  expect(page.url()).toContain(`entry=${e1.id}`);
  const bar = page.getByTestId("queue-bar");
  await expect(bar).toContainText("entry 1/3");
  await expect(bar.getByRole("button", { name: "← prev" })).toBeDisabled();

  // next → cross-artifact hop onto kitchen-sink §secA.
  await bar.getByTestId("queue-next").click();
  await expect(page).toHaveURL(/kitchen-sink\.html/);
  expect(page.url()).toContain(`sec=${secA}`);
  expect(page.url()).toContain(`entry=${e2.id}`);
  await expect(bar).toContainText("entry 2/3");
  const frame = page.frameLocator(".detail__frame");
  await expect(frame.locator(`#${secA}`)).toBeInViewport({
    timeout: 10_000,
  });

  // Plant a marker in the live iframe; the 2→3 hop is params-only and
  // must NOT remount it (the marker survives).
  await frame.locator("body").evaluate(() => {
    (window as unknown as Record<string, unknown>).__kb_trail_marker = 42;
  });
  await bar.getByTestId("queue-next").click();
  expect(page.url()).toContain(`entry=${e3.id}`);
  expect(page.url()).toContain(`sec=${secB}`);
  await expect(bar).toContainText("entry 3/3");
  await expect(frame.locator(`#${secB}`)).toBeInViewport({
    timeout: 10_000,
  });
  const marker = await frame
    .locator("body")
    .evaluate(
      () => (window as unknown as Record<string, unknown>).__kb_trail_marker,
    );
  expect(marker, "same-artifact hop must not remount the iframe").toBe(42);
  await expect(bar.getByTestId("queue-next")).toBeDisabled();

  // mark read on the bar → the list detail agrees.
  await bar.getByRole("button", { name: /mark read/ }).click();
  await expect(bar.getByRole("button", { name: /read/ })).toContainText(
    "● read",
  );

  // The trail URL is refresh-safe; ✕ drops the params.
  await page.reload();
  await expect(page.getByTestId("queue-bar")).toContainText("entry 3/3");
  await page.getByRole("button", { name: "exit trail" }).click();
  expect(page.url()).not.toContain("list=");
  expect(page.url()).not.toContain("entry=");
  await expect(page.getByTestId("queue-bar")).toHaveCount(0);

  const del = await request.delete(`${BASE}/api/kb/canon/lists/${listId}`);
  expect(del.status()).toBe(204);
});
