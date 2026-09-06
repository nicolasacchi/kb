import { test, expect, type APIRequestContext } from "@playwright/test";
import { PORT } from "./helpers";

// RL-track (v0.18) — /lists index + detail round-trip. Lists are
// title-unique per kb and the suite's daemon is shared across parallel
// workers, so every test mints a unique title and deletes what it made.

const BASE = `http://127.0.0.1:${PORT}`;

async function firstDocs(
  request: APIRequestContext,
  n: number,
): Promise<{ id: string; source_relative: string }[]> {
  const r = await request.get(`${BASE}/api/kb/canon/docs?limit=10`);
  expect(r.status()).toBe(200);
  const docs = (await r.json()) as { id: string; source_relative: string }[];
  expect(docs.length).toBeGreaterThanOrEqual(n);
  return docs.slice(0, n);
}

async function createList(
  request: APIRequestContext,
  title: string,
): Promise<string> {
  const r = await request.post(`${BASE}/api/kb/canon/lists`, {
    data: { title },
  });
  expect(r.status()).toBe(201);
  return ((await r.json()) as { id: string }).id;
}

async function addEntry(
  request: APIRequestContext,
  listId: string,
  body: Record<string, unknown>,
): Promise<{ id: string; artifact_id: string }> {
  const r = await request.post(
    `${BASE}/api/kb/canon/lists/${listId}/entries`,
    { data: body },
  );
  expect(r.status(), await r.text()).toBe(201);
  return (await r.json()) as { id: string; artifact_id: string };
}

async function seedFullRead(
  request: APIRequestContext,
  artifactId: string,
): Promise<void> {
  const open = await request.post(`${BASE}/api/kb/canon/history/open`, {
    data: { artifact_id: artifactId },
  });
  expect(open.ok()).toBe(true);
  const { visit_id } = (await open.json()) as { visit_id: number };
  const scroll = await request.post(`${BASE}/api/kb/canon/history/scroll`, {
    data: { visit_id, scroll_y: 960, scroll_max: 1000 },
  });
  expect(scroll.status()).toBe(204);
}

test("create via UI, entries appear live over SSE, reorder persists", async ({
  page,
  request,
}) => {
  const title = `RT list ${Date.now()}`;
  const docs = await firstDocs(request, 3);

  // Create through the index form.
  await page.goto(`${BASE}/lists`);
  await page.getByLabel("new list title").fill(title);
  await page.getByRole("button", { name: "Create" }).click();
  const card = page
    .locator(".kb-list-card", { hasText: title })
    .first();
  await expect(card).toBeVisible();

  // Open the detail; add two entries via HTTP — rows appear LIVE (the
  // list.entry.added SSE → bridge path; no reload).
  await card.locator(".kb-list-card__title").click();
  await expect(page.getByTestId("list-detail")).toBeVisible();
  const listId = page.url().split("/").pop() as string;
  const e1 = await addEntry(request, listId, {
    artifact_id: docs[0].id,
    note: "start here",
  });
  const e2 = await addEntry(request, listId, { artifact_id: docs[1].id });
  const rows = page.getByTestId("list-entry");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toHaveAttribute("data-entry-id", e1.id);
  await expect(rows.nth(1)).toHaveAttribute("data-entry-id", e2.id);
  await expect(rows.nth(0)).toContainText("start here");

  // Reorder: move row 2 up via its ↑ button (hover reveals actions);
  // optimistic flip, then survives a reload (daemon persisted).
  await rows.nth(1).hover();
  await rows.nth(1).getByRole("button", { name: "move up" }).click();
  await expect(rows.nth(0)).toHaveAttribute("data-entry-id", e2.id);
  await page.reload();
  await expect(page.getByTestId("list-entry").nth(0)).toHaveAttribute(
    "data-entry-id",
    e2.id,
  );

  // Cleanup.
  const del = await request.delete(`${BASE}/api/kb/canon/lists/${listId}`);
  expect(del.status()).toBe(204);
});

