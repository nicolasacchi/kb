import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// A4 — the per-session page at /sessions/:sid and the collapsed list.
//
// Seeding uses the same mechanism as spa-sessions.spec.ts: POST a
// memory-session artifact with a JSONL `<pre>` body so the indexer's
// session-enrichment hook fires. Lineage (root/parent) rides in an
// `adapter-meta` JSONL record (`parentSessionId`/`rootSessionId`), which
// the enrichment parser folds into the V0047 columns. All content is
// synthetic. Session ids use unique prefixes (the e2e harness shares one
// daemon across specs).

async function seedSession(
  page: import("@playwright/test").Page,
  base: string,
  sid: string,
  opts: {
    prompt: string;
    closing: string;
    harness?: string;
    parent?: string;
    root?: string;
  },
): Promise<void> {
  const lines: string[] = [];
  if (opts.parent !== undefined || opts.root !== undefined) {
    lines.push(
      JSON.stringify({
        type: "adapter-meta",
        harness: opts.harness ?? "claude",
        sessionId: sid,
        ...(opts.parent !== undefined
          ? { parentSessionId: opts.parent }
          : {}),
        ...(opts.root !== undefined ? { rootSessionId: opts.root } : {}),
      }),
    );
  }
  lines.push(
    `{"type":"user","timestamp":"2026-07-01T09:00:00Z","cwd":"/proj/a4","gitBranch":"main","promptSource":"typed","message":{"role":"user","content":${JSON.stringify(opts.prompt)}}}`,
  );
  lines.push(
    `{"type":"assistant","timestamp":"2026-07-01T09:05:00Z","message":{"role":"assistant","content":[{"type":"text","text":${JSON.stringify(opts.closing)}}]}}`,
  );
  const seed = await page.request.post(`${base}/api/kb/mem/artifacts`, {
    data: {
      title: `Session transcript 20260701T0900${sid.slice(-4)}Z`,
      body_html: `<pre>${lines.join("\n")}</pre>`,
      category: "memory-session",
      session_id: sid,
    },
  });
  expect(seed.ok()).toBeTruthy();
}

async function waitForList(
  page: import("@playwright/test").Page,
  base: string,
  pred: (sessions: { session_id: string; child_count?: number }[]) => boolean,
  what: string,
): Promise<void> {
  const deadline = Date.now() + 20_000;
  let last: unknown = null;
  while (Date.now() < deadline) {
    const r = await page.request.get(`${base}/api/sessions`);
    if (r.ok()) {
      const b = (await r.json()) as {
        sessions: { session_id: string; child_count?: number }[];
      };
      last = b.sessions;
      if (pred(b.sessions)) return;
    }
    await new Promise((rs) => setTimeout(rs, 200));
  }
  throw new Error(
    `list never reached: ${what} (last: ${JSON.stringify(last)?.slice(0, 400)})`,
  );
}

