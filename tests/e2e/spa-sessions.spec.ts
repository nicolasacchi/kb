import { test, expect } from "@playwright/test";
import { writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { PORT } from "./helpers";

// v0.14 T6 — /sessions view round-trip.
//
// Shape:
// 1. Seed a memory-session artifact via the daemon's
//    /api/kb/mem/artifacts route (POST + body_html carrying a JSONL
//    `<pre>` so the indexer's session-enrichment hook fires).
// 2. Wait for /api/sessions to surface the row.
// 3. /sessions renders the row in a day-bucket section + the
//    Header's "Sessions" tab lights up + g s hotkey navigates here.
// 4. Selecting the row populates the right rail's inspector.

const SESSION_ID = "spec-sess-abc";
const SESSION_TS = "20260524T100000Z";
const SESSION_TITLE = `Session transcript ${SESSION_TS}`;

function seedTranscriptBody(): string {
  // The JSONL `<pre>` is what `kb_core::sessions::parse_session_html`
  // counts + scrapes for the first-user-prompt preview. Two lines:
  // a synthetic command-caveat that should be skipped, then a real
  // user prompt that should surface as `first_user_prompt`.
  const lines = [
    '{"type":"user","message":{"role":"user","content":"<local-command-caveat>noise</local-command-caveat>"},"isMeta":true}',
    '{"type":"user","message":{"role":"user","content":"hello from the playwright spec"}}',
  ];
  return `<pre>${lines.join("\n")}</pre>`;
}

test.describe("/sessions view", () => {
  test("seed via POST, surface in list, render inspector, hotkey navigates", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;

    // 1. Seed: POST a memory-session memory artifact. The artifacts
    // route renders the wrapper HTML; we pass body_html so the
    // `<pre>` JSONL lands verbatim. The route also writes <meta
    // name="kb-session"> when session_id is provided, so the parser
    // pulls the id from the meta rather than the filename.
    const seed = await page.request.post(
      `${base}/api/kb/mem/artifacts`,
      {
        data: {
          title: SESSION_TITLE,
          body_html: seedTranscriptBody(),
          category: "memory-session",
          session_id: SESSION_ID,
        },
      },
    );
    expect(seed.ok()).toBeTruthy();

    // 2. Wait for the daemon's /api/sessions to see the row.
    let surfaced: { session_id: string }[] = [];
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline) {
      const r = await page.request.get(`${base}/api/sessions`);
      if (r.ok()) {
        const body = (await r.json()) as { sessions: { session_id: string }[] };
        surfaced = body.sessions;
        if (surfaced.some((s) => s.session_id === SESSION_ID)) break;
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(
      surfaced.some((s) => s.session_id === SESSION_ID),
      "seeded session must appear in /api/sessions",
    ).toBeTruthy();

    // 3. /sessions renders the row + day bucket.
    await page.goto(`${base}/sessions`);
    await expect(page.locator('[data-testid="sessions-view"]')).toBeVisible();
    const row = page.locator(`[data-session-id="${SESSION_ID}"]`);
    await expect(row).toBeVisible({ timeout: 10_000 });
    // The first-user-prompt preview reads through the row.
    await expect(row).toContainText("hello from the playwright spec");

    // 4. Header tab is highlighted on this route.
    await expect(
      page.locator('[data-testid="header-sessions-tab"]'),
    ).toHaveAttribute("aria-selected", "true");

    // 5. Click the row → inspector populates the right rail.
    await row.locator(".kb-ses__row-btn").click();
    await expect(page.locator(".kb-ses__inspector")).toBeVisible();
    await expect(page.locator(".kb-ses__inspector")).toContainText(SESSION_ID);

    // 6. Hotkey: from /, `g s` should navigate to /sessions. Hop to
    // gallery first, then chord. (HotkeyRoot binds `g s` to /sessions
    // and rebound Settings to `g ,` in S5.)
    await page.goto(`${base}/`);
    await page.keyboard.press("g");
    await page.keyboard.press("s");
    await expect(page).toHaveURL(/\/sessions(\?|$)/);
  });

  // P0–P7 — a rich transcript exercises the full feature surface: readable
  // name (A2), folder facet (A1), file manifest (A4/A6), decisions (S9),
  // commits (P5), resume (S10), search (P6), and the threads view (P7).
  test("rich transcript drives identity, manifest, decisions, commits, resume, search, threads", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-rich-001";
    const lines = [
      `{"type":"user","timestamp":"2026-06-20T09:00:00Z","cwd":"/proj/widget","gitBranch":"main","promptSource":"typed","message":{"role":"user","content":"build the widget feature"}}`,
      `{"type":"assistant","timestamp":"2026-06-20T09:01:00Z","message":{"role":"assistant","model":"claude-opus-4-8","usage":{"input_tokens":2000,"output_tokens":500},"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/proj/widget/src/main.rs","old_string":"a","new_string":"b"}}]}}`,
      `{"type":"user","timestamp":"2026-06-20T09:02:00Z","toolUseResult":{"answers":{"Which approach?":"Option B"},"questions":[{"question":"Which approach?"}]},"message":{"role":"user","content":[{"type":"tool_result","content":"Your questions have been answered: ..."}]}}`,
      `{"type":"assistant","timestamp":"2026-06-20T09:03:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"git commit -m \\"feat: widget\\""}}]}}`,
      `{"type":"user","timestamp":"2026-06-20T09:03:30Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"[main beadf00d] feat: widget"}]}}`,
    ];
    const seed = await page.request.post(`${base}/api/kb/mem/artifacts`, {
      data: {
        title: "Session transcript 20260620T090000Z",
        body_html: `<pre>${lines.join("\n")}</pre>`,
        category: "memory-session",
        session_id: sid,
      },
    });
    expect(seed.ok()).toBeTruthy();

    // Wait for enrichment (folder cwd persisted ⇒ the folders facet has it).
    const deadline = Date.now() + 15_000;
    let ready = false;
    while (Date.now() < deadline) {
      const r = await page.request.get(
        `${base}/api/sessions/${sid}/decisions`,
      );
      if (r.ok()) {
        const b = (await r.json()) as { decisions: unknown[] };
        if (b.decisions.length >= 1) {
          ready = true;
          break;
        }
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(ready, "decisions must be persisted").toBeTruthy();

    // A1/A2 — two-line row with the readable name + folder facet present.
    await page.goto(`${base}/sessions`);
    const row = page.locator(`[data-session-id="${sid}"]`);
    await expect(row).toBeVisible({ timeout: 10_000 });
    await expect(row).toContainText("build the widget feature");
    await expect(row).toContainText("widget"); // folder badge
    await expect(
      page.locator('[data-testid="sessions-folder-filter"]'),
    ).toBeVisible();

    // Inspector — manifest (A4), decisions (S9), commits (P5), resume (S10).
    await row.locator(".kb-ses__row-btn").click();
    const insp = page.locator(".kb-ses__inspector");
    await expect(insp).toContainText("Files touched");
    await expect(insp).toContainText("main.rs");
    await expect(insp).toContainText("Decisions");
    await expect(insp).toContainText("Which approach?");
    await expect(insp).toContainText("Option B");
    await expect(insp).toContainText("Commits");
    await expect(insp).toContainText("beadf00d");
    await expect(insp).toContainText("Resume context");
    await expect(insp.locator('[data-testid="session-resume-copy"]')).toBeVisible();

    // SP4 — the "bundle" button downloads a portable .kbsession.zip for
    // cross-machine `claude -r` resume (the daemon export route).
    const bundleBtn = insp.locator('[data-testid="session-download-bundle"]');
    await expect(bundleBtn).toBeVisible();
    const dlPromise = page.waitForEvent("download");
    await bundleBtn.click();
    const dl = await dlPromise;
    expect(dl.suggestedFilename()).toBe(`${sid}.kbsession.zip`);

    // P6 — keyword search narrows to the seeded session.
    await page.locator('[data-testid="sessions-search"]').fill("widget");
    await expect(page.locator(`[data-session-id="${sid}"]`)).toBeVisible({
      timeout: 10_000,
    });
    await page.locator('[data-testid="sessions-search"]').fill("");

    // P7 — the threads view toggle renders a thread for this session.
    await page.locator('[data-testid="sessions-view-threads"]').click();
    await expect(page).toHaveURL(/view=threads/);
    await expect(
      page.locator('[data-testid="session-thread"]').first(),
    ).toBeVisible({ timeout: 10_000 });

    // P8 — "save as list" materialises the thread into an editable list and
    // navigates to it; the session entry there carries a neutral, NON-clickable
    // read-state marker (invariant #25 — transcripts have no read progress).
    await page
      .locator('[data-testid="thread-save-as-list"]')
      .first()
      .click();
    await expect(page).toHaveURL(/\/lists\/[^/]+\/[^/]+/, { timeout: 10_000 });
    const entry = page.locator('[data-testid="list-entry"]').first();
    await expect(entry).toBeVisible({ timeout: 10_000 });
    // The session dot is a <span> (not a <button>) — read-state suppressed.
    await expect(entry.locator(".kb-listd__dot.is-session")).toBeVisible();
    await expect(entry.locator("button.kb-listd__dot")).toHaveCount(0);
  });

  // The `?kb=<corpus>` gallery renders session transcripts as session-aware
  // cards (readable prompt + folder + counts, not the timestamped filename),
  // shows the first-class sessions landing strip, and the transcript reader
  // links back into /sessions.
  test("gallery renders session-aware cards, landing strip, and transcript backlink", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-gallery-001";
    const ts = "20260618T100000Z";
    const filename = `session-${ts}-${sid}.html`;
    // Write a REAL capture-format file (session-<ts>-<sid>.html) into the mem
    // corpus so started_at + the filename-derived backlink are authentic — the
    // POST /artifacts path generates a slug name, which the reader backlink
    // (sid parsed from the filename) can't resolve.
    const memCorpus = process.env.KB_E2E_MEM_CORPUS;
    expect(memCorpus, "KB_E2E_MEM_CORPUS must be set").toBeTruthy();
    const lines = [
      `{"type":"user","timestamp":"2026-06-18T10:00:00Z","cwd":"/proj/dashkit","gitBranch":"main","promptSource":"typed","message":{"role":"user","content":"ship the export button to the dashboard"}}`,
      `{"type":"assistant","timestamp":"2026-06-18T10:02:00Z","message":{"role":"assistant","model":"claude-opus-4-8","usage":{"input_tokens":3000,"output_tokens":700},"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/proj/dashkit/src/export.tsx","old_string":"a","new_string":"b"}}]}}`,
    ];
    const esc = lines
      .join("\n")
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
    const doc = `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>Session transcript ${ts}</title><meta name="kb-category" content="memory-session"><meta name="kb-session" content="${sid}"></head><body><h1>Session transcript ${ts}</h1><pre>${esc}</pre></body></html>`;
    const filePath = join(memCorpus as string, filename);
    writeFileSync(filePath, doc, "utf-8");

    // Wait for enrichment so the gallery join + folders facet have the row.
    const deadline = Date.now() + 15_000;
    let ready = false;
    while (Date.now() < deadline) {
      const r = await page.request.get(`${base}/api/sessions`);
      if (r.ok()) {
        const b = (await r.json()) as { sessions: { session_id: string }[] };
        if (b.sessions.some((s) => s.session_id === sid)) {
          ready = true;
          break;
        }
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(ready, "seeded session must be indexed").toBeTruthy();

    // The gallery grid for the mem corpus.
    await page.goto(`${base}/?kb=mem`);

    // The session-aware card: readable prompt heading (NOT the timestamped
    // filename), the working-folder badge, a counts strip, and the SESSION tag.
    const sCard = page
      .locator(".kb-card", { has: page.locator(".kb-card__sbadge") })
      .filter({ hasText: "ship the export button to the dashboard" });
    await expect(sCard).toBeVisible({ timeout: 10_000 });
    await expect(sCard.locator(".kb-card__sfolder")).toHaveText("dashkit");
    await expect(sCard.locator(".kb-card__smeta")).toContainText("edited");
    await expect(sCard.locator(".kb-card__sbadge")).toContainText("session");
    // The timestamped title must NOT be the card heading.
    await expect(sCard.locator(".kb-card__stitle")).not.toContainText(
      "Session transcript",
    );

    // The first-class sessions landing strip + its jump to the worklog.
    const strip = page.locator(".gallery-sessions-strip");
    await expect(strip).toBeVisible();
    await expect(strip).toContainText("Sessions");
    await strip.click();
    await expect(page).toHaveURL(/\/sessions\?kb=mem/, { timeout: 10_000 });

    // W3.E/S3 — the transcript reader links back into /sessions via the
    // SessionContextCard's "worklog →" action (the by-artifact join,
    // #11's canonical sqlite lookup — replaces the deleted filename-regex
    // `SessionSelfLink`). Visible above the iframe with no rail interaction.
    await page.goto(`${base}/a/mem/${filename}`);
    const card = page.locator('[data-testid="session-context-card"]');
    await expect(card).toBeVisible({ timeout: 10_000 });
    const backlink = card.locator('[data-testid="session-context-worklog"]');
    await expect(backlink).toBeVisible({ timeout: 10_000 });
    await expect(backlink).toHaveAttribute("href", /focus=spec-gallery-001/);

    // Don't leave the synthetic transcript behind for sibling specs.
    try {
      unlinkSync(filePath);
    } catch {
      /* best-effort */
    }
  });
});

// ── W3 — surfaces wave (projects/list-v2/mobile-sheet/context-card/?turn=) ──

async function seedClosedSession(
  page: import("@playwright/test").Page,
  base: string,
  sid: string,
  opts: { prompt: string; closing: string; cwd?: string },
): Promise<void> {
  const lines = [
    `{"type":"user","timestamp":"2026-07-01T09:00:00Z","cwd":"${opts.cwd ?? "/proj/ctx"}","gitBranch":"main","promptSource":"typed","message":{"role":"user","content":${JSON.stringify(opts.prompt)}}}`,
    `{"type":"assistant","timestamp":"2026-07-01T09:05:00Z","message":{"role":"assistant","content":[{"type":"text","text":${JSON.stringify(opts.closing)}}]}}`,
  ];
  const seed = await page.request.post(`${base}/api/kb/mem/artifacts`, {
    data: {
      title: `Session transcript 20260701T09000${sid.slice(-1)}Z`,
      body_html: `<pre>${lines.join("\n")}</pre>`,
      category: "memory-session",
      session_id: sid,
    },
  });
  expect(seed.ok()).toBeTruthy();
}

// Polls `/api/sessions/{sid}` until the enrichment hook has run, returning
// the artifact's `source_relative` (needed to build the reader URL — the
// POST /artifacts path generates a slug name, which is FINE now: the W3
// by-artifact join (#11's canonical sqlite lookup) replaces the old
// filename-regex `SessionSelfLink`, so any backing filename resolves).
async function waitForSourceRelative(
  page: import("@playwright/test").Page,
  base: string,
  sid: string,
): Promise<string> {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    const r = await page.request.get(`${base}/api/sessions/${sid}`);
    if (r.ok()) {
      const b = (await r.json()) as { source_relative?: string };
      if (b.source_relative) return b.source_relative;
    }
    await new Promise((rs) => setTimeout(rs, 200));
  }
  throw new Error(`session ${sid} never surfaced a source_relative`);
}

test.describe("W3.E/S3 — SessionContextCard on a capture", () => {
  test("renders above the iframe with outcome, worklog, and replay links", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-ctxcard-001";
    await seedClosedSession(page, base, sid, {
      prompt: "add the context card",
      closing: "Done — the context card ships above the iframe.",
    });
    const rel = await waitForSourceRelative(page, base, sid);

    await page.goto(`${base}/a/mem/${rel}`);
    const card = page.locator('[data-testid="session-context-card"]');
    await expect(card).toBeVisible({ timeout: 10_000 });
    // The collapsed row shows the outcome (the closure), not the opening
    // prompt — S1/S3's whole point.
    await expect(card).toContainText(
      "Done — the context card ships above the iframe.",
    );
    await expect(
      card.locator('[data-testid="session-context-worklog"]'),
    ).toHaveAttribute("href", new RegExp(`focus=${sid}`));
    await expect(
      card.locator('[data-testid="session-context-replay"]'),
    ).toHaveAttribute("href", new RegExp(`/replay/mem/${sid}`));

    // Expand → the full outcome text renders inline (sessionStorage-
    // persisted per artifact — see SessionContextCard's readExpanded).
    await card.locator('[data-testid="session-context-toggle"]').click();
    await expect(card.locator(".kb-sesctx__full-text")).toContainText(
      "Done — the context card ships above the iframe.",
    );
  });

  test("a non-session artifact renders no context card", async ({ page }) => {
    const base = `http://127.0.0.1:${PORT}`;
    await page.goto(`${base}/a/canon/kitchen-sink.html`);
    await expect(
      page.locator('[data-testid="session-context-card"]'),
    ).toHaveCount(0);
  });
});

test.describe("W3.E/S5 — ?turn=end deep link", () => {
  test("jumps the iframe to the #ses-outcome anchor", async ({ page }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-turnend-001";
    await seedClosedSession(page, base, sid, {
      prompt: "ship the turn=end deep link",
      closing: "Shipped — outcome anchor is reachable via ?turn=end.",
    });
    const rel = await waitForSourceRelative(page, base, sid);

    await page.goto(`${base}/a/mem/${rel}?turn=end`);
    const frame = page.frameLocator(".detail__frame");
    await expect(frame.locator("#ses-outcome")).toBeInViewport({
      timeout: 10_000,
    });
  });

  test("the context card's ↧ outcome button also lands on #ses-outcome", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-turnbtn-001";
    await seedClosedSession(page, base, sid, {
      prompt: "wire the outcome jump button",
      closing: "Wired — the card button posts the same jump as ?turn=end.",
    });
    const rel = await waitForSourceRelative(page, base, sid);

    await page.goto(`${base}/a/mem/${rel}`);
    await page
      .locator('[data-testid="session-jump-outcome"]')
      .click();
    await expect(page).toHaveURL(/turn=end/);
    const frame = page.frameLocator(".detail__frame");
    await expect(frame.locator("#ses-outcome")).toBeInViewport({
      timeout: 10_000,
    });
  });
});

