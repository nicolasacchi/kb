import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

// RAILBOARD — the Board tab of the terminal's right panel. The session detail,
// the slate board read and the slate append are mocked (synthetic posts only);
// the answer POST makes the mocked board flip the ask to answered, and the
// post's own invalidation drives the refetch. CHROMIUM desktop only.

const base = (seq: number, kind: string, over: Record<string, unknown> = {}) => ({
  seq,
  id: `p${seq}`,
  kind,
  line: `synthetic ${kind} ${seq}`,
  who: { origin: "agent", harness: "claude", session_short: "zzzz", tag: "claude/zzzz" },
  age_secs: 240,
  tier: "whole",
  marks: 0,
  pinned: false,
  contested: false,
  body: null,
  marks_by: [],
  has_sketch: false,
  ...over,
});

function boardBody(answered: boolean) {
  return {
    slug: "demo",
    head_seq: answered ? 8 : 7,
    topics: [],
    folded_found_idea: 0,
    sections: {
      now: [base(1, "now", { line: "synthetic now line" })],
      warn: [base(2, "warn")],
      hand: [],
      ask: [
        base(3, "ask", { line: "synthetic agent-to-agent ask", to: "claude" }),
        base(4, "ask", { line: "synthetic question for the operator", to: "you", answers: answered ? 1 : 0 }),
      ],
      take: [base(5, "take")],
      found_idea: [],
      tried: [],
    },
  };
}

async function mockBoard(page: Page, state: { answered: boolean; posts: unknown[] }): Promise<void> {
  await page.route(new RegExp(`/api/sessions/${SID}$`), (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ session_id: SID, kb: "mem", source_relative: "sessions/synthetic-board.html", project_slug: "demo" }),
    }),
  );
  await page.route(/\/api\/slates\/demo\?view=board/, (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(boardBody(state.answered)) }),
  );
  await page.route("**/api/slates/demo/posts", async (route) => {
    state.posts.push(route.request().postDataJSON());
    state.answered = true;
    await route.fulfill({
      status: 201,
      contentType: "application/json",
      body: JSON.stringify({ post: { seq: 8 }, displaced: [], displaced_total: 0, nudge: null, head_seq: 8 }),
    });
  });
}

async function admit(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
  });
}

test.describe("RAILBOARD Board tab", () => {
  test("order, badge, and answering in place flips the ask and clears the badge", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    const state = { answered: false, posts: [] as unknown[] };
    await mockBoard(page, state);

    await page.goto(`${BASE}/term/${TERM_ID}`);
    const tab = page.locator('[data-testid="term-tab-board"]');
    await expect(tab.locator(".kb-termshell__badge")).toHaveText("1", { timeout: 20_000 });
    await expect(page.locator('[data-testid="term-tab-conversation"] .kb-termshell__badge')).toHaveCount(0);
    await tab.click();

    const asks = page.locator('[data-testid="board-ask"]');
    await expect(asks).toHaveCount(2);
    await expect(asks.nth(0)).toContainText("synthetic question for the operator");
    await expect(asks.nth(1)).toContainText("synthetic agent-to-agent ask");
    await expect(page.locator('[data-testid="board-asof"]')).toHaveText("board as of seq 7");

    await asks.nth(0).locator('[data-act="answer"]').click();
    await page.locator('[data-testid="board-answer-input"]').fill("synthetic answer");
    await page.locator('[data-testid="board-answer-send"]').click();

    await expect(page.locator('[data-testid="board-answer-form"]')).toHaveCount(0);
    expect(state.posts).toEqual([
      { kind: "answer", line: "synthetic answer", re: 4, prov: { harness: "spa", origin: "human" } },
    ]);
    await expect(asks.nth(1)).toHaveAttribute("data-open", "0");
    await expect(tab.locator(".kb-termshell__badge")).toHaveCount(0);
  });
});
