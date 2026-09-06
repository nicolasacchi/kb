// v0.34 kb-users — two identities against one daemon. The e2e daemon is
// loopback, and loopback is a trusted hop for the identity ladder, so a
// browser context carrying `Remote-User` attributes as that user (exactly
// how Authelia-behind-traefik delivers identity in production). Verifies:
// independent read-state per user, comment attribution + the owner-only
// edit gate, and the identity chip surfaces the resolved user.
import { test, expect, type BrowserContext } from "@playwright/test";
import { BASE } from "./helpers";

async function apiJson(
  ctx: BrowserContext,
  path: string,
  init?: { method?: string; data?: unknown },
): Promise<{ status: number; body: any }> {
  const resp = await ctx.request.fetch(`${BASE}${path}`, {
    method: init?.method ?? "GET",
    data: init?.data,
  });
  let body: any = null;
  try {
    body = await resp.json();
  } catch {
    /* non-JSON (204 etc.) */
  }
  return { status: resp.status(), body };
}

test.describe("multi-user attribution", () => {
  let alice: BrowserContext;
  let bob: BrowserContext;

  test.beforeAll(async ({ browser }) => {
    alice = await browser.newContext({
      extraHTTPHeaders: { "Remote-User": "alice" },
    });
    bob = await browser.newContext({
      extraHTTPHeaders: { "Remote-User": "bob" },
    });
  });

  test.afterAll(async () => {
    await alice.close();
    await bob.close();
  });

  test("identity reflects Remote-User; users lists both", async () => {
    const a = await apiJson(alice, "/api/identity");
    expect(a.status).toBe(200);
    expect(a.body.user).toBe("alice");
    expect(a.body.identity_source).toBe("header");

    const b = await apiJson(bob, "/api/identity");
    expect(b.body.user).toBe("bob");
  });

  test("independent history + read-state per user", async ({}, testInfo) => {
    testInfo.setTimeout(60_000);
    // Find an artifact in the canon corpus.
    const docs = await apiJson(alice, "/api/kb/canon/docs?limit=1");
    expect(docs.status).toBe(200);
    const id: string = docs.body.docs?.[0]?.id ?? docs.body[0]?.id;
    expect(id).toBeTruthy();

    // alice opens + scrolls to completion; bob opens and stays at top.
    const aOpen = await apiJson(alice, "/api/kb/canon/history/open", {
      method: "POST",
      data: { artifact_id: id, source: "web" },
    });
    expect(aOpen.status).toBe(200);
    const bOpen = await apiJson(bob, "/api/kb/canon/history/open", {
      method: "POST",
      data: { artifact_id: id, source: "web" },
    });
    expect(bOpen.status).toBe(200);
    expect(aOpen.body.visit_id).not.toBe(bOpen.body.visit_id);

    await apiJson(alice, "/api/kb/canon/history/scroll", {
      method: "POST",
      data: { visit_id: aOpen.body.visit_id, scroll_y: 5000, scroll_max: 5000 },
    });

    // Gallery ?read= evaluates per requester: read for alice, not for bob.
    const aRead = await apiJson(alice, "/api/kb/canon/docs?read=read&limit=50");
    const aIds = (aRead.body.docs ?? aRead.body).map((d: any) => d.id);
    expect(aIds).toContain(id);
    const bRead = await apiJson(bob, "/api/kb/canon/docs?read=read&limit=50");
    const bIds = (bRead.body.docs ?? bRead.body).map((d: any) => d.id);
    expect(bIds).not.toContain(id);
  });

  test("comment attribution + owner-only edit gate", async () => {
    const docs = await apiJson(bob, "/api/kb/canon/docs?limit=1");
    const id: string = docs.body.docs?.[0]?.id ?? docs.body[0]?.id;

    // bob comments; the server stamps user=bob regardless of body.
    const created = await apiJson(bob, `/api/kb/canon/review/${id}/comments`, {
      method: "POST",
      data: { body: "bob e2e note", author: "you", anchor: { kind: "file" } },
    });
    expect(created.status).toBe(201);
    expect(created.body.user).toBe("bob");
    const cid = created.body.id;

    // alice cannot edit bob's body (403 not-owner) but can resolve it.
    const edit = await apiJson(alice, `/api/kb/canon/review/${id}/comments/${cid}`, {
      method: "PATCH",
      data: { body: "hijack" },
    });
    expect(edit.status).toBe(403);
    expect(edit.body.type).toBe("urn:kb:errors:not-owner");

    const resolve = await apiJson(alice, `/api/kb/canon/review/${id}/apply`, {
      method: "POST",
      data: { ops: [{ op: "resolve", comment_id: cid }] },
    });
    expect(resolve.status).toBe(200);
  });

  test("SPA renders the identity chip for the resolved user", async () => {
    const page = await alice.newPage();
    await page.goto(`${BASE}/?kb=canon`);
    // The chip carries the resolved username (phase W: subtle top-bar chip).
    await expect(
      page.locator("[data-kb-identity], .kb-identity-chip").first(),
    ).toContainText("alice", { timeout: 15_000 });
    await page.close();
  });
});
