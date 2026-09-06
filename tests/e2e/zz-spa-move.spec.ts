import { test, expect, type APIRequestContext, type Page } from "@playwright/test";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { BASE, PORT } from "./helpers";

// F4 / FU2 — move-artifact + rename-folder UI coverage (desktop + mobile).
//
// Product already ships: MoveArtifactModal (reader Folder rail),
// RenameFolderModal (gallery when ?folder= is set), relocate engine
// (POST …/docs/{id}/move, …/folders/rename). Artifact ids are
// source-relative-path-derived, so a successful move ALWAYS changes the
// id; old paths resolve through the moves chain (shell 301).
//
// Named `zz-` so it runs after read-only specs that assert fixed
// doc-counts on the shared temp corpus (same rationale as zz-live-fs).
//
// Seeding strategy — ALL fixture folders are written in ONE file-level
// beforeAll, before any test mutates the corpus. A file created inside a
// brand-new directory right after a recursive dir removal can miss the
// inotify watcher (add-watch race) and stay invisible until the 60s
// reconcile pass — far past any sane poll timeout. Seeding everything on
// the fresh watcher (and cleaning up once in afterAll) removes that
// window entirely; the moves/renames the tests then perform are
// actor-backed (the relocate engine updates storage directly), so their
// post-move waits never depend on watcher pickup.

type Doc = {
  id: string;
  source_relative: string;
  folder: string;
  path: string;
  title?: string;
};

const CORPUS = process.env.KB_E2E_CORPUS;

// One folder per test — tests are order-independent and never touch
// another test's seeds (or the canon fixtures other specs assert on).
const SEEDS: Array<{ folder: string; file: string; title: string }> = [
  { folder: "fu2-move", file: "alpha.html", title: "FU2 Move Alpha" },
  { folder: "fu2-move", file: "sibling.html", title: "FU2 Move Sibling" },
  { folder: "fu2-rename", file: "beta.html", title: "FU2 Rename Beta" },
  { folder: "fu2-rename", file: "other.html", title: "FU2 Rename Other" },
  { folder: "fu2-val", file: "gamma.html", title: "FU2 Val Gamma" },
  { folder: "fu2-val", file: "keep.html", title: "FU2 Val Keep" },
  { folder: "fu2-mobile", file: "delta.html", title: "FU2 Mobile Delta" },
  { folder: "fu2-mobile", file: "mate.html", title: "FU2 Mobile Mate" },
];

// Every dir any test can leave behind (incl. rename target) — afterAll sweep.
const ALL_DIRS = ["fu2-move", "fu2-rename", "fu2-rename-done", "fu2-val", "fu2-mobile"];

