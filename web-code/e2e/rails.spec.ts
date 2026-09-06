import { expect, test, type APIRequestContext } from "@playwright/test";
import {
  RAILS_HAML_PARTIAL,
  RAILS_HAML_VIEW,
  RAILS_MODEL_FILE,
  RAILS_ORPHAN_VIEW,
} from "./fixture-repo";
import { BASE, REPO_NAME } from "./helpers";

/// V72-I2 — `~rails`, the `rails/1` dashboard, against a LIVE daemon.
///
/// This is also `rails/1`'s first live browser exercise: V72-I1 shipped the
/// wire with Rust-side goldens only. Four things are checked, and each maps
/// to a rule the surface is supposed to keep:
///
///   1. the PASSPORT renders, with the daemon's own per-noun counts;
///   2. a card opens the READER at the row's address (the Location Contract's
///      one door);
///   3. the ORPHAN report renders a lane with its witness and a real row;
///   4. the HAML view is in the index and its partial is NOT an orphan —
///      the browser-side half of `rails_route.rs`'s
///      `the_rails_lens_reads_haml_through_the_real_ingest_path`.
///
/// `rails/1` is derived from lanes that land AFTER the file count settles
/// (symbols, `rails_edges`, `entity_defs`), so every test waits on the API
/// for a settled index first — the same readiness signal `rails_route.rs`
/// uses, for the same reason (the first capture of its goldens recorded
/// `route: 0`).
interface RailsHome {
  detected: boolean;
  counts: Record<string, number>;
}

async function waitForRailsIndex(request: APIRequestContext): Promise<RailsHome> {
  const deadline = Date.now() + 90_000;
  let last: RailsHome | null = null;
  let previous = "";
  for (;;) {
    const res = await request.get(`${BASE}/api/rails/home?repo=${REPO_NAME}`);
    if (res.ok()) {
      const body = (await res.json()) as RailsHome;
      last = body;
      const key = JSON.stringify(body.counts);
      // Settled = detected, the nouns this fixture declares are all
      // non-zero, and one further identical read (so a half-written
      // pipeline can never look ready).
      const ready =
        body.detected &&
        ["model", "controller", "action", "route", "view"].every((n) => (body.counts[n] ?? 0) > 0);
      if (ready && key === previous) return body;
      previous = key;
    }
    if (Date.now() > deadline) {
      throw new Error(`the rails/1 index never settled — last: ${JSON.stringify(last)}`);
    }
    await new Promise((r) => setTimeout(r, 500));
  }
}

