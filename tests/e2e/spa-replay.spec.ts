import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// W3.R-c — the session-replay reader (`/replay/:kb/:sid`).
//
// Shape (mirrors spa-sessions.spec.ts's harness exactly):
// 1. Seed a memory-session artifact via POST /api/kb/mem/artifacts, whose
//    body carries a `<pre>` JSONL transcript with REAL ISO timestamps and
//    tool calls pointing at absolute paths inside the e2e canon corpus (the
//    daemon's `[kb.canon] path`, exported by global-setup as KB_E2E_CORPUS).
//    Those paths resolve through `resolve_corpus_path` → real artifact ids.
// 2. Wait for /api/sessions to surface the row, then read the replay wire
//    directly to learn the resolved artifact ids.
// 3. Drive the route: the rail renders every beat, the keyboard steps the
//    playhead, the stage's artifact + section highlight follow it, and an
//    unresolved path renders as plain text rather than disappearing.

const SESSION_ID = "spec-replay-abc";
const SESSION_TITLE = "Session transcript 20260720T100000Z";

type ReplayBeat = {
  beat: { seq: number; kind: string; detail: string; path?: string };
  kb?: string;
  artifact_id?: string;
  heading_slug?: string;
};
type ReplayResponse = {
  grammar: string;
  session_id: string;
  beats: ReplayBeat[];
  total_beats: number;
  matched: number;
};

function corpusDir(): string {
  const dir = process.env.KB_E2E_CORPUS;
  if (!dir) throw new Error("KB_E2E_CORPUS not set by global-setup");
  return dir;
}

// A transcript the replay extractor can actually chew on: two human prompts
// (so there are two segments), an assistant narration, two corpus reads WITH
// offset/limit (so each carries a line range → a heading slug), a bash beat,
// and one read of a path in no corpus at all.
function seedTranscriptBody(): string {
  const c = corpusDir();
  const lines = [
    // Metadata-ish first line with no timestamp: counted, never a beat.
    `{"type":"user","message":{"role":"user","content":"<local-command-caveat>noise</local-command-caveat>"},"isMeta":true}`,
    `{"type":"user","timestamp":"2026-07-20T10:00:00Z","message":{"role":"user","content":"open the kitchen sink"}}`,
    `{"type":"assistant","timestamp":"2026-07-20T10:00:27Z","message":{"role":"assistant","content":[{"type":"text","text":"Reading it now."},{"type":"tool_use","name":"Read","input":{"file_path":"${c}/kitchen-sink.html","offset":150,"limit":10}}]}}`,
    `{"type":"assistant","timestamp":"2026-07-20T10:02:51Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}]}}`,
    `{"type":"user","timestamp":"2026-07-20T10:05:00Z","message":{"role":"user","content":"now the other one"}}`,
    `{"type":"assistant","timestamp":"2026-07-20T10:05:30Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"${c}/cost-of-abstraction.html","offset":250,"limit":5}}]}}`,
    `{"type":"assistant","timestamp":"2026-07-20T10:06:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/etc/hosts"}}]}}`,
  ];
  return `<pre>${lines.join("\n")}</pre>`;
}

