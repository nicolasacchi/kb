import { rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { CALLER_FILE, LOCAL_TARGET_DEF_LINE, LOCAL_TARGET_FN, KNOWN_SYMBOL } from "./fixture-repo";
import {
  BASE,
  DOCLENS_DOC_ID,
  DOCLENS_DOC_PATH,
  DOCLENS_FIXTURE_PORT,
  DOCLENS_KB,
  REPO_DIR,
  REPO_NAME,
} from "./helpers";
import {
  DOCLENS_CITEDBY_DOC_A_ID,
  DOCLENS_CITEDBY_DOC_A_PATH,
  DOCLENS_CITEDBY_DOC_A_TITLE,
  DOCLENS_CITEDBY_DOC_B_BASENAME,
  DOCLENS_CITEDBY_DOC_B_ID,
  DOCLENS_CITEDBY_DOC_B_PATH,
  DOCLENS_CITEDBY_FILE,
  DOCLENS_CITEDBY_FILE_CONTENT,
  DOCLENS_CITEDBY_GROUP_LABEL,
  DOCLENS_DOC_TITLE,
  DOCLENS_GROUP_KEY,
  DOCLENS_GROUP_LABEL,
  DOCLENS_NEVER_SCANNED_ID,
  DOCLENS_REMAP_ANCHOR_LINE,
  DOCLENS_REMAP_DOC_ID,
  DOCLENS_REMAP_INSERTED_LINES,
  DOCLENS_REMAP_TARGET_OLD_LINE,
  DOCLENS_TRUNCATED_DOC_ID,
} from "./doclens-fixture";

/// DCB W2.B — the doc↔code lens page, end to end, against the
/// `doclens-fixture.ts` mock standing in for kb (R16). `doc1` exercises the
/// tiered resolution UX (present/absent/unverifiable/symbol) + group nav;
/// `doc2` exercises the rev_remap "moved"/"confirmed" badges over TWO real
/// git commits `doclens-fixture.ts` seeds additively.

const LENS_URL = `/r/${REPO_NAME}/~lens/${DOCLENS_KB}/${DOCLENS_DOC_ID}`;

async function gotoLens(page: Page) {
  await page.goto(`${BASE}${LENS_URL}`);
  await expect(page.locator("[data-kbc-lens-scorecard]")).toBeVisible({ timeout: 10_000 });
}

test.describe("doc-lens", () => {
  test("scorecard renders the one configured repo with counts from the canned payload", async ({ page }) => {
    await gotoLens(page);
    const repoRow = page.locator(`[data-kbc-lens-repo="${REPO_NAME}"]`);
    await expect(repoRow).toBeVisible({ timeout: 10_000 });
    await expect(repoRow).toHaveClass(/is-selected/);
    // present=3 (lib.rs, resolver.rs:2, resolver.rs:9999 — the SAME path
    // "resolver.rs" resolves regardless of its line; `resolve_path`'s
    // present/ambiguous/absent cardinalities are PATH-state, never
    // line-state) absent=1 (nonexistent-file.rb — the one genuine path
    // miss) ambiguous=2 (`DOCLENS_AMBIGUOUS_PAIR_BASENAME`/`_MANY_BASENAME`,
    // fix 9) — the pathless symbol ref (ordinal 4) and the issue ref
    // (ordinal 7) contribute to neither (`path_state: null`). Asserted
    // against the EXACT rendered title (`Scorecard.tsx`'s own template),
    // not just a loose "contains present" match.
    await expect(repoRow).toHaveAttribute("title", "3 present · 2 ambiguous · 1 absent");
  });

  test("the lens body renders groups from the fixture doc — 'Section One' + Ungrouped", async ({ page }) => {
    await gotoLens(page);
    await expect(page.locator("[data-kbc-lens-body]")).toBeVisible({ timeout: 10_000 });

    const sectionGroup = page.locator(`[data-kbc-lens-group="${DOCLENS_GROUP_KEY}"]`);
    await expect(sectionGroup).toBeVisible();
    await expect(sectionGroup.locator(".kbc-lens__rail-count")).toHaveText("2");

    // fix 9 bumped the ungrouped count from 3 to 7 (4 new refs — ambiguous
    // ×2, issue, external — all ungrouped) and the total from 5 to 9.
    const ungrouped = page.locator('[data-kbc-lens-group="__ungrouped__"]');
    await expect(ungrouped).toBeVisible();
    await expect(ungrouped.locator(".kbc-lens__rail-count")).toHaveText("7");

    // "All" is selected by default — every ref row renders.
    await expect(page.locator("[data-kbc-lens-ref-ordinal]")).toHaveCount(9);
  });

  test("DCB-W2.B.R fix 9 — ambiguous/issue/external refs render their own tiers", async ({ page }) => {
    await gotoLens(page);
    await expect(page.locator("[data-kbc-lens-body]")).toBeVisible({ timeout: 10_000 });

    // ordinal 5 — ≤3 real candidates ⇒ ambiguous-inline, an inline list of
    // exactly 2 candidates (never falls into the >3 search-only arm).
    const pairRow = page.locator('[data-kbc-lens-ref-ordinal="5"]');
    await expect(pairRow).toHaveClass(/kbc-lens__row--ambiguous-inline/);
    await expect(pairRow.locator(".kbc-lens__candidates li")).toHaveCount(2);

    // ordinal 6 — 5 real candidates (> AMBIGUITY_INLINE_MAX) ⇒
    // ambiguous-search: the count renders, the inline list does NOT — the
    // B2-named regression (`candidate_count: 5, candidates: []` must still
    // hit this tier, never fall through to an empty ≤3 render).
    const manyRow = page.locator('[data-kbc-lens-ref-ordinal="6"]');
    await expect(manyRow).toHaveClass(/kbc-lens__row--ambiguous-search/);
    await expect(manyRow.locator(".kbc-lens__row-search")).toContainText("5 candidates");
    await expect(manyRow.locator(".kbc-lens__candidates")).toHaveCount(0);

    // ordinal 7 — an issue ref: its own tier, an outbound GitHub link, no
    // path/line badges.
    const issueRow = page.locator('[data-kbc-lens-ref-ordinal="7"]');
    await expect(issueRow).toHaveClass(/kbc-lens__row--issue/);
    await expect(issueRow.locator(".kbc-lens__row-link")).toHaveAttribute(
      "href",
      "https://github.com/acme/shopfront/issues/15357",
    );

    // ordinal 8 — external (vendor) ref: fix 6's OWN tier, never folded
    // into "absent" — its own tag renders, not a "didn't resolve" lie.
    const externalRow = page.locator('[data-kbc-lens-ref-ordinal="8"]');
    await expect(externalRow).toHaveClass(/kbc-lens__row--external/);
    await expect(externalRow.locator("[data-kbc-lens-external-tag]")).toHaveText("external");
  });

  test("clicking the lib.rs ref opens LensCodeView with lib.rs's content", async ({ page }) => {
    await gotoLens(page);
    await page.locator('[data-kbc-lens-ref-ordinal="0"] .kbc-lens__row-link').click();
    await expect(page.locator("[data-kbc-lens-reader] .kbc-codeview")).toContainText(KNOWN_SYMBOL, {
      timeout: 10_000,
    });
  });

  test("clicking the resolver.rs:2 ref opens resolver.rs and centers the cited line", async ({ page }) => {
    await gotoLens(page);
    await page.locator('[data-kbc-lens-ref-ordinal="1"] .kbc-lens__row-link').click();
    const codeview = page.locator("[data-kbc-lens-reader] .kbc-codeview");
    await expect(codeview).toContainText(LOCAL_TARGET_FN, { timeout: 10_000 });

    // `gotoSel` dispatches a COLLAPSED cursor (CM6 gates `.cm-cursor` on
    // `.cm-focused`) — `.focus()` (not `.click()`, which would move the
    // selection to the click point) reveals it without perturbing the
    // selection gotoSel already set.
    await codeview.locator(".cm-content").focus();
    const cursor = codeview.locator(".cm-cursor").first();
    await expect(cursor).toBeVisible({ timeout: 5_000 });

    const gutterRow = codeview.locator(".cm-gutterElement", { hasText: new RegExp(`^${LOCAL_TARGET_DEF_LINE}$`) });
    const [gutterBox, cursorBox] = await Promise.all([gutterRow.boundingBox(), cursor.boundingBox()]);
    expect(gutterBox).not.toBeNull();
    expect(cursorBox).not.toBeNull();
    // Same row, within one line height.
    expect(
      Math.abs(gutterBox!.y + gutterBox!.height / 2 - (cursorBox!.y + cursorBox!.height / 2)),
    ).toBeLessThan(gutterBox!.height);
  });

  test("(/) cycle groups; j/k cycle rows; the buffer guard stops both once focused", async ({ page }) => {
    await gotoLens(page);
    const all = page.locator('[data-kbc-lens-group="all"]');
    const section = page.locator(`[data-kbc-lens-group="${DOCLENS_GROUP_KEY}"]`);
    const ungrouped = page.locator('[data-kbc-lens-group="__ungrouped__"]');

    await expect(all).toHaveClass(/is-selected/);
    await page.keyboard.press(")");
    await expect(section).toHaveClass(/is-selected/);
    await page.keyboard.press(")");
    await expect(ungrouped).toHaveClass(/is-selected/);
    await page.keyboard.press(")");
    await expect(all).toHaveClass(/is-selected/); // wraps forward

    // Backward: the SAME ring, walked the other way.
    await page.keyboard.press("(");
    await expect(ungrouped).toHaveClass(/is-selected/);
    await page.keyboard.press("(");
    await expect(section).toHaveClass(/is-selected/);
    await page.keyboard.press("(");
    await expect(all).toHaveClass(/is-selected/); // wraps backward, back to "All"

    // j/k over "All" (every ref, in ordinal order).
    const row0 = page.locator('[data-kbc-lens-ref-ordinal="0"]');
    const row1 = page.locator('[data-kbc-lens-ref-ordinal="1"]');
    await page.keyboard.press("j");
    await expect(row0).toHaveClass(/is-selected/);
    await page.keyboard.press("j");
    await expect(row1).toHaveClass(/is-selected/);
    await page.keyboard.press("k");
    await expect(row0).toHaveClass(/is-selected/);

    // Enter the buffer — the isInsideBuffer guard must stop ()/j/k from
    // moving group/row selection from here on.
    const codeview = page.locator("[data-kbc-lens-reader] .kbc-codeview");
    await expect(codeview).toBeVisible({ timeout: 10_000 });
    await codeview.locator(".cm-content").focus();
    await page.keyboard.press(")");
    await page.keyboard.press("j");
    await expect(row0).toHaveClass(/is-selected/); // unchanged
    await expect(all).toHaveClass(/is-selected/); // unchanged
  });

  test("pin round-trip: picking a repo PUTs /api/doc-lens/pin, and a repo-less reload pre-selects it", async ({
    page,
  }) => {
    await gotoLens(page);
    const [pinRequest] = await Promise.all([
      page.waitForRequest(
        (req) => req.url().includes("/api/doc-lens/pin") && req.method() === "PUT",
      ),
      page.locator(`[data-kbc-lens-repo="${REPO_NAME}"]`).click(),
    ]);
    const body = pinRequest.postDataJSON() as { kb: string; doc: string; repo: string };
    expect(body.kb).toBe(DOCLENS_KB);
    expect(body.doc).toBe(DOCLENS_DOC_ID);
    expect(body.repo).toBe(REPO_NAME);

    // The repo-less, id-addressed entry ramp — pinned_repo pre-selects it.
    await page.goto(`${BASE}/~lens/${DOCLENS_KB}/${DOCLENS_DOC_ID}`);
    await expect(page).toHaveURL(new RegExp(`${LENS_URL}$`), { timeout: 10_000 });
  });

  test("the doc-prose link-out is the server-built doc_href, verbatim", async ({ page }) => {
    await gotoLens(page);
    const link = page.locator("[data-kbc-lens-doc-link]");
    await expect(link).toBeVisible({ timeout: 10_000 });
    // `[kb_daemon] url` for this harness IS `http://127.0.0.1:4758` (no
    // `public_url` configured) — `doc_href` is minted from that.
    await expect(link).toHaveAttribute(
      "href",
      `http://127.0.0.1:${DOCLENS_FIXTURE_PORT}/a/${DOCLENS_KB}/${DOCLENS_DOC_PATH}`,
    );
  });

  test("never-scanned state: the hint renders, the scorecard still does, and the body doesn't", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~lens/${DOCLENS_KB}/${DOCLENS_NEVER_SCANNED_ID}`);
    await expect(page.locator("[data-kbc-lens-never-scanned]")).toContainText(/not scanned yet/i, {
      timeout: 10_000,
    });
    await expect(page.locator("[data-kbc-lens-scorecard]")).toBeVisible();
    await expect(page.locator("[data-kbc-lens-body]")).toHaveCount(0);
  });

  test("the id-addressed entry ramp resolves to the repo-scoped route via the sole configured repo", async ({
    page,
  }) => {
    await page.goto(`${BASE}/~lens/${DOCLENS_KB}/${DOCLENS_DOC_ID}`);
    await expect(page).toHaveURL(new RegExp(`${LENS_URL}$`), { timeout: 10_000 });
  });

  test("the path-addressed entry ramp resolves through kb-code's own resolve-path route to the id ramp", async ({
    page,
  }) => {
    await page.goto(`${BASE}/~lens/${DOCLENS_KB}/by-path/${DOCLENS_DOC_PATH}`);
    await expect(page).toHaveURL(new RegExp(`${LENS_URL}$`), { timeout: 10_000 });
    await expect(page.locator("[data-kbc-lens-scorecard]")).toBeVisible();
  });

  test("rev_remap: an identity-mapped ref renders plain 'confirmed'; a shifted one renders 'moved +N · git-verified'", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~lens/${DOCLENS_KB}/${DOCLENS_REMAP_DOC_ID}`);
    await expect(page.locator("[data-kbc-lens-body]")).toBeVisible({ timeout: 10_000 });

    const anchorRow = page.locator('[data-kbc-lens-ref-ordinal="0"]');
    await expect(anchorRow.locator('[data-kbc-lens-line="confirmed"]')).toHaveText("✓ confirmed");
    await expect(anchorRow.locator('[data-kbc-lens-line="confirmed-moved"]')).toHaveCount(0);

    const targetRow = page.locator('[data-kbc-lens-ref-ordinal="1"]');
    await expect(targetRow.locator('[data-kbc-lens-line="confirmed-moved"]')).toHaveText(
      `✓ moved +${DOCLENS_REMAP_INSERTED_LINES} · git-verified`,
    );

    // Sanity: the fixture's own cited lines are the OLD (pre-insertion)
    // positions — asserted here so a future edit to the fixture's line
    // layout fails loudly in THIS spec rather than silently downstream.
    expect(DOCLENS_REMAP_ANCHOR_LINE).toBeGreaterThan(0);
    expect(DOCLENS_REMAP_TARGET_OLD_LINE).toBeGreaterThan(DOCLENS_REMAP_ANCHOR_LINE);
  });

  test("DCB-W2.B.R fix 3 — a truncated payload renders the honest 'showing N of M' caption", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~lens/${DOCLENS_KB}/${DOCLENS_TRUNCATED_DOC_ID}`);
    await expect(page.locator("[data-kbc-lens-body]")).toBeVisible({ timeout: 10_000 });
    // 2 refs actually shipped, kb's own claimed `ref_count` is 20 — never
    // fabricated, never silently rounded.
    await expect(page.locator("[data-kbc-lens-truncated]")).toHaveText("showing 2 of 20");
  });
});

/// DCB W3.B — the reverse "cited by" index, rendered as the reader's
/// always-visible `InspectorRail` slot (`components/lens/CitedBy.tsx`).
/// Reuses THIS spec's own `doclens-fixture.ts` mock (16-w3-reverse-index.md
/// §4: "the SAME spec file, appended… the SAME fixture server" — standing up
/// a second fixture/daemon pair for the reverse index alone would duplicate
/// `global-setup.ts` wiring for no isolation benefit, since Playwright specs
/// already run in separate worker processes).
///
/// `doc4`/`doc5` both cite `DOCLENS_CITEDBY_FILE` — a dedicated fixture file
/// (`doclens-fixture.ts`'s own `seedCitedByDemo`), never `resolver.rs`
/// (which `doc1` already cites at TWO ordinals — reusing it here would make
/// "2 docs" ambiguous with "2 claims from 1 doc", the exact miscount
/// `lib/citedBy.ts`'s `citedByDocCount` dedup exists to avoid; see that
/// file's own unit tests for the isolated case).
test.describe("cited-by (DCB-W3.B)", () => {
  async function docRefs(path: string): Promise<{ live: boolean; claims: Array<{ doc_id: string }> }> {
    const resp = await fetch(`${BASE}/api/doc-refs?repo=${REPO_NAME}&path=${encodeURIComponent(path)}`);
    expect(resp.ok).toBe(true);
    return resp.json();
  }

  test.beforeAll(async () => {
    // Pin doc4 + doc5 directly (raw `fetch`, bypassing the Scorecard UI —
    // the pin round-trip itself is already covered by the "pin round-trip"
    // test above; these two pins are pure setup here) then run a real sync
    // pass so `doc_refs` reflects them — the plan's own §4: "a direct fetch
    // from the spec, matching `global-setup.ts`'s own raw-fetch style;
    // loopback-only routes are reachable from the Playwright process itself."
    for (const doc of [DOCLENS_CITEDBY_DOC_A_ID, DOCLENS_CITEDBY_DOC_B_ID]) {
      const pinResp = await fetch(`${BASE}/api/doc-lens/pin`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ kb: DOCLENS_KB, doc, repo: REPO_NAME, doc_hash: null }),
      });
      expect(pinResp.ok).toBe(true);
    }
    const syncResp = await fetch(`${BASE}/api/doc-lens/sync`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{}",
    });
    expect(syncResp.ok).toBe(true);
    const stats = (await syncResp.json()) as { docs_resolved: number };
    // `>=` not `===` — a PRIOR test in this file ("pin round-trip") already
    // pinned `doc1` to the fixture repo; this pass resolves that too. Only
    // `doc4`/`doc5` are asserted precisely below, via `GET /api/doc-refs`
    // scoped to `DOCLENS_CITEDBY_FILE` specifically.
    expect(stats.docs_resolved).toBeGreaterThanOrEqual(2);
  });

  test("GET /api/doc-refs returns exactly the 2 cited-by demo claims, live", async () => {
    const out = await docRefs(DOCLENS_CITEDBY_FILE);
    expect(out.live).toBe(true);
    expect(out.claims.map((c) => c.doc_id).sort()).toEqual(
      [DOCLENS_CITEDBY_DOC_A_ID, DOCLENS_CITEDBY_DOC_B_ID].sort(),
    );
  });

  test("the InspectorRail chip shows the doc count, expands to render title + group + hint, and links out", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    await page.locator(".kbc-tree__row", { hasText: DOCLENS_CITEDBY_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });

    const chip = page.locator("[data-kbc-citedby]");
    await expect(chip).toBeVisible({ timeout: 10_000 });
    await expect(chip.locator("[data-kbc-citedby-label]")).toHaveText("Cited by 2 docs");
    await expect(chip).not.toHaveAttribute("data-kbc-citedby-rotted", "true");

    await chip.locator("[data-kbc-citedby-toggle]").click();
    const rows = chip.locator("[data-kbc-citedby-row]");
    await expect(rows).toHaveCount(2);

    const rowA = rows.filter({ hasText: DOCLENS_CITEDBY_DOC_A_TITLE });
    await expect(rowA.locator("[data-kbc-citedby-group]")).toHaveText(DOCLENS_CITEDBY_GROUP_LABEL);
    await expect(rowA.locator("[data-kbc-citedby-hint]")).toHaveText(DOCLENS_CITEDBY_FILE);
    const linkA = rowA.locator("[data-kbc-citedby-link]");
    await expect(linkA).toHaveAttribute(
      "href",
      `http://127.0.0.1:${DOCLENS_FIXTURE_PORT}/a/${DOCLENS_KB}/${DOCLENS_CITEDBY_DOC_A_PATH}`,
    );
    await expect(linkA).toHaveAttribute("target", "_blank");

    // `doc5` carries NO title (mid-flight W3.A review note, corrected by
    // W3.B.R: `doc_title` is non-null AND never persisted empty either —
    // `sync.rs`'s `doc_title_or_fallback`, DCB-W3.A.R fix 6, resolves it to
    // the doc_path basename server-side before this ever hits the wire).
    // This case is what exercises that SERVER-side fallback end to end
    // (`citedBy.test.ts` covers the client's own redundant copy of the same
    // ladder) — the row falls back to the doc_path basename, never an
    // invisible empty-text link, and carries no group.
    const rowB = rows.filter({ hasText: DOCLENS_CITEDBY_DOC_B_BASENAME });
    await expect(rowB.locator("[data-kbc-citedby-group]")).toHaveCount(0);
    await expect(rowB.locator("[data-kbc-citedby-link]")).toHaveAttribute(
      "href",
      `http://127.0.0.1:${DOCLENS_FIXTURE_PORT}/a/${DOCLENS_KB}/${DOCLENS_CITEDBY_DOC_B_PATH}`,
    );
  });

  test("a file with zero claims renders no CitedBy slot at all", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}`);
    // m5 (W3.B.R review): a bare `toHaveCount(0)` right after the click
    // passes VACUOUSLY at first paint — `CitedBy` renders nothing until its
    // own `useDocRefs` fetch resolves either way, so this assertion would
    // pass identically whether the fetch hadn't landed yet OR had landed
    // and correctly found zero claims. Waiting for the actual `/api/doc-refs`
    // response first means a regression that renders claims LATE can
    // actually fail this test. Registered BEFORE the click so it can't race
    // a fetch that starts (and finishes) between the click and the await.
    const docRefsResponse = page.waitForResponse(
      (r) => r.url().includes("/api/doc-refs") && r.status() === 200,
    );
    await page.locator(".kbc-tree__row", { hasText: CALLER_FILE }).click();
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });
    await docRefsResponse;
    await expect(page.locator("[data-kbc-citedby]")).toHaveCount(0);
  });

  // Deliberately LAST in this describe block — it deletes the one file the
  // two tests above depend on, then restores it (`afterAll`) so every OTHER
  // spec that shares this daemon+repo (this harness spawns exactly ONE,
  // `global-setup.ts`) still sees a clean working tree; several later specs
  // drive real checkouts, which 409 on a dirty tree.
  test.describe("the rotted-claim case", () => {
    test.afterAll(() => {
      if (REPO_DIR) writeFileSync(join(REPO_DIR, DOCLENS_CITEDBY_FILE), DOCLENS_CITEDBY_FILE_CONTENT);
    });

    test("deleting the cited file rots the claim in place — inert note, doc links stay live", async ({ page }) => {
      test.skip(!REPO_DIR, "KB_CODE_E2E_REPO_DIR not set — global-setup didn't run");

      await page.goto(`${BASE}/r/${REPO_NAME}`);
      await page.locator(".kbc-tree__row", { hasText: DOCLENS_CITEDBY_FILE }).click();
      await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });

      const chip = page.locator("[data-kbc-citedby]");
      await expect(chip).toBeVisible({ timeout: 10_000 });
      await expect(chip).not.toHaveAttribute("data-kbc-citedby-rotted", "true");
      await chip.locator("[data-kbc-citedby-toggle]").click();
      await expect(chip.locator("[data-kbc-citedby-row]")).toHaveCount(2);

      rmSync(join(REPO_DIR, DOCLENS_CITEDBY_FILE), { force: true });

      // No navigation/reload from here — a hard nav to a dotted-extension
      // reader URL 404s server-side (`spa.rs`'s asset-vs-shell split; see
      // `blame-gutter.spec.ts`'s own doc on this), so the ONLY safe way to
      // observe the rot is the SAME already-mounted panel updating in
      // place. The watcher's debounce fires `mirror.updated`; the SSE
      // bridge invalidates every query keyed on this repo
      // (`queryClient.ts`'s `invalidateForRepo` — `docRefsQueryKey`'s own
      // position [1] is `repo`, so this query is incidentally caught by
      // it, same as `useLenses`'s), so `CitedBy` refetches and re-renders
      // without any test-driven reload.
      await expect(chip.locator("[data-kbc-citedby-label]")).toHaveText(
        "Cited by 2 docs — path no longer present",
        { timeout: 20_000 },
      );
      await expect(chip).toHaveAttribute("data-kbc-citedby-rotted", "true");

      // The rot is about the CITED FILE, not the citing docs — per-claim
      // doc links stay live regardless (16-w3-reverse-index.md §2.2: "no
      // links disabled per-row since the ambiguity is about the FILE, not
      // any one claim"). The list stays expanded from the click above (no
      // collapse-on-refetch), so both links are still queryable directly.
      const rowA = chip.locator("[data-kbc-citedby-row]").filter({ hasText: DOCLENS_CITEDBY_DOC_A_TITLE });
      await expect(rowA.locator("[data-kbc-citedby-link]")).toHaveAttribute(
        "href",
        `http://127.0.0.1:${DOCLENS_FIXTURE_PORT}/a/${DOCLENS_KB}/${DOCLENS_CITEDBY_DOC_A_PATH}`,
      );
    });
  });
});

