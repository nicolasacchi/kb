import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// T21 - the Images strip of /sessions/:sid on the DEFAULT daemon, where the
// terminal tier is off: a transcript picture reference renders a caption
// tile ("image not stored by kb") and the page makes NO request to the
// terminal media route. The terminal-enabled strip is covered by unit tests
// (the e2e terminal harness is another unit's). All content is synthetic;
// the sha below is an invented 64-hex string, not a real picture's hash.

const SID = "spec-imgstrip-0001";
const SHA = "ab".repeat(32);
const TURN_UUID = "0a1b2c3d-4e5f-4a6b-8c7d-0123456789ab";

async function seedImageSession(
  page: import("@playwright/test").Page,
  base: string,
): Promise<void> {
  const lines = [
    JSON.stringify({
      type: "user",
      uuid: TURN_UUID,
      timestamp: "2026-07-01T09:00:00Z",
      cwd: "/proj/imgstrip",
      gitBranch: "main",
      promptSource: "typed",
      message: {
        role: "user",
        content: [
          { type: "text", text: "look at this picture" },
          {
            type: "image",
            source: {
              type: "blob_ref",
              media_type: "image/png",
              sha256: SHA,
            },
          },
        ],
      },
    }),
    JSON.stringify({
      type: "assistant",
      timestamp: "2026-07-01T09:05:00Z",
      message: {
        role: "assistant",
        content: [{ type: "text", text: "Seen it." }],
      },
    }),
  ];
  const seed = await page.request.post(`${base}/api/kb/mem/artifacts`, {
    data: {
      title: "Session transcript 20260701T0900imgs",
      body_html: `<pre>${lines.join("\n")}</pre>`,
      category: "memory-session",
      session_id: SID,
    },
  });
  expect(seed.ok()).toBeTruthy();
}

async function waitForImageItem(
  page: import("@playwright/test").Page,
  base: string,
): Promise<void> {
  const deadline = Date.now() + 25_000;
  while (Date.now() < deadline) {
    const r = await page.request.get(
      `${base}/api/sessions/${SID}/view?fields=turns&turns=all`,
    );
    if (r.ok() && (await r.text()).includes('"item":"Image"')) return;
    await new Promise((rs) => setTimeout(rs, 250));
  }
  throw new Error("the seeded session never produced an Image item");
}

test.describe("T21 - Images strip", () => {
  test("terminal tier off: a caption tile, no media request, one column on mobile", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    await seedImageSession(page, base);
    await waitForImageItem(page, base);

    const mediaRequests: string[] = [];
    page.on("request", (req) => {
      if (req.url().includes("/api/terms/media/")) mediaRequests.push(req.url());
    });
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(String(e)));
    page.on("console", (m) => {
      // A 4xx the page itself tolerates logs "Failed to load resource";
      // that is not a script error.
      if (m.type() === "error" && !m.text().includes("Failed to load resource")) {
        errors.push(m.text());
      }
    });

    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto(`${base}/sessions/${SID}`);
    const section = page.locator('[data-testid="session-sec-images"]');
    await expect(section).toBeVisible();
    const tiles = section.locator('[data-testid="kb-imgs-tile"]');
    await expect(tiles).toHaveCount(1);
    await expect(tiles.first().locator('[data-testid="kb-imgs-caption"]')).toHaveText(
      "image not stored by kb",
    );
    await expect(tiles.first().locator("img")).toHaveCount(0);
    await expect(tiles.first().locator('[data-testid="kb-imgs-turn"]')).toHaveAttribute(
      "href",
      "#t-0a1b2c3d4e5f",
    );

    // Mobile: collapsed until opened, then ONE column of tiles.
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(`${base}/sessions/${SID}`);
    const mobile = page.locator('[data-testid="session-sec-images"]');
    await mobile.locator("summary").click();
    const mobileTiles = mobile.locator('[data-testid="kb-imgs-tile"]');
    await expect(mobileTiles).toHaveCount(1);
    const boxes = await mobileTiles.evaluateAll((els) =>
      els.map((e) => {
        const r = e.getBoundingClientRect();
        return { x: Math.round(r.x), w: Math.round(r.width) };
      }),
    );
    expect(new Set(boxes.map((b) => b.x)).size).toBe(1);

    expect(mediaRequests).toEqual([]);
    expect(errors).toEqual([]);
  });
});
