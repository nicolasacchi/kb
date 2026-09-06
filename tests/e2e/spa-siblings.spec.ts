import { test, expect, type Page } from "@playwright/test";
import { PORT, artifactUrlRe } from "./helpers";

// G3/G4/G7: PreviewInspector Folder section. The right-rail inspector
// renders a Folder section listing the artifact's same-folder + nested
// siblings, with a sort toggle (name|title) and a subfolder checkbox
// (direct-only vs all descendants). `[` and `]` step prev/next direct
// siblings with wrap-around, guarded against typing in inputs. Toggle
// preferences persist in localStorage under `kb:siblings`.
//
// X1 finish (v0.13) retired the FloatingPill popover that previously
// hosted this surface. G7 re-introduced it inline in the inspector
// (`.kb-pinsp__folder*`). Tests that depended on the popover trigger /
// tabbed UX (Esc-to-close, Folder|Recent tabs) are dropped — Recent
// moved to Cmdk; see spa-cmdk-recent.spec.ts.

type Doc = {
  id: string;
  source_relative: string;
  folder: string;
  path: string;
};

async function listDocs(
  request: import("@playwright/test").APIRequestContext,
): Promise<Doc[]> {
  const r = await request.get(
    `http://127.0.0.1:${PORT}/api/kb/canon/docs?limit=50`,
  );
  expect(r.status()).toBe(200);
  return (await r.json()) as Doc[];
}

async function pmDocs(
  request: import("@playwright/test").APIRequestContext,
): Promise<Doc[]> {
  const docs = await listDocs(request);
  // pm/ subfolder has 4 files (00-summary, 01-timeline, 02-cause,
  // 03-actions) per global-setup.ts. The G4 fixture adds one nested
  // pm/extra/note.html that's NOT a direct sibling and should not
  // appear in the direct-only count.
  const pm = docs
    .filter((d) => d.folder === "pm")
    .sort((a, b) =>
      (a.path.split("/").pop() ?? "").localeCompare(b.path.split("/").pop() ?? ""),
    );
  expect(pm.length, "pm/ should have 4 direct-folder docs").toBe(4);
  return pm;
}

async function pmExtraDoc(
  request: import("@playwright/test").APIRequestContext,
): Promise<Doc> {
  const docs = await listDocs(request);
  const hit = docs.find((d) => d.folder === "pm/extra");
  expect(hit, "global-setup.ts should have seeded pm/extra/note.html").toBeDefined();
  return hit!;
}

async function gotoArtifact(page: Page, sourceRelative: string) {
  await page.goto(`http://127.0.0.1:${PORT}/a/canon/${sourceRelative}`);
  await expect(
    page.getByRole("navigation", { name: "artifact context" }),
  ).toBeVisible();
  // PF-F1 — wait for the Folder section's CURRENT-ARTIFACT ROW, not just
  // its container. `.kb-pinsp__folder-list` itself renders as soon as
  // `doc` resolves (PreviewInspector's `showFolderBrowser` is already true
  // via `browserWired && doc.folder !== ""`, independently of whether the
  // descendants/siblings fetch has landed) — so the list can be visible
  // but EMPTY for a beat while `folderRows` is still `[]`. ArtifactPane's
  // `[`/`]` hotkey handler reads that SAME descendants query and
  // early-returns as a silent no-op while it's empty (`direct.length <=
  // 1`), which is the race behind the "wraps to the last" flake: the key
  // gets pressed before the handler has any siblings to walk, so no
  // navigation ever fires and `toHaveURL` times out. The current-row
  // marker only renders once descendants has actually landed, so it's a
  // ready signal for BOTH the panel and the hotkey handler. Off-route
  // tests that don't need the section can skip this — only the
  // inspector-aware cases call this helper.
  await expect(page.locator(".kb-pinsp__folder-row.is-current")).toHaveCount(1);
}

// Workers share a browser context; localStorage is per-origin and
// persists across tests within one worker. Reset BOTH the inspector
// collapsed state (so the Folder section is visible) AND `kb:siblings`
// (so persistence-sensitive cases start from a known state).
test.beforeEach(async ({ page }) => {
  await page.goto(`http://127.0.0.1:${PORT}/`);
  await page.evaluate(() => {
    try {
      localStorage.removeItem("kb:siblings");
      localStorage.removeItem("kb:inspector-collapsed.detail");
    } catch {
      /* noop */
    }
  });
});

