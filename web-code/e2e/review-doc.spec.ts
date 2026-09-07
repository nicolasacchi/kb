import { expect, test } from "@playwright/test";
import { FEATURE_BRANCH, FEATURE_FILE } from "./fixture-repo";
import { BASE, REPO_DIR, REPO_NAME } from "./helpers";

/// V73-K2b — the Review Room's Document tab (`kbc-review/1`, design D9/D9-a).
///
/// Composes a real document through the LOOPBACK-ONLY `POST …/compose` — the
/// same shape `review-room.spec.ts` uses for `findings/import` and `PUT
/// /report`; the CLI is never spawned and this suite stays HTTP/DOM-only.
///
/// WHY THERE IS NO ORPHAN CASE HERE. An orphan card is the failure this
/// surface is built not to hide, and it is asserted in `lib/reviewDoc.test.ts`
/// (state label, class and the null href) rather than in the browser —
/// because `compose` REFUSES a document that contains one: `ref_orphan` is a
/// lint ERROR (`review_doc::lint::RULES`), and a document read is pinned to
/// the patchset it was composed against, whose blobs do not change. So an
/// orphan cannot be composed, and a later patchset does not have this
/// document at all. That refusal is a stronger guarantee than the assertion
/// would have been, and it is the reason this spec asserts the two honest
/// failures that ARE reachable: a MALFORMED ref (a `warn`, so it composes and
/// must render as a visible chip naming the reason) and an INERT one.

/// No `reading_order:` — that absence is assertion (3). `findings: []` is
/// REQUIRED at every tier: an empty list is the explicit claim that this
/// review found nothing, which the lint distinguishes from not having looked.
const DOC_MD = `---
schema: kbc-review/1
summary_md: The feature branch adds one file.
findings: []
blocks:
  context: |
    A one-file change; the guard lives in [[code:${FEATURE_FILE}:1]].
---

The change lands in [[code:${FEATURE_FILE}:1]].

A typo'd ref is a visible failure, not silence: [[code:]] names a kbc scheme
and does not parse.

An inert link makes no claim at all: [[gh:pr/42]].

A bare [[Order]] is kb's wikilink and must stay literal text.
`;

test.describe("review document (kbc-review/1)", () => {
  test("renders live cards, the honest failures, a derived reading order and the compose line", async ({
    page,
    request,
  }) => {
    test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

    const createRes = await request.post(`${BASE}/api/reviews`, {
      data: {
        repo: REPO_NAME,
        head_ref: FEATURE_BRANCH,
        base_ref: "main",
        title: "e2e review document",
      },
    });
    expect(createRes.ok(), `create: ${createRes.status()} ${await createRes.text()}`).toBeTruthy();
    const { id: reviewId } = (await createRes.json()) as { id: number };

    const composeRes = await request.post(`${BASE}/api/reviews/${reviewId}/compose`, {
      data: { schema: "kbc-compose/1", doc_md: DOC_MD, tier: "minimal" },
    });
    if (!composeRes.ok()) {
      // Degrade path: a daemon without the V73-K1 document surface. The
      // cockpit hides the tab entirely, and THAT is the behaviour to assert.
      await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}`);
      await expect(page.locator("[data-kbc-review-title]")).toBeVisible({ timeout: 10_000 });
      expect(await page.locator('[data-kbc-review-view="doc"]').count()).toBe(0);
      return;
    }

    await page.goto(`${BASE}/r/${REPO_NAME}/~reviews/${reviewId}?tab=doc`);
    await expect(page.locator("[data-kbc-doc]")).toBeVisible({ timeout: 15_000 });

    // (1) A PINNED card — a live card for a path that really is in this
    // patchset, always carrying the daemon's own caption.
    const pinned = page.locator(
      `[data-kbc-refcard="code:${FEATURE_FILE}:1"][data-kbc-refcard-state="pinned"]`,
    );
    await expect(pinned.first()).toBeVisible({ timeout: 10_000 });
    await expect(pinned.first().locator("[data-kbc-refcard-state-label]").first()).toHaveText(
      "pinned",
    );
    await expect(pinned.first().locator("[data-kbc-refcard-caption]").first()).not.toBeEmpty();
    // The snippet is the server's bytes, and it is really rendered.
    await expect(pinned.first().locator("[data-kbc-refcard-snippet]").first()).toBeVisible();

    // (2a) A MALFORMED ref is a VISIBLE chip naming the reason — silently
    // degrading it into a kb wikilink would make the failure invisible on
    // both sides (kb-code-server/CLAUDE.md invariant 22(b)).
    const malformed = page.locator('[data-kbc-refcard-state="malformed"]');
    await expect(malformed.first()).toBeVisible();
    await expect(malformed.first()).toContainText("empty path");

    // (2b) An INERT link makes no claim and therefore carries no link:
    // kb-code never calls GitHub and a bare `gh:pr/42` names no host it
    // could resolve.
    const inert = page.locator('[data-kbc-refcard="gh:pr/42"][data-kbc-refcard-state="inert"]');
    await expect(inert.first()).toBeVisible();
    expect(await inert.first().locator("[data-kbc-refcard-link]").count()).toBe(0);

    // (3) The DERIVED reading order, captioned as such — the author's own
    // order and the daemon's fallback must never look alike.
    const order = page.locator('[data-kbc-doc-order="derived"]');
    await expect(order).toBeVisible();
    await expect(order.locator("[data-kbc-doc-order-derived]")).toHaveText("derived");
    await expect(order.locator("[data-kbc-doc-order-caption]")).not.toBeEmpty();

    // `omitted[]` renders as a degrade list — this `minimal` document
    // declares neither `risk:` nor `author:`, and the tab says so.
    await expect(page.locator("[data-kbc-doc-omitted]")).toBeVisible();
    await expect(page.locator('[data-kbc-doc-omitted-item="risk"]')).toBeVisible();

    // A bare `[[…]]` stays kb's wikilink: literal text, never a card.
    expect(await page.locator('[data-kbc-refcard="Order"]').count()).toBe(0);
    await expect(page.locator("[data-kbc-doc]")).toContainText("[[Order]]");

    // The lint panel is read-only and renders the daemon's own rows —
    // `[[Order]]` is exactly the `bare_wikilink` info row it exists for.
    await expect(page.locator("[data-kbc-doclint-census]")).toBeVisible();
    await expect(page.locator('[data-kbc-doclint-row="bare_wikilink"]').first()).toBeVisible();

    // Composing stays loopback-only (D22): the tab offers the LINE to copy.
    await expect(page.locator("[data-kbc-doc-compose-copy]")).toBeVisible();

    // `?cards=folded` is the tab's one knob, and it lives in the URL.
    await page.locator("[data-kbc-doc-fold-toggle]").click();
    await expect(page).toHaveURL(/cards=folded/);
    await expect(page.locator('[data-kbc-doc-fold-toggle="1"]')).toBeVisible();

    // The rail lists every ref as a jump target, with its own state.
    const rail = page.locator("[data-kbc-refcards]");
    await expect(rail).toBeVisible();
    await expect(
      rail.locator(`[data-kbc-refcards-row="code:${FEATURE_FILE}:1"]`),
    ).toBeVisible();
  });
});
