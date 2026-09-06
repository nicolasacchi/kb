import { test, expect, type APIRequestContext } from "@playwright/test";
import { PORT } from "./helpers";

// G5: reading-progress chip. After a recordOpen + recordScroll round
// trip, the gallery Card and the FloatingPill sibling popover both
// render a mini bar + percentage for that artifact. ≥95% flips to a
// green ✓; unread artifacts show no chip at all.
//
// Each test seeds its own scroll state via HTTP (Playwright's
// `request` fixture) so we don't need a real iframe scroll, and the
// state survives the page navigation that follows.

const BASE = `http://127.0.0.1:${PORT}`;

type Doc = {
  id: string;
  source_relative: string;
  folder: string;
  path: string;
};

async function pmDocs(request: APIRequestContext): Promise<Doc[]> {
  const r = await request.get(`${BASE}/api/kb/canon/docs?limit=50`);
  expect(r.status()).toBe(200);
  const docs = (await r.json()) as Doc[];
  return docs
    .filter((d) => d.folder === "pm")
    .sort((a, b) =>
      (a.path.split("/").pop() ?? "").localeCompare(
        b.path.split("/").pop() ?? "",
      ),
    );
}

async function seedProgress(
  request: APIRequestContext,
  artifactId: string,
  scrollY: number,
  scrollMax: number,
): Promise<void> {
  const open = await request.post(`${BASE}/api/kb/canon/history/open`, {
    data: { artifact_id: artifactId },
  });
  expect(open.ok(), `recordOpen for ${artifactId} (${open.status()})`).toBe(
    true,
  );
  const body = (await open.json()) as { visit_id: number };
  const scroll = await request.post(`${BASE}/api/kb/canon/history/scroll`, {
    data: { visit_id: body.visit_id, scroll_y: scrollY, scroll_max: scrollMax },
  });
  expect(
    scroll.ok(),
    `recordScroll for ${artifactId} (${scroll.status()})`,
  ).toBe(true);
}