test.describe("~rails (rails/1)", () => {
  test("passport, a card that opens the reader, an orphan row, and the HAML view", async ({
    page,
    request,
  }) => {
    const home = await waitForRailsIndex(request);

    await page.goto(`${BASE}/r/${REPO_NAME}/~rails`);
    await expect(page.locator("[data-kbc-rails]")).toBeVisible({ timeout: 15_000 });

    // 1 — the passport, and its counts are the DAEMON's.
    await expect(page.locator("[data-kbc-rails]")).toHaveAttribute("data-kbc-rails-detected", "true");
    const passport = page.locator("[data-kbc-rails-passport]");
    await expect(passport).toBeVisible();
    await expect(passport.locator('[data-kbc-rails-passport-fact="Rails"] dd')).toHaveText("8.1.0");
    for (const noun of ["model", "controller", "route", "view"]) {
      await expect(page.locator(`[data-kbc-rails-count="${noun}"]`)).toContainText(
        String(home.counts[noun]),
      );
    }

    // 2 — the model card. Its trust class is never `exact`, and its link is
    // the ONE address the row carries.
    const modelSection = page.locator('[data-kbc-rails-section="model"]');
    const modelCard = modelSection.locator('[data-kbc-rails-card="model"]').first();
    await expect(modelCard).toBeVisible({ timeout: 20_000 });
    await expect(modelCard).toHaveAttribute("data-kbc-rails-trust", /likely|candidate/);
    await expect(modelSection.locator("[data-kbc-rails-total]").first()).toHaveText(
      String(home.counts.model),
    );

    // 3 — the views section carries BOTH the HAML template and the ERB one
    // nothing renders. `q=` is the SERVER's filter, so typing into the box
    // narrows the daemon's own page, not a client slice. Only ONE section is
    // open by default (each one costs a whole server-side join), so open it.
    await page.locator('[data-kbc-rails-section-toggle="view"]').click();
    const viewFilter = page.locator('[data-kbc-rails-filter="view"]');
    await expect(viewFilter).toBeVisible();
    await viewFilter.fill("haml");
    const hamlCard = page.locator(`[data-kbc-rails-card="view"]`).filter({
      hasText: "acme_orders/summary.html.haml",
    });
    await expect(hamlCard).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('[data-kbc-rails-page-caption="view"]')).toContainText("of");

    // 4 — the orphan report: the lane renders its own `why`, the ERB view
    // nothing renders IS listed, and the partial rendered FROM HAML is NOT.
    const lane = page.locator('[data-kbc-rails-lane="view_never_rendered"]');
    await expect(lane).toBeVisible({ timeout: 20_000 });
    await expect(lane.locator("[data-kbc-rails-lane-why]")).toContainText("render");
    await expect(lane).toContainText(RAILS_ORPHAN_VIEW);
    await expect(lane).not.toContainText(RAILS_HAML_PARTIAL);
    await expect(lane).not.toContainText(RAILS_HAML_VIEW);
    await expect(page.locator("[data-kbc-rails-orphan-caption]")).toContainText("triage queue");

    // 5 — a card opens the reader AT ITS ADDRESS.
    await viewFilter.fill("");
    const modelLink = modelCard.locator("[data-kbc-rails-open]");
    await expect(modelLink).toBeVisible();
    await modelLink.click();
    await expect(page).toHaveURL(new RegExp(`/r/${REPO_NAME}/${RAILS_MODEL_FILE.replace(/[.]/g, "\\.")}`));
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 20_000 });
  });

  test("the Schema card reads the annotaterb banner and the buffer folds it", async ({
    page,
    request,
  }) => {
    await waitForRailsIndex(request);
    await page.goto(`${BASE}/r/${REPO_NAME}/${RAILS_MODEL_FILE}`);
    await expect(page.locator(".kbc-codeview")).toBeVisible({ timeout: 20_000 });

    // The card is DERIVED from the file text the reader already has — no
    // daemon fact, and the caption says so.
    const schema = page.locator("[data-kbc-schema]");
    await expect(schema).toBeVisible({ timeout: 20_000 });
    await expect(schema.locator("[data-kbc-schema-table]")).toHaveText("acme_orders");
    await expect(schema.locator('[data-kbc-schema-col="state"]')).toContainText("string");
    await expect(schema.locator("[data-kbc-schema-caption]")).toContainText("not a daemon fact");

    // The banner is folded in the buffer by default (the pref is an
    // opt-OUT), and the placeholder names what it replaced.
    const placeholder = page.locator("[data-kbc-schema-fold-widget]");
    await expect(placeholder).toBeVisible();
    await expect(placeholder).toContainText("== Schema Information");
    await expect(placeholder).toContainText("acme_orders");

    // The card's button is the persisted opt-OUT (it applies to every
    // annotated model, not just this file) — one click and the banner is
    // back in the buffer.
    const foldToggle = schema.locator("[data-kbc-schema-fold]");
    await expect(foldToggle).toHaveAttribute("aria-pressed", "true");
    await foldToggle.click();
    await expect(foldToggle).toHaveAttribute("aria-pressed", "false");
    await expect(placeholder).toHaveCount(0);
    await expect(page.locator(".kbc-codeview")).toContainText("Table name: acme_orders");
  });
});
