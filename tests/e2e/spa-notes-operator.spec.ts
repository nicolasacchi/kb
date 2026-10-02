import { test, expect, type APIRequestContext } from "@playwright/test";
import { BASE } from "./helpers";

// v0.44 P2 — private notes, second pass. Drives the real daemon + SPA:
//   1. the operator (SPA, explicit intent) can resolve their own note, while
//      the same request WITHOUT the intent marker (what an agent sends) is
//      refused 409 and a delete is refused too (A2-7 / A2-8);
//   2. the first private note on an uncommented artifact leaves the public
//      GET (body + headers) byte-identical (A2-5);
//   3. a note-only write emits no `comments.updated` frame, a public write
//      still does (A2-6);
//   4. the /review-notes page lists the note and the SPA can resolve it from
//      the comments panel (A11-4).

const ART = "e2enotesop0001";

async function postComment(
  request: APIRequestContext,
  art: string,
  body: string,
  priv: boolean,
) {
  const r = await request.post(`${BASE}/api/kb/canon/review/${art}/comments`, {
    data: { body, anchor: { kind: "file" }, author: "you", private: priv },
  });
  expect(r.status()).toBe(201);
  return (await r.json()) as { id: string };
}

/// Collect `comments.updated` payloads for `art` while `fn` runs.
async function framesDuring(art: string, fn: () => Promise<void>) {
  const ac = new AbortController();
  const frames: Array<Record<string, unknown>> = [];
  const res = await fetch(`${BASE}/api/events?types=comments.updated`, {
    signal: ac.signal,
  });
  const reader = res.body!.getReader();
  const dec = new TextDecoder();
  let buf = "";
  let kind = "";
  const pump = (async () => {
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        const lines = buf.split("\n");
        buf = lines.pop() ?? "";
        for (const line of lines) {
          if (line.startsWith("event:")) kind = line.slice(6).trim();
          else if (line.startsWith("data:") && kind === "comments.updated") {
            try {
              const p = JSON.parse(line.slice(5).trim()).payload;
              if (p?.artifact_id === art) frames.push(p);
            } catch {
              /* partial line */
            }
          }
        }
      }
    } catch {
      /* aborted */
    }
  })();
  // Let the subscription settle before the writes.
  await new Promise((r) => setTimeout(r, 400));
  await fn();
  await new Promise((r) => setTimeout(r, 1200));
  ac.abort();
  await pump;
  return frames;
}

test.describe("private notes, operator intent and quiet events (v0.44 P2)", () => {
  test("an agent-shaped request cannot resolve or delete a note", async ({
    request,
  }) => {
    const art = "e2enotesop0002";
    const note = await postComment(request, art, "agent must not touch", true);
    const base = `${BASE}/api/kb/canon/review/${art}/comments/${note.id}`;
    // No X-Kb-Visibility header: exactly what the CLI and every agent send.
    expect((await request.post(`${base}/resolve`)).status()).toBe(409);
    expect((await request.delete(base)).status()).toBe(409);
    const all = await (
      await request.get(`${BASE}/api/kb/canon/review/${art}?visibility=all`)
    ).json();
    const row = all.comments.find((c: { id: string }) => c.id === note.id);
    expect(row, "the note must still exist").toBeTruthy();
    expect(row.status).toBe("open");
    // The operator's explicit intent is honoured.
    const ok = await request.post(`${base}/resolve`, {
      headers: { "X-Kb-Visibility": "all" },
    });
    expect(ok.status()).toBe(200);
  });

  test("the first note leaves the public GET unchanged; note-only writes are quiet", async ({
    request,
  }) => {
    const before = await request.get(`${BASE}/api/kb/canon/review/${ART}`);
    const beforeBody = await before.text();
    const beforeEtag = before.headers()["etag"] ?? null;

    const frames = await framesDuring(ART, async () => {
      await postComment(request, ART, "NOTE-QUIET", true);
    });
    expect(frames, "a note-only write must emit no comments.updated").toEqual([]);

    const after = await request.get(`${BASE}/api/kb/canon/review/${ART}`);
    expect(await after.text()).toBe(beforeBody);
    expect(after.headers()["etag"] ?? null).toBe(beforeEtag);

    // Non-vacuous: a PUBLIC write on the same artifact still announces.
    const loud = await framesDuring(ART, async () => {
      await postComment(request, ART, "PUBLIC-LOUD", false);
    });
    expect(loud.length).toBeGreaterThan(0);
  });

  test("the SPA resolves the operator's own note from the comments panel", async ({
    page,
    request,
  }) => {
    const docs = (await (
      await request.get(`${BASE}/api/kb/canon/docs?limit=50`)
    ).json()) as { id: string; path: string; source_relative: string }[];
    const doc = docs.find((d) => d.path.endsWith("kitchen-sink.html"));
    expect(doc).toBeTruthy();
    const NOTE = `OPERATOR-OWN-NOTE-${Date.now()}`;
    const note = await postComment(request, doc!.id, NOTE, true);

    await page.goto(`${BASE}/a/canon/${doc!.source_relative}`);
    await page.locator('[data-kb-act="dock-comments"]').click();
    const panel = page.getByRole("complementary", { name: "comments" });
    const row = panel.locator(".cp__row", { hasText: NOTE });
    await expect(row).toBeVisible({ timeout: 10_000 });
    await row.getByRole("button", { name: /resolve/ }).click();

    await expect
      .poll(async () => {
        const all = await (
          await request.get(
            `${BASE}/api/kb/canon/review/${doc!.id}?visibility=all`,
          )
        ).json();
        return all.comments.find((c: { id: string }) => c.id === note.id)?.status;
      })
      .toBe("resolved");

    // The note browser lists it (it is resolved now, so ask for all).
    await page.goto(`${BASE}/review-notes`);
    await expect(page.getByText(NOTE).first()).toBeVisible({ timeout: 10_000 });
  });
});
