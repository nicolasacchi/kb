import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V73-K2a — draft-as-you-go and the ATOMIC publish.
///
/// Two claims, both of which have to be true on a real browser against a
/// real daemon or the feature is a lie:
///
///   1. a comment composed in the diff does NOT reach the server until the
///      operator publishes, and it SURVIVES a reload while it waits;
///   2. `Publish` sends ONE batch — every draft lands together, and the
///      tray empties only after the server says so.

const DRAFTS_BRANCH = "e2e-rdiff-drafts";
const DRAFTS_FILE = "e2e_rdiff_drafts.rs";

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

test.describe("review drafts + atomic publish (V73-K2a)", () => {
  test.afterAll(() => {
    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", DRAFTS_BRANCH]);
  });

  test("drafts stay local, survive a reload, and publish as one batch", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    tryGit(["checkout", "main"]);
    tryGit(["branch", "-D", DRAFTS_BRANCH]);
    git(["checkout", "-q", "-b", DRAFTS_BRANCH, FEATURE_BRANCH]);
    writeFileSync(
      join(REPO_DIR, DRAFTS_FILE),
      "fn drafts_one() -> i32 { 1 }\nfn drafts_two() -> i32 { 2 }\nfn drafts_three() -> i32 { 3 }\n",
    );
    git(["add", DRAFTS_FILE]);
    git(["commit", "-q", "-m", "e2e drafts file"]);
    git(["checkout", "-q", "main"]);

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: { repo: REPO_NAME, head_ref: DRAFTS_BRANCH, base_ref: "main", title: "e2e drafts" },
    });
    expect(createRes.ok(), `create review: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const reviewId = ((await createRes.json()) as { id: number }).id;

    await page.goto(
      `${BASE}/r/${REPO_NAME}/~reviews/${reviewId}/diff/${encodeURIComponent(DRAFTS_FILE)}`,
    );
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });

    // Compose two comments on the new side, through the gutter composer
    // the page already had.
    const bodies = ["first drafted note", "second drafted note"];
    for (let i = 0; i < bodies.length; i++) {
      const line = i + 1;
      await page.locator(`[data-kbc-review-compose-new="${line}"]`).first().click();
      const box = page.locator("[data-kbc-review-composer-body]").first();
      await box.fill(bodies[i]);
      await page.locator("[data-kbc-review-composer-submit]").first().click();
      await expect(page.locator("[data-kbc-rdiff-drafts-count]")).toHaveText(String(line), {
        timeout: 10_000,
      });
    }

    // CLAIM 1a — nothing reached the server.
    const beforeRes = await request.get(`${BASE}/api/reviews/${reviewId}/comments?all=true`);
    expect(beforeRes.ok()).toBeTruthy();
    const beforeBody = (await beforeRes.json()) as { groups: Array<{ comments: unknown[] }> };
    const beforeCount = beforeBody.groups.reduce((n, g) => n + g.comments.length, 0);
    expect(beforeCount).toBe(0);

    // CLAIM 1b — a reload restores the tray.
    await page.reload();
    await expect(page.locator("[data-kbc-rdiff]")).toBeVisible({ timeout: 15_000 });
    await page.locator("[data-kbc-rdiff-drafts-toggle]").click();
    await expect(page.locator("[data-kbc-rdiff-draft]")).toHaveCount(2);
    for (const body of bodies) {
      await expect(page.locator("[data-kbc-rdiff-drafts]")).toContainText(body);
    }

    // CLAIM 2 — ONE batch. The request is asserted directly, so "one
    // transaction" is a fact about the wire rather than a hope about the
    // handler.
    const batchReq = page.waitForRequest(
      (r) => r.url().includes("/api/annotations/batch") && r.method() === "POST",
    );
    await page.locator("[data-kbc-rdiff-drafts-publish]").click();
    const req = await batchReq;
    const payload = JSON.parse(req.postData() ?? "{}") as {
      repo: string;
      ops: Array<{ op: string; body: string; review_id: number }>;
    };
    expect(payload.repo).toBe(REPO_NAME);
    expect(payload.ops).toHaveLength(2);
    expect(payload.ops.every((o) => o.op === "add_comment")).toBe(true);
    expect(payload.ops.every((o) => o.review_id === reviewId)).toBe(true);
    expect(payload.ops.map((o) => o.body)).toEqual(bodies);

    // Both landed, together.
    await expect
      .poll(
        async () => {
          const res = await request.get(`${BASE}/api/reviews/${reviewId}/comments?all=true`);
          if (!res.ok()) return -1;
          const body = (await res.json()) as { groups: Array<{ comments: unknown[] }> };
          return body.groups.reduce((n, g) => n + g.comments.length, 0);
        },
        { timeout: 15_000 },
      )
      .toBe(2);

    // The tray empties only after the server accepted the batch (it also
    // closes on success, so the toolbar's own counter is what remains).
    await expect(page.locator("[data-kbc-rdiff-drafts-toggle]")).toHaveText(/Drafts 0/, {
      timeout: 10_000,
    });

    // And it stays empty across a reload — a published draft is gone from
    // sessionStorage, not merely hidden.
    await page.reload();
    await expect(page.locator("[data-kbc-rdiff-drafts-toggle]")).toHaveText(/Drafts 0/, {
      timeout: 15_000,
    });
  });
});