/// DCB W3.C — `POST /api/sets/from-doc` + reading-set doc provenance + the
/// "doc changed since" banner + re-materialize. Reuses `doc1`
/// (`DOCLENS_DOC_ID`) rather than the "cited-by" describe block's `doc4`/
/// `doc5` — those two cite `DOCLENS_CITEDBY_FILE`, which that block's OWN
/// rotted-claim case deletes-then-restores via a raw filesystem write with
/// no re-index wait, racing this block's `resolve_lens` calls against the
/// watcher's re-indexing of the restore. `doc1`'s citations (`lib.rs`,
/// `resolver.rs`) are never touched by any other spec in this file, so
/// they're a stable target — and, as a bonus, `doc1` has THREE
/// `path_state: "present"` refs across two groups (ordinals 0-1 grouped
/// "Section One", ordinal 2 ungrouped), which exercises the GROUP-ORDERING
/// rule (`(group.ordinal, ref.ordinal)`, ungrouped trailing) end to end —
/// something a single-span doc never could. No PIN is needed here
/// (`resolve_lens` is called with an EXPLICIT `repo`, `Some(&repo.name)` —
/// a from-doc materialization always names its own checkout, R2/D-A's
/// "never auto-selected" pin rule notwithstanding); the "pin round-trip"
/// test earlier in this file already pinned `doc1`, which is inert here.
///
/// Declared LAST in this file (single worker, `fullyParallel: false` —
/// `playwright.config.ts`, so top-level describes run in declaration order):
/// this block's own `POST /__test__/bump-hash` call mutates `doc1`'s canned
/// `doc_hash` in the fixture's in-memory store, which must never leak
/// backward into any EARLIER test in this file that reads `doc1`'s lens
/// (the scorecard/group-nav/rev_remap tests above all assert against the
/// un-bumped payload).
///
/// Cleanup: every set this block creates is DELETED in `afterAll` — this
/// suite's OTHER specs (`sets.spec.ts`) share this SAME daemon + `fixture`
/// repo and assert the reading-sets list is exactly what THEY created
/// (a strict-mode Playwright locator on `[data-kbc-sets-row-link]`, which
/// throws if more than one row exists); leaving this block's sets behind
/// would otherwise pollute that list for every spec file that runs after
/// this one alphabetically.
test.describe("sets-from-doc (DCB-W3.C)", () => {
  let firstSetId = "";
  const createdSetIds: string[] = [];

  test.afterAll(async () => {
    for (const id of createdSetIds) {
      await fetch(`${BASE}/api/sets/${id}`, { method: "DELETE" }).catch(() => {});
    }
  });

  test("POST /api/sets/from-doc materializes present-only spans, group-ordered, and stamps provenance", async () => {
    const resp = await fetch(`${BASE}/api/sets/from-doc`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      // An EXPLICIT name (rather than letting the server default) — this
      // test isn't exercising the default-name generator, and giving it a
      // fixed name means it can never collide with the "bump the hash"
      // test's OWN re-materialize below, which DOES rely on the server's
      // minute-precision default (and targets this SAME doc) — the two
      // must never share a name, or the second `POST` 409s.
      body: JSON.stringify({
        repo: REPO_NAME,
        kb: DOCLENS_KB,
        doc: DOCLENS_DOC_ID,
        name: "W3.C direct materialize",
      }),
    });
    expect(resp.status).toBe(201);
    const view = (await resp.json()) as {
      id: string;
      source_kb: string | null;
      source_doc_id: string | null;
      source_doc_path: string | null;
      source_doc_hash: string | null;
      spans: Array<{ path: string; line_start?: number; line_end?: number; note?: string; ref?: string }>;
    };
    // DCB-W3.C.R Minor 4 — pushed IMMEDIATELY after parsing the response, so
    // a mid-test assertion failure below can never leak this set past
    // `afterAll`'s cleanup into `sets.spec.ts`'s own strict-mode row count.
    createdSetIds.push(view.id);

    expect(view.source_kb).toBe(DOCLENS_KB);
    expect(view.source_doc_id).toBe(DOCLENS_DOC_ID);
    expect(view.source_doc_path).toBe(DOCLENS_DOC_PATH);
    // The fixture's OWN canned `doc_hash` for doc1 (`codeRefsBodies`) —
    // asserted precisely here so the LATER "bump" test's own delta is
    // unambiguous.
    expect(view.source_doc_hash).toBe("fixturehash1");

    // Ordinals 0 (lib.rs) and 1 (resolver.rs:2) share group "Section One"
    // (group ordinal 0) and sort by their OWN ref ordinal; ordinal 2
    // (resolver.rs:9999, ungrouped) trails both despite its own ref ordinal
    // being lower than neither — the "ungrouped always last" rule. The
    // ambiguous/issue/external/symbol/absent refs (ordinals 3-8) contribute
    // NO spans at all.
    expect(view.spans).toHaveLength(3);

    expect(view.spans[0].path).toBe("lib.rs");
    expect(view.spans[0].note).toBe(`${DOCLENS_GROUP_LABEL} · lib.rs`);

    expect(view.spans[1].path).toBe("resolver.rs");
    expect(view.spans[1].note).toBe(`${DOCLENS_GROUP_LABEL} · resolver.rs`);
    // The CONFIRMED resolved line (ordinal 1's cited `resolver.rs:2`) —
    // never the doc's own hint verbatim (they happen to coincide here).
    expect(view.spans[1].line_start).toBe(2);
    // DCB-W3.C.R Blocker 1 — BOTH-or-NEITHER: a single-line citation carries
    // the SAME value on both sides, never a lone `line_start` with no
    // `line_end` (the old `resolved_line_end.or(line_hint_end)` construction
    // could produce exactly that half-range here).
    expect(view.spans[1].line_end).toBe(2);

    expect(view.spans[2].path).toBe("resolver.rs");
    // Ungrouped ⇒ falls back to the doc's own title, not a blank group.
    expect(view.spans[2].note).toBe(`${DOCLENS_DOC_TITLE} · resolver.rs`);
    // `line_state: unverifiable` (wildly-off hint) ⇒ no verified landing ⇒
    // falls back to the doc's own unverified hint (9999), never a fabricated
    // confirmed line.
    expect(view.spans[2].line_start).toBe(9999);
    expect(view.spans[2].line_end).toBe(9999);

    // `lib.rs` (ordinal 0) carries no line hint at all — BOTH sides absent,
    // never a fabricated one-sided value.
    expect(view.spans[0].line_start).toBeUndefined();
    expect(view.spans[0].line_end).toBeUndefined();

    // DCB-W3.C.R Blocker 2 — `ref` is a REAL git revspec, never the old
    // `"{repo}@{sha}[+dirty]"` label `GET /api/file?ref=` couldn't resolve:
    // a full 40-hex sha on a clean resolve, or ABSENT (never a fabricated
    // unresolvable string) when the daemon's fixture tree happens to be
    // dirty at the moment this suite runs — either way the span's own
    // `note` carries the honest provenance instead.
    for (const span of view.spans) {
      if (span.ref !== undefined) {
        expect(span.ref).toMatch(/^[0-9a-f]{40}$/);
      } else {
        expect(span.note).toContain("dirty tree");
      }
    }

    firstSetId = view.id;
  });

  test("a from-doc span link opens the cited file at the resolved line, never a 404 (DCB-W3.C.R Blocker 2)", async ({
    page,
  }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~sets/${firstSetId}`);
    // Ordinal 1 in the previous test's `view.spans` — `resolver.rs:2`, the
    // one span this block's fixture resolves to a CONFIRMED line, so the
    // assertion below can pin an exact `line=` param, not just "some file
    // rendered."
    const spanLink = page.locator('[data-kbc-set-span-link="1"]');
    await expect(spanLink).toBeVisible({ timeout: 10_000 });
    await spanLink.click();

    await expect(page).toHaveURL(/\/resolver\.rs\?(?:ref=[0-9a-f]{40}&)?line=2$/, {
      timeout: 10_000,
    });
    // The OLD fabricated `"{repo}@{sha}[+dirty]"` label 404d here
    // (`GET /api/file?ref=` → `rev_parse_single` → not found) — this is the
    // empirical proof that no longer happens.
    await expect(page.locator(".kbc-reader__hint--error")).toHaveCount(0);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 10_000 });
  });

  test("SetDetail renders no 'changed since' banner right after materialization", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~sets/${firstSetId}`);
    await expect(page.locator("[data-kbc-set-spans]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-set-spans] li")).toHaveCount(3);
    await expect(page.locator("[data-kbc-set-stale-banner]")).toHaveCount(0);
  });

  test("bumping the doc's hash renders the banner; re-materialize creates a SECOND set, never rewriting the first", async ({
    page,
  }) => {
    const before = await fetch(`${BASE}/api/sets?repo=${REPO_NAME}`);
    const beforeIds = ((await before.json()) as { sets: Array<{ id: string }> }).sets.map((s) => s.id);
    expect(beforeIds).toContain(firstSetId);

    const bump = await fetch(`http://127.0.0.1:${DOCLENS_FIXTURE_PORT}/__test__/bump-hash`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ doc: DOCLENS_DOC_ID }),
    });
    expect(bump.ok).toBe(true);

    await page.goto(`${BASE}/r/${REPO_NAME}/~sets/${firstSetId}`);
    const banner = page.locator("[data-kbc-set-stale-banner]");
    await expect(banner).toBeVisible({ timeout: 10_000 });

    await banner.locator("[data-kbc-set-rematerialize]").click();
    await expect(page).toHaveURL(new RegExp(`/~sets/(?!${firstSetId}$)[^/]+$`), { timeout: 10_000 });
    // Land on the NEW set's own detail — no banner (its own provenance is
    // freshly stamped against the bumped hash).
    await expect(page.locator("[data-kbc-set-spans]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-set-stale-banner]")).toHaveCount(0);

    const secondSetId = new URL(page.url()).pathname.split("/").pop() as string;
    createdSetIds.push(secondSetId);

    const after = await fetch(`${BASE}/api/sets?repo=${REPO_NAME}`);
    const afterIds = ((await after.json()) as { sets: Array<{ id: string }> }).sets.map((s) => s.id);
    // The base plan's own "never rewrites" contract: the FIRST set still
    // exists, unmodified, alongside a brand-new second one.
    expect(afterIds).toContain(firstSetId);
    expect(afterIds).toContain(secondSetId);
    expect(afterIds.length).toBe(beforeIds.length + 1);
  });

  // A DIFFERENT doc from the rest of this block (`doc2`, the rev_remap
  // demo — 2 present `path_line` refs against `DOCLENS_REMAP_FILE`, both
  // real/committed) rather than `doc1`: the button never passes an
  // explicit `name` (same as `rematerialize()` above — R26b's own
  // Scorecard doc), so its server-generated default name embeds `doc2`'s
  // OWN title ("Rev-remap demo"), which can never collide with the "bump
  // the hash" test's own doc1-titled re-materialize above regardless of
  // which wall-clock minute either test lands in.
  test("the Scorecard's 'Save as set' button materializes the currently open doc-lens", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~lens/${DOCLENS_KB}/${DOCLENS_REMAP_DOC_ID}`);
    const saveBtn = page.locator("[data-kbc-lens-save-set]");
    await expect(saveBtn).toBeVisible({ timeout: 10_000 });
    await expect(saveBtn).toBeEnabled({ timeout: 10_000 });

    const before = await fetch(`${BASE}/api/sets?repo=${REPO_NAME}`);
    const beforeCount = ((await before.json()) as { sets: unknown[] }).sets.length;

    await saveBtn.click();
    await expect(page).toHaveURL(/\/~sets\/[^/]+$/, { timeout: 10_000 });
    await expect(page.locator("[data-kbc-set-spans]")).toBeVisible({ timeout: 10_000 });
    await expect(page.locator("[data-kbc-set-spans] li")).toHaveCount(2);

    createdSetIds.push(new URL(page.url()).pathname.split("/").pop() as string);

    const after = await fetch(`${BASE}/api/sets?repo=${REPO_NAME}`);
    const afterCount = ((await after.json()) as { sets: unknown[] }).sets.length;
    expect(afterCount).toBe(beforeCount + 1);
  });

  // DCB-W3.C.R Blocker 1 — the exact operation the bug broke: a from-doc
  // set's own spans, PATCHed straight back (`SetDetail.tsx`'s `reorder`,
  // a FULL `spans` replacement) through `validate_span`'s both-or-neither
  // invariant. Pre-fix, `resolver.rs:2`'s half-range shape
  // (`line_start: 2` with NO `line_end`) 400d on this exact round-trip —
  // a from-doc set was uneditable. Declared LAST in this block (after every
  // other test that reads `firstSetId`'s ordinal-keyed span links) since a
  // successful reorder permanently changes the stored order.
  test("reordering a from-doc set's spans round-trips without an error toast", async ({ page }) => {
    await page.goto(`${BASE}/r/${REPO_NAME}/~sets/${firstSetId}`);
    const rows = page.locator("[data-kbc-set-spans] li");
    await expect(rows).toHaveCount(3, { timeout: 10_000 });
    await expect(rows.nth(0)).toContainText("lib.rs");
    await expect(rows.nth(1)).toContainText("resolver.rs");

    await rows.nth(1).locator("[data-kbc-set-span-up]").click();

    // The swap actually landed — order changed, not silently rejected.
    await expect(rows.nth(0)).toContainText("resolver.rs", { timeout: 10_000 });
    await expect(rows.nth(1)).toContainText("lib.rs", { timeout: 10_000 });
    // No error toast — the PATCH succeeded rather than 400ing on a
    // structurally-invalid span shape.
    await expect(page.locator('[data-kbc-toast="err"]')).toHaveCount(0);
  });
});
