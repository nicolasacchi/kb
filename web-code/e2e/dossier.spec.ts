// V72-G1.2 — the entity dossier against a REAL daemon.
//
// G1.1 shipped `entity/1` with pure-builder tests and a golden; nothing had
// ever driven the route over HTTP against a live index. This spec is that
// first exercise, and it is written to fail loudly if the wire and the
// renderer disagree — the assertions below are about the DAEMON'S OWN
// numbers appearing on screen, not about a fixture the SPA could satisfy on
// its own.
//
// WHAT THE FIXTURE MAKES TRUE (see `fixture-repo.ts`'s `RUBY_*` block for the
// two shape decisions and why they were forced):
//
//   * `Shop::Order` is defined by lexical NESTING (`module Shop; class
//     Order`), so its definition block is `exact` even though the fixture is
//     flat and Zeitwerk has no `app/` to read;
//   * that same flatness makes the Zeitwerk read DEGRADE, so the response's
//     `honesty.state` is `partial` with a real caption — which is the state
//     this page most needs proven on live output, not a synthesised one;
//   * `to_s` is tree-derived (`exact`), `save` comes from
//     `ApplicationRecord` and appears ONLY under `?inherited=1`, and `Line`
//     is a nested class so the namespace tree is non-empty.

import { expect, test } from "@playwright/test";
import { BASE, REPO_NAME } from "./helpers";
import {
  RUBY_ENTITY,
  RUBY_INHERITED_MEMBER,
  RUBY_MEMBER,
  RUBY_NESTED_CHILD,
  RUBY_ORDER_FILE,
} from "./fixture-repo";

const DOSSIER_URL = `${BASE}/r/${REPO_NAME}/${RUBY_ORDER_FILE}?ent=${encodeURIComponent(RUBY_ENTITY)}`;

