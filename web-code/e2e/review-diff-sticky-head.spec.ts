import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// v0.47 SH — the per-file header of the review diff stays pinned under the
/// toolbar while that file's body (longer than the viewport) scrolls, is
/// replaced by the next file's header at the boundary, and its "viewed"
/// checkbox still works from the pinned position. Asserted with both the file
/// map open (the library Group's overflow used to defeat sticky) and closed.

const BRANCH = "e2e-rdiff-sticky";
const FIRST = "a_sh_first.rs";
const LONG = "b_sh_long.rs";
const LAST = "c_sh_last.rs";

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

async function metrics(page: Page, path: string) {
  return page.evaluate((p) => {
    const sel = (x: string) => `[data-kbc-rdiff-file="${x}"] [data-kbc-rdiff-section]`;
    const tb = document.querySelector("[data-kbc-rdiff-toolbar]")!.getBoundingClientRect();
    const head = document.querySelector(sel(p))!.getBoundingClientRect();
    return { toolbarBottom: tb.bottom, headTop: head.top, headBottom: head.bottom };
  }, path);
}

test.describe("review diff sticky file header (v0.47 SH)", () => {
  test.afterAll(() => {
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", BRANCH]);
  });

  test("pinned under the toolbar mid-file, handed off at the boundary, viewed works", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");
    await page.setViewportSize({ width: 1280, height: 720 });

    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", BRANCH]);
    git(["checkout", "-q", "-b", BRANCH, "main"]);
    const long = Array.from({ length: 400 }, (_, i) => `fn sh_long_line_${i}() -> i32 { ${i} }`).join("\n");
    writeFileSync(join(REPO_DIR, FIRST), "fn sh_first() -> i32 { 1 }\n");
    writeFileSync(join(REPO_DIR, LONG), `${long}\n`);
    writeFileSync(join(REPO_DIR, LAST), "fn sh_last() -> i32 { 3 }\n");
    git(["add", FIRST, LONG, LAST]);
    git(["commit", "-q", "-m", "e2e sticky header fixture"]);
    git(["checkout", "-q", "main"]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: { repo: REPO_NAME, head_ref: BRANCH, base_ref: "main", title: "e2e sticky head" },
    });
    expect(createRes.ok(), `create: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const reviewId = ((await createRes.json()) as { id: number }).id;

    for (const mapOpen of [true, false]) {
      await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff`);
      await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });
      await expect(page.locator("[data-kbc-rdiff-file]")).toHaveCount(3, { timeout: 15_000 });
      if (!mapOpen) {
        await page.locator("[data-kbc-rdiff-map-close]").click();
        await expect(page.locator("[data-kbc-rdiff-map]")).toHaveCount(0);
      }

      // Bring the long file's body in so it has its real height.
      await page.locator(`[data-kbc-rdiff-file="${LONG}"]`).scrollIntoViewIfNeeded();
      await expect(
        page.locator(`[data-kbc-rdiff-file="${LONG}"]`).getByText("sh_long_line_399").first(),
      ).toBeAttached({ timeout: 15_000 });

      // Scroll 1500px into the long file: well past its natural header position.
      await page.evaluate((p) => {
        const sec = document.querySelector(`[data-kbc-rdiff-file="${p}"]`)!;
        const scroller = sec.closest(".kbc-approute") as HTMLElement;
        const top = sec.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
        scroller.scrollTop += top + 1500;
      }, LONG);

      await expect
        .poll(async () => {
          const m = await metrics(page, LONG);
          return Math.abs(m.headTop - m.toolbarBottom) <= 3;
        })
        .toBe(true);
      const prev = await metrics(page, FIRST);
      const pinned = await metrics(page, LONG);
      expect(prev.headBottom, "previous file header is scrolled away").toBeLessThanOrEqual(pinned.toolbarBottom + 1);

      // Clicking "viewed" in the pinned header works (server round-trip).
      const progress = page.locator("[data-kbc-review-progress]");
      const before = await progress.getAttribute("data-kbc-review-progress");
      await page.locator(`[data-kbc-review-viewed="${LONG}"]`).click();
      await expect(progress).not.toHaveAttribute("data-kbc-review-progress", before ?? "", {
        timeout: 10_000,
      });
      // Collapse-on-tick: the section collapsed and its header was kept at the
      // pin line (the next file's header follows directly).
      await expect(page.locator(`[data-kbc-rdiff-file="${LONG}"]`)).toHaveAttribute(
        "data-kbc-rdiff-collapsed",
        "1",
      );
      await expect
        .poll(async () => {
          const m = await metrics(page, LONG);
          return Math.abs(m.headTop - m.toolbarBottom) <= 3;
        })
        .toBe(true);

      // Undo so the second pass starts clean.
      await page.locator(`[data-kbc-review-viewed="${LONG}"]`).click();
      await expect(progress).toHaveAttribute("data-kbc-review-progress", before ?? "", {
        timeout: 10_000,
      });
    }
  });
});