function htmlArtifact(title: string): string {
  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>${title}</title>
    <meta name="kb-tags" content="e2e, fu2-move" />
    <meta name="kb-category" content="notes" />
  </head>
  <body>
    <h1>${title}</h1>
    <p>Seeded by zz-spa-move.spec.ts.</p>
  </body>
</html>
`;
}

/** Wipe a seed folder under the watched corpus (best-effort). */
function rmSeed(relDir: string) {
  if (!CORPUS) return;
  try {
    rmSync(join(CORPUS, relDir), { recursive: true, force: true });
  } catch {
    // already gone / never written
  }
}

test.beforeAll(async () => {
  expect(CORPUS, "KB_E2E_CORPUS must be exported by global-setup.ts").toBeTruthy();
  for (const dir of ALL_DIRS) rmSeed(dir);
  for (const s of SEEDS) {
    const dir = join(CORPUS as string, s.folder);
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, s.file), htmlArtifact(s.title), "utf-8");
  }
  // Wait until every seed is indexed (fetch — the `request` fixture is
  // test-scoped and unavailable in beforeAll). A file written into a
  // brand-new directory can land in the watcher's dir-create→add-watch
  // blind window (the 60s reconciler is the daemon's designed heal); when
  // a seed hasn't appeared, REWRITE it — by then the directory watch is
  // established, so the fresh write event always lands.
  const missingSeeds = async (): Promise<typeof SEEDS> => {
    const r = await fetch(`${BASE}/api/kb/canon/docs?limit=200`);
    if (!r.ok) return SEEDS;
    const docs = (await r.json()) as Doc[];
    return SEEDS.filter(
      (s) => !docs.some((d) => d.source_relative === `${s.folder}/${s.file}`),
    );
  };
  for (let round = 0; round < 3; round++) {
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline) {
      if ((await missingSeeds()).length === 0) return;
      await new Promise((r) => setTimeout(r, 500));
    }
    for (const s of await missingSeeds()) {
      writeFileSync(
        join(CORPUS as string, s.folder, s.file),
        htmlArtifact(s.title),
        "utf-8",
      );
    }
  }
  const still = await missingSeeds();
  expect(
    still.map((s) => `${s.folder}/${s.file}`),
    "seeds never indexed after retouch rounds",
  ).toEqual([]);
});

test.afterAll(() => {
  for (const dir of ALL_DIRS) rmSeed(dir);
});

async function listDocs(request: APIRequestContext): Promise<Doc[]> {
  const r = await request.get(`${BASE}/api/kb/canon/docs?limit=200`);
  expect(r.status()).toBe(200);
  return (await r.json()) as Doc[];
}

/** Poll until `source_relative` is live (actor-backed after a move). */
async function waitIndexed(
  request: APIRequestContext,
  sourceRel: string,
): Promise<Doc> {
  let found: Doc | undefined;
  await expect
    .poll(
      async () => {
        const docs = await listDocs(request);
        found = docs.find((d) => d.source_relative === sourceRel);
        return found?.id ?? null;
      },
      { timeout: 20_000 },
    )
    .toBeTruthy();
  return found!;
}

/** Reset inspector prefs so the Folder section is visible (tab=all, expanded). */
async function resetReaderChrome(page: Page) {
  await page.goto(`http://127.0.0.1:${PORT}/`);
  await page.evaluate(() => {
    try {
      localStorage.removeItem("kb:inspector-collapsed.detail");
      localStorage.removeItem("kb:inspector-tab.detail");
      localStorage.removeItem("kb:siblings");
    } catch {
      /* noop */
    }
  });
}

/** Open a canon artifact and wait for the Folder list (inspector default "all"). */
async function gotoArtifact(page: Page, sourceRel: string) {
  await page.goto(`http://127.0.0.1:${PORT}/a/canon/${sourceRel}`);
  await expect(
    page.getByRole("navigation", { name: "artifact context" }),
  ).toBeVisible();
  // Focus the Folder tab so the move control is not buried under stacked
  // About sections (and so mobile sheet content is the folder browser).
  const folderTab = page.locator('[data-kb-itab="folder"]');
  if (await folderTab.count()) {
    await folderTab.click();
  }
  await expect(page.locator(".kb-pinsp__folder-list")).toBeVisible({
    timeout: 15_000,
  });
}

/** Source-relative path segment of the current `/a/canon/<rel>` URL. */
function urlSourceRel(url: string): string {
  const m = url.match(/\/a\/canon\/(.+?)(?:\?|$)/);
  expect(m, `expected /a/canon/… URL, got ${url}`).toBeTruthy();
  return decodeURIComponent(m![1]);
}