test.describe("A4 — session page + collapsed list", () => {
  test("session page at 1280: sections open, one column; open-page link navigates; back link returns", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 844 });
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-a4-page-0001";
    await seedSession(page, base, sid, {
      prompt: "page walkthrough",
      closing: "Done.",
    });
    await waitForList(
      page,
      base,
      (sessions) => sessions.some((s) => s.session_id === sid),
      "session must surface in /api/sessions",
    );

    // Arrive the way the operator does: from the ?focus= inspector sheet.
    await page.goto(`${base}/sessions?focus=${sid}`);
    await expect(page.locator(".kb-ses__inspector, .kb-ses__inspector--sheet")).toBeVisible({
      timeout: 10_000,
    });
    await page.locator('[data-testid="session-open-page"]').click();
    await expect(page).toHaveURL(new RegExp(`/sessions/${sid}$`));
    await expect(page.locator('[data-testid="session-page"]')).toBeVisible();

    // Sections are open at 1280px and stacked in one column.
    const outcome = page.locator('[data-testid="session-sec-outcome"]');
    await expect(outcome).toHaveAttribute("open", "");
    const transcript = page.locator('[data-testid="session-sec-transcript"]');
    const outcomeBox = await outcome.boundingBox();
    const transcriptBox = await transcript.boundingBox();
    expect(outcomeBox && transcriptBox).toBeTruthy();
    expect(transcriptBox!.y).toBeGreaterThan(outcomeBox!.y);

    // The back link returns to the originating list URL (?focus= from).
    await page.locator('[data-testid="session-back"]').click();
    await expect(page).toHaveURL(new RegExp(`/sessions\\?focus=${sid}$`));
  });

  test("session page at 390: sections collapsed by default, no horizontal overflow", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-a4-page-0002";
    await seedSession(page, base, sid, {
      prompt: "phone walkthrough",
      closing: "Done.",
    });
    await waitForList(
      page,
      base,
      (sessions) => sessions.some((s) => s.session_id === sid),
      "session must surface in /api/sessions",
    );
    await page.goto(`${base}/sessions/${sid}`);
    await expect(page.locator('[data-testid="session-page"]')).toBeVisible();
    await expect(
      page.locator('[data-testid="session-sec-outcome"]'),
    ).not.toHaveAttribute("open", /.+/);
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth > window.innerWidth,
    );
    expect(overflow).toBe(false);
  });

  test("list collapses subagents under their root and the flat toggle shows them", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 844 });
    const base = `http://127.0.0.1:${PORT}`;
    const root = "spec-a4-root-0001";
    const child1 = "spec-a4-child-001";
    const child2 = "spec-a4-child-002";
    await seedSession(page, base, root, {
      prompt: "root session",
      closing: "Done.",
    });
    await seedSession(page, base, child1, {
      prompt: "child one",
      closing: "Done.",
      parent: root,
      root,
    });
    await seedSession(page, base, child2, {
      prompt: "child two",
      closing: "Done.",
      parent: root,
      root,
    });
    await waitForList(
      page,
      base,
      (sessions) => {
        const r = sessions.find((s) => s.session_id === root);
        return r !== undefined && r.child_count === 2;
      },
      "root must surface with child_count 2",
    );

    // Default (collapsed): only the root row, carrying the +2 chip.
    await page.goto(`${base}/sessions`);
    const rootRow = page.locator(`[data-session-id="${root}"]`);
    await expect(rootRow).toBeVisible({ timeout: 10_000 });
    const chip = page.locator(
      `[data-session-id="${root}"] [data-testid="session-subagents-chip"]`,
    );
    await expect(chip).toContainText("+2 subagents");
    await expect(chip).toHaveAttribute("href", `/sessions/${root}#sec-children`);
    await expect(
      page.locator(`[data-session-id="${child1}"]`),
    ).toHaveCount(0);
    await expect(
      page.locator(`[data-session-id="${child2}"]`),
    ).toHaveCount(0);

    // Flat: every capture row shows.
    await page.locator('[data-testid="sessions-tree-flat"]').click();
    await expect(page).toHaveURL(/tree=flat/);
    await expect(page.locator(`[data-session-id="${root}"]`)).toBeVisible();
    await expect(page.locator(`[data-session-id="${child1}"]`)).toBeVisible();
    await expect(page.locator(`[data-session-id="${child2}"]`)).toBeVisible();
  });

  test("terminal slot absent when terminal is disabled (regression pin only)", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 844 });
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-a4-page-0003";
    await seedSession(page, base, sid, {
      prompt: "terminal check",
      closing: "Done.",
    });
    await waitForList(
      page,
      base,
      (sessions) => sessions.some((s) => s.session_id === sid),
      "session must surface in /api/sessions",
    );
    await page.goto(`${base}/sessions/${sid}`);
    await expect(page.locator('[data-testid="session-page"]')).toBeVisible();
    // FXA4 regression pin only: the page wrapper never emits a terminal
    // section while the slot renders nothing. The real pin is the vitest
    // TerminalSlot/shouldRenderTerminal unit test.
    await expect(
      page.locator('[data-testid="session-sec-terminal"]'),
    ).toHaveCount(0);
  });
});
