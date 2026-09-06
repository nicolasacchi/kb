import { test, expect as baseExpect, type APIRequestContext, type Page } from "@playwright/test";

// This box is an IO-bound RAID5 shared with other builds: a mutation's fsync
// plus the SSE-driven refetch can take seconds, so every expectation here
// waits up to 15 s (the default 5 s flaked once at 45% IO pressure) and each
// test gets 90 s. Nothing below is a logic change.
const expect = baseExpect.configure({ timeout: 15_000 });
test.setTimeout(90_000);
import { PORT } from "./helpers";

// SL4 — the slate board (design §10). The daemon comes from
// `global-setup.ts` at PORT; every fixture is seeded through the ONE write
// route (`POST /api/slates/{slug}/posts`), because the ledger has exactly one
// mutation and a test that reached around it would be testing a board the
// daemon can never produce.
//
// NOT RUN BY SL4 — the SL2 routes land in a sibling phase, so this file is
// written against §9's wire shapes and the orchestrator runs it once
// `/api/slates` answers. Everything it asserts is either a §10 sentence or a
// rules-matrix row; nothing here is a guess about the SPA's internals beyond
// the selectors SL4 actually shipped.

const BASE = `http://127.0.0.1:${PORT}`;

/// A slug per spec file run, so a failing test never poisons the next one.
function slugFor(name: string): string {
  return `e2e-${name}-${Date.now().toString(36)}`;
}

type Prov = { harness: string; origin: "agent" | "human" | "import"; session_id?: string };
// One session id per seeded post: the daemon caps a session at six posts a
// minute (429 slate-rate), and a seed of seven fixtures is not a rate abuse.
let agentSeq = 0;
function nextAgent(): Prov {
  agentSeq += 1;
  return { harness: "codex", origin: "agent", session_id: `e2e-agent-${agentSeq}` };
}
const HUMAN: Prov = { harness: "spa", origin: "human" };

async function post(
  request: APIRequestContext,
  slug: string,
  body: Record<string, unknown>,
): Promise<{ status: number; json: Record<string, unknown> }> {
  const r = await request.post(`${BASE}/api/slates/${slug}/posts`, {
    // A seeded `found` needs a ref (the daemon refuses one without); the
    // fixtures that care pass their own.
    data: {
      prov: nextAgent(),
      ...(body.kind === "found" && !("refs" in body) ? { refs: ["path:e2e/fixture.rs:1"] } : {}),
      ...body,
    },
    headers: { "Content-Type": "application/json" },
  });
  let json: Record<string, unknown> = {};
  try {
    json = (await r.json()) as Record<string, unknown>;
  } catch {
    /* a 4xx may carry problem+json; a torn body is the assertion's problem */
  }
  return { status: r.status(), json };
}

async function seq(
  request: APIRequestContext,
  slug: string,
  body: Record<string, unknown>,
): Promise<number> {
  const { status, json } = await post(request, slug, body);
  expect(status, `POST ${JSON.stringify(body)}`).toBeLessThan(300);
  return (json.post as { seq: number }).seq;
}

/// Seed a board with one card in every column plus a NOW and a WARN.
async function seedBoard(request: APIRequestContext, slug: string) {
  const now = await seq(request, slug, {
    kind: "now",
    line: "A4 the Desk in flight",
    topic: "v7",
  });
  const warn = await seq(request, slug, {
    kind: "warn",
    line: "every cargo goes through the flock",
  });
  const hand = await seq(request, slug, {
    kind: "hand",
    line: "review_gate.rs — bearer path done, 3 red",
    subject: "crates/kb-server/src/review_gate.rs",
  });
  const ask = await seq(request, slug, {
    kind: "ask",
    line: "does refuse_if_volume_ahead run before migrations?",
  });
  const take = await seq(request, slug, {
    kind: "take",
    line: "A2 bearer graduation",
    subject: "crates/kb-server/src/review_gate.rs",
  });
  const found = await seq(request, slug, {
    kind: "found",
    line: "Store::open calls refuse_if_volume_ahead BEFORE migrations",
    refs: ["path:crates/kb-code-server/src/store.rs:141"],
    topic: "v7",
  });
  const tried = await seq(request, slug, {
    kind: "tried",
    line: "e2e + cargo test concurrently -> OOM at the 10g cgroup",
    failed: "OOM",
  });
  return { now, warn, hand, ask, take, found, tried };
}

async function openBoard(page: Page, slug: string) {
  await page.goto(`${BASE}/slates/${slug}`);
  await expect(page.getByRole("status", { name: /now and warnings/i })).toBeVisible();
}

