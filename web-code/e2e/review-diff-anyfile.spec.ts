import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { KNOWN_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V80-M1 — "the review diff shows ANY file, and every line is
/// commentable" (web-code/CLAUDE.md § Review diff v2's own "A file outside
/// the diff renders whole at the tip" paragraph).
///
/// Four claims, each a real network/DOM fact rather than a hope:
///
///   (a) `?files=all` lists a file `files_changed` never named, marked
///       plain (not carrying the "changed" flag the real diff rows do);
///   (b) opening it renders the WHOLE FILE — a caption, a real gutter, a
///       commentable line — never the old bare "No textual difference";
///   (c) a comment posted there lands on the server AND stays discoverable
///       in the map's "outside the diff" group even after switching BACK
///       to `Changed` mode — a thread is never hidden because its file has
///       no hunks;
///   (d) a bare path-segment deep link (`threadHref`'s own shape) lands on
///       the requested line with NO `?files=` at all — "regardless of the
///       mode".
///
/// Disposable branch off `main` directly (never `feature-x`, so this
/// spec's own added file is the ONLY change — `KNOWN_FILE` stays
/// untouched and is exactly the "outside the diff" target every claim
/// above needs).

const ANYFILE_BRANCH = "e2e-rdiff-anyfile";
const ANYFILE_ADDED = "e2e_rdiff_anyfile.rs";

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

test.describe("review diff shows ANY file (V80-M1)", () => {
  test.afterAll(() => {
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", ANYFILE_BRANCH]);
  });

  test("all files → whole-file body → comment → outside-the-diff group → deep link", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", ANYFILE_BRANCH]);
    git(["checkout", "-q", "-b", ANYFILE_BRANCH, "main"]);
    writeFileSync(
      join(REPO_DIR, ANYFILE_ADDED),
      "fn e2e_rdiff_anyfile() -> i32 { 1 }\n",
    );
    git(["add", ANYFILE_ADDED]);
    git(["commit", "-q", "-m", "e2e any-file review fixture"]);
    git(["checkout", "-q", "main"]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: { repo: REPO_NAME, head_ref: ANYFILE_BRANCH, base_ref: "main", title: "e2e any file" },
    });
    expect(createRes.ok(), `create: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const reviewId = ((await createRes.json()) as { id: number }).id;

    // --- (a) `?files=all` lists an unchanged file --------------------------
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff?files=all`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator("[data-kbc-rdiff-map]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('[data-kbc-rdiff-files-mode="all"]')).toHaveAttribute(
      "aria-pressed",
      "true",
      { timeout: 10_000 },
    );

    const unchangedRow = page.locator(`[data-kbc-rdiff-map-row="${KNOWN_FILE}"]`);
    await expect(unchangedRow).toBeVisible({ timeout: 10_000 });
    await expect(unchangedRow).toHaveAttribute("data-kbc-ftree-changed", "0");
    // The review's OWN added file is also in the union, marked changed —
    // "All files" widens the list, it never hides what was already there.
    await expect(page.locator(`[data-kbc-rdiff-map-row="${ANYFILE_ADDED}"]`)).toHaveAttribute(
      "data-kbc-ftree-changed",
      "1",
    );

    // --- (b) opening it renders the whole file, not "No textual difference" ---
    await unchangedRow.click();
    await expect(page).toHaveURL(new RegExp(`/diff/${KNOWN_FILE}.*file=`));
    const section = page.locator(`[data-kbc-rdiff-file="${KNOWN_FILE}"]`);
    await expect(section).toBeVisible({ timeout: 10_000 });
    await expect(section).not.toContainText("No textual difference");
    const note = section.locator("[data-kbc-diff-wholefile-note]");
    await expect(note).toBeVisible({ timeout: 10_000 });
    await expect(note).toContainText("Not changed in this patchset");
    await expect(note).toContainText("showing the whole file");
    // A real gutter with real content — never a synthesized "…".
    await expect(section.locator(".kbc-diff__text, .kbc-sdiff__text").first()).toBeVisible();
    const gutterLine1 = section.locator('[data-kbc-review-compose-new="1"]');
    await expect(gutterLine1).toBeVisible({ timeout: 10_000 });

    // --- (c) a comment there lands on the server, and stays discoverable ---
    await gutterLine1.click();
    const composer = section.locator("[data-kbc-review-composer-body]").first();
    await composer.fill("a comment on a file outside the diff");
    await section.locator("[data-kbc-review-composer-submit]").first().click();
    await expect(page.locator("[data-kbc-rdiff-drafts-count]")).toHaveText("1", {
      timeout: 10_000,
    });

    // `draftCreate` auto-opens the tray (`setDraftsOpen(true)`) — the
    // toggle button is not clicked here, or this would CLOSE it again
    // (`review-drafts.spec.ts`'s own click-to-open only makes sense AFTER
    // a reload, which resets `draftsOpen` to its default `false`).
    const batchReq = page.waitForRequest(
      (r) => r.url().includes("/api/annotations/batch") && r.method() === "POST",
    );
    await page.locator("[data-kbc-rdiff-drafts-publish]").click();
    await batchReq;

    await expect
      .poll(
        async () => {
          const res = await request.get(`${BASE}/api/reviews/${reviewId}/comments?all=true`);
          if (!res.ok()) return -1;
          const body = (await res.json()) as {
            groups: Array<{ path: string; comments: unknown[] }>;
          };
          return body.groups.find((g) => g.path === KNOWN_FILE)?.comments.length ?? 0;
        },
        { timeout: 15_000 },
      )
      .toBe(1);

    // Switch BACK to `Changed` mode — the thread must still be discoverable,
    // in a small "outside the diff" group (never hidden because its file
    // has no hunks).
    await page.locator('[data-kbc-rdiff-files-mode="changed"]').click();
    await expect(page.locator('[data-kbc-rdiff-files-mode="changed"]')).toHaveAttribute(
      "aria-pressed",
      "true",
      { timeout: 10_000 },
    );
    const outsideGroup = page.locator("[data-kbc-rdiff-map-outside-diff]");
    await expect(outsideGroup).toBeVisible({ timeout: 10_000 });
    const outsideRow = page.locator(`[data-kbc-rdiff-map-outside="${KNOWN_FILE}"]`);
    await expect(outsideRow).toBeVisible();
    await expect(outsideRow).toContainText("1");

    // --- (d) a bare deep link (threadHref's own shape) lands on the line ---
    // Fresh navigation, NO `?files=` at all — "regardless of the mode".
    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff/${KNOWN_FILE}?line=3&side=new`);
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(`[data-kbc-rdiff-file="${KNOWN_FILE}"]`)).toBeVisible({
      timeout: 10_000,
    });
    // The whole-file body's own fetch lands async, so this may take a beat
    // longer than a same-diff row's flash (`routes/ReviewDiff.tsx`'s
    // bounded retry) — one assertion, not two, so there is no gap between
    // "found" and "still has the class" for the 1400ms removal timer to
    // race into.
    await expect(page.locator('[data-new-line="3"]')).toHaveClass(/kbc-rdiff__flash/, {
      timeout: 10_000,
    });
  });
});