test.describe("spa preview-inspector folder section", () => {
  test("Folder section lists direct siblings with the current row marked", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    await gotoArtifact(page, pm[0].source_relative);

    const list = page.locator(".kb-pinsp__folder-list");
    // F2 — subfolder DIR rows are ambient siblings of the file rows; the
    // file count excludes them.
    const items = list.locator(
      ".kb-pinsp__folder-row:not(.kb-pinsp__folder-row--dir)",
    );
    await expect(items).toHaveCount(4);
    // The pm/extra subfolder renders as an always-visible dir row.
    await expect(
      list.locator(".kb-pinsp__folder-row--dir").filter({ hasText: "extra" }),
    ).toHaveCount(1);
    await expect(
      list.locator(".kb-pinsp__folder-row.is-current"),
    ).toHaveCount(1);
    // Position header reads "N of M" using the filtered FILE count.
    await expect(page.locator(".kb-pinsp__folder-pos")).toContainText("of 4");
  });

  test("clicking a sibling navigates to it and updates the current marker", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    await gotoArtifact(page, pm[0].source_relative);

    const targetFilename = pm[1].path.split("/").pop()!;
    await page
      .locator(".kb-pinsp__folder-row")
      .filter({ hasText: targetFilename })
      .click();

    await expect(page).toHaveURL(artifactUrlRe("canon", pm[1].source_relative));
    // After nav, the inspector re-renders for the new artifact; the
    // current marker now sits on pm[1].
    await expect(page.locator(".kb-pinsp__folder-list")).toBeVisible();
    const current = page.locator(".kb-pinsp__folder-row.is-current");
    await expect(current).toContainText(targetFilename);
  });

  test("] hotkey navigates to the next sibling", async ({ page, request }) => {
    const pm = await pmDocs(request);
    await gotoArtifact(page, pm[0].source_relative);
    await page.locator("body").click({ position: { x: 5, y: 5 } });

    await page.keyboard.press("]");
    await expect(page).toHaveURL(artifactUrlRe("canon", pm[1].source_relative));
  });

  test("[ hotkey from the first sibling wraps to the last", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    await gotoArtifact(page, pm[0].source_relative);
    await page.locator("body").click({ position: { x: 5, y: 5 } });

    await page.keyboard.press("[");
    await expect(page).toHaveURL(
      artifactUrlRe("canon", pm[pm.length - 1].source_relative),
    );
  });

  test("] does NOT navigate when an input has focus", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    await gotoArtifact(page, pm[0].source_relative);

    await page.evaluate(() => {
      const inp = document.createElement("input");
      inp.id = "__guard_input";
      inp.type = "text";
      document.body.appendChild(inp);
      inp.focus();
    });
    const urlBefore = page.url();
    await page.keyboard.press("]");
    expect(page.url()).toBe(urlBefore);
    const v = await page.locator("#__guard_input").inputValue();
    expect(v).toBe("]");
  });

  // ---------- G4 ----------

  test("subfolder toggle reveals the nested pm/extra/note.html", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    const extra = await pmExtraDoc(request);
    const extraFilename = extra.path.split("/").pop()!;

    await gotoArtifact(page, pm[0].source_relative);
    const list = page.locator(".kb-pinsp__folder-list");
    // F2 — dir rows are ambient regardless of the flatten toggle; count
    // FILE rows only.
    const fileRows = list.locator(
      ".kb-pinsp__folder-row:not(.kb-pinsp__folder-row--dir)",
    );

    // Off by default → only the 4 direct siblings.
    await expect(fileRows).toHaveCount(4);

    // Tick the toggle → the FILE list grows to include the nested file.
    await page.locator(".kb-pinsp__folder-toggle input").check();
    await expect(fileRows).toHaveCount(5);
    const nested = list
      .locator(".kb-pinsp__folder-row")
      .filter({ hasText: extraFilename });
    await expect(nested).toBeVisible();
    // Nested entries surface their sub-folder prefix in a dedicated span.
    await expect(nested.locator(".kb-pinsp__folder-prefix")).toHaveText(
      "extra/",
    );

    // Clicking it navigates to the nested artifact.
    await nested.click();
    await expect(page).toHaveURL(artifactUrlRe("canon", extra.source_relative));
  });

  test("sort=title reorders the folder list", async ({ page, request }) => {
    const pm = await pmDocs(request);
    await gotoArtifact(page, pm[0].source_relative);

    const list = page.locator(".kb-pinsp__folder-list");
    // F1 — the NEW-user default is updated (mtime desc); select name
    // explicitly and assert alphabetical filename order (file rows only —
    // F2 dir rows are not part of the sorted file list).
    await page.locator(".kb-pinsp__folder-sort").selectOption("name");
    const nameOrder = await list
      .locator(
        ".kb-pinsp__folder-row:not(.kb-pinsp__folder-row--dir) .kb-pinsp__folder-name",
      )
      .allInnerTexts();
    expect(nameOrder.slice().sort()).toEqual(nameOrder);

    // Switch to title — the rendered .kb-pinsp__folder-title column
    // (or the .kb-pinsp__folder-name fallback for titleless rows) is
    // alphabetically sorted afterwards.
    await page.locator(".kb-pinsp__folder-sort").selectOption("title");
    const titles = await list
      .locator(".kb-pinsp__folder-row:not(.kb-pinsp__folder-row--dir)")
      .evaluateAll((rows) =>
        rows.map((row) => {
          const t = row.querySelector(".kb-pinsp__folder-title");
          const n = row.querySelector(".kb-pinsp__folder-name");
          return (t?.textContent ?? n?.textContent ?? "").trim();
        }),
      );
    const sorted = titles
      .slice()
      .sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }));
    expect(titles).toEqual(sorted);
  });

  test("subfolders + sort preferences persist across reload", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    await gotoArtifact(page, pm[0].source_relative);

    await page.locator(".kb-pinsp__folder-toggle input").check();
    await page.locator(".kb-pinsp__folder-sort").selectOption("title");

    // The hook writes to localStorage synchronously inside setState.
    const persisted = await page.evaluate(() =>
      localStorage.getItem("kb:siblings"),
    );
    expect(persisted).not.toBeNull();
    const parsed = JSON.parse(persisted!);
    expect(parsed.includeSubfolders).toBe(true);
    expect(parsed.sort).toBe("title");

    // Reload — prefs survive on the same artifact.
    await page.reload();
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();
    await expect(page.locator(".kb-pinsp__folder-list")).toBeVisible();
    await expect(
      page.locator(".kb-pinsp__folder-toggle input"),
    ).toBeChecked();
    await expect(page.locator(".kb-pinsp__folder-sort")).toHaveValue("title");
  });
});