test.describe("W6 — comment on this turn (moonshots M2)", () => {
  // The affordance is designed to work WITHOUT toggling annotate mode first
  // (that's the point — the pencil/`?cm=on` click-anywhere path already
  // covers annotate mode; this is the always-present entry point). The
  // click is fired via `el.click()` inside the iframe's own JS context
  // (matching spa-comments.spec.ts's documented precedent: a real
  // click-chain through a cross-origin iframe "proved fragile" for the
  // popover/selection flows there, so those tests dispatch the resulting
  // postMessage directly — this test instead fires a REAL native click on
  // the actual button element from inside the iframe, which exercises
  // session_render_runtime.js's delegated listener for real without the
  // fragile parts: no drag, no selection, no popover, one element, one
  // click).
  test("the ❝ affordance posts a Section anchor for the clicked turn, no annotate toggle needed", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-turncomment-001";
    await seedClosedSession(page, base, sid, {
      prompt: "flag this turn for review",
      closing:
        "Flagged — turn-anchored comments now resolve fresh across reindex.",
    });
    const rel = await waitForSourceRelative(page, base, sid);

    await page.goto(`${base}/a/mem/${rel}`);
    const frame = page.frameLocator(".detail__frame");

    // The button renders on every turn in ordinary VIEW mode — its own
    // data attribute IS the turn's stable t-<uuid12> id (M2's grammar;
    // this fixture carries no `uuid` field, so the engine's SHA-based
    // fallback id still matches the same t-<12 hex> shape).
    const btn = frame.locator("[data-kb-turn-comment]").first();
    await expect(btn).toBeVisible({ timeout: 10_000 });
    const turnId = await btn.getAttribute("data-kb-turn-comment");
    expect(turnId).toMatch(/^t-[0-9a-f]{12}$/);

    await btn.evaluate((el) => (el as HTMLElement).click());

    // Same destination as an annotate-mode click-to-compose (spa-comments.
    // spec.ts): the panel opens with its inline composer armed at a
    // Section anchor whose id is exactly the clicked turn's.
    const panel = page.getByRole("complementary", { name: "comments" });
    await expect(panel).toBeVisible({ timeout: 10_000 });
    const compose = panel.locator(".cp__compose");
    await expect(compose).toBeVisible({ timeout: 10_000 });
  });
});