test.describe("the entity dossier (`?ent=`)", () => {
  test("renders the live entity/1 response in the reader shell's center", async ({ page }) => {
    await page.goto(DOSSIER_URL);

    const dossier = page.locator("[data-kbc-dossier]");
    await expect(dossier).toBeVisible({ timeout: 15_000 });

    // The shell swapped its CENTER, not its route: same URL family, and the
    // Desk reports the mode it actually mounted.
    await expect(page.locator("[data-desk-center-mode]")).toHaveAttribute(
      "data-desk-center-mode",
      "dossier",
    );
    // D1 — the dossier lands in the EXISTING main region. (The full region
    // set is `desk-landmarks.spec.ts`'s golden, which now loops this mode
    // too; this is the containment half, asserted where the surface lives.)
    const insideMain = await page.evaluate(() => {
      const main = document.querySelector('[data-region="main"]');
      const dos = document.querySelector("[data-kbc-dossier]");
      return !!main && !!dos && main.contains(dos);
    });
    expect(insideMain, "the dossier rendered outside the main region").toBe(true);

    await expect(page.locator("[data-kbc-dossier-fqn]")).toHaveText(RUBY_ENTITY);
    await expect(page.locator("[data-kbc-dossier-kind]")).toHaveAttribute(
      "data-kbc-dossier-kind",
      "class",
    );

    // Every D6 section is present, in order — the `]s`/`[s` spine.
    const sections = await page
      .locator("[data-kbc-dossier-section]")
      .evaluateAll((els) => els.map((e) => e.getAttribute("data-kbc-dossier-section")));
    expect(sections).toEqual([
      "definitions",
      "members",
      "hierarchy",
      "usages",
      "unknown-members",
      "namespace",
    ]);
  });

  test("the honesty block is a visible caption, and trust is a LINE STYLE", async ({ page }) => {
    await page.goto(DOSSIER_URL);
    await expect(page.locator("[data-kbc-dossier]")).toBeVisible({ timeout: 15_000 });

    // The flat fixture degrades the Zeitwerk read, so the daemon answers
    // `partial` — and the page must SAY so rather than rendering as if it
    // were whole.
    const honesty = page.locator("[data-kbc-dossier-honesty]");
    await expect(honesty).toBeVisible();
    await expect(honesty).toHaveAttribute("data-kbc-dossier-honesty", "partial");
    await expect(honesty).toContainText("budget");
    // The daemon's own notes are rendered verbatim, one per note.
    await expect(page.locator("[data-kbc-dossier-note]").first()).toBeVisible();
    await expect(page.locator("[data-kbc-dossier]")).toContainText("zeitwerk");

    // The metaprogramming-holes caption is the exact wording D6 asks for.
    await expect(page.locator("[data-kbc-dossier-holes-caption]")).toContainText(
      "metaprogramming holes",
    );
    await expect(page.locator("[data-kbc-dossier-mech='define_method']")).toBeVisible();

    // kbc-theme/1's Lane Budget: the trust channel is LINE STYLE, driven by
    // `--kbc-trust-style`, never hue alone. Assert the computed value, since
    // "we set a class" is not the same claim as "the browser resolved it".
    const styles = await page
      .locator("[data-kbc-dossier-section='definitions'] .kbc-trust__pill")
      .first()
      .evaluate((el) => getComputedStyle(el).getPropertyValue("--kbc-trust-style").trim());
    expect(["solid", "dashed", "dotted"]).toContain(styles);
  });

  test("a member row opens the reader at the definition site", async ({ page }) => {
    await page.goto(DOSSIER_URL);
    await expect(page.locator("[data-kbc-dossier]")).toBeVisible({ timeout: 15_000 });

    const row = page.locator(`[data-kbc-dossier-member="${RUBY_MEMBER}"]`);
    await expect(row).toBeVisible();
    await row.locator("[data-kbc-dossier-member-open]").click();

    // Leaving the dossier is an ordinary reader URL — no `?ent=`, and the
    // center swaps back. This is the assertion that `?ent=` is a MODE of the
    // one shell rather than a page you get trapped on.
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${RUBY_ORDER_FILE}\\?line=\\d+$`));
    await expect(page.locator("[data-desk-center-mode]")).toHaveAttribute(
      "data-desk-center-mode",
      "reader",
    );
    await expect(page.locator("[data-kbc-dossier]")).toHaveCount(0);
  });

  test("the inherited toggle RE-FETCHES and brings an ancestor's member with it", async ({
    page,
  }) => {
    await page.goto(DOSSIER_URL);
    await expect(page.locator("[data-kbc-dossier]")).toBeVisible({ timeout: 15_000 });

    const inherited = page.locator(`[data-kbc-dossier-member="${RUBY_INHERITED_MEMBER}"]`);
    const toggle = page.locator("[data-kbc-dossier-inherited-toggle]");

    // `save` is defined on `ApplicationRecord`, so it is absent until the
    // request asks for it — proving the toggle is a re-fetch and not a
    // client-side unhide of rows that were already delivered.
    await expect(inherited).toHaveCount(0);
    await expect(toggle).toHaveAttribute("aria-pressed", "false");

    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-pressed", "true");
    await expect(inherited).toBeVisible({ timeout: 10_000 });
    await expect(inherited).toHaveAttribute("data-kbc-dossier-member-inherited", "1");

    // And back off again — the row leaves, because the server stopped
    // sending it.
    await toggle.click();
    await expect(inherited).toHaveCount(0, { timeout: 10_000 });
  });

  test("the namespace tree lists the nested child and the tree is scoped to the entity", async ({
    page,
  }) => {
    await page.goto(DOSSIER_URL);
    await expect(page.locator("[data-kbc-dossier]")).toBeVisible({ timeout: 15_000 });

    await expect(
      page.locator(`[data-kbc-dossier-ns-open="${RUBY_NESTED_CHILD}"]`),
    ).toBeVisible();

    // The dock's tree carries the shell-imposed `ns:` scope, stated with a
    // way out (the chip is the positive counterpart to the honesty strip's
    // `scope_applied: false` refusal).
    const chip = page.locator("[data-kbc-tree-scope-chip]");
    await expect(chip).toBeVisible();
    await expect(chip).toContainText(RUBY_ENTITY);

    // Clearing it leaves the dossier entirely — one action, stated on the
    // chip, no second way out to keep in sync.
    await chip.locator("[data-kbc-tree-scope-clear]").click();
    await expect(page.locator("[data-desk-center-mode]")).toHaveAttribute(
      "data-desk-center-mode",
      "reader",
    );
  });
});
