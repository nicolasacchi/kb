import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V73-K2a — review diff v2: the file MAP column, per-hunk VIEWED state,
/// the PATCHSET switcher and the CONTEXT dial. Follows
/// `review-diff-page.spec.ts` exactly (create the review over the API, then
/// drive the SPA) and creates its own disposable branch so it never runs
/// against main's tip.
///
/// It deliberately asserts the URL after every control: diff v2's whole
/// contract is "the URL is the state", so a control that changes the view
/// without changing the URL is the regression this spec exists to catch.

const V2_BASE = "e2e-rdiff-v2-base";
const V2_BRANCH = "e2e-rdiff-v2";
const V2_FILE = "e2e_rdiff_v2.rs";
const V2_FILE2 = "e2e_rdiff_v2b.rs";

function git(args: string[]): string {
  return execFileSync("git", ["-C", REPO_DIR, ...args], { encoding: "utf-8" }).trim();
}

function tryGit(args: string[]): void {
  try {
    execFileSync("git", ["-C", REPO_DIR, ...args], { stdio: "ignore" });
  } catch {
    // best-effort cleanup
  }
}

/// A 60-line file, so a `-U3` hunk covers ~7 rows and the context dial has
/// 53 more to reveal. Line 30 carries a real declaration so the file is not
/// pure comments; every OTHER line is `// <marker> line <n>`, which is what
/// the edits below address by name.
function longFile(marker: string): string {
  const lines = Array.from({ length: 60 }, (_, i) => `// ${marker} line ${i + 1}`);
  lines[29] = `fn ${marker}_target() -> i32 { 1 }`;
  return `${lines.join("\n")}\n`;
}

