import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// A-SPA — durable comment-composer drafts (web/src/lib/drafts.ts): every
// composer's text is hydrated from / persisted to localStorage per
// `kb-draft/1:{kb}:{artifactId}:{slot}`, debounced ~400ms, so a reload / a
// crashed tab no longer throws an in-progress comment away. This spec
// drives the file-scope composer (slot `"file"`, the simplest of the three
// slots) end to end — mirrors spa-comments.spec.ts's "resolve toggle"
// spec's own "open the panel, fill the file-scope composer" setup, plus
// the panel-reopen-after-reload pattern from its `]`-sibling-swap spec
// (`[data-kb-act="dock-comments"]`, invariant #30 — the docked rail icon,
// not the annotate pencil, so no `?cm=on` iframe reload is needed here).

async function pickArtifact(
  request: import("@playwright/test").APIRequestContext,
): Promise<{ id: string; rel: string }> {
  // kitchen-sink.html — seeded once by global-setup.ts, indexed for the
  // whole suite's lifetime (see spa-comments.spec.ts's own pickArtifact
  // for why resolving BY PATH, never a `docs[0]` fallback, matters).
  const r = await request.get(
    `http://127.0.0.1:${PORT}/api/kb/canon/docs/by-path/kitchen-sink.html`,
  );
  expect(
    r.status(),
    "canon must index kitchen-sink.html (seeded by global-setup's CANON_FILES)",
  ).toBe(200);
  const doc = (await r.json()) as { id: string; source_relative: string };
  return { id: doc.id, rel: doc.source_relative };
}

async function readDraftKey(
  page: import("@playwright/test").Page,
  key: string,
): Promise<string | null> {
  return page.evaluate((k) => localStorage.getItem(k), key);
}

test.describe("comment draft persistence (A-SPA)", () => {
  test("a typed file-scope draft survives a reload and clears on submit", async ({
    page,
    request,
  }) => {
    const { id, rel } = await pickArtifact(request);
    const draftKey = `kb-draft/1:canon:${id}:file`;
    const body = `draft-persists-${Date.now()}`;

    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await page.locator('[data-kb-act="dock-comments"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();

    const draft = panel.getByRole("textbox", { name: "file-scope comment" });
    await draft.fill(body);

    // The ~400ms debounced write has landed in localStorage — poll rather
    // than a fixed sleep, and assert the exact grammar (drafts.ts's
    // `draftKey`/`StoredDraft`), not just "something got written".
    await expect
      .poll(() => readDraftKey(page, draftKey), { timeout: 5_000 })
      .not.toBeNull();
    const stored = await readDraftKey(page, draftKey);
    expect(JSON.parse(stored!).text).toBe(body);

    // Reload drops every in-memory composer + panel-open state; the panel
    // must rehydrate the SAME text purely from storage once reopened.
    await page.reload();
    await page.locator('[data-kb-act="dock-comments"]').click();
    const panelAfterReload = page.getByRole("complementary", {
      name: "comments",
    });
    await expect(panelAfterReload).toBeVisible();
    // Assert via the hidden mirror `<textarea>` (`.cp__file-scope-input`,
    // invariant #22) — `toHaveValue` can't read the CodeMirror
    // contenteditable the role=textbox locator resolves to (same pattern
    // as spa-comments.spec.ts's write/preview round-trip).
    await expect(panelAfterReload.locator(".cp__file-scope-input")).toHaveValue(
      body,
    );

    // Submit — a successful add clears the slot outright (drafts.ts's
    // `useDraft().clear`), not merely lets it expire on the 7-day TTL.
    await panelAfterReload
      .getByRole("button", { name: "add file-scope" })
      .click();
    await expect(panelAfterReload.getByText(body)).toBeVisible({
      timeout: 5_000,
    });
    await expect
      .poll(() => readDraftKey(page, draftKey), { timeout: 5_000 })
      .toBeNull();
  });

  test("clearing the composer text removes the storage slot (no blank tombstone)", async ({
    page,
    request,
  }) => {
    const { id, rel } = await pickArtifact(request);
    const draftKey = `kb-draft/1:canon:${id}:file`;
    const body = `draft-discarded-${Date.now()}`;

    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${rel}`);
    await page.locator('[data-kb-act="dock-comments"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible();

    const draft = panel.getByRole("textbox", { name: "file-scope comment" });
    await draft.fill(body);
    await expect
      .poll(() => readDraftKey(page, draftKey), { timeout: 5_000 })
      .not.toBeNull();

    // A user clearing the box by hand is the file-scope composer's only
    // "discard" affordance (no cancel button, unlike the routed-anchor
    // composer) — drafts.ts's `writeDraft` treats an empty string as a
    // clear, not a `{text:"",…}` tombstone kept around for the 7-day TTL.
    await draft.fill("");
    await expect
      .poll(() => readDraftKey(page, draftKey), { timeout: 5_000 })
      .toBeNull();
  });
});