test.describe("slate board", () => {
  test("the /slates list shows a seeded slate and links to its board", async ({
    page,
    request,
  }) => {
    const slug = slugFor("list");
    await seedBoard(request, slug);

    await page.goto(`${BASE}/slates`);
    const row = page.getByTestId(`slate-row-${slug}`);
    await expect(row).toBeVisible();
    // The attention chip = unacked hand + open ask (the seed has one of
    // each), summed client-side off /api/slates' counts.
    await expect(row.locator(".slates-row__attention")).toHaveText(/[1-9]/);
    await row.click();
    await expect(page).toHaveURL(new RegExp(`/slates/${slug}$`));
  });

  test("the board renders the NOW band and the five columns as named regions", async ({
    page,
    request,
  }) => {
    const slug = slugFor("regions");
    await seedBoard(request, slug);
    await openBoard(page, slug);

    // §10 "Accessibility": the band is role=status, columns are role=region
    // WITH the section name.
    const band = page.getByRole("status", { name: /now and warnings/i });
    await expect(band).toContainText("A4 the Desk in flight");
    await expect(band).toContainText("every cargo goes through the flock");

    for (const name of ["Hands", "Asks", "Takes", "Found and ideas", "Tried"]) {
      await expect(page.getByRole("region", { name })).toBeVisible();
    }
    // The kind WORD is in the DOM beside the emoji — nothing is conveyed by
    // glyph or colour alone.
    await expect(page.getByRole("region", { name: "Takes" })).toContainText("TAKE");
    // Cards are <article>s.
    await expect(page.locator("article.slate-card").first()).toBeVisible();
  });

  test("mark adds +1 to the card", async ({ page, request }) => {
    const slug = slugFor("mark");
    const s = await seedBoard(request, slug);
    await openBoard(page, slug);

    const card = page.locator(`#slate-post-${s.found}`);
    await expect(card.locator(".slate-card__marks")).toHaveCount(0);
    await card.locator('[data-act="mark"]').click();
    await expect(card.locator(".slate-card__marks-n")).toHaveText("+1");
  });

  test("drop confirms, removes the card, and the history drawer shows it struck through", async ({
    page,
    request,
  }) => {
    const slug = slugFor("drop");
    const s = await seedBoard(request, slug);
    await openBoard(page, slug);

    // The seeded TAKE is a live agent's coordination post, so the board's
    // local courtesy prompt fires before the append (§10 "Actions").
    await page.locator(`#slate-post-${s.take} [data-act="drop"]`).click();
    await page.locator(".confirm__go").click();
    await expect(page.locator(`#slate-post-${s.take}`)).toHaveCount(0);

    await page.locator('[data-act="history"]').click();
    const hist = page.locator("#slate-history");
    await expect(hist).toBeVisible();
    const row = hist.locator(`[data-hist-seq="${s.take}"]`);
    await expect(row).toBeVisible();
    await expect(row.locator("s")).toHaveText("A2 bearer graduation");
    await expect(row).toContainText("dropped");
    // ?history=1 is the deep link, so the drawer survives a reload.
    await expect(page).toHaveURL(/history=1/);
  });

  test("edit supersedes: the new card carries (was #n)", async ({ page, request }) => {
    const slug = slugFor("edit");
    const s = await seedBoard(request, slug);
    await openBoard(page, slug);

    await page.locator(`#slate-post-${s.found} [data-act="edit"]`).click();
    const composer = page.locator("#slate-composer");
    await expect(composer).toBeVisible();
    // The kind select is LOCKED while superseding — a post carrying
    // `supersedes` must carry the target's kind (400 kind-mismatch).
    await expect(composer.locator(".slate-composer__kind")).toBeDisabled();
    await composer.locator(".slate-composer__line").fill("Store::open checks the epoch first");
    await composer.locator(".slate-composer__go").click();

    await expect(page.locator(`#slate-post-${s.found}`)).toHaveCount(0);
    const fresh = page.locator(".slate-card__was");
    await expect(fresh).toHaveText(`(was #${s.found})`);
  });
});