test.describe("review diff v2 (V73-K2a)", () => {
  test.afterAll(() => {
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", V2_BRANCH]);
    tryGit(["branch", "-D", V2_BASE]);
  });

  test("map column, per-hunk viewed, patchset switch, context dial — all in the URL", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", V2_BRANCH]);
    tryGit(["branch", "-D", V2_BASE]);

    // The review's BASE already contains both long files, so the head's
    // patch is a small MODIFICATION with a `-U3` window — which is what
    // makes "widen the context dial and more rows appear" a real
    // assertion. A file ADDED on the branch is one all-additions hunk that
    // already spans the file, and `full` would legitimately add nothing.
    git(["checkout", "-q", "-b", V2_BASE, FEATURE_BRANCH]);
    writeFileSync(join(REPO_DIR, V2_FILE), longFile("v2"));
    writeFileSync(join(REPO_DIR, V2_FILE2), longFile("v2b"));
    git(["add", V2_FILE, V2_FILE2]);
    git(["commit", "-q", "-m", "e2e diff v2 base files"]);

    git(["checkout", "-q", "-b", V2_BRANCH, V2_BASE]);
    writeFileSync(join(REPO_DIR, V2_FILE), longFile("v2").replace("v2 line 20", "v2 line 20 EDITED"));
    writeFileSync(
      join(REPO_DIR, V2_FILE2),
      longFile("v2b").replace("v2b line 20", "v2b line 20 EDITED"),
    );
    git(["add", V2_FILE, V2_FILE2]);
    git(["commit", "-q", "-m", "e2e diff v2 ps1"]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: { repo: REPO_NAME, head_ref: V2_BRANCH, base_ref: V2_BASE, title: "e2e diff v2" },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const reviewId = ((await createRes.json()) as { id: number }).id;

    // A SECOND patchset, so the switcher has two stops to move between.
    writeFileSync(
      join(REPO_DIR, V2_FILE),
      longFile("v2").replace("v2 line 20", "v2 line 20 EDITED").replace("v2 line 45", "v2 line 45 EDITED"),
    );
    git(["add", V2_FILE]);
    git(["commit", "-q", "-m", "e2e diff v2 ps2"]);
    git(["checkout", "-q", "main"]);
    const snapRes = await request.post(`${BASE}/api/reviews/${reviewId}/snapshot`, { data: {} });
    expect(snapRes.ok(), `snapshot: ${snapRes.status()} ${await snapRes.text()}`).toBeTruthy();

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });

    // --- the map column ---------------------------------------------------
    const map = page.locator("[data-kbc-rdiff-map]");
    await expect(map).toBeVisible({ timeout: 10_000 });
    // Every row on the map is a file on the page — the column renders the
    // review's own file set, it does not compute a second one.
    const mapRows = page.locator("[data-kbc-rdiff-map-row]");
    await expect(mapRows.first()).toBeVisible();
    const rowCount = await mapRows.count();
    expect(rowCount).toBeGreaterThanOrEqual(2);
    await expect(page.locator("[data-kbc-rdiff-map-census]")).toContainText("viewed");
    // Exactly one current row, and it follows the `]f` file motion.
    await expect(page.locator("[data-kbc-rdiff-map-current]")).toHaveCount(1);
    const before = await page.locator("[data-kbc-rdiff-map-current]").getAttribute("data-kbc-rdiff-map-row");
    await page.keyboard.press("]");
    await page.keyboard.press("f");
    await expect
      .poll(async () => page.locator("[data-kbc-rdiff-map-current]").getAttribute("data-kbc-rdiff-map-row"))
      .not.toBe(before);

    // `Space m` hides it, and the URL says so.
    await page.keyboard.press("Space");
    await page.keyboard.press("m");
    await expect(map).toHaveCount(0);
    await expect(page).toHaveURL(/map=0/);
    // A RELOAD reproduces the hidden column — the URL is the only state.
    await page.reload();
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator("[data-kbc-rdiff-map]")).toHaveCount(0);
    await page.keyboard.press("Space");
    await page.keyboard.press("m");
    await expect(page.locator("[data-kbc-rdiff-map]")).toBeVisible();
    await expect(page).not.toHaveURL(/map=0/);

    // --- per-hunk state ---------------------------------------------------
    const strips = page.locator("[data-kbc-hunk]");
    await expect(strips.first()).toBeVisible({ timeout: 15_000 });
    const firstStrip = strips.first();
    const hunkId = await firstStrip.getAttribute("data-kbc-hunk");
    expect(hunkId).toMatch(/^[0-9a-f]{16}$/);
    await expect(firstStrip).toHaveAttribute("data-kbc-hunk-viewed", "0");

    // The checkbox marks it viewed SERVER-side; the wire is the proof.
    // `.click()`, NOT `.check()`: the box is CONTROLLED by `hunks_viewed`
    // off the files query, so it only flips after the PUT lands and the
    // query is invalidated — `.check()` asserts the flip synchronously and
    // fails on exactly the async round trip that makes this server state
    // rather than a local flag.
    await page.locator(`[data-kbc-hunk-viewed-toggle="${hunkId}"]`).click();
    await expect(firstStrip).toHaveAttribute("data-kbc-hunk-viewed", "1", { timeout: 10_000 });
    const filesRes = await request.get(`${BASE}/api/reviews/${reviewId}/files`);
    expect(filesRes.ok()).toBeTruthy();
    const filesBody = (await filesRes.json()) as { hunks_viewed?: Array<{ hunk_id: string }> };
    expect((filesBody.hunks_viewed ?? []).map((h) => h.hunk_id)).toContain(hunkId);

    // A reload keeps it: this is server state, not a local flag.
    await page.reload();
    await expect(page.locator(`[data-kbc-hunk="${hunkId}"]`)).toHaveAttribute(
      "data-kbc-hunk-viewed",
      "1",
      { timeout: 15_000 },
    );

    // --- fold -------------------------------------------------------------
    await page.locator(`[data-kbc-hunk-fold="${hunkId}"]`).click();
    await expect(page.locator(`[data-kbc-hunk="${hunkId}"]`)).toHaveAttribute(
      "data-kbc-hunk-collapsed",
      "1",
    );
    await page.locator(`[data-kbc-hunk-fold="${hunkId}"]`).click();
    await expect(page.locator(`[data-kbc-hunk="${hunkId}"]`)).toHaveAttribute(
      "data-kbc-hunk-collapsed",
      "0",
    );

    // --- the noise toggle is a COLLAPSE, never a filter --------------------
    // Scoped to ONE file's section, because sections mount lazily on scroll
    // and a page-wide count would drift for reasons unrelated to noise.
    const firstSection = page.locator("[data-kbc-rdiff-file]").first();
    const sectionHunks = firstSection.locator("[data-kbc-hunk]");
    const hunkTotal = await sectionHunks.count();
    expect(hunkTotal).toBeGreaterThan(0);
    await page.keyboard.press("Space");
    await page.keyboard.press("n");
    await expect(page).toHaveURL(/noise=collapsed/);
    // Same number of hunks on screen: a labelled hunk collapses, it never
    // disappears.
    await expect(sectionHunks).toHaveCount(hunkTotal);
    await expect(page.locator("[data-kbc-rdiff-noise-census]")).toContainText("hunks");
    await page.keyboard.press("Space");
    await page.keyboard.press("n");
    await expect(page).not.toHaveURL(/noise=collapsed/);

    // --- the context dial -------------------------------------------------
    // On the SINGLE-file view of the 60-line fixture file, so the widened
    // window has somewhere to go: at `-U3` the wire sends ~7 rows, and the
    // whole file is 60. A small file could already be fully covered at
    // `-U3`, which would make "more rows" an untestable claim rather than
    // a false one.
    await page.goto(
      `${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff/${encodeURIComponent(V2_FILE)}`,
    );
    await expect(page.locator("[data-kbc-rdiff]")).toHaveAttribute("data-kbc-rdiff-mode", "single", {
      timeout: 15_000,
    });
    const diffLines = page.locator(`[data-kbc-rdiff-file="${V2_FILE}"] .kbc-diff__line`);
    await expect(diffLines.first()).toBeVisible({ timeout: 15_000 });
    const rowsAt3 = await diffLines.count();
    await page.locator("[data-kbc-rdiff-ctx]").selectOption("full");
    await expect(page).toHaveURL(/ctx=full/);
    await expect.poll(async () => diffLines.count(), { timeout: 20_000 }).toBeGreaterThan(rowsAt3);
    await page.locator("[data-kbc-rdiff-ctx]").selectOption("3");
    await expect(page).not.toHaveURL(/ctx=/);
    await expect.poll(async () => diffLines.count(), { timeout: 20_000 }).toBe(rowsAt3);

    // --- the patchset switcher --------------------------------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });
    await page.locator("[data-kbc-rdiff-psw-head]").selectOption("1");
    await expect(page).toHaveURL(/ps=1/);
    await expect(page.locator("[data-kbc-rdiff-ps]")).toHaveText("ps1");
    // ps1 → ps2 as an INTERDIFF, with its own honest caption about the
    // columns the interdiff wire cannot fill.
    await page.locator("[data-kbc-rdiff-psw-head]").selectOption("2");
    await page.locator("[data-kbc-rdiff-psw-base]").selectOption("1");
    await expect(page).toHaveURL(/ps=1\.\.2/);
    await expect(page.locator("[data-kbc-rdiff-ps-note]")).toContainText("interdiff");
    // Back/forward reproduce the view — `?ps=` is real URL state.
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff?ps=1`);
    await expect(page.locator("[data-kbc-rdiff-ps]")).toHaveText("ps1", { timeout: 15_000 });
    await expect(page.locator("[data-kbc-rdiff-ps-note]")).toHaveCount(0);
    // `] p` steps the head patchset forward.
    await page.keyboard.press("]");
    await page.keyboard.press("p");
    await expect(page.locator("[data-kbc-rdiff-ps]")).toHaveText("ps2", { timeout: 10_000 });

    // Both changed files are still on the page — the switcher never
    // silently narrows the review.
    await expect(page.locator(`[data-kbc-rdiff-file="${V2_FILE}"]`)).toHaveCount(1);
    await expect(page.locator(`[data-kbc-rdiff-file="${V2_FILE2}"]`)).toHaveCount(1);
  });
});
