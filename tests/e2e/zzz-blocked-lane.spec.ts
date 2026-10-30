import { test, expect, type APIRequestContext } from "@playwright/test";
import { BASE } from "./helpers";

// OL IDEA1 - "Blocked on you": a permission prompt becomes a first-class
// live state. Beats go straight to the daemon (the hook's own wire shape);
// the SPA must show the "Needs you" lane first, the header chip must carry
// the blocked marker, a heartbeat must not clear the block, and an
// `unblocked` beat must clear both WITHOUT a reload (session.state SSE edge
// -> the existing ["sessions","live"] invalidation; the poll is 10 s).
//
// Named `zzz-` so it runs after every visual spec (files run alphabetically,
// one worker): it adds a live session the other specs never expect. Earlier
// specs may leave Tier-0 `waiting` rows, so assertions target the blocked
// markers, not the chip's absence.
const SID = `e2e-blocked-${Date.now()}`;

async function beat(request: APIRequestContext, event: string, detail?: object) {
  const r = await request.post(`${BASE}/api/sessions/beat`, {
    data: {
      v: 1,
      session_id: SID,
      harness: "claude",
      event,
      at: new Date().toISOString().replace(/\.\d+Z$/, "Z"),
      cwd: "/work/e2e-blocked-proj",
      ...(detail ? { detail } : {}),
    },
  });
  expect(r.status(), `beat ${event}`).toBe(200);
}

test.describe("blocked on you", () => {
  test.afterAll(async ({ request }) => {
    await beat(request, "end");
  });

  test("lane, chip, stickiness and the live unblock", async ({ page, request }) => {
    await beat(request, "start");
    await beat(request, "blocked", {
      reason: "Claude needs your permission to use Bash",
      tool: "Bash",
    });

    await page.goto(`${BASE}/sessions`);
    const lane = page.getByTestId("now-lane-blocked");
    await expect(lane).toBeVisible();
    // The blocked lane is the first lane of the Now band.
    await expect(
      page.locator('[data-testid="sessions-now-band"] section').first(),
    ).toHaveAttribute("data-testid", "now-lane-blocked");
    await expect(lane.getByTestId("now-row-blocked-reason")).toHaveText("permission: Bash");

    const chip = page.getByTestId("header-waiting");
    await expect(chip).toHaveAttribute("data-kb-blocked", "");
    await expect(chip).toContainText("1 blocked");
    await expect(chip).toHaveAttribute("href", /#kb-now-blocked$/);

    // A tool heartbeat keeps the block, even across a reload.
    await beat(request, "tool");
    await page.reload();
    await expect(page.getByTestId("now-lane-blocked")).toBeVisible();

    // unblocked clears the lane and the chip marker with no reload.
    await beat(request, "unblocked");
    await expect(page.getByTestId("now-lane-blocked")).toHaveCount(0, { timeout: 5_000 });
    await expect(page.locator('[data-testid="header-waiting"][data-kb-blocked]')).toHaveCount(0, {
      timeout: 5_000,
    });
  });
});
