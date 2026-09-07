import { expect, test, type APIRequestContext } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";
import { LOCAL_TARGET_FN, RESOLVER_FILE } from "./fixture-repo";

/// V74-L2 — the kbc-canvas/1 board SURFACE: a board applied through the
/// loopback route, then rendered, folded and walked.
///
/// The board deliberately carries one of each interesting shape: a resolvable
/// `code` node (which becomes a live card with a painted snippet), an ORPHAN
/// `code` node pointing at a file that does not exist (which must be a VISIBLE
/// card saying the code is gone — the product rule this whole surface exists
/// for), a `note`, a `query` card whose count is NOT re-run on an ordinary
/// read, and two steps so the walkthrough has a reading order.
///
/// Mutations are loopback-only server-side; this harness hits 127.0.0.1, so
/// `apply` works exactly as `kb-code canvas apply` would.

const SLUG = "e2e-board";

function boardDoc() {
  return {
    schema: "kbc-canvas/1",
    repo: REPO_NAME,
    slug: SLUG,
    title: "E2E board",
    description_md: "the fixture's own walkthrough",
    nodes: [
      {
        id: "live",
        kind: "code",
        title: "the definition",
        path: RESOLVER_FILE,
        // Starts at the DOC COMMENT, so `local_target` sits on the snippet's
        // SECOND line — a dropped `snippet_start + lineIndex` offset would ask
        // about line 1 instead of line 2, which is what the click below pins.
        range: [1, 4],
        context: [1, 8],
        symbol: LOCAL_TARGET_FN,
      },
      {
        id: "dead",
        kind: "code",
        title: "a file that is gone",
        path: "no-such-file.rs",
        range: [1, 2],
      },
      { id: "why", kind: "note", title: "why", body_md: "**because** the fixture says so" },
      {
        id: "counts",
        kind: "query",
        title: "callers",
        query: `repo:${REPO_NAME} /${LOCAL_TARGET_FN}`,
        authored_count: 2,
      },
    ],
    edges: [
      { from: "why", to: "live", kind: "reads" },
      { from: "live", to: "counts", kind: "then", provenance: "derived", trust: "likely" },
      { from: "live", to: "dead", kind: "calls" },
    ],
    steps: [
      { node: "live", caption: "start at the definition" },
      { node: "dead", caption: "then at the hole it left" },
    ],
  };
}