test("read state derives from reading progress; manual toggle overrides", async ({
  page,
  request,
}) => {
  // FRESH artifacts in the mem kb (the spa-sessions ingest precedent):
  // the shared canon docs accumulate visits from parallel specs, which
  // both pollutes the derived state and — via the 30-min visit-gap rule
  // — suppresses the INSERT-only history.recorded event this test's
  // liveness assertion rides on.
  const stamp = Date.now();
  const mkDoc = async (title: string): Promise<string> => {
    const seed = await request.post(`${BASE}/api/kb/mem/artifacts`, {
      data: {
        title,
        body_html: `<h2 id="s1">Part one</h2><p>${"words ".repeat(80)}</p>`,
        category: "memory-user",
      },
    });
    expect(seed.ok()).toBe(true);
    const deadline = Date.now() + 15_000;
    for (;;) {
      const r = await request.get(`${BASE}/api/kb/mem/docs?limit=100`);
      const docs = (await r.json()) as { id: string; title: string }[];
      const hit = docs.find((d) => d.title === title);
      if (hit) return hit.id;
      expect(Date.now(), `artifact ${title} never indexed`).toBeLessThan(
        deadline,
      );
      await new Promise((res) => setTimeout(res, 250));
    }
  };
  const a1 = await mkDoc(`RT derived one ${stamp}`);
  const a2 = await mkDoc(`RT derived two ${stamp}`);

  const created = await request.post(`${BASE}/api/kb/mem/lists`, {
    data: { title: `RT derived ${stamp}` },
  });
  expect(created.status()).toBe(201);
  const listId = ((await created.json()) as { id: string }).id;
  const addEntryMem = async (artifactId: string) => {
    const r = await request.post(
      `${BASE}/api/kb/mem/lists/${listId}/entries`,
      { data: { artifact_id: artifactId } },
    );
    expect(r.status(), await r.text()).toBe(201);
    return (await r.json()) as { id: string; artifact_id: string };
  };
  const e1 = await addEntryMem(a1);
  const e2 = await addEntryMem(a2);

  await page.goto(`${BASE}/lists/mem/${listId}`);
  const row1 = page.locator(`[data-entry-id="${e1.id}"]`);
  const row2 = page.locator(`[data-entry-id="${e2.id}"]`);
  await expect(row1.locator(".kb-listd__dot")).toHaveClass(/is-unread/);

  // ≥95% scroll on entry 1's fresh artifact → a NEW visit row →
  // history.recorded → the bridge refreshes the lists → the dot flips
  // to read WITHOUT a reload.
  const open = await request.post(`${BASE}/api/kb/mem/history/open`, {
    data: { artifact_id: e1.artifact_id },
  });
  expect(open.ok()).toBe(true);
  const { visit_id } = (await open.json()) as { visit_id: number };
  const scroll = await request.post(`${BASE}/api/kb/mem/history/scroll`, {
    data: { visit_id, scroll_y: 960, scroll_max: 1000 },
  });
  expect(scroll.status()).toBe(204);
  await expect(row1.locator(".kb-listd__dot")).toHaveClass(/is-read/, {
    timeout: 10_000,
  });

  // Manual toggle on entry 2 → override read (ring class) + stats.
  await row2.locator(".kb-listd__dot").click();
  await expect(row2.locator(".kb-listd__dot")).toHaveClass(/is-read/);
  await expect(row2.locator(".kb-listd__dot")).toHaveClass(/is-override/);
  await expect(page.locator(".kb-list-card__stats").first()).toContainText(
    "2/2 read",
  );

  // Toggling a derived-read entry overrides it UNREAD (re-read intent).
  await row1.locator(".kb-listd__dot").click();
  await expect(row1.locator(".kb-listd__dot")).toHaveClass(/is-unread/);
  await expect(row1.locator(".kb-listd__dot")).toHaveClass(/is-override/);

  const del = await request.delete(`${BASE}/api/kb/mem/lists/${listId}`);
  expect(del.status()).toBe(204);
});

test("entry remove + list delete (confirm) land back on the index", async ({
  page,
  request,
}) => {
  const title = `RT delete ${Date.now()}`;
  const listId = await createList(request, title);
  const docs = await firstDocs(request, 1);
  const e1 = await addEntry(request, listId, { artifact_id: docs[0].id });

  await page.goto(`${BASE}/lists/canon/${listId}`);
  const row = page.locator(`[data-entry-id="${e1.id}"]`);
  await row.hover();
  await row.getByRole("button", { name: "remove entry" }).click();
  await expect(page.getByTestId("list-entry")).toHaveCount(0);

  await page.getByRole("button", { name: "delete" }).click();
  await page.locator("dialog.confirm .confirm__go").click();
  await expect(page).toHaveURL(/\/lists$/);
  await expect(
    page.locator(".kb-list-card", { hasText: title }),
  ).toHaveCount(0);
});
