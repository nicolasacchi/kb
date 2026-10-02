import { test, expect } from "@playwright/test";
import { BASE } from "./helpers";

// v0.44 P1 — a private note never crosses the operator boundary.
//
// The SPA fetches the review with `?visibility=all` so the operator can see
// their notes in the panel. Two things used to leak that file: the
// `cm:refresh` postMessage into the (untrusted) artifact iframe, and the
// panel's "Claude prompt" / Markdown exports. Both now go through
// `lib/publicView.ts`. This spec drives the real daemon and asserts:
//   1. the iframe never receives the note body (message payloads, the
//      daemon-injected window.__KB_COMMENTS, nor any DOM attribute);
//   2. the Claude-prompt and Markdown exports omit the note but keep the
//      public comment;
//   3. the review-notes page links without putting the note text in the URL.

const NOTE = `NOTE-SECRET-${Date.now()}`;
const PUBLIC = `PUBLIC-VISIBLE-${Date.now()}`;

test.describe("private notes stay on the operator side (v0.44 P1)", () => {
  test("iframe never receives the note; exports omit it", async ({
    page,
    request,
  }) => {
    const docs = (await (
      await request.get(`${BASE}/api/kb/canon/docs?limit=50`)
    ).json()) as { id: string; path: string; source_relative: string }[];
    const doc = docs.find((d) => d.path.endsWith("kitchen-sink.html"));
    expect(doc).toBeTruthy();

    for (const [body, priv] of [
      [PUBLIC, false],
      [NOTE, true],
    ] as const) {
      const r = await request.post(
        `${BASE}/api/kb/canon/review/${doc!.id}/comments`,
        { data: { body, anchor: { kind: "file" }, author: "you", private: priv } },
      );
      expect(r.status()).toBe(201);
    }

    // Record every message that reaches any frame (the init script runs in
    // the cross-origin artifact iframe too).
    await page.addInitScript(() => {
      (window as unknown as { __seen: string[] }).__seen = [];
      window.addEventListener("message", (e) => {
        try {
          (window as unknown as { __seen: string[] }).__seen.push(
            JSON.stringify(e.data),
          );
        } catch {
          /* unserialisable */
        }
      });
    });

    await page.goto(`${BASE}/a/canon/${doc!.source_relative}`);
    await page.locator('[data-kb-act="dock-comments"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    // The operator sees both in their own panel.
    await expect(panel.getByText(PUBLIC)).toBeVisible({ timeout: 10_000 });
    await expect(panel.getByText(NOTE)).toBeVisible();

    // Let the cm:refresh repaint run, then inspect the artifact frame.
    await page.waitForTimeout(1500);
    const frame = page
      .frames()
      .find((f) => f !== page.mainFrame() && f.url().includes("--"));
    expect(frame, "artifact iframe").toBeTruthy();
    const inside = await frame!.evaluate(() => {
      const w = window as unknown as {
        __KB_COMMENTS?: unknown;
        __seen?: string[];
      };
      return JSON.stringify({
        env: w.__KB_COMMENTS ?? null,
        seen: w.__seen ?? [],
        dom: document.documentElement.outerHTML,
      });
    });
    expect(inside).not.toContain(NOTE);
    // The public comment DID reach the frame, so the absence above is not
    // vacuous (the refresh path ran).
    expect(inside).toContain(PUBLIC);

    // The Claude-prompt export omits the note and keeps the public comment.
    await panel.locator(".cp__export-btn").first().click();
    const dialog = page.locator("dialog.cp__export");
    await expect(dialog).toBeVisible();
    const text = await dialog.innerText();
    expect(text).toContain(PUBLIC);
    expect(text).not.toContain(NOTE);
  });

  test("review-notes page cite links carry no note text", async ({ page }) => {
    await page.goto(`${BASE}/review-notes`);
    const links = page.locator("a[href*='comment=']");
    const n = await links.count();
    // Not vacuous: the first test created a note, so its cite link exists.
    expect(n).toBeGreaterThan(0);
    for (let i = 0; i < n; i++) {
      const href = (await links.nth(i).getAttribute("href")) ?? "";
      expect(decodeURIComponent(href)).not.toContain(NOTE);
      expect(href).not.toContain(":~:text=");
    }
  });
});
