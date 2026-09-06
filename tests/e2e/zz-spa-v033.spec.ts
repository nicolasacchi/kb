import { test, expect, type APIRequestContext, type Page } from "@playwright/test";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { BASE, PORT } from "./helpers";

// Wave Z / v0.33 — exact-folder · folder note · list prune · move-conflict ·
// rename-in-place. Named `zz-` so it runs after read-only specs that assert
// fixed doc-counts on the shared temp corpus.
//
// Seeding strategy (copied from zz-spa-move.spec.ts): ALL fixture folders
// land in ONE file-level beforeAll with a retouch loop. Files written into
// brand-new dirs can miss the inotify add-watch window; the 60s reconciler
// is the heal; a rewrite after ~10s always lands. Tests never create NEW
// dirs later. Engine-backed moves/renames are actor-safe; list-prune's
// in-test file DELETE is fine (creates race the watcher, not deletes).

type Doc = {
  id: string;
  source_relative: string;
  folder: string;
  path: string;
  title?: string;
  first_indexed_unix?: number | null;
  mtime_unix?: number | null;
  indexed_at_unix?: number | null;
};

type ListEntry = {
  id: string;
  artifact_id: string;
  tombstone?: boolean;
  source_relative?: string;
};

type ListDetail = {
  list: { id: string; title: string };
  entries: ListEntry[];
};

const CORPUS = process.env.KB_E2E_CORPUS;

type Seed =
  | { kind: "html"; folder: string; file: string; title: string }
  | { kind: "md"; folder: string; file: string; title: string; body: string };

const SEEDS: Seed[] = [
  // exact-folder (API + UI)
  { kind: "html", folder: "z33-exact", file: "a.html", title: "Z33 Exact A" },
  {
    kind: "html",
    folder: "z33-exact/sub",
    file: "b.html",
    title: "Z33 Exact Sub B",
  },
  // first_indexed (API)
  {
    kind: "html",
    folder: "z33-first",
    file: "doc.html",
    title: "Z33 First Indexed",
  },
  // folder note (UI)
  {
    kind: "md",
    folder: "z33-note",
    file: "index.md",
    title: "Z33 Folder Note",
    body: "Folder note body for v0.33 Z coverage.",
  },
  {
    kind: "html",
    folder: "z33-note",
    file: "doc.html",
    title: "Z33 Note Sibling",
  },
  // list prune
  { kind: "html", folder: "z33-list", file: "x.html", title: "Z33 List X" },
  { kind: "html", folder: "z33-list", file: "y.html", title: "Z33 List Y" },
  // move-conflict + rename-in-place
  { kind: "html", folder: "z33-mv", file: "one.html", title: "Z33 Move One" },
  { kind: "html", folder: "z33-mv", file: "two.html", title: "Z33 Move Two" },
];

// Every dir any test can leave behind (incl. nested exact sub + rename targets).
const ALL_DIRS = [
  "z33-exact",
  "z33-first",
  "z33-note",
  "z33-list",
  "z33-mv",
];

function seedRel(s: Seed): string {
  return `${s.folder}/${s.file}`;
}

function htmlArtifact(title: string): string {
  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>${title}</title>
    <meta name="kb-tags" content="e2e, z33" />
    <meta name="kb-category" content="notes" />
  </head>
  <body>
    <h1>${title}</h1>
    <p>Seeded by zz-spa-v033.spec.ts.</p>
  </body>
</html>
`;
}

/** Plain markdown (no `kb-category: note`) so it stays gallery-visible
 *  and still matches the `<folder>/index.md` folder-note convention. */
function mdArtifact(title: string, body: string): string {
  return `# ${title}\n\n${body}\n`;
}

