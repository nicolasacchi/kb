import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// RLs4 — AddToListButton in the detail ContextBar: membership checkbox
// round-trip against a list created over HTTP, and the popover's inline
// "New list…" create+add. (The bookmarks-era `.kb-bookmark-btn-wrap` is
// gone — pinned here so a stale bundle fails loudly.)

const BASE = `http://127.0.0.1:${PORT}`;

test("ContextBar add-to-list toggles membership; bookmarks button is gone", async ({
  page,
  request,
}) => {
  const title = `RT ctx ${Date.now()}`;
  const created = await request.post(`${BASE}/api/kb/canon/lists`, {
    data: { title },
  });
  expect(created.status()).toBe(201);
  const listId = ((await created.json()) as { id: string }).id;

  await page.goto(`${BASE}/a/canon/kitchen-sink.html`);
  // Two instances render (ContextBar + PreviewInspector) — scope to the
  // ContextBar's.
  const btn = page
    .getByLabel("artifact context")
    .locator('[data-kb-act="add-to-list"]');
  await expect(btn).toBeVisible();
  await expect(page.locator(".kb-bookmark-btn-wrap")).toHaveCount(0);

  // Check the list in the popover → a whole-artifact entry lands.
  // (.click(), not .check(): the checkbox is CONTROLLED and only flips
  // after the POST + cache refetch, which .check()'s immediate-state
  // assertion can't see.)
  await btn.click();
  const row = page.locator(".kb-atl__row", { hasText: title });
  await expect(row).toBeVisible();
  await expect(row.locator("input[type=checkbox]")).toBeEnabled();
  await row.locator("input[type=checkbox]").click();
  await expect(row.locator("input[type=checkbox]")).toBeChecked();
  await expect
    .poll(async () => {
      const r = await request.get(`${BASE}/api/kb/canon/lists/${listId}`);
      const d = (await r.json()) as { entries: { anchor?: unknown }[] };
      return d.entries.length;
    })
    .toBe(1);
  // The face lights with the membership count.
  await expect(btn).toContainText("·1");

  // Uncheck → removed (idempotent server-side).
  await row.locator("input[type=checkbox]").click();
  await expect(row.locator("input[type=checkbox]")).not.toBeChecked();
  await expect
    .poll(async () => {
      const r = await request.get(`${BASE}/api/kb/canon/lists/${listId}`);
      const d = (await r.json()) as { entries: unknown[] };
      return d.entries.length;
    })
    .toBe(0);

  const del = await request.delete(`${BASE}/api/kb/canon/lists/${listId}`);
  expect(del.status()).toBe(204);
});

test("popover's New-list input creates the list and adds this artifact", async ({
  page,
  request,
}) => {
  const title = `RT ctx new ${Date.now()}`;
  await page.goto(`${BASE}/a/canon/multi-page.html`);
  await page
    .getByLabel("artifact context")
    .locator('[data-kb-act="add-to-list"]')
    .click();
  const pop = page.locator(".kb-atl__pop");
  await pop.getByLabel("new list title").fill(title);
  await pop.getByRole("button", { name: "add" }).click();

  // The fresh list shows as a checked row; daemon agrees.
  const row = page.locator(".kb-atl__row", { hasText: title });
  await expect(row.locator("input[type=checkbox]")).toBeChecked();
  const lists = await request.get(`${BASE}/api/lists`);
  const body = (await lists.json()) as {
    lists: { id: string; title: string; entry_count: number }[];
  };
  const mine = body.lists.find((l) => l.title === title);
  expect(mine?.entry_count).toBe(1);

  const del = await request.delete(
    `${BASE}/api/kb/canon/lists/${mine?.id ?? ""}`,
  );
  expect(del.status()).toBe(204);
});