test.describe("spa move / rename-folder UI (F4)", () => {
  test("desktop: move artifact → URL/id changes, toast, old path resolves", async ({
    page,
    request,
  }) => {
    const FILE = "alpha.html";
    const TITLE = "FU2 Move Alpha";
    const destSub = "nested";
    const oldRel = `fu2-move/${FILE}`;
    const newRel = `fu2-move/${destSub}/${FILE}`;

    const before = await waitIndexed(request, oldRel);
    await resetReaderChrome(page);
    await gotoArtifact(page, oldRel);

    const beforeUrlRel = urlSourceRel(page.url());
    expect(beforeUrlRel).toBe(oldRel);

    // Reader Folder rail → move (PreviewInspector FolderBrowserNav).
    await page.locator('[data-kb-act="move-artifact"]').click();
    const dlg = page.locator("dialog.kb-move");
    await expect(dlg).toBeVisible();
    await expect(
      dlg.getByRole("heading", { name: "Move artifact" }),
    ).toBeVisible();

    // cwd is the current folder (fu2-move); new subfolder builds the target.
    const subInput = dlg
      .locator(".kb-move__field")
      .filter({ hasText: "New subfolder" })
      .locator("input");
    await subInput.fill(destSub);
    await expect(dlg.locator(".kb-move__path").last()).toHaveText(newRel);

    await dlg.locator("button.kb-move__btn--primary").click();

    // onMoved → navigate(artifactHref(kb, newRel), { replace: true })
    await expect
      .poll(() => urlSourceRel(page.url()), { timeout: 20_000 })
      .toBe(newRel);
    expect(urlSourceRel(page.url())).not.toBe(beforeUrlRel);

    // Doc still renders (title via document.title · kb suffix).
    await expect(page).toHaveTitle(new RegExp(TITLE));
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();
    await expect(
      page
        .getByRole("navigation", { name: "artifact context" })
        .locator(".kb-ctxbar__crumb"),
    ).toContainText(FILE);

    // Success toast from MoveArtifactModal (toast.ok(`moved to ${…}`)).
    await expect(
      page.locator('[data-kb-toast="ok"]').filter({ hasText: /moved to/ }),
    ).toBeVisible({ timeout: 5_000 });

    // API: new path is live under a different id (actor-backed, no watcher).
    const after = await waitIndexed(request, newRel);
    expect(after.id).not.toBe(before.id);
    expect(after.source_relative).toBe(newRel);

    // Old path resolves through the moves chain (shell 301 → new rel).
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${oldRel}`);
    await expect
      .poll(() => urlSourceRel(page.url()), { timeout: 15_000 })
      .toBe(newRel);
    await expect(page).toHaveTitle(new RegExp(TITLE));
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();
  });

  test("rename folder from gallery → new folder name + new artifact ids", async ({
    page,
    request,
  }) => {
    // RenameFolderModal is wired on the gallery (active ?folder=), not the
    // reader FolderBrowserNav — that surface only has move (see gallery.tsx
    // data-kb-act="rename-folder").
    const FROM = "fu2-rename";
    const TO = "fu2-rename-done";
    const FILE = "beta.html";
    const TITLE = "FU2 Rename Beta";
    const oldRel = `${FROM}/${FILE}`;
    const newRel = `${TO}/${FILE}`;

    const before = await waitIndexed(request, oldRel);
    await resetReaderChrome(page);

    await page.goto(
      `http://127.0.0.1:${PORT}/?kb=canon&folder=${encodeURIComponent(FROM)}`,
    );
    // Folder-scoped gallery shows the rename control.
    await expect(page.locator('[data-kb-act="rename-folder"]')).toBeVisible({
      timeout: 15_000,
    });
    await expect(
      page.getByRole("link", { name: new RegExp(TITLE) }),
    ).toBeVisible({ timeout: 15_000 });

    await page.locator('[data-kb-act="rename-folder"]').click();
    const dlg = page.locator("dialog.kb-move");
    await expect(dlg).toBeVisible();
    await expect(
      dlg.getByRole("heading", { name: "Rename folder" }),
    ).toBeVisible();

    const pathInput = dlg.getByLabel("new folder path");
    await pathInput.fill(TO);
    await expect(dlg.locator(".kb-move__path").last()).toHaveText(TO);
    await dlg.locator("button.kb-move__btn--primary").click();

    // onRenamed → galleryUrl with folder: to
    await expect
      .poll(() => {
        const u = new URL(page.url());
        return u.searchParams.get("folder");
      }, { timeout: 20_000 })
      .toBe(TO);

    await expect(
      page
        .locator('[data-kb-toast="ok"]')
        .filter({ hasText: /renamed folder/ }),
    ).toBeVisible({ timeout: 5_000 });

    // Breadcrumb / h1 reflects the new folder path.
    await expect(page.locator("h1")).toContainText(TO);

    const after = await waitIndexed(request, newRel);
    expect(after.id).not.toBe(before.id);

    // The gallery card's href now carries the renamed path (auto-retries
    // through the SSE-driven row refresh; a raw .click() can race the
    // rename-dialog close / virtualized re-render and get swallowed).
    await expect(
      page.getByRole("link", { name: new RegExp(TITLE) }),
    ).toHaveAttribute("href", `/a/canon/${newRel}`, { timeout: 15_000 });
    // And the artifact serves under the new path.
    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${newRel}`);
    await expect
      .poll(() => urlSourceRel(page.url()), { timeout: 15_000 })
      .toBe(newRel);
    await expect(page).toHaveTitle(new RegExp(TITLE));

    // Folder browser crumbs show the renamed path.
    const folderTab = page.locator('[data-kb-itab="folder"]');
    if (await folderTab.count()) {
      await folderTab.click();
    }
    await expect(page.locator(".kb-pinsp__folder-crumbs")).toContainText(TO);
  });

  test("validation: invalid target shows movePath error and keeps modal open", async ({
    page,
    request,
  }) => {
    const rel = "fu2-val/gamma.html";

    await waitIndexed(request, rel);
    await resetReaderChrome(page);
    await gotoArtifact(page, rel);

    const urlBefore = page.url();

    await page.locator('[data-kb-act="move-artifact"]').click();
    const dlg = page.locator("dialog.kb-move");
    await expect(dlg).toBeVisible();

    // movePath.ts joinMoveTarget → validateRelPath on newSubfolder:
    // segment ".." ⇒ `new subfolder must not contain "." or ".."`
    const subInput = dlg
      .locator(".kb-move__field")
      .filter({ hasText: "New subfolder" })
      .locator("input");
    await subInput.fill("../escape");

    await expect(dlg.locator(".kb-move__preview-err")).toHaveText(
      'new subfolder must not contain "." or ".."',
    );
    // Submit stays disabled; modal remains open; no navigation.
    await expect(dlg.locator("button.kb-move__btn--primary")).toBeDisabled();
    await expect(dlg).toBeVisible();
    expect(page.url()).toBe(urlBefore);

    // Dismiss without mutating.
    await dlg.getByRole("button", { name: "Cancel" }).click();
    await expect(dlg).toHaveCount(0);
  });
});

// Mobile viewport is a separate describe so test.use applies only here
// (same pattern as spa-responsive.spec.ts — 390×844, touch).
test.describe("spa move UI (mobile)", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test("mobile: open inspect sheet → move → URL id/path changes", async ({
    page,
    request,
  }) => {
    const FILE = "delta.html";
    const TITLE = "FU2 Mobile Delta";
    const destSub = "pocket";
    const oldRel = `fu2-mobile/${FILE}`;
    const newRel = `fu2-mobile/${destSub}/${FILE}`;

    const before = await waitIndexed(request, oldRel);
    await resetReaderChrome(page);

    await page.goto(`http://127.0.0.1:${PORT}/a/canon/${oldRel}`);
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();

    // v0.23 — sole mobile reader entry is data-kb-act="inspect" (bottom sheet).
    await page.locator('[data-kb-act="inspect"]').click();
    await expect(page.locator(".detail")).toHaveClass(/detail--inspector-open/);

    await page.locator('[data-kb-itab="folder"]').click();
    await expect(page.locator('[data-kb-act="move-artifact"]')).toBeVisible();
    await page.locator('[data-kb-act="move-artifact"]').click();

    const dlg = page.locator("dialog.kb-move");
    await expect(dlg).toBeVisible();
    await dlg
      .locator(".kb-move__field")
      .filter({ hasText: "New subfolder" })
      .locator("input")
      .fill(destSub);
    await dlg.locator("button.kb-move__btn--primary").click();

    await expect
      .poll(() => urlSourceRel(page.url()), { timeout: 20_000 })
      .toBe(newRel);
    expect(urlSourceRel(page.url())).not.toBe(oldRel);

    const after = await waitIndexed(request, newRel);
    expect(after.id).not.toBe(before.id);
    await expect(page).toHaveTitle(new RegExp(TITLE));
  });
});
