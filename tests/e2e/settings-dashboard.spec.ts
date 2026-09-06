import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// S1 — Settings operator dashboard shell + Preferences tab. Asserts
// the tab strip renders all 8 tabs, hash-routing flips the active
// panel, Preferences round-trips a theme change to /api/settings, and
// the identity header reads name/version from /api/identity.
test.describe("settings dashboard (S1 — shell + Preferences)", () => {
  function port(): number {
    return PORT;
  }

  test("renders the identity header and all 8 tabs", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings`);
    // Identity header — title is the daemon name; the version badge
    // renders `v{identity.version}` from /api/identity. Assert it
    // against the daemon's OWN payload rather than a lexical shape:
    // `version` comes from `kb_server::set_build_stamp` — PF-B1: this
    // suite boots the kb-server BIN, which stamps from plain
    // `option_env!` and so reports the `0.0.0-dev` fallback in dev/CI;
    // the real `git describe --tags --match 'v[0-9]*' --always` probe
    // lives in crates/kb-buildstamp/build.rs (linked by kb-cli). Either
    // way the stamp's shape is a property of the BUILD, not of the
    // product. CI
    // checks out with `git fetch --no-tags --depth=1` (ci.yml's e2e
    // job), which grafts HEAD as a root commit, so no release tag is
    // reachable and `--always` legitimately falls back to a bare
    // commit sha ("be3b941" → badge "vbe3b941"). The old `/^v\d/` pin
    // therefore passed or failed on whether the head sha happened to
    // start with a hex digit. Comparing to the payload keeps the real
    // contract (the header reads version from /api/identity) and is
    // strictly stronger — it also catches a WRONG version rendering.
    await expect(page.locator(".settings__title")).toBeVisible();
    const identity = await (
      await page.request.get(`http://127.0.0.1:${port()}/api/identity`)
    ).json();
    expect(identity.version).toBeTruthy();
    // Stamp-shape guard, deliberately kept alongside the fidelity check
    // below. The buildstamp probe pins describe to the release series with
    // `--match 'v[0-9]*'` precisely because a side-series tag
    // (`kb-code-v3.4`) otherwise hijacks the stamp into
    // `kb-code-v3.4-30-g…` — a real incident recorded in
    // crates/kb-buildstamp/build.rs, whose ONLY detector in this repo was
    // the `^v\d` pin this test used to carry. HONEST CAVEAT (PF-B1): since
    // the daemon under test is the kb-server BIN, this payload is now the
    // constant `0.0.0-dev` fallback, so this guard can no longer catch a
    // describe-probe regression (the probe never runs here) — it survives
    // as a shape check on the fallback contract; the hijack class is
    // guarded only by kb-buildstamp's own comment + `kb --version` in the
    // wild.
    expect(
      identity.version,
      `version should stamp from a v[0-9]* release tag (or the 0.0.0-dev fallback), got "${identity.version}"`,
    ).toMatch(/^\d/);
    // `[title="kb version"]` is the version chip specifically — bare
    // `.settings__badge` also matches the host/sha/uptime/kb-count
    // chips beside it (and the per-user chips in the Users tab).
    const versionBadge = page.locator('.settings__badge[title="kb version"]');
    await expect(versionBadge).toBeVisible();
    await expect(versionBadge).toHaveText(`v${identity.version}`);

    // All eight tabs from the plan exist as <button role="tab">
    // (+ the v0.24 X4 Excluded pane).
    const labels = [
      "Overview",
      "Pipeline",
      "Traffic",
      "Errors",
      "Excluded",
      "Shares",
      "Live",
      "Admin",
      "Preferences",
    ];
    for (const label of labels) {
      await expect(page.getByRole("tab", { name: label })).toBeVisible();
    }
  });

  test("hash-routing flips the active panel + persists across reload", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings`);
    // Default — first tab (Overview) is active and renders its
    // "coming soon" stub.
    await expect(page.getByRole("tab", { name: "Overview" })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    // Click into Preferences; the URL hash flips and the panel
    // swaps to the appearance form.
    await page.getByRole("tab", { name: "Preferences" }).click();
    await expect(page).toHaveURL(/#preferences$/);
    await expect(page.locator("#theme-select")).toBeVisible();

    // Reload — Tabs reads the hash on mount, so Preferences stays.
    await page.reload();
    await expect(page.getByRole("tab", { name: "Preferences" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await expect(page.locator("#theme-select")).toBeVisible();
  });

  test("deep-link to a tab via #hash opens that tab directly", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#preferences`);
    await expect(page.getByRole("tab", { name: "Preferences" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await expect(page.locator("#theme-select")).toBeVisible();
  });

  test("Preferences still PATCHes /api/settings on theme change", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#preferences`);

    // patchSettingsDebounced fires 500ms after the last edit; wait
    // for the PATCH to land. The body carries `theme`, which is the
    // only field the spec asserts (accent/density are serialized
    // independently and not the focus of this test).
    const reqPromise = page.waitForRequest(
      (r) => r.url().endsWith("/api/settings") && r.method() === "PATCH",
    );
    await page.locator("#theme-select").selectOption("light");
    const req = await reqPromise;
    expect(req.postDataJSON()).toMatchObject({ theme: "light" });

    // Restore to avoid leaking the change to other specs.
    await page.locator("#theme-select").selectOption("dark");
  });

  test("Admin tab renders the danger banner + drop/purge/drain buttons", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#admin`);
    await expect(page.locator(".settings__danger-banner")).toBeVisible();
    // Per-kb drop + purge buttons + the daemon-wide drain button.
    await expect(page.getByRole("button", { name: /drop kb/i }).first()).toBeVisible();
    await expect(page.getByRole("button", { name: /purge history/i }).first()).toBeVisible();
    // SH.I2 — the drain button's leading glyph is now a drawn Icon.Warn
    // (aria-hidden), not a literal "⚠" character, so the accessible name is
    // just "drain"; anchor on that instead of the old glyph-prefixed string.
    await expect(page.getByRole("button", { name: /^drain$/i })).toBeVisible();
  });

  // invariant:32
  test("Admin drain button opens ConfirmModal that requires typing DRAIN", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#admin`);
    await page.getByRole("button", { name: /^drain$/i }).click();
    // Modal appears with the title + the confirm button disabled
    // until the right token is typed.
    await expect(page.locator(".confirm__title")).toContainText("Drain the daemon?");
    const go = page.locator(".confirm__go");
    await expect(go).toBeDisabled();
    await page.locator(".confirm__input").fill("DRAIN");
    await expect(go).toBeEnabled();
    // Close without confirming (we don't want to actually drain the
    // test daemon mid-suite).
    await page.locator(".confirm__cancel").click();
  });

  test("Overview tab shows KPI tiles + the canon kb card", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#overview`);
    // Four KPI tiles always render (values may show "—" until the
    // first /api/stats response lands, but the labels are static).
    const kpis = page.locator(".dash__kpi");
    await expect(kpis).toHaveCount(4);
    await expect(page.locator(".dash__kpi-label").filter({ hasText: "documents" })).toBeVisible();
    await expect(page.locator(".dash__kpi-label").filter({ hasText: "open errors" })).toBeVisible();
    // Per-kb card for `canon` (fixture corpus). Card title matches kb name.
    await expect(page.locator(".dash__card").filter({ hasText: /^canon/ })).toBeVisible();
  });

  test("Pipeline tab lists sources / runs / queries sections per kb", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#pipeline`);
    // One per-kb card per configured kb.
    const card = page.locator(".dash__kbcard", { hasText: "canon" });
    await expect(card).toBeVisible();
    // The three accordion sections always render (open by default in
    // S2). Use hasText narrow on the summary so we don't false-match
    // a row in the body.
    await expect(card.locator("summary", { hasText: "sources" })).toBeVisible();
    await expect(card.locator("summary", { hasText: /recent runs/ })).toBeVisible();
    await expect(card.locator("summary", { hasText: /recent queries/ })).toBeVisible();
  });

  test("Traffic tab renders KPI sparklines + per-route latency", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#traffic`);
    // Two KPI tiles: req/sec + storage queue. Wait for the first
    // metrics.tick (≤1s) — the per-route table renders only once
    // it has data.
    await expect(page.locator(".dash__kpi--spark")).toHaveCount(2);
    await expect(
      page.locator(".dash__kpi-label").filter({ hasText: "req / sec" }),
    ).toBeVisible();
    // Per-route latency table appears within ~2s of the first tick.
    await expect(page.locator("h3", { hasText: "per-route latency" })).toBeVisible();
  });

  test("Live tab renders the filter checklist + pause/clear actions", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#live`);
    // Filter groups render their summary even when the <details>
    // body is collapsed; pin assertions on the <summary> text so
    // they're independent of toggle state.
    await expect(page.locator(".live__group summary", { hasText: "index" })).toBeVisible();
    await expect(page.locator(".live__group summary", { hasText: "watcher" })).toBeVisible();
    await expect(page.getByRole("button", { name: /pause/i })).toBeVisible();
    await expect(page.getByRole("button", { name: /clear/i })).toBeVisible();
  });

  test("Live tab pause button toggles its label", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#live`);
    const btn = page.getByRole("button", { name: /pause/i });
    await btn.click();
    // Once paused, the label flips to "resume".
    await expect(page.getByRole("button", { name: /resume/i })).toBeVisible();
  });

  test("Pipeline reindex button POSTs /api/kb/{kb}/reindex", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#pipeline`);
    const card = page.locator(".dash__kbcard", { hasText: "canon" });
    await expect(card).toBeVisible();
    const req = page.waitForRequest(
      (r) =>
        r.method() === "POST" && r.url().endsWith("/api/kb/canon/reindex"),
    );
    await card.getByRole("button", { name: /reindex/i }).first().click();
    await req;
  });

  test("Pipeline source pause button POSTs /api/kb/{kb}/sources/{src}/pause", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#pipeline`);
    const card = page.locator(".dash__kbcard", { hasText: "canon" });
    // The source row's pause button is inside .dash__row-actions; the
    // first `pause` button on the page is the source pause (the kb
    // reindex button doesn't match /pause/i).
    const req = page.waitForRequest(
      (r) => r.method() === "POST" && /\/api\/kb\/canon\/sources\/.+\/pause$/.test(r.url()),
    );
    await card.getByRole("button", { name: /pause/i }).first().click();
    await req;
    // Resume it so the next test run starts clean.
    const resumeReq = page.waitForRequest(
      (r) => r.method() === "POST" && /\/api\/kb\/canon\/sources\/.+\/resume$/.test(r.url()),
    );
    await card.getByRole("button", { name: /resume/i }).first().click();
    await resumeReq;
  });

  test("Errors tab renders the per-kb section + 'no open errors' state", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#errors`);
    const card = page.locator(".dash__kbcard", { hasText: "canon" });
    await expect(card).toBeVisible();
    // Fresh fixture corpus has no errors — the empty-state hint
    // renders. (When errors exist, the table shows; a separate
    // integration spec would need to inject an error to exercise
    // the dismiss/apply-fix buttons.)
    await expect(card).toContainText(/no open errors/);
  });

  test("Shares tab renders the create input + empty-state per kb", async ({ page }) => {
    await page.goto(`http://127.0.0.1:${port()}/settings#shares`);
    const card = page.locator(".dash__kbcard", { hasText: "canon" });
    await expect(card).toBeVisible();
    // The create input + button are part of the per-kb card head.
    await expect(card.getByPlaceholder(/path\/to\/file/)).toBeVisible();
    await expect(card.getByRole("button", { name: /create/ })).toBeVisible();
  });
});