test.describe("slate board — pin, composer, refusals, drawings", () => {
  test("pin puts the card first in its column", async ({ page, request }) => {
    const slug = slugFor("pin");
    await seedBoard(request, slug);
    // Two more founds so "first" is a real claim, not a one-card tautology.
    await seq(request, slug, { kind: "found", line: "second found" });
    const third = await seq(request, slug, { kind: "found", line: "third found" });
    await openBoard(page, slug);

    const col = page.getByRole("region", { name: "Found and ideas" });
    // Newest first before the pin.
    await expect(col.locator("article.slate-card").first()).toHaveAttribute(
      "data-seq",
      String(third),
    );

    const oldest = col.locator("article.slate-card").last();
    const oldestSeq = await oldest.getAttribute("data-seq");
    await oldest.locator('[data-act="pin"]').click();
    await expect(col.locator("article.slate-card").first()).toHaveAttribute(
      "data-seq",
      oldestSeq!,
    );
    await expect(col.locator("article.slate-card").first()).toHaveClass(/is-pinned/);
  });

  test("the composer posts through the CM6 mirror textarea (invariant #22)", async ({
    page,
    request,
  }) => {
    const slug = slugFor("composer");
    await seedBoard(request, slug);
    await openBoard(page, slug);

    await page.locator('[data-act="compose"]').click();
    const composer = page.locator("#slate-composer");
    await expect(composer).toBeVisible();
    // Default kind is `found` (§10 "Composer").
    await expect(composer.locator(".slate-composer__kind")).toHaveValue("found");
    await composer.locator(".slate-composer__line").fill("posted from the board");
    // A found needs a ref (the daemon refuses one without; the composer says so
    // inline and keeps Post disabled until one is typed).
    await expect(composer.locator(".slate-composer__go")).toBeDisabled();
    await expect(composer.locator(".slate-composer__hint")).toContainText("needs a ref");
    await composer.locator(".slate-composer__refs").fill("path:e2e/board.rs:1");
    // The 200-char counter is live.
    await expect(composer.locator(".slate-composer__count").first()).toContainText("/200");
    // The hidden mirror <textarea> carries the aria label + the value; it is
    // the e2e handle for the CM6 editor (invariant #22).
    const mirror = page.getByLabel("slate post body");
    await mirror.fill("a body written through the mirror");
    await composer.locator(".slate-composer__go").click();

    await expect(composer).toBeHidden();
    await expect(
      page.getByRole("region", { name: "Found and ideas" }),
    ).toContainText("posted from the board");
  });

  test("a 409 slate-taken surfaces a toast with a 'post anyway' action", async ({
    page,
    request,
  }) => {
    const slug = slugFor("conflict");
    await seedBoard(request, slug);
    await openBoard(page, slug);

    await page.locator('[data-act="compose"]').click();
    const composer = page.locator("#slate-composer");
    await composer.locator(".slate-composer__kind").selectOption("take");
    await composer.locator(".slate-composer__line").fill("A2 bearer graduation, again");
    // The SAME subject the seeded live take holds → 409 slate-taken.
    await composer
      .locator(".slate-composer__subject")
      .fill("crates/kb-server/src/review_gate.rs");
    await composer.locator(".slate-composer__go").click();

    const toast = page.locator(".kb-toast--err").first();
    await expect(toast).toBeVisible();
    await expect(toast.locator(".kb-toast__action")).toHaveText(/post anyway/i);
  });

  test("a mermaid fence renders inside a frame sandboxed to exactly allow-scripts", async ({
    page,
    request,
  }) => {
    const slug = slugFor("sketch");
    const seqNo = await seq(request, slug, {
      kind: "found",
      line: "the auth chain",
      body: "```mermaid\nflowchart LR\n  auth --> review_gate --> handler\n```",
    });
    await openBoard(page, slug);

    const card = page.locator(`#slate-post-${seqNo}`);
    await card.locator(".slate-card__bodytoggle").click();
    const frame = card.locator("iframe.slate-sketch__frame");
    await expect(frame).toBeVisible();
    // EXACTLY "allow-scripts" — no allow-same-origin, so the frame has an
    // opaque origin: no cookies, no storage, no parent DOM. This assertion is
    // the whole security posture of §10 "Drawings" in one line.
    await expect(frame).toHaveAttribute("sandbox", "allow-scripts");
    await expect(frame).toHaveAttribute("src", "/sketch.html");
    await expect(frame).toHaveAttribute("title", `sketch #${seqNo}`);
    // The height cap: 480px, scroll beyond.
    const h = await frame.evaluate((el) => (el as HTMLIFrameElement).clientHeight);
    expect(h).toBeLessThanOrEqual(480);
  });

  test("mobile: one column of accordions, and the composer is a sheet", async ({
    page,
    request,
  }) => {
    const slug = slugFor("mobile");
    const s = await seedBoard(request, slug);
    await page.setViewportSize({ width: 390, height: 780 });
    await openBoard(page, slug);

    // Accordions with counts (§10 "Mobile").
    const takes = page.getByRole("region", { name: "Takes" });
    const toggle = takes.locator(".slate-col__accbtn");
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
    await expect(toggle.locator(".slate-col__count")).toHaveText("1");
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-expanded", "false");

    // The composer is a bottom sheet with a scrim; ✕ / scrim / Esc dismiss.
    await page.locator('[data-act="compose"]').click();
    const composer = page.locator("#slate-composer");
    await expect(composer).toHaveAttribute("role", "dialog");
    await expect(page.locator(".kb-pinsp-scrim.is-open")).toBeVisible();
    // No body editor on mobile — a reader with a capture slot.
    await expect(page.getByLabel("slate post body")).toHaveCount(0);
    await page.keyboard.press("Escape");
    await expect(composer).toBeHidden();

    // Card actions on mobile are mark / drop / done only.
    const card = page.locator(`#slate-post-${s.found}`);
    await expect(card.locator('[data-act="mark"]')).toBeVisible();
    await expect(card.locator('[data-act="edit"]')).toHaveCount(0);
    await expect(card.locator('[data-act="pin"]')).toHaveCount(0);
  });
});

