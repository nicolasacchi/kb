import { test, expect, type Page } from "@playwright/test";
import { BASE } from "./helpers";
import { installFakeTerminal, SID, TERM_ID } from "./fixtures/fake-tap.mjs";

// RAILCHANGES — the Changes tab of the terminal's right panel. The session
// detail, files, commits, kbs and kb-code lane routes are mocked (synthetic
// rows only). CHROMIUM desktop only.

async function admit(page: Page): Promise<void> {
  await page.route("**/api/identity", async (route) => {
    const res = await route.fetch();
    const j = (await res.json()) as Record<string, unknown>;
    await route.fulfill({ response: res, json: { ...j, terminal_admitted: true } });
  });
}

const NOW = Math.floor(Date.now() / 1000);

async function mockSession(page: Page, state: { commits: unknown[] }, codeUrl: string | null): Promise<void> {
  await page.route("**/api/kbs", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify([
        { name: "mem", path: "/synthetic", doc_count: 1, last_index_at: null, code_url: codeUrl },
      ]),
    }),
  );
  await page.route(new RegExp(`/api/sessions/${SID}$`), (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        id: "synthetic",
        kb: "mem",
        artifact_id: "synthetic",
        session_id: SID,
        started_at: NOW - 900,
        ended_at: NOW - 120,
        source_relative: "sessions/synthetic-changes.html",
        memory_ids: [],
        segment_chain: [],
        continued_in: [],
        checks: [{ label: "tests", cmd: "just ci", runners: [], outcome: "pass", text: "passed", after_last_edit: true }],
        evidence: {
          verdict: "verified",
          phrase: "Verified: tests passed after the last edit",
          checks_total: 1,
          checks_after_edit: 1,
          edits_ignored: 0,
          uncommitted: false,
        },
      }),
    }),
  );
  await page.route(`**/api/sessions/${SID}/files`, (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        files: [
          { path: "src/alpha.ts", basename: "alpha.ts", action: "edit", in_corpus: false, via_subagent: false },
          { path: "docs/beta.md", basename: "beta.md", action: "write", in_corpus: false, via_subagent: true },
          { path: "src/gamma.ts", basename: "gamma.ts", action: "read", in_corpus: false, via_subagent: false },
        ],
      }),
    }),
  );
  await page.route(`**/api/sessions/${SID}/commits`, (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ commits: state.commits }) }),
  );
  await page.route("**/api/session-card?*", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        schema: "kbc-session-card/1",
        sid: SID,
        now_unix: NOW,
        commits: {
          available: true,
          total: 1,
          shown: 1,
          truncated: false,
          items: [{ repo: "synthetic", sha: "abc1234def5678", visible: true, confidence: "exact", via: "trailer", attributed_here: true }],
        },
      }),
    }),
  );
}

const COMMITS = [
  { kind: "commit", sha: "abc1234def5678", subject: "synthetic first commit", resolved: true, trailers: [] },
  { kind: "commit", sha: "fedcba9876543", subject: "synthetic second commit", resolved: false, trailers: [] },
];

test.describe("RAILCHANGES Changes tab", () => {
  test("grouped files, commits and the checks phrase; doors exist only with a code_url", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await mockSession(page, { commits: COMMITS }, BASE);
    await page.goto(`${BASE}/term/${TERM_ID}`);
    await page.locator('[data-testid="term-tab-changes"]').click();

    await expect(page.locator('[data-testid="changes-dir"]')).toHaveCount(2, { timeout: 20_000 });
    await expect(page.locator('[data-testid="changes-subagent"]')).toHaveCount(1);
    await expect(page.locator('[data-testid="changes-commit"]')).toHaveCount(2);
    await expect(page.locator('[data-testid="changes-unresolved"]')).toHaveCount(1);
    await expect(page.locator('[data-testid="session-checks-phrase"]')).toHaveText("Verified: tests passed after the last edit");
    await expect(page.locator('[data-testid="changes-fresh"]')).toContainText("As of last capture");
    await expect(page.locator('[data-testid="changes-commit-door"]')).toHaveCount(1);
    await expect(page.locator('[data-testid="changes-commit-door"]')).toHaveAttribute("href", /\/r\/synthetic\/~commit\/abc1234def5678$/);
    expect(await page.locator('[data-testid="changes-file-door"]').count()).toBeGreaterThan(0);
  });

  test("without a code_url the rows are plain text", async ({ page }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    await mockSession(page, { commits: COMMITS }, null);
    await page.goto(`${BASE}/term/${TERM_ID}`);
    await page.locator('[data-testid="term-tab-changes"]').click();
    await expect(page.locator('[data-testid="changes-commit"]')).toHaveCount(2, { timeout: 20_000 });
    await expect(page.locator('[data-testid="changes-commit-door"]')).toHaveCount(0);
    await expect(page.locator('[data-testid="changes-file-door"]')).toHaveCount(0);
  });

  test("a capture event refetches and shows the new commit", async ({ page, request }) => {
    await installFakeTerminal(page, { total: 300, seedLines: 100 });
    await admit(page);
    const state = { commits: COMMITS as unknown[] };
    await mockSession(page, state, null);
    await page.goto(`${BASE}/term/${TERM_ID}`);
    await page.locator('[data-testid="term-tab-changes"]').click();
    await expect(page.locator('[data-testid="changes-commit"]')).toHaveCount(2, { timeout: 20_000 });

    state.commits = [...COMMITS, { kind: "commit", sha: "1234567abcdef", subject: "synthetic third commit", resolved: true, trailers: [] }];
    const seed = await request.post(`${BASE}/api/kb/mem/artifacts`, {
      data: {
        title: `Session transcript 20260701T0900${Date.now() % 10000}Z`,
        body_html: `<pre>${[
          `{"type":"user","timestamp":"2026-07-01T09:00:00Z","cwd":"/work/synthetic","message":{"role":"user","content":"synthetic prompt"}}`,
          `{"type":"assistant","timestamp":"2026-07-01T09:05:00Z","message":{"role":"assistant","content":[{"type":"text","text":"synthetic closing"}]}}`,
        ].join("\n")}</pre>`,
        category: "memory-session",
        session_id: SID,
      },
    });
    expect(seed.ok()).toBeTruthy();
    await expect(page.locator('[data-testid="changes-commit"]')).toHaveCount(3, { timeout: 30_000 });
  });
});