test.describe("spa reading-progress chip", () => {
  test.beforeEach(async ({ page }) => {
    // Ensure the inspector starts expanded so the Folder section is
    // visible — earlier workers may have collapsed it.
    await page.goto(`${BASE}/`);
    await page.evaluate(() => {
      try {
        localStorage.removeItem("kb:inspector-collapsed.detail");
        localStorage.removeItem("kb:siblings");
      } catch {
        /* noop */
      }
    });
  });

  test("gallery Card and PreviewInspector Folder row show the same partial chip", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    // Seed pm[3] (03-actions) to 50% (500 / 1000). pm[3] deliberately: the
    // synthetic scroll_max=1000 only survives while no worker's BROWSER
    // opens the doc — since linkflow, merely opening an artifact and
    // navigating away leave-flushes one real `kb:scroll`, correcting
    // scroll_max to the document's real height and re-deriving the pct out
    // from under an exact pin. Other specs (backbutton, linkflow) open the
    // HEAD of the pm chain (summary → timeline); nothing ever opens the
    // tail. The card is likewise pinned to that doc by title — "first card
    // with a chip" would race those same workers' freshly-chipped docs.
    await seedProgress(request, pm[3].id, 500, 1000);

    // Gallery → pm/ folder. The chip on pm[3]'s card shows 50%.
    await page.goto(`${BASE}/?folder=pm`);
    const targetCard = page
      .getByRole("link", { name: /^Open INC-0315 · Action Items/ })
      .first();
    await expect(targetCard).toBeVisible();
    const cardChip = targetCard.locator(".reading-chip").first();
    await expect(cardChip).toContainText("50%");
    await expect(cardChip).not.toHaveClass(/reading-chip--done/);
    // The fill style carries the percentage as a width.
    const fillWidth = await cardChip
      .locator(".reading-chip__fill")
      .evaluate((el) => (el as HTMLElement).style.width);
    expect(fillWidth).toBe("50%");

    // Folder section row for pm[0]'s sibling pm[3] carries the same chip.
    await page.goto(`${BASE}/a/canon/${pm[0].source_relative}`);
    await expect(page.locator(".kb-pinsp__folder-list")).toBeVisible();
    const targetFilename = pm[3].path.split("/").pop()!;
    const row = page
      .locator(".kb-pinsp__folder-row")
      .filter({ hasText: targetFilename });
    const rowChip = row.locator(".reading-chip");
    await expect(rowChip).toBeVisible();
    await expect(rowChip).toHaveClass(/reading-chip--compact/);
    await expect(rowChip).toContainText("50%");
  });

  test("≥95% flips card + Folder-row chips to the green ✓", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    // Seed pm[2] to 96% (960 / 1000).
    await seedProgress(request, pm[2].id, 960, 1000);

    // Gallery card shows ✓ instead of the bar.
    await page.goto(`${BASE}/?folder=pm`);
    const doneChip = page
      .getByRole("link", { name: /^Open / })
      .locator(".reading-chip--done");
    await expect(doneChip.first()).toBeVisible();
    await expect(doneChip.first()).toHaveText("✓");
    // No bar inside a done chip.
    await expect(doneChip.first().locator(".reading-chip__bar")).toHaveCount(0);

    // Folder section row for pm[2] (viewing from a different sibling).
    await page.goto(`${BASE}/a/canon/${pm[0].source_relative}`);
    await expect(page.locator(".kb-pinsp__folder-list")).toBeVisible();
    const targetFilename = pm[2].path.split("/").pop()!;
    const row = page
      .locator(".kb-pinsp__folder-row")
      .filter({ hasText: targetFilename });
    const rowChip = row.locator(".reading-chip");
    await expect(rowChip).toHaveClass(/reading-chip--done/);
    await expect(rowChip).toHaveText("✓");
  });

  test("unread artifact has no chip on its card", async ({
    page,
    request,
  }) => {
    const docs = (await (
      await request.get(`${BASE}/api/kb/canon/docs?limit=50`)
    ).json()) as Doc[];
    // A root-level canon artifact that no test has seeded — kitchen-sink
    // has a stable name and lives at root, so it stays "no recorded
    // visit" unless prior tests opened it (they may have via
    // gotoArtifact navigations; if so, scroll_max would be 0 from the
    // synthetic short content, and the chip still wouldn't render).
    const target = docs.find((d) =>
      d.path.endsWith("kitchen-sink.html"),
    );
    expect(target).toBeDefined();

    await page.goto(`${BASE}/?folder=`);
    const card = page.getByRole("link", {
      name: new RegExp(`Open `),
    }).filter({
      hasNot: page.locator(".reading-chip"),
    });
    // At least one root card has no chip (the unread fixture above).
    await expect(card.first()).toBeVisible();
  });

  // RP-track — the inspector's "Read by you" block renders from the
  // per-artifact reading summary after a reading beacon (open + scroll +
  // section dwell). Distinct from the legacy chip above (FLAG-4: both coexist).
  test("inspector 'Read by you' block reflects a reading beacon", async ({
    page,
    request,
  }) => {
    const pm = await pmDocs(request);
    const target = pm[0];
    const open = await request.post(`${BASE}/api/kb/canon/history/open`, {
      data: { artifact_id: target.id },
    });
    const { visit_id } = (await open.json()) as { visit_id: number };
    await request.post(`${BASE}/api/kb/canon/history/scroll`, {
      data: { visit_id, scroll_y: 800, scroll_max: 1000 },
    });
    const beacon = await request.post(`${BASE}/api/kb/canon/history/reading`, {
      data: {
        visit_id,
        artifact_id: target.id,
        active_ms: 42000,
        last_section: "intro",
        sections: [
          {
            id: "intro",
            idx: 0,
            text: "Intro",
            level: 2,
            words: 100,
            content_px: 500,
            dwell_ms: 40000,
            enters: 1,
          },
        ],
      },
    });
    expect(beacon.status()).toBe(204);

    await page.goto(`${BASE}/a/canon/${target.source_relative}`);
    const body = page.locator(".kb-pinsp__body");
    await expect(body).toContainText("Read by you");
    await expect(page.locator(".kb-pinsp__readby").first()).toBeVisible();
    await expect(body).toContainText("80% scrolled");
  });
});
