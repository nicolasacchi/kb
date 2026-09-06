import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// N-track — /notes view + native note render round-trip.
//
// global-setup seeds no notes, so the spec creates one via the new
// POST /api/kb/{kb}/notes endpoint, then:
//   1. /notes lists it live with a 1/2 progress badge.
//   2. Opening its permalink renders it NATIVELY (interactive checkboxes,
//      not the iframe) — task 0 is unchecked.
//   3. Checking task 0 persists to the file (survives a reload).
//   4. Back on /notes the progress badge advances to 2/2.

const base = `http://127.0.0.1:${PORT}`;

async function firstKb(page: import("@playwright/test").Page): Promise<string> {
  const resp = await page.request.get(`${base}/api/kbs`);
  expect(resp.ok()).toBeTruthy();
  const kbs = (await resp.json()) as Array<{ name: string }>;
  expect(kbs.length).toBeGreaterThan(0);
  return kbs[0].name;
}

test.describe("/notes — interactive todo notes", () => {
  test("create, list with progress, native toggle persists, progress advances", async ({
    page,
  }) => {
    const kb = await firstKb(page);

    // 1. Create a note with two GFM tasks (one already done) at the kb root.
    const created = await page.request.post(
      `${base}/api/kb/${encodeURIComponent(kb)}/notes`,
      {
        data: {
          title: "Spec checklist",
          body_md: "- [ ] first task\n- [x] second task\n",
        },
      },
    );
    expect(created.ok()).toBeTruthy();
    const { id, path } = (await created.json()) as { id: string; path: string };
    expect(id).toBeTruthy();

    // 2. /notes lists it live (note.created SSE) with progress 1/2.
    //    Scope to the main list — the same note also appears in the
    //    contextual scope panel in the rail.
    await page.goto(`${base}/notes`);
    await expect(page.locator('[data-testid="notes-view"]')).toBeVisible();
    const row = page.locator(
      `.kb-notes__main .kb-note-row[data-note-id="${id}"]`,
    );
    await expect(row).toBeVisible({ timeout: 10_000 });
    await expect(row).toContainText("Spec checklist");
    await expect(row.locator(".kb-note-row__progress")).toHaveAttribute(
      "data-done",
      "1",
    );

    // 3. Open the note natively via its permalink. The native NoteEditor
    //    renders the body with interactive checkboxes (no iframe).
    await page.goto(
      `${base}/a/${kb}/${path.split("/").map(encodeURIComponent).join("/")}`,
    );
    await expect(page.locator(`.kb-note[data-note-id="${id}"]`)).toBeVisible({
      timeout: 10_000,
    });
    const box0 = page.locator('input[type=checkbox][data-task-index="0"]');
    await expect(box0).toBeVisible();
    await expect(box0).not.toBeChecked();

    // Check task 0 → optimistic flip + POST /toggle.
    await box0.check();
    await expect(box0).toBeChecked();

    // 4. Reload → the server flipped the source markdown, so it persists.
    await page.reload();
    const box0b = page.locator('input[type=checkbox][data-task-index="0"]');
    await expect(box0b).toBeChecked({ timeout: 10_000 });

    // 5. Back on /notes the progress badge now reads 2/2.
    await page.goto(`${base}/notes`);
    await expect(
      page.locator(
        `.kb-notes__main .kb-note-row[data-note-id="${id}"] .kb-note-row__progress`,
      ),
    ).toHaveAttribute("data-done", "2", { timeout: 10_000 });
  });

  test("notes are excluded from the gallery grid but reachable from /notes", async ({
    page,
  }) => {
    const kb = await firstKb(page);
    const created = await page.request.post(
      `${base}/api/kb/${encodeURIComponent(kb)}/notes`,
      { data: { title: "Hidden gallery note", body_md: "just a note\n" } },
    );
    const { id } = (await created.json()) as { id: string };

    // Wait until indexed (visible in /notes), then assert it's NOT in /docs.
    await page.goto(`${base}/notes`);
    await expect(
      page.locator(`.kb-notes__main .kb-note-row[data-note-id="${id}"]`),
    ).toBeVisible({ timeout: 10_000 });

    const docsResp = await page.request.get(
      `${base}/api/kb/${encodeURIComponent(kb)}/docs?limit=200`,
    );
    const body = await docsResp.json();
    const docs = (Array.isArray(body) ? body : body.docs) as Array<{
      id: string;
    }>;
    expect(docs.some((d) => d.id === id)).toBeFalsy();
  });

  test("file-scope comments thread in-place on the native note view", async ({
    page,
  }) => {
    const kb = await firstKb(page);
    const created = await page.request.post(
      `${base}/api/kb/${encodeURIComponent(kb)}/notes`,
      { data: { title: "Commentable note", body_md: "- [ ] do the thing\n" } },
    );
    expect(created.ok()).toBeTruthy();
    const { id, path } = (await created.json()) as { id: string; path: string };

    // Open the note natively (no iframe), wait for the editor to mount.
    await page.goto(
      `${base}/a/${kb}/${path.split("/").map(encodeURIComponent).join("/")}`,
    );
    await expect(page.locator(`.kb-note[data-note-id="${id}"]`)).toBeVisible({
      timeout: 10_000,
    });

    // Open the comments panel from the ContextBar — same affordance as an
    // iframe artifact, but here it drives the file-scope composer only.
    await page.locator('[data-kb-act="dock-comments"]').click();
    const panel = page.locator(".comments-panel");
    await expect(panel).toBeVisible();

    // Add a whole-note (file-scope) comment.
    await panel
      .getByRole("textbox", { name: "file-scope comment" })
      .fill("review this note");
    await panel.locator(".cp__file-scope-btn").click();

    // It threads into the list and the ContextBar badge advances to 1.
    await expect(panel.locator(".cp__row")).toContainText("review this note", {
      timeout: 10_000,
    });
    await expect(page.locator('[data-kb-act="comments-badge"]')).toHaveText(
      "1",
      { timeout: 10_000 },
    );

    // It persists: the daemon wrote a kb-comments/1 review file keyed on
    // the note's artifact id, so a fresh fetch returns the open comment.
    const review = await page.request.get(
      `${base}/api/kb/${encodeURIComponent(kb)}/review/${id}`,
    );
    expect(review.ok()).toBeTruthy();
    const rf = (await review.json()) as {
      comments: Array<{ body: string; status: string; anchor: { kind: string } }>;
    };
    expect(
      rf.comments.some(
        (c) => c.body === "review this note" && c.anchor.kind === "file",
      ),
    ).toBeTruthy();
  });

  test("ordered-list task toggles the correct item (comrak sourcepos)", async ({
    page,
  }) => {
    const kb = await firstKb(page);
    // Ordered-list tasks — the old hand-rolled scanner couldn't address these,
    // so toggling flipped the wrong line. comrak's AST makes the SPA's nth
    // rendered checkbox map to the right source task.
    const created = await page.request.post(
      `${base}/api/kb/${encodeURIComponent(kb)}/notes`,
      { data: { title: "Ordered tasks", body_md: "1. [ ] a\n2. [ ] b\n3. [ ] c\n" } },
    );
    const { id, path } = (await created.json()) as { id: string; path: string };
    await page.goto(
      `${base}/a/${kb}/${path.split("/").map(encodeURIComponent).join("/")}`,
    );
    await expect(page.locator(`.kb-note[data-note-id="${id}"]`)).toBeVisible({
      timeout: 10_000,
    });
    const box = (i: number) =>
      page.locator(`input[type=checkbox][data-task-index="${i}"]`);
    await expect(box(0)).not.toBeChecked();

    // Toggle the MIDDLE task and confirm only it persists checked.
    await box(1).check();
    await expect(box(1)).toBeChecked();
    await page.reload();
    await expect(box(1)).toBeChecked({ timeout: 10_000 });
    await expect(box(0)).not.toBeChecked();
    await expect(box(2)).not.toBeChecked();
  });

  test("renders an Obsidian callout natively (parity with the iframe render)", async ({
    page,
  }) => {
    const kb = await firstKb(page);
    // The native NoteMarkdown view and the server's `rewrite_callouts`
    // (iframe path) must render a `> [!type] …` blockquote identically as
    // <div class="kb-callout kb-callout--type">. The header parse is pinned
    // in lib/callout.test.ts; this verifies the rehype wiring end-to-end.
    const created = await page.request.post(
      `${base}/api/kb/${encodeURIComponent(kb)}/notes`,
      {
        data: {
          title: "Callout note",
          body_md: "> [!warning] Heads up\n> be careful **here**\n",
        },
      },
    );
    const { id, path } = (await created.json()) as { id: string; path: string };
    await page.goto(
      `${base}/a/${kb}/${path.split("/").map(encodeURIComponent).join("/")}`,
    );
    await expect(page.locator(`.kb-note[data-note-id="${id}"]`)).toBeVisible({
      timeout: 10_000,
    });
    const callout = page.locator(".kb-note-md .kb-callout.kb-callout--warning");
    await expect(callout).toBeVisible({ timeout: 10_000 });
    await expect(callout.locator(".kb-callout__title")).toHaveText("Heads up");
    await expect(callout).toContainText("be careful");
    // The body markdown still renders, and the raw marker is stripped.
    await expect(callout.locator("strong")).toHaveText("here");
    await expect(callout).not.toContainText("[!warning]");
  });

  test("native editor appends a task, edits the body, and deletes", async ({
    page,
  }) => {
    const kb = await firstKb(page);
    const created = await page.request.post(
      `${base}/api/kb/${encodeURIComponent(kb)}/notes`,
      { data: { title: "Editable", body_md: "- [ ] start\n" } },
    );
    const { id, path } = (await created.json()) as { id: string; path: string };
    await page.goto(
      `${base}/a/${kb}/${path.split("/").map(encodeURIComponent).join("/")}`,
    );
    const note = page.locator(`.kb-note[data-note-id="${id}"]`);
    await expect(note).toBeVisible({ timeout: 10_000 });

    // Append a task via the inline "add a task" form → a 2nd checkbox appears.
    await note.locator(".kb-note__add-input").fill("second");
    await note.locator(".kb-note__add button[type=submit]").click();
    await expect(
      note.locator('input[type=checkbox][data-task-index="1"]'),
    ).toBeVisible({ timeout: 10_000 });

    // Edit mode → rewrite the body → Save → render reflects it.
    await note.getByRole("button", { name: "Edit" }).click();
    await note
      .getByRole("textbox", { name: "note body" })
      .fill("- [ ] rewritten body\n");
    await note.getByRole("button", { name: "Save" }).click();
    await expect(note.locator(".kb-note-md")).toContainText("rewritten body", {
      timeout: 10_000,
    });

    // Delete (accept the ConfirmModal) → routed back to /notes.
    await note.getByRole("button", { name: "Delete" }).click();
    await page.locator("dialog.confirm .confirm__go").click();
    await expect(page).toHaveURL(/\/notes(\?|$)/, { timeout: 10_000 });
  });
});