async function applyBoard(request: APIRequestContext) {
  const res = await request.post(`${BASE}/api/boards/apply`, {
    headers: { "X-Kbc-Request": "1", "Content-Type": "application/json" },
    data: boardDoc(),
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
  return res.json();
}

test.describe("kbc-canvas/1 boards", () => {
  test.beforeAll(async ({ request }) => {
    await applyBoard(request);
  });

  test.afterAll(async ({ request }) => {
    await request
      .delete(`${BASE}/api/boards/${SLUG}?repo=${REPO_NAME}`, {
        headers: { "X-Kbc-Request": "1" },
      })
      .catch(() => undefined);
  });

  test("the list shows the board with its status, and links the frozen legacy canvas", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~boards`);
    const row = page.locator(`[data-kbc-boards-row="${SLUG}"]`);
    await expect(row).toBeVisible({ timeout: 15_000 });
    // D21: an agent-proposed board is PENDING until a human accepts it.
    await expect(row.locator("[data-kbc-boards-row-status]")).toHaveText("pending");
    await expect(row).toContainText("4 nodes");
    // The two surfaces coexist: `~canvas` is linked once, as Legacy.
    await expect(page.locator("[data-kbc-boards-legacy-link]")).toHaveAttribute(
      "href",
      `/r/${REPO_NAME}/~canvas`,
    );
  });

  test("every node renders with its state and reason — including the orphan", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~boards/${SLUG}`);
    await expect(page.locator("[data-kbc-board-surface]")).toBeVisible({ timeout: 15_000 });

    for (const id of ["live", "dead", "why", "counts"]) {
      const card = page.locator(`[data-kbc-board-node="${id}"]`);
      await expect(card, `${id} must render`).toBeVisible();
      await expect(card).toHaveAttribute("data-kbc-board-state", /pinned|carried|orphan|present|inert/);
      await expect(card).toHaveAttribute("data-kbc-board-reason", /.+/);
    }

    // The live code card is painted from the daemon's own snippet.
    const live = page.locator('[data-kbc-board-node="live"]');
    await expect(live).toHaveAttribute("data-kbc-board-state", "pinned");
    await expect(live).toContainText(LOCAL_TARGET_FN);

    // The ORPHAN is a visible card that says the code is gone — never dropped,
    // never given a guessed position.
    const dead = page.locator('[data-kbc-board-node="dead"]');
    await expect(dead).toHaveAttribute("data-kbc-board-state", "orphan");
    await expect(dead.locator("[data-kbc-refcard-orphan]")).toContainText("the code is gone");
    // …and it carries no link, because there is nothing honest to link to.
    await expect(dead.locator("[data-kbc-board-link], a.kbc-refcard__addr")).toHaveCount(0);

    // A query card's count is NOT re-run on an ordinary read, and says so.
    await expect(page.locator('[data-kbc-board-node="counts"]')).toContainText(
      "count not re-run on this read",
    );

    // The census is the daemon's, and it names the orphan out loud.
    await expect(page.locator("[data-kbc-board-census]")).toContainText("1 orphan");
    await expect(page.locator("[data-kbc-board-census]")).toContainText("shown, never dropped");

    // A derived edge carries its trust class; an authored one carries none.
    await expect(page.locator('[data-kbc-board-edge="live->counts"]')).toHaveClass(
      /kbc-trust-likely/,
    );
    await expect(page.locator('[data-kbc-board-edge="why->live"]')).toHaveClass(
      /kbc-board__edge--authored/,
    );
    await expect(page.locator('[data-kbc-board-edge="why->live"]')).not.toHaveClass(
      /kbc-trust-/,
    );
  });

  test("folding: a card, then all of them, by button and by key", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~boards/${SLUG}`);
    const live = page.locator('[data-kbc-board-node="live"]');
    await expect(live).toBeVisible({ timeout: 15_000 });
    await expect(live).not.toHaveClass(/kbc-boardcard--folded/);

    // The fold control on the card itself (a code card's fold is `LiveRefCard`'s).
    await live.locator("[data-kbc-refcard-fold]").click();
    await expect(live).toHaveClass(/kbc-boardcard--folded/);
    await live.locator("[data-kbc-refcard-fold]").click();
    await expect(live).not.toHaveClass(/kbc-boardcard--folded/);

    // `z M` / `z R` — fold all, expand all.
    await page.locator("[data-kbc-board-census]").click();
    await page.keyboard.press("z");
    await page.keyboard.press("M");
    await expect(live).toHaveClass(/kbc-boardcard--folded/);
    await expect(page.locator('[data-kbc-board-node="why"]')).toHaveClass(
      /kbc-boardcard--folded/,
    );
    await page.keyboard.press("z");
    await page.keyboard.press("R");
    await expect(live).not.toHaveClass(/kbc-boardcard--folded/);

    // `j` walks READING order — the board's own `steps` first, then authored
    // order. Clicking inside `live` above already focused it, so `j` moves on
    // to step 2 (`dead`), NOT back to the top of the document.
    await page.keyboard.press("j");
    const dead = page.locator('[data-kbc-board-node="dead"]');
    await expect(dead).toHaveClass(/is-focused/);
    // …and `k` comes back to step 1.
    await page.keyboard.press("k");
    await expect(live).toHaveClass(/is-focused/);
    // `z c` folds the focused one, and only that one.
    await page.keyboard.press("z");
    await page.keyboard.press("c");
    await expect(live).toHaveClass(/kbc-boardcard--folded/);
    await expect(dead).not.toHaveClass(/kbc-boardcard--folded/);
  });

  test("walkthrough: enter, step, and leave with Escape", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~boards/${SLUG}`);
    await expect(page.locator("[data-kbc-board-surface]")).toBeVisible({ timeout: 15_000 });
    await page.locator("[data-kbc-board-census]").click();

    // `p` enters the walkthrough at step 1, and the URL carries it.
    await page.keyboard.press("p");
    await expect(page.locator("[data-kbc-board-step-counter]")).toHaveText("step 1 of 2");
    await expect(page).toHaveURL(/[?&]step=1/);
    await expect(page.locator("[data-kbc-board-step-caption]")).toHaveText(
      "start at the definition",
    );

    // `n` advances; the caption follows the step, not the other way round.
    await page.keyboard.press("n");
    await expect(page.locator("[data-kbc-board-step-counter]")).toHaveText("step 2 of 2");
    await expect(page).toHaveURL(/[?&]step=2/);
    await expect(page.locator("[data-kbc-board-step-caption]")).toHaveText(
      "then at the hole it left",
    );

    // `k` goes back, and does NOT fall through to "previous card" — the two
    // halves of the board's key set are provably disjoint.
    await page.keyboard.press("k");
    await expect(page.locator("[data-kbc-board-step-counter]")).toHaveText("step 1 of 2");

    // A reload reproduces the view: `?step=` is the only state.
    await page.reload();
    await expect(page.locator("[data-kbc-board-step-counter]")).toHaveText("step 1 of 2", {
      timeout: 15_000,
    });

    // Escape leaves through the EXISTING dismiss stack, and never navigates.
    const before = page.url();
    await page.locator("[data-kbc-board-census]").click();
    await page.keyboard.press("Escape");
    await expect(page.locator("[data-kbc-board-walkthrough-bar]")).toHaveCount(0);
    await expect(page).toHaveURL(new RegExp(`~boards/${SLUG}`));
    expect(before).toContain("step=1");
  });

  test("an identifier inside a card resolves through the reader's own peek", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~boards/${SLUG}?ctx=1`);
    const live = page.locator('[data-kbc-board-node="live"]');
    await expect(live).toBeVisible({ timeout: 15_000 });
    // Click the identifier in the card's painted snippet. The card's own
    // `snippet_start + lineIndex` arithmetic is what turns this into the SAME
    // `/api/resolve` request the buffer would make (pinned by
    // `lib/identResolve.test.ts`'s golden); here we only assert the request
    // actually goes out with the card's FILE coordinates, not the snippet's.
    const line = live
      .locator(".kbc-refcard__line")
      .filter({ hasText: LOCAL_TARGET_FN })
      .first()
      .locator(".kbc-refcard__text");
    await expect(line).toBeVisible();
    // Click ON the identifier, not at the line element's centre: the card reads
    // the caret's own COLUMN, and a column with no word under it is an honest
    // nothing — so a centre-click (which lands in `-> ` on this line) would
    // assert nothing at all. The daemon painted `local_target` as its own
    // highlight span, so clicking that span is exact.
    const ident = line.locator("span", { hasText: LOCAL_TARGET_FN }).last();
    await expect(ident).toHaveText(LOCAL_TARGET_FN);
    const [request] = await Promise.all([
      page.waitForRequest((r) => r.url().includes("/api/resolve"), { timeout: 15_000 }),
      ident.click(),
    ]);
    const url = new URL(request.url());
    expect(url.searchParams.get("path")).toBe(RESOLVER_FILE);
    // The snippet starts at file line 1 and `local_target` is on its SECOND
    // line, so the card's `snippet_start + lineIndex` arithmetic is what makes
    // this 2 — a dropped offset would ask about line 1.
    expect(Number(url.searchParams.get("line"))).toBe(2);
  });
});