function writeSeed(s: Seed, content?: string) {
  const dir = join(CORPUS as string, s.folder);
  mkdirSync(dir, { recursive: true });
  const body =
    content ??
    (s.kind === "md"
      ? mdArtifact(s.title, s.body)
      : htmlArtifact(s.title));
  writeFileSync(join(dir, s.file), body, "utf-8");
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
  for (const s of SEEDS) writeSeed(s);

  // Wait until every seed is indexed (fetch — the `request` fixture is
  // test-scoped and unavailable in beforeAll). A file written into a
  // brand-new directory can land in the watcher's dir-create→add-watch
  // blind window (the 60s reconciler is the daemon's designed heal); when
  // a seed hasn't appeared, REWRITE it — by then the directory watch is
  // established, so the fresh write event always lands.
  const missingSeeds = async (): Promise<Seed[]> => {
    const r = await fetch(`${BASE}/api/kb/canon/docs?limit=200`);
    if (!r.ok) return SEEDS;
    const docs = (await r.json()) as Doc[];
    return SEEDS.filter(
      (s) => !docs.some((d) => d.source_relative === seedRel(s)),
    );
  };
  for (let round = 0; round < 3; round++) {
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline) {
      if ((await missingSeeds()).length === 0) return;
      await new Promise((r) => setTimeout(r, 500));
    }
    for (const s of await missingSeeds()) {
      writeSeed(s);
    }
  }
  const still = await missingSeeds();
  expect(
    still.map((s) => seedRel(s)),
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

async function listDocsFolder(
  request: APIRequestContext,
  folder: string,
  exact?: boolean,
): Promise<Doc[]> {
  const q = new URLSearchParams({ limit: "200", folder });
  if (exact) q.set("folder_exact", "1");
  const r = await request.get(`${BASE}/api/kb/canon/docs?${q}`);
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
  // About sections.
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
  artifactId: string,
): Promise<ListEntry> {
  const r = await request.post(
    `${BASE}/api/kb/canon/lists/${listId}/entries`,
    { data: { artifact_id: artifactId } },
  );
  expect(r.status(), await r.text()).toBe(201);
  return (await r.json()) as ListEntry;
}

async function fetchListDetail(
  request: APIRequestContext,
  listId: string,
): Promise<ListDetail> {
  const r = await request.get(`${BASE}/api/kb/canon/lists/${listId}`);
  expect(r.status()).toBe(200);
  return (await r.json()) as ListDetail;
}

test.describe("spa v0.33 Z — exact-folder · folder note · list prune · move", () => {
  test("exact-folder (API): folder= includes descendants; folder_exact=1 does not", async ({
    request,
  }) => {
    await waitIndexed(request, "z33-exact/a.html");
    await waitIndexed(request, "z33-exact/sub/b.html");

    const loose = await listDocsFolder(request, "z33-exact");
    const looseRels = loose.map((d) => d.source_relative).sort();
    expect(looseRels).toEqual(
      ["z33-exact/a.html", "z33-exact/sub/b.html"].sort(),
    );

    const exact = await listDocsFolder(request, "z33-exact", true);
    const exactRels = exact.map((d) => d.source_relative);
    expect(exactRels).toContain("z33-exact/a.html");
    expect(exactRels).not.toContain("z33-exact/sub/b.html");
    expect(exactRels.every((r) => r === "z33-exact/a.html" || !r.startsWith("z33-exact/sub/"))).toBe(
      true,
    );
    // Only the exact-folder seed for this prefix (a.html).
    expect(exact.filter((d) => d.folder === "z33-exact").map((d) => d.source_relative)).toEqual([
      "z33-exact/a.html",
    ]);
  });

  test("exact-folder (UI): toggle folder-exact hides subfolder cards + URL", async ({
    page,
    request,
  }) => {
    await waitIndexed(request, "z33-exact/a.html");
    await waitIndexed(request, "z33-exact/sub/b.html");

    await page.goto(
      `http://127.0.0.1:${PORT}/?kb=canon&folder=${encodeURIComponent("z33-exact")}`,
    );

    const cardA = page.getByRole("link", { name: /Z33 Exact A/ });
    const cardB = page.getByRole("link", { name: /Z33 Exact Sub B/ });
    await expect(cardA).toBeVisible({ timeout: 15_000 });
    await expect(cardB).toBeVisible({ timeout: 15_000 });

    const toggle = page.locator('[data-kb-act="folder-exact"]');
    await expect(toggle).toBeVisible();
    await expect(toggle).toHaveAttribute("aria-pressed", "false");

    await toggle.click();

    await expect
      .poll(() => {
        const u = new URL(page.url());
        return u.searchParams.get("folder_exact");
      }, { timeout: 10_000 })
      .toBe("1");
    await expect(toggle).toHaveAttribute("aria-pressed", "true");
    await expect(cardA).toBeVisible();
    await expect(cardB).toHaveCount(0);

    await toggle.click();
    await expect
      .poll(() => {
        const u = new URL(page.url());
        return u.searchParams.get("folder_exact");
      }, { timeout: 10_000 })
      .toBeNull();
    await expect(toggle).toHaveAttribute("aria-pressed", "false");
    await expect(cardA).toBeVisible();
    await expect(cardB).toBeVisible();
  });

  test("first_indexed (API): reindex keeps first_indexed_unix stable", async ({
    request,
  }) => {
    const rel = "z33-first/doc.html";
    const before = await waitIndexed(request, rel);
    expect(typeof before.first_indexed_unix).toBe("number");
    expect(before.first_indexed_unix).toBeGreaterThan(0);
    const firstIndexed = before.first_indexed_unix as number;
    const prevIndexedAt = before.indexed_at_unix ?? 0;
    const prevMtime = before.mtime_unix ?? 0;

    // Small content change → watcher reindex; first_indexed must not move.
    writeFileSync(
      join(CORPUS as string, "z33-first", "doc.html"),
      htmlArtifact("Z33 First Indexed").replace(
        "Seeded by zz-spa-v033.spec.ts.",
        "Seeded by zz-spa-v033.spec.ts. retouch-1",
      ),
      "utf-8",
    );

    await expect
      .poll(
        async () => {
          const docs = await listDocs(request);
          const d = docs.find((x) => x.source_relative === rel);
          if (!d) return null;
          const mtimeMoved =
            (d.mtime_unix ?? 0) > prevMtime ||
            (d.indexed_at_unix ?? 0) > prevIndexedAt;
          return mtimeMoved ? d : null;
        },
        { timeout: 30_000 },
      )
      .toBeTruthy();

    const after = await waitIndexed(request, rel);
    expect(after.first_indexed_unix).toBe(firstIndexed);
    expect(after.id).toBe(before.id);
  });

  test("folder note: gallery strip links to <folder>/index.md", async ({
    page,
    request,
  }) => {
    await waitIndexed(request, "z33-note/index.md");
    await waitIndexed(request, "z33-note/doc.html");

    await page.goto(
      `http://127.0.0.1:${PORT}/?kb=canon&folder=${encodeURIComponent("z33-note")}`,
    );

    const strip = page.locator('[data-kb-act="folder-note"]');
    await expect(strip).toBeVisible({ timeout: 15_000 });
    await expect(strip).toHaveAttribute(
      "href",
      /\/a\/canon\/z33-note\/index\.md/,
    );
    // Title from the H1 / filename surface.
    await expect(strip).toContainText(/Z33 Folder Note|folder note/i);

    await strip.click();
    await expect
      .poll(() => page.url(), { timeout: 15_000 })
      .toMatch(/z33-note\/index\.md/);
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();
  });

  test("list prune: tombstone after delete → Clean up → one live entry", async ({
    page,
    request,
  }) => {
    const x = await waitIndexed(request, "z33-list/x.html");
    const y = await waitIndexed(request, "z33-list/y.html");

    const listId = await createList(request, "Z33 Prune");
    await addEntry(request, listId, x.id);
    await addEntry(request, listId, y.id);

    // In-test DELETE only (creates into new dirs are the race; deletes are fine).
    rmSync(join(CORPUS as string, "z33-list", "x.html"), { force: true });

    // Poll until the list surface marks the missing artifact as tombstoned
    // (watcher drop from lance → enrichment sets tombstone).
    await expect
      .poll(
        async () => {
          const detail = await fetchListDetail(request, listId);
          const tombs = detail.entries.filter((e) => e.tombstone);
          return tombs.length;
        },
        { timeout: 45_000 },
      )
      .toBe(1);

    await page.goto(`http://127.0.0.1:${PORT}/lists/canon/${listId}`);
    await expect(page.getByTestId("list-detail")).toBeVisible({
      timeout: 15_000,
    });

    const pruneBtn = page.locator('[data-kb-act="list-prune"]');
    await expect(pruneBtn).toBeVisible();
    await expect(pruneBtn).toContainText(/1 missing|Clean up 1/i);

    await pruneBtn.click();
    await page.locator(".confirm__go").click();

    await expect(
      page
        .locator('[data-kb-toast="ok"]')
        .filter({ hasText: /removed 1 missing/i }),
    ).toBeVisible({ timeout: 10_000 });

    await expect
      .poll(
        async () => {
          const detail = await fetchListDetail(request, listId);
          const tombs = detail.entries.filter((e) => e.tombstone).length;
          return { n: detail.entries.length, tombs };
        },
        { timeout: 15_000 },
      )
      .toEqual({ n: 1, tombs: 0 });

    const final = await fetchListDetail(request, listId);
    expect(final.entries[0].artifact_id).toBe(y.id);

    // Cleanup list so it does not leak into other list-index assertions.
    const del = await request.delete(`${BASE}/api/kb/canon/lists/${listId}`);
    expect(del.status()).toBe(204);
  });

  test("rename-in-place + move-conflict: filename-only rename; existing target errors", async ({
    page,
    request,
  }) => {
    const oneRel = "z33-mv/one.html";
    const twoRel = "z33-mv/two.html";
    const unoRel = "z33-mv/uno.html";

    const beforeOne = await waitIndexed(request, oneRel);
    const beforeTwo = await waitIndexed(request, twoRel);

    // --- (a) rename-in-place: one.html → uno.html (same folder) ---
    await resetReaderChrome(page);
    await gotoArtifact(page, oneRel);

    await page.locator('[data-kb-act="move-artifact"]').click();
    const dlg = page.locator("dialog.kb-move");
    await expect(dlg).toBeVisible();
    await expect(
      dlg.getByRole("heading", { name: "Move artifact" }),
    ).toBeVisible();

    const filenameInput = dlg.getByLabel("filename");
    await filenameInput.fill("uno.html");
    await expect(dlg.locator(".kb-move__path").last()).toHaveText(unoRel);

    await dlg.locator("button.kb-move__btn--primary").click();

    await expect
      .poll(() => urlSourceRel(page.url()), { timeout: 20_000 })
      .toBe(unoRel);

    await expect(
      page.locator('[data-kb-toast="ok"]').filter({ hasText: /moved to/ }),
    ).toBeVisible({ timeout: 5_000 });

    const afterRename = await waitIndexed(request, unoRel);
    expect(afterRename.id).not.toBe(beforeOne.id);
    expect(afterRename.source_relative).toBe(unoRel);

    // --- (b) conflict: uno.html → two.html (existing) ---
    await resetReaderChrome(page);
    await gotoArtifact(page, unoRel);

    const twoStill = await waitIndexed(request, twoRel);
    expect(twoStill.id).toBe(beforeTwo.id);

    await page.locator('[data-kb-act="move-artifact"]').click();
    const dlg2 = page.locator("dialog.kb-move");
    await expect(dlg2).toBeVisible();
    await dlg2.getByLabel("filename").fill("two.html");
    await expect(dlg2.locator(".kb-move__path").last()).toHaveText(twoRel);

    // Target path is client-valid (no .kb-move__preview-err); server rejects
    // the collision → toast.err, modal stays open (MoveArtifactModal catch).
    await dlg2.locator("button.kb-move__btn--primary").click();

    await expect(
      page.locator('[data-kb-toast="err"]').filter({ hasText: /move failed/i }),
    ).toBeVisible({ timeout: 10_000 });
    await expect(dlg2).toBeVisible();

    // Docs unchanged: uno still live under rename id; two keeps original id.
    const unoStill = await waitIndexed(request, unoRel);
    expect(unoStill.id).toBe(afterRename.id);
    const twoFinal = await waitIndexed(request, twoRel);
    expect(twoFinal.id).toBe(beforeTwo.id);

    // one.html must not reappear as a live path after the failed conflict.
    const docs = await listDocs(request);
    expect(docs.some((d) => d.source_relative === oneRel)).toBe(false);
  });
});