// ── v0.42 · D27 seen chips + D29 groundedness captions (SL7d) ────────────
//
// NOT RUN BY SL7d — same rule as the SL4 block above: the cursor route lands
// in SL7a, so these cases are written against §24's wire (`POST
// /api/slates/{slug}/cursor {session_id, seq, harness}` → 201, idempotent,
// monotonic) and the orchestrator runs them once it answers.

/// Report a served cursor. Fire-and-forget in the CLI; here we assert the
/// status, because a silently-refused cursor would make the chip assertions
/// below fail for the wrong reason.
async function cursor(
  request: APIRequestContext,
  slug: string,
  sessionId: string,
  seqNo: number,
): Promise<number> {
  const r = await request.post(`${BASE}/api/slates/${slug}/cursor`, {
    data: { session_id: sessionId, seq: seqNo, harness: "codex" },
    headers: { "Content-Type": "application/json" },
  });
  return r.status();
}

/// kb-code's origin in the e2e config (`[kb.canon] code_url`) — nothing is
/// ever listening there; every test that cares owns it with `page.route`.
function codeOrigin(url: URL): boolean {
  return url.hostname === "127.0.0.1" && url.port === "4747";
}

test.describe("slate board · seen cursors and groundedness (v0.42)", () => {
  test("two reported cursors put `seen by 2` on the hand card", async ({
    page,
    request,
  }) => {
    const slug = slugFor("seen");
    const s = await seedBoard(request, slug);

    // Two OTHER sessions report having been served the whole board. The
    // author of the hand is neither of them (`seedBoard` gives every seeded
    // post its own session id), so both count — the projection excludes only
    // the post's own author. Their ids differ in the FIRST FOUR characters,
    // because `session_short` is the first four (kb-core `slate.rs`) and a
    // shared prefix would make the hover unreadable.
    expect(await cursor(request, slug, "aa11-seen-one", s.tried)).toBeLessThan(300);
    expect(await cursor(request, slug, "bb22-seen-two", s.tried)).toBeLessThan(300);
    // Monotonic + idempotent: a LOWER seq from a session already counted
    // changes nothing, and a repeat does not double-count.
    await cursor(request, slug, "aa11-seen-one", 1);
    await cursor(request, slug, "bb22-seen-two", s.tried);

    await openBoard(page, slug);
    const chip = page.locator(`#slate-post-${s.hand} .slate-card__seen`);
    await expect(chip).toHaveText("seen by 2");
    // D27's own sentence: a cursor is attribution, not acknowledgement of
    // reading — so the hover says SERVED and lists who.
    await expect(chip).toHaveAttribute("title", /^served to .+, .+$/);

    // A knowledge card never carries the chip, however far the cursors
    // reached: D27 puts it on NOW / WARN / HAND / ASK, exactly where the
    // digest puts it.
    await expect(page.locator(`#slate-post-${s.found} .slate-card__seen`)).toHaveCount(
      0,
    );
  });

  test("the /slates list shows how many sessions have been served", async ({
    page,
    request,
  }) => {
    const slug = slugFor("served");
    const s = await seedBoard(request, slug);
    await cursor(request, slug, "cc33-served-one", s.tried);
    await cursor(request, slug, "dd44-served-two", s.tried);

    await page.goto(`${BASE}/slates`);
    const row = page.getByTestId(`slate-row-${slug}`);
    await expect(row).toBeVisible();
    await expect(row.locator(".slates-row__served")).toHaveText("served 2");
  });

  test("a found card's path ref carries no caption when the kb has no code_url", async ({
    page,
    request,
  }) => {
    const slug = slugFor("nocap");
    const s = await seedBoard(request, slug);

    let hit = false;
    await page.route(
      (url) => codeOrigin(url),
      () => {
        hit = true;
        throw new Error("no code_url ⇒ the board must ask kb-code nothing");
      },
    );

    // `?kb=` is the SPA's own active-kb rule (#33), and `nocode` is the
    // e2e config's code_url-LESS corpus — so the board has no kb-code to ask.
    await page.goto(`${BASE}/slates/${slug}?kb=nocode`);
    await expect(page.getByRole("status", { name: /now and warnings/i })).toBeVisible();

    const card = page.locator(`#slate-post-${s.found}`);
    // The ref chip itself still renders (the post said what it said) —
    // only the caption is absent, and the chip is inert text, not a link.
    await expect(card.locator(".slate-ref")).toContainText("crates/");
    await expect(card.locator(".slate-ref__cap")).toHaveCount(0);
    expect(hit).toBe(false);
  });

  test("with a code_url, kb-code's codelens captions the chip (grounded / ungrounded / unknown)", async ({
    page,
    request,
  }) => {
    const slug = slugFor("caption");
    const s = await seedBoard(request, slug);

    // SHIPPED (SL7e landed the route; SL7f added `?repo=`/`?context=`): the
    // board sends BOTH on every call — `repo` is this board's own slug (the
    // kb-code repo name by design, D29/SL7e/SL7f) and `context` is the
    // citing post's own line text (`SlateBoardCard.line`), the ONLY way a
    // ref naming a line can resolve to `confirmed`/`drifted` rather than an
    // unverifiable default. Captured here and asserted after navigation.
    let sawRepo: string | null = null;
    let sawContext: string | null = null;
    await page.route(
      (url) => codeOrigin(url) && url.pathname === "/api/doc-lens/path",
      (route) => {
        const u = new URL(route.request().url());
        sawRepo = u.searchParams.get("repo");
        sawContext = u.searchParams.get("context");
        return route.fulfill({
          status: 200,
          headers: {
            "content-type": "application/json",
            // `credentials: "include"` ⇒ the browser refuses a `*` ACAO;
            // the exact origin + allow-credentials is what kb-code's own
            // `read_cors` layer sends (spa-coderefs.spec.ts's `fulfillJson`).
            "access-control-allow-origin": BASE,
            "access-control-allow-credentials": "true",
          },
          // `line_hint` echoes the `?line=` this ref asked about (141, from
          // `path:crates/kb-code-server/src/store.rs:141` in `seedBoard`) —
          // without it `groundednessOf` cannot tell "a line was asked" from
          // "no line was asked at all" (SL7f).
          body: JSON.stringify({
            path_state: "present",
            line_hint: 141,
            line_state: "confirmed",
          }),
        });
      },
    );

    // `canon` is the e2e config's kb WITH a `code_url`.
    await page.goto(`${BASE}/slates/${slug}?kb=canon`);
    await expect(page.getByRole("status", { name: /now and warnings/i })).toBeVisible();
    const cap = page.locator(`#slate-post-${s.found} .slate-ref__cap`);
    await expect(cap).toHaveText("grounded");
    await expect(cap).toHaveAttribute("data-grounded", "grounded");

    expect(sawRepo).toBe(slug);
    expect(sawContext).toBe(
      "Store::open calls refuse_if_volume_ahead BEFORE migrations",
    );

    // A HAND is not a knowledge card: no caption, whatever kb-code says.
    await expect(page.locator(`#slate-post-${s.hand} .slate-ref__cap`)).toHaveCount(0);
  });

  test("a kb-code that cannot answer captions `unknown`, never `ungrounded`", async ({
    page,
    request,
  }) => {
    const slug = slugFor("unknown");
    const s = await seedBoard(request, slug);

    await page.route(
      (url) => codeOrigin(url) && url.pathname === "/api/doc-lens/path",
      (route) => route.abort("failed"),
    );

    await page.goto(`${BASE}/slates/${slug}?kb=canon`);
    await expect(page.getByRole("status", { name: /now and warnings/i })).toBeVisible();
    const cap = page.locator(`#slate-post-${s.found} .slate-ref__cap`);
    // "kb-code did not answer" and "the path is gone" are different facts;
    // the board must never merge them into a red verdict.
    await expect(cap).toHaveText("unknown");
    await expect(cap).not.toHaveAttribute("data-grounded", "ungrounded");
  });
});