test.describe("/replay — the session-replay reader", () => {
  test("rail, playhead keyboard, artifact swap + section highlight", async ({
    page,
  }) => {
    const base = `http://127.0.0.1:${PORT}`;

    // 1. Seed the capture.
    const seed = await page.request.post(`${base}/api/kb/mem/artifacts`, {
      data: {
        title: SESSION_TITLE,
        body_html: seedTranscriptBody(),
        category: "memory-session",
        session_id: SESSION_ID,
      },
    });
    expect(seed.ok()).toBeTruthy();

    // 2. Wait for the daemon to surface the session row.
    const deadline = Date.now() + 20_000;
    let surfaced = false;
    while (Date.now() < deadline) {
      const r = await page.request.get(`${base}/api/sessions`);
      if (r.ok()) {
        const body = (await r.json()) as {
          sessions: { session_id: string }[];
        };
        if (body.sessions.some((s) => s.session_id === SESSION_ID)) {
          surfaced = true;
          break;
        }
      }
      await new Promise((rs) => setTimeout(rs, 200));
    }
    expect(surfaced, "seeded session must appear in /api/sessions").toBeTruthy();

    // 3. Read the wire so the UI assertions can name real artifact ids.
    const wire = await page.request.get(
      `${base}/api/sessions/${SESSION_ID}/replay`,
    );
    expect(wire.ok()).toBeTruthy();
    const replay = (await wire.json()) as ReplayResponse;
    expect(replay.grammar).toBe("session-replay/1");
    // prompt · outcome · read · bash · prompt · read · read
    //
    // The 2nd beat is "outcome", not "assistant": "Reading it now." is the
    // fixture's ONLY assistant prose (every later assistant record is
    // tool-only), so under the W2 digest-closure rule it IS
    // `closing_assistant_text`'s pick — `assistant_beats` (kb-core
    // sessions/replay.rs) promotes the record whose full prose matches it
    // from Assistant to Outcome (R3/R7/S6).
    expect(replay.beats.length).toBe(7);
    expect(replay.beats.map((b) => b.beat.kind)).toEqual([
      "prompt",
      "outcome",
      "read",
      "bash",
      "prompt",
      "read",
      "read",
    ]);
    const kitchen = replay.beats[2];
    const cost = replay.beats[5];
    expect(kitchen.artifact_id, "corpus read must resolve").toBeTruthy();
    expect(cost.artifact_id).toBeTruthy();
    expect(kitchen.artifact_id).not.toBe(cost.artifact_id);
    // The ranged read landed under a real heading — the id the iframe
    // runtime accepts as `kb:scroll-to-id`.
    expect(kitchen.heading_slug).toMatch(/^kb-h-/);
    // The out-of-corpus read keeps its raw path and is NOT dropped.
    expect(replay.beats[6].artifact_id).toBeFalsy();
    expect(replay.beats[6].beat.path).toBe("/etc/hosts");

    // 4. The route itself.
    await page.goto(`${base}/replay/mem/${SESSION_ID}`);
    const view = page.locator('[data-testid="replay-view"]');
    await expect(view).toBeVisible({ timeout: 10_000 });

    // The rail lists every beat, in wire order, grouped into two
    // prompt-rooted segments.
    const beats = page.locator('[data-testid="replay-rail"] .kb-replay__beat');
    await expect(beats).toHaveCount(7);
    await expect(
      page.locator('[data-testid="replay-rail"] .kb-replay__seg'),
    ).toHaveCount(2);
    await expect(beats.first()).toContainText("open the kitchen sink");
    // Δt is rendered per beat, deterministic grammar (lib/replay.ts): the
    // assistant turn came 27 s after the prompt, the bash call 2m24s after
    // the read (which shares the assistant record's instant, so +0s).
    await expect(beats.nth(1)).toContainText("+27s");
    await expect(beats.nth(2)).toContainText("+0s");
    await expect(beats.nth(3)).toContainText("+2m24s");
    // The unresolved read shows its raw path rather than vanishing.
    await expect(beats.nth(6)).toContainText("/etc/hosts");

    // Playhead starts at beat 1 — nothing has been read yet, so the stage
    // is honestly empty rather than guessing.
    const stage = page.locator('[data-testid="replay-stage"]');
    await expect(page.locator('[data-testid="replay-stop"]')).toContainText(
      "1/7",
    );
    await expect(stage).toContainText("Nothing in play yet");

    // 5. Keyboard: → steps one beat. Two steps lands on the corpus read.
    // No click first — focus sits on <body> after navigation, and the route's
    // handler is a window listener (a stray click could hit a rail row).
    await page.keyboard.press("ArrowRight");
    await page.keyboard.press("ArrowRight");
    await expect(page.locator('[data-testid="replay-stop"]')).toContainText(
      "3/7",
    );
    // The stage swapped to the artifact that beat touched, and carries the
    // section the line range resolved to (the same id posted into the
    // iframe as `kb:scroll-to-id`).
    await expect(stage).toHaveAttribute(
      "data-kb-artifact",
      kitchen.artifact_id as string,
    );
    await expect(stage).toHaveAttribute(
      "data-kb-slug",
      kitchen.heading_slug as string,
    );
    const frame = page.locator(".kb-replay__frame");
    await expect(frame).toHaveAttribute(
      "src",
      // Host-grammar v2 (invariant #7): the SPA emits the kb-QUALIFIED
      // `{kb_enc}--{id}` label, never the bare id.
      new RegExp(`^https?://canon--${kitchen.artifact_id}\\.`),
    );

    // 6. `]` jumps to the next segment's first beat (the second prompt).
    await page.keyboard.press("]");
    await expect(page.locator('[data-testid="replay-stop"]')).toContainText(
      "5/7",
    );
    // A prompt touches no file, so the playhead honestly keeps showing the
    // artifact most recently in play — and SAYS which beat it came from.
    await expect(stage).toHaveAttribute(
      "data-kb-artifact",
      kitchen.artifact_id as string,
    );
    await expect(stage).toHaveAttribute("data-beat-index", "2");
    await expect(stage).toContainText("still showing beat 3");

    // 7. One more step crosses into a DIFFERENT artifact — the iframe src
    // swaps.
    await page.keyboard.press("j");
    await expect(page.locator('[data-testid="replay-stop"]')).toContainText(
      "6/7",
    );
    await expect(stage).toHaveAttribute(
      "data-kb-artifact",
      cost.artifact_id as string,
    );
    await expect(frame).toHaveAttribute(
      "src",
      new RegExp(`^https?://canon--${cost.artifact_id}\\.`),
    );

    // 8. The last beat is out of corpus: a plain-path stage, no iframe.
    await page.keyboard.press("j");
    await expect(page.locator('[data-testid="replay-stop"]')).toContainText(
      "7/7",
    );
    await expect(stage).toHaveAttribute("data-kb-path", "/etc/hosts");
    await expect(page.locator(".kb-replay__frame")).toHaveCount(0);
    await expect(stage).toContainText("Not in any corpus");

    // 9. `[` rewinds to this segment's head (the second prompt).
    await page.keyboard.press("[");
    await expect(page.locator('[data-testid="replay-stop"]')).toContainText(
      "5/7",
    );

    // 10. The playhead is in the URL, so a replay stop is linkable.
    expect(new URL(page.url()).searchParams.get("b")).toBe("4");
  });

  test("the sessions view links into the replay", async ({ page }) => {
    const base = `http://127.0.0.1:${PORT}`;
    await page.goto(`${base}/sessions?focus=${SESSION_ID}`);
    const link = page.locator('[data-testid="session-replay-link"]');
    await expect(link).toBeVisible({ timeout: 15_000 });
    await link.click();
    await expect(page.locator('[data-testid="replay-view"]')).toBeVisible({
      timeout: 10_000,
    });
    expect(page.url()).toContain(`/replay/mem/${SESSION_ID}`);
  });
});