test.describe("W3.D/S2 — mobile sessions bottom sheet", () => {
  test("row tap opens the sheet as ?focus=; ✕ and scrim dismiss it", async ({
    page,
  }) => {
    // ≤860px — the unified mobile breakpoint (#31/useIsMobile.ts).
    await page.setViewportSize({ width: 390, height: 844 });
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-mobsheet-001";
    await seedClosedSession(page, base, sid, {
      prompt: "open the mobile sheet",
      closing: "Done.",
    });

    // Wait for /api/sessions to surface the row (list-page fetch, not the
    // by-artifact join used above).
    const deadline = Date.now() + 15_000;
    let surfaced = false;
    while (Date.now() < deadline) {
      const r = await page.request.get(`${base}/api/sessions`);
      if (r.ok()) {
        const b = (await r.json()) as { sessions: { session_id: string }[] };
        if (b.sessions.some((s) => s.session_id === sid)) {
          surfaced = true;
          break;
        }
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(surfaced, "seeded session must surface in /api/sessions").toBeTruthy();

    await page.goto(`${base}/sessions`);
    const row = page.locator(`[data-session-id="${sid}"]`);
    await expect(row).toBeVisible({ timeout: 10_000 });
    // S2 — the desktop rail column is NOT in the DOM at all on mobile
    // (`{!isMobile && <aside className="kb-ses__rail">}`).
    await expect(page.locator(".kb-ses__rail")).toHaveCount(0);

    await row.locator(".kb-ses__row-btn").click();
    await expect(page).toHaveURL(new RegExp(`focus=${sid}`));
    const sheet = page.locator(".kb-ses__inspector--sheet");
    await expect(sheet).toBeVisible({ timeout: 10_000 });
    // ResumeCommand always renders `claude -r <sid>` — the most reliable
    // "this is the right session's sheet" signal (title/outcome placement
    // inside the sheet isn't pinned by this spec).
    await expect(sheet).toContainText(sid);
    const scrim = page.locator(".kb-pinsp-scrim.is-open");
    await expect(scrim).toBeVisible();

    // ✕ dismisses — URL loses ?focus=, sheet unmounts.
    await page.locator('[data-testid="session-sheet-close"]').click();
    await expect(sheet).toHaveCount(0);
    await expect(page).not.toHaveURL(/focus=/);

    // Re-open, dismiss via scrim tap.
    await row.locator(".kb-ses__row-btn").click();
    await expect(sheet).toBeVisible({ timeout: 10_000 });
    await scrim.click({ position: { x: 5, y: 5 } });
    await expect(sheet).toHaveCount(0);

    // Re-open, dismiss via Esc.
    await row.locator(".kb-ses__row-btn").click();
    await expect(sheet).toBeVisible({ timeout: 10_000 });
    await page.keyboard.press("Escape");
    await expect(sheet).toHaveCount(0);
    await expect(page).not.toHaveURL(/focus=/);
  });

  test("a shared /sessions?focus= link opens straight into the sheet", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-mobsheet-002";
    await seedClosedSession(page, base, sid, {
      prompt: "deep link into the sheet",
      closing: "Done.",
    });
    const deadline = Date.now() + 15_000;
    let surfaced = false;
    while (Date.now() < deadline) {
      const r = await page.request.get(`${base}/api/sessions`);
      if (r.ok()) {
        const b = (await r.json()) as { sessions: { session_id: string }[] };
        if (b.sessions.some((s) => s.session_id === sid)) {
          surfaced = true;
          break;
        }
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(surfaced).toBeTruthy();

    // URL truth (#23 ethos): the sheet IS `?focus=` presence, no separate
    // "is open" state — a direct deep link must open it with zero taps.
    await page.goto(`${base}/sessions?focus=${sid}`);
    await expect(
      page.locator(".kb-ses__inspector--sheet"),
    ).toBeVisible({ timeout: 10_000 });
  });
});

test.describe("W3.C — projects home + husk triage", () => {
  test("the project pill opens the projects home; a project card scopes the list", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-projhome-001";
    await seedClosedSession(page, base, sid, {
      prompt: "build the projects home",
      closing: "Shipped.",
      cwd: "/proj/projhome",
    });
    const deadline = Date.now() + 15_000;
    let surfaced = false;
    while (Date.now() < deadline) {
      const r = await page.request.get(`${base}/api/sessions`);
      if (r.ok()) {
        const b = (await r.json()) as { sessions: { session_id: string }[] };
        if (b.sessions.some((s) => s.session_id === sid)) {
          surfaced = true;
          break;
        }
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(surfaced).toBeTruthy();

    await page.goto(`${base}/sessions`);
    await page.locator('[data-testid="sessions-project-pill"]').click();
    await expect(page).toHaveURL(/view=projects/);
    const cards = page.locator('[data-testid="session-project-card"]');
    await expect(cards.first()).toBeVisible({ timeout: 10_000 });

    // Clicking a card scopes ?project= and returns to list view.
    const target = cards.filter({ hasText: "projhome" }).first();
    if (await target.count()) {
      await target.click();
      await expect(page).toHaveURL(/project=/);
      await expect(page).not.toHaveURL(/view=projects/);
    }
  });

  test("hide-trivial defaults OFF; toggling it narrows the list", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    await page.goto(`${base}/sessions`);
    const toggle = page.locator('[data-testid="sessions-hide-trivial"]');
    await expect(toggle).toBeVisible();
    // W3.C build-order decision: hide-trivial starts UNCHECKED (every
    // session shown by default, including trivial husks — never hidden by
    // omission; see the W3 build report for the deviation from the design
    // doc's "hide by default" recommendation).
    await expect(toggle).not.toBeChecked();
    // DEP-RR7 — plain `.click()` + a polling `toBeChecked()` assertion, not
    // `.check()`: `.check()` verifies the post-click state with a single
    // immediate read (no retry), and under `v7_startTransition` (main.tsx)
    // the `checked` prop — driven by this route's `setParams`-backed
    // `hideTrivial`, not local state — now commits through a deferred React
    // transition rather than synchronously inside the click. The click
    // itself and the resulting navigation are correct (verified checked
    // AND the URL both land); only `.check()`'s one-shot verify races it.
    await toggle.click();
    await expect(toggle).toBeChecked();
    await expect(page).toHaveURL(/substance=routine%2Csubstantive|substance=routine,substantive/);
  });
});

// ── W7 (sessions-rethink R15) — live-follow ─────────────────────────────
//
// What's covered here: the parts of LF-1/LF-2 that don't need a real live
// transcript file (`[sessions] live_transcripts_dir` isn't configured on
// the e2e harness's daemon — Tier 1 is opt-in, host/dev-only by design,
// D-LF1). Everything that DOES need Tier 1 — the pulsing "live" dot, the
// LiveTailPanel actually rendering turns, a `/live` poll cycle observed
// through the browser — is explicitly a TODO-W7GATE below; those are
// covered instead by the Rust-side unit/integration tests
// (`kb_core::sessions::tail`, `routes::sessions::live_follow_tests`) and
// the real-smoke curl walkthrough in the build report.
//
// A session seeded with a FIXED historical timestamp (like
// `seedClosedSession` above) reads Tier-0 "idle" by construction — the
// Follow chip is honestly absent for those fixtures (LF-2: "present when
// LF-1 reports live/active"). This section seeds its OWN fresh-timestamped
// session so the chip has something real to assert on.
async function seedFreshSession(
  page: import("@playwright/test").Page,
  base: string,
  sid: string,
  opts: { prompt: string; closing: string; cwd?: string },
): Promise<void> {
  const now = new Date();
  const started = new Date(now.getTime() - 5 * 60_000).toISOString();
  const ended = new Date(now.getTime() - 30_000).toISOString(); // 30s ago ⇒ Tier-0 "active"
  const lines = [
    `{"type":"user","timestamp":"${started}","cwd":"${opts.cwd ?? "/proj/live"}","gitBranch":"main","promptSource":"typed","message":{"role":"user","content":${JSON.stringify(opts.prompt)}}}`,
    `{"type":"assistant","timestamp":"${ended}","message":{"role":"assistant","content":[{"type":"text","text":${JSON.stringify(opts.closing)}}]}}`,
  ];
  const seed = await page.request.post(`${base}/api/kb/mem/artifacts`, {
    data: {
      title: `Session transcript ${sid}`,
      body_html: `<pre>${lines.join("\n")}</pre>`,
      category: "memory-session",
      session_id: sid,
    },
  });
  expect(seed.ok()).toBeTruthy();
}

/// Evaluate inside the artifact iframe, tolerating a remount mid-flight.
///
/// Mirrors `spa-comments.spec.ts`'s own `frameEval`: the reader folds an
/// `artifact.indexed` SSE-bumped nonce into the iframe `key`
/// (`routes/detail.tsx`/`ArtifactPane.tsx`, both commented "folded into the
/// iframe key to remount"), so the currently-open doc's iframe can be torn
/// down and rebuilt at any moment an index event lands for it — e.g. the
/// new-capture reindex this file's W7 "session.captured while pinned"
/// spec deliberately triggers. Any raw `evaluate` in flight at that instant
/// dies with "Execution context was destroyed" — a THROWN error that
/// `expect.poll` would otherwise propagate instead of retrying. Folding the
/// destroyed-context error into a falsy result turns it back into an
/// ordinary retry.
async function frameEval<T>(
  frame: import("@playwright/test").FrameLocator,
  fn: () => T,
): Promise<T | null> {
  try {
    return await frame.locator(":root").evaluate(fn);
  } catch {
    return null;
  }
}

test.describe("W7 (R15/LF-1/LF-2) — follow mode, Tier-0 surfaces", () => {
  test("a fresh (Tier-0 active) session shows the Follow chip; toggling it sets ?follow=1", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-follow-001";
    await seedFreshSession(page, base, sid, {
      prompt: "wire up follow mode",
      closing: "Follow mode wired.",
    });
    const rel = await waitForSourceRelative(page, base, sid);

    await page.goto(`${base}/a/mem/${rel}`);
    const card = page.locator('[data-testid="session-context-card"]');
    await expect(card).toBeVisible({ timeout: 10_000 });

    const followChip = card.locator('[data-testid="session-follow-toggle"]');
    await expect(followChip).toBeVisible({ timeout: 10_000 });
    await expect(followChip).toHaveAttribute("aria-pressed", "false");

    await followChip.click();
    await expect(page).toHaveURL(/follow=1/);
    await expect(followChip).toHaveAttribute("aria-pressed", "true");
    await expect(followChip).toContainText("following");

    // Toggling off removes the param again (round-trip, not a one-way flag).
    await followChip.click();
    await expect(page).not.toHaveURL(/follow=1/);
    await expect(followChip).toHaveAttribute("aria-pressed", "false");
  });

  test("?follow=1 as a direct deep link starts the page in follow mode", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-follow-002";
    await seedFreshSession(page, base, sid, {
      prompt: "deep-link into follow mode",
      closing: "Deep link works.",
    });
    const rel = await waitForSourceRelative(page, base, sid);

    await page.goto(`${base}/a/mem/${rel}?follow=1`);
    const followChip = page.locator('[data-testid="session-follow-toggle"]');
    await expect(followChip).toBeVisible({ timeout: 10_000 });
    await expect(followChip).toHaveAttribute("aria-pressed", "true");
  });

  test("an idle (stale-timestamp) session never shows the Follow chip", async ({
    page,
  }) => {
    // Uses the OTHER seed helper on purpose — its fixed historical
    // timestamps read Tier-0 "idle", so LF-2's "present when LF-1 reports
    // live/active" gate must keep the chip hidden. Honest-absence test.
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-follow-idle-001";
    await seedClosedSession(page, base, sid, {
      prompt: "an old, closed session",
      closing: "Closed long ago.",
    });
    const rel = await waitForSourceRelative(page, base, sid);

    await page.goto(`${base}/a/mem/${rel}`);
    const card = page.locator('[data-testid="session-context-card"]');
    await expect(card).toBeVisible({ timeout: 10_000 });
    await expect(
      card.locator('[data-testid="session-follow-toggle"]'),
    ).toHaveCount(0);
  });

  test("a non-loopback-simulating request still exercises presence as loopback in dev (sanity: route exists, no-store)", async ({
    page,
  }) => {
    // The daemon under test here binds loopback (127.0.0.1) — this is a
    // reachability + header + Tier-1 wiring sanity check, NOT a security
    // test (that's the Rust-side `looks_non_loopback`/`is_loopback_origin`
    // unit pins). Confirms the route is wired end-to-end through the real
    // router. global-setup DOES configure `[sessions] live_transcripts_dir`
    // (and exports `KB_E2E_LIVE_TRANSCRIPTS`), so `enabled` is true.
    const base = `http://127.0.0.1:${PORT}`;
    const liveDir = process.env.KB_E2E_LIVE_TRANSCRIPTS;
    expect(liveDir, "KB_E2E_LIVE_TRANSCRIPTS must be set by global-setup").toBeTruthy();

    // Seed a fresh live transcript so presence reports ≥1 entry with a slug.
    const sid = "spec-presence-seed-001";
    const projectSlug = "e2e-test";
    const { mkdirSync, writeFileSync, unlinkSync } = await import("node:fs");
    const dir = join(liveDir as string, projectSlug);
    mkdirSync(dir, { recursive: true });
    const transcriptPath = join(dir, `${sid}.jsonl`);
    writeFileSync(
      transcriptPath,
      `{"type":"user","timestamp":"${new Date().toISOString()}","cwd":"/proj/e2e-test","message":{"role":"user","content":"presence seed"}}\n`,
      "utf-8",
    );

    // The daemon serves presence from a ~1s TTL snapshot (PSRV-3), so a
    // poll issued just before the seed can be answered stale — poll until
    // the seeded session lands rather than asserting the first response.
    type PresenceBody = {
      enabled: boolean;
      live: { session_id: string; project_slug?: string }[];
    };
    let body: PresenceBody = { enabled: false, live: [] };
    await expect
      .poll(
        async () => {
          const r = await page.request.get(`${base}/api/sessions/presence`);
          expect(r.ok()).toBeTruthy();
          expect(r.headers()["cache-control"]).toContain("no-store");
          body = (await r.json()) as PresenceBody;
          return body.live.some((e) => e.session_id === sid);
        },
        { timeout: 5_000 },
      )
      .toBe(true);
    expect(body.enabled).toBe(true);
    const hit = body.live.find((e) => e.session_id === sid);
    expect(hit?.project_slug).toBeTruthy();

    try {
      unlinkSync(transcriptPath);
    } catch {
      /* best-effort */
    }
  });

  test("LiveTailPanel renders turns from a real poll cycle", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const liveDir = process.env.KB_E2E_LIVE_TRANSCRIPTS;
    expect(liveDir, "KB_E2E_LIVE_TRANSCRIPTS must be set").toBeTruthy();

    const sid = "spec-w7-livetail-001";
    const projectSlug = "e2e-test";
    const transcriptPath = join(liveDir as string, projectSlug, `${sid}.jsonl`);

    // Ensure the directory exists
    const { mkdirSync } = await import("node:fs");
    mkdirSync(join(liveDir as string, projectSlug), { recursive: true });

    // Seed an initial minimal transcript
    const initialLine = `{"type":"user","timestamp":"2026-07-30T14:00:00Z","cwd":"/proj/e2e-test","gitBranch":"main","promptSource":"typed","message":{"role":"user","content":"render from live transcript"}}`;
    const { writeFileSync } = await import("node:fs");
    writeFileSync(transcriptPath, initialLine + "\n", "utf-8");

    // Seed a capture artifact for this session so it's discoverable
    const esc = initialLine
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
    const doc = `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>Session transcript 20260730T140000Z</title><meta name="kb-category" content="memory-session"><meta name="kb-session" content="${sid}"></head><body><h1>Session transcript 20260730T140000Z</h1><pre>${esc}</pre></body></html>`;
    const memCorpus = process.env.KB_E2E_MEM_CORPUS;
    expect(memCorpus).toBeTruthy();
    const captureFile = join(memCorpus as string, `session-20260730T140000Z-${sid}.html`);
    writeFileSync(captureFile, doc, "utf-8");

    // Wait for the capture to be indexed
    const deadline = Date.now() + 15_000;
    let ready = false;
    while (Date.now() < deadline) {
      const r = await page.request.get(`${base}/api/sessions/${sid}`);
      if (r.ok()) {
        ready = true;
        break;
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(ready, "seeded session must be indexed").toBeTruthy();

    // Get the source_relative path for the capture
    const sessionResp = await page.request.get(`${base}/api/sessions/${sid}`);
    const sessionData = (await sessionResp.json()) as { source_relative?: string };
    const sourceRel = sessionData.source_relative;
    expect(sourceRel).toBeTruthy();

    // Warm the daemon's presence cache (PSRV-3, a ~1s TTL snapshot keyed on
    // the whole live_transcripts_dir, shared across every session) BEFORE
    // navigating: the reader's own presence fetch (SessionContextCard mount
    // → useSessionPresence) is a ONE-SHOT query (refetchInterval 30s), so if
    // it lands on a snapshot taken by an earlier test just before this
    // file existed, the LiveTailPanel would wait the full 30s for presence
    // to notice it — far past this test's own timeouts. Poll directly until
    // the cache is guaranteed fresh (mirrors the sibling presence-sanity
    // test's own pattern above).
    await expect
      .poll(
        async () => {
          const r = await page.request.get(`${base}/api/sessions/presence`);
          if (!r.ok()) return false;
          const b = (await r.json()) as { live: { session_id: string }[] };
          return b.live.some((e) => e.session_id === sid);
        },
        { timeout: 5_000 },
      )
      .toBe(true);

    // Navigate to the reader with follow mode
    await page.goto(`${base}/a/mem/${sourceRel}?follow=1`);

    // The session context card should be visible
    const card = page.locator('[data-testid="session-context-card"]');
    await expect(card).toBeVisible({ timeout: 10_000 });

    // Wait for the LiveTailPanel to render (present when follow=1 and Tier-0 active)
    await expect(page.locator('[data-testid="live-tail-panel"]')).toBeVisible({
      timeout: 5_000,
    });

    // W7 wire truth (verified by hand against a scratch daemon booted the
    // same way global-setup does, then polling `GET .../live?from=` across
    // two appends): the FIRST `/live` poll for a never-before-cached session
    // is always a cold `view_bootstrap` — kb-server's `build_live_delta`
    // warms the `ViewCarry` over the tail window but deliberately returns
    // `events: []` for it (LF-4: "the live view is a tail window, not the
    // full document"). So the seeded prompt above never renders as a turn
    // here — the panel legitimately reads "0 turn(s)" at this point; that is
    // correct wire behaviour, not a bug.
    //
    // The live wire only ever emits `TurnClosed` events (W7) — an OPEN turn
    // never renders. Polling right after appending ONLY the assistant record
    // below still returns `events: []` (confirmed by hand): its turn stays
    // open with nothing to close it. `ViewCarry::ingest_user` closes
    // whatever was open the moment a real human turn arrives ("a human turn
    // boundary"), so a follow-up user record is what actually closes it —
    // and by hand that follow-up poll returns TWO `TurnClosed` events: the
    // assistant turn (text "rendered live") and the human turn after it.
    const newTurn = `{"type":"assistant","timestamp":"2026-07-30T14:01:00Z","message":{"role":"assistant","content":[{"type":"text","text":"rendered live"}]}}`;
    const closingTurn = `{"type":"user","timestamp":"2026-07-30T14:02:00Z","message":{"role":"user","content":"thanks, noted"}}`;
    const { appendFileSync } = await import("node:fs");
    appendFileSync(transcriptPath, newTurn + "\n" + closingTurn + "\n", "utf-8");

    // The panel should detect the new lines and render the now-CLOSED
    // assistant turn. Up to 2 poll cycles (LIVE_TAIL_POLL_MS = 3s): the two
    // appended lines above may land inside one delta or split across ticks.
    const panel = page.locator('[data-testid="live-tail-panel"]');
    await expect(panel).toContainText("rendered live", { timeout: 10_000 });

    // Cleanup
    const { unlinkSync } = await import("node:fs");
    try {
      unlinkSync(transcriptPath);
      unlinkSync(captureFile);
    } catch {
      /* best-effort */
    }
  });

  test("session.captured while pinned navigates to the new capture", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const liveDir = process.env.KB_E2E_LIVE_TRANSCRIPTS;
    expect(liveDir, "KB_E2E_LIVE_TRANSCRIPTS must be set").toBeTruthy();

    const sid = "spec-w7-pinned-001";
    const projectSlug = "e2e-test";
    const transcriptPath = join(liveDir as string, projectSlug, `${sid}.jsonl`);

    // Ensure the directory exists
    const { mkdirSync } = await import("node:fs");
    mkdirSync(join(liveDir as string, projectSlug), { recursive: true });

    // Seed an initial live transcript
    const initialLine = `{"type":"user","timestamp":"2026-07-30T15:00:00Z","cwd":"/proj/e2e-test","gitBranch":"main","promptSource":"typed","message":{"role":"user","content":"test pinned handoff"}}`;
    const { writeFileSync } = await import("node:fs");
    writeFileSync(transcriptPath, initialLine + "\n", "utf-8");

    // Seed the INITIAL capture artifact
    const esc1 = initialLine
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
    const doc1 = `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>Session transcript 20260730T150000Z</title><meta name="kb-category" content="memory-session"><meta name="kb-session" content="${sid}"></head><body><h1>Session transcript 20260730T150000Z</h1><pre>${esc1}</pre></body></html>`;
    const memCorpus = process.env.KB_E2E_MEM_CORPUS;
    expect(memCorpus).toBeTruthy();
    const captureFile = join(memCorpus as string, `session-20260730T150000Z-${sid}.html`);
    writeFileSync(captureFile, doc1, "utf-8");

    // Wait for the initial capture to be indexed
    let deadline = Date.now() + 15_000;
    let ready = false;
    while (Date.now() < deadline) {
      const r = await page.request.get(`${base}/api/sessions/${sid}`);
      if (r.ok()) {
        ready = true;
        break;
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(ready, "seeded session must be indexed").toBeTruthy();

    // Get the capture's source_relative for the reader
    const sessionResp = await page.request.get(`${base}/api/sessions/${sid}`);
    const sessionData = (await sessionResp.json()) as { source_relative?: string };
    const sourceRel = sessionData.source_relative;
    expect(sourceRel).toBeTruthy();

    // Navigate to the reader and scroll to the bottom (pinned state)
    await page.goto(`${base}/a/mem/${sourceRel}?follow=1`);
    const frame = page.frameLocator(".detail__frame");
    const body = frame.locator("body");
    await expect(body).toBeVisible({ timeout: 10_000 });

    // Scroll to the bottom — through `frameEval` (not a raw `body.evaluate`):
    // an SSE-driven iframe remount can land between the visibility check
    // above and this call, and a raw evaluate racing that remount throws
    // "Execution context was destroyed" instead of retrying.
    await expect
      .poll(
        async () =>
          frameEval(frame, () => {
            window.scrollTo(0, document.body.scrollHeight);
            return true;
          }),
        { timeout: 15_000 },
      )
      .toBe(true);

    // Append a new turn to the live transcript
    const { appendFileSync } = await import("node:fs");
    const newTurn = `{"type":"assistant","timestamp":"2026-07-30T15:01:00Z","message":{"role":"assistant","content":[{"type":"text","text":"outcome: handoff complete"}]}}`;
    appendFileSync(transcriptPath, newTurn + "\n", "utf-8");

    // Simulate a new capture being written while pinned
    // Write a new capture with both turns
    const allLines = [initialLine, newTurn];
    const escAll = allLines
      .join("\n")
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
    const ts2 = "20260730T150100Z";
    const doc2 = `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>Session transcript ${ts2}</title><meta name="kb-category" content="memory-session"><meta name="kb-session" content="${sid}"></head><body><h1>Session transcript ${ts2}</h1><pre>${escAll}</pre></body></html>`;
    const captureFile2 = join(memCorpus as string, `session-${ts2}-${sid}.html`);
    writeFileSync(captureFile2, doc2, "utf-8");

    // Wait for the daemon to index the new capture
    deadline = Date.now() + 15_000;
    let newCaptureReady = false;
    while (Date.now() < deadline) {
      const r = await page.request.get(`${base}/api/sessions/${sid}`);
      if (r.ok()) {
        const sessionData2 = (await r.json()) as { source_relative?: string };
        if (sessionData2.source_relative && sessionData2.source_relative.includes(ts2)) {
          newCaptureReady = true;
          break;
        }
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(newCaptureReady, "new capture must be indexed").toBeTruthy();

    // Verify the new capture is indexed
    const newSessionResp = await page.request.get(`${base}/api/sessions/${sid}`);
    const newSessionData = (await newSessionResp.json()) as { source_relative?: string };
    expect(newSessionData.source_relative).toContain(ts2);

    // Cleanup
    const { unlinkSync } = await import("node:fs");
    try {
      unlinkSync(transcriptPath);
      unlinkSync(captureFile);
      unlinkSync(captureFile2);
    } catch {
      /* best-effort */
    }
  });
});

test.describe("MI-W4.2c — 'Memories recalled' (the pull side of 'Memories produced')", () => {
  test("a kb-recall hook injection in the transcript surfaces in the inspector", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;
    const sid = "spec-recalls-001";
    // Same "hook_additional_context" attachment shape kb-recall.sh emits
    // (see plugins/kb-memory/hooks/kb-recall.sh) — the memory-recall-ledger
    // enrichment hook parses this into a `memory_recalls` row.
    const lines = [
      `{"type":"user","timestamp":"2026-06-21T09:00:00Z","message":{"role":"user","content":"hello"}}`,
      `{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\\n- Prefers tabs  [mem]  (id aaaaaaaaaaaa)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"20000001-0000-4000-8000-000000000001","timestamp":"2026-06-21T09:00:05.000Z"}`,
      `{"type":"assistant","timestamp":"2026-06-21T09:00:10Z","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}`,
    ];
    const seed = await page.request.post(`${base}/api/kb/mem/artifacts`, {
      data: {
        title: "Session transcript 20260621T090000Z",
        body_html: `<pre>${lines.join("\n")}</pre>`,
        category: "memory-session",
        session_id: sid,
      },
    });
    expect(seed.ok()).toBeTruthy();

    const deadline = Date.now() + 15_000;
    let ready = false;
    while (Date.now() < deadline) {
      const r = await page.request.get(`${base}/api/sessions/${sid}/recalls`);
      if (r.ok()) {
        const b = (await r.json()) as { recalls: unknown[] };
        if (b.recalls.length >= 1) {
          ready = true;
          break;
        }
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(ready, "the memory_recalls ledger must land").toBeTruthy();

    await page.goto(`${base}/sessions`);
    const row = page.locator(`[data-session-id="${sid}"]`);
    await expect(row).toBeVisible({ timeout: 10_000 });
    await row.locator(".kb-ses__row-btn").click();

    const insp = page.locator(".kb-ses__inspector");
    await expect(insp).toContainText("Memories recalled");
    // The injected line names a literal id ("aaaaaaaaaaaa") that won't
    // match any REAL memory's content-hashed id in this corpus, so the
    // row falls back to rendering the bare id (title unresolved) — this
    // is itself the contract under test (`fetchSessionRecalls`' doc
    // comment: the ledger ROW survives even when the memory doesn't).
    await expect(
      insp.locator('[data-testid="session-recalls-list"] li').first(),
    ).toContainText("aaaaaaaaaaaa", { timeout: 10_000 });
  });
});
