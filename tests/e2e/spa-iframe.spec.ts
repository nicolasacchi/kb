import { test, expect } from "@playwright/test";
import { PORT } from "./helpers";

// Open a detail view via the SPA. The iframe inside resolves
// http://<id>.artifacts.localhost:port/ — the daemon's content-hash
// fallback maps the id to the file path. Sanity-checks the SPA's full
// click-through flow rather than the bare-iframe sandbox (which the
// existing iframe.spec.ts covers).
test.describe("spa detail view", () => {
  function port(): number {
    return PORT;
  }

  test("clicking a card navigates to /a/{kb}/{path} and the iframe loads", async ({
    page,
  }) => {
    await page.goto(`http://127.0.0.1:${port()}/`);
    const card = page.getByRole("link", {
      name: /Visualizing the Borrow Checker/,
    });
    await expect(card).toBeVisible();
    await card.click();

    // Track U — URL pattern: /a/canon/<source-relative path> (ends .html).
    await expect(page).toHaveURL(/\/a\/canon\/[^/]+\.html(\?|$)/);

    // Detail chrome (ContextBar) shows back nav + breadcrumb + the icon-only
    // fullscreen action (U1 — replaced the old ↗ open-in-tab primary slot).
    const ctxbar = page.getByRole("navigation", { name: "artifact context" });
    await expect(ctxbar).toBeVisible();
    await expect(ctxbar.locator('[data-kb-act="fullscreen"]')).toBeVisible();

    // Iframe loads — wait for the artifact's H1 to appear inside the frame.
    const iframe = page.frameLocator(".detail__frame");
    await expect(
      iframe.getByRole("heading", {
        name: /Borrow Checker/i,
        level: 1,
      }),
    ).toBeVisible();
  });

  test("permalink /a/{kb}/{path} loads the detail directly", async ({
    page,
    request,
  }) => {
    // Find any artifact's source-relative path via the docs list.
    const r = await request.get(
      `http://127.0.0.1:${port()}/api/kb/canon/docs?limit=1`,
    );
    expect(r.status()).toBe(200);
    const docs = await r.json();
    const rel = docs[0].source_relative as string;

    await page.goto(`http://127.0.0.1:${port()}/a/canon/${rel}`);
    await expect(
      page.getByRole("navigation", { name: "artifact context" }),
    ).toBeVisible();
    const iframe = page.frameLocator(".detail__frame");
    // Any heading inside the artifact body confirms the iframe rendered.
    await expect(iframe.locator("h1, h2").first()).toBeVisible();
  });

  test("IP-literal parent suppresses the mismatch banner (H3/P2)", async ({
    page,
  }) => {
    // verifyAgainstIdentity skips its cross-check for an IP-literal
    // parent: at 127.0.0.1 the window.location heuristic can't recover a
    // DNS suffix, so the SPA adopts the daemon's authoritative value and
    // a mismatch is expected, not a misconfiguration. Even a deliberately
    // wrong /api/identity suffix must NOT raise the banner here — the
    // pre-P2 code gave every 127.0.0.1 user a permanent red banner.
    await page.route("**/api/identity", async (route) => {
      const resp = await route.fetch();
      const body = await resp.json();
      await route.fulfill({
        json: { ...body, artifact_host_suffix: ".artifacts.wrong.example" },
      });
    });

    await page.goto(`http://127.0.0.1:${port()}/`);
    // Gallery rendered → identity has resolved and been checked.
    await expect(
      page.getByRole("link", { name: /Visualizing the Borrow Checker/ }),
    ).toBeVisible();
    // ...and no *suffix*-mismatch banner despite the wrong suffix. Match
    // the specific suffix-mismatch text (not a bare /mismatch/), so an
    // unrelated build-stamp-drift banner — which legitimately appears when
    // the daemon binary and SPA bundle were built from different commits —
    // doesn't false-fail this suppression check.
    await expect(
      page
        .getByRole("alert")
        .filter({ hasText: /artifact host suffix mismatch/ }),
    ).toHaveCount(0);
  });

  test("DNS-name parent surfaces the suffix mismatch banner (H3)", async ({
    page,
    request,
  }) => {
    // kb-host.test is mapped to the loopback daemon via
    // --host-resolver-rules (playwright.config.ts) — a non-IP-literal
    // parent, so the cross-check runs. The heuristic derives
    // `.artifacts.test` from the hostname; identity reports
    // `.artifacts.localhost`. A real mismatch the banner must surface.
    //
    // The real identity is fetched off 127.0.0.1: route.fetch() runs in
    // Node, which (unlike Chromium's --host-resolver-rules) can't
    // resolve kb-host.test — so fulfill from a pre-fetched body instead.
    const real = await (
      await request.get(`http://127.0.0.1:${port()}/api/identity`)
    ).json();
    await page.route("**/api/identity", (route) =>
      route.fulfill({
        json: { ...real, artifact_host_suffix: ".artifacts.localhost" },
      }),
    );
    const warnings: string[] = [];
    page.on("console", (m) => {
      if (m.type() === "warning") warnings.push(m.text());
    });

    await page.goto(`http://kb-host.test:${port()}/`);

    // Env-drift warnings moved from an always-visible banner onto the
    // status pill (39cec07) — open it to read the alert.
    await page
      .getByRole("button", { name: /environment warning/i })
      .click();
    const banner = page.getByRole("alert").filter({ hasText: /mismatch/ });
    await expect(banner).toBeVisible();
    await expect(banner).toContainText(/artifact host suffix mismatch/);
    // The same diagnostic also lands in the console.
    await expect
      .poll(() =>
        warnings.some((w) => /artifact host suffix mismatch/.test(w)),
      )
      .toBe(true);
  });

  test("DNS-name parent surfaces the parent_origin mismatch banner (H13)", async ({
    page,
    request,
  }) => {
    // Override /api/identity to (a) report a suffix matching the
    // heuristic so the suffix branch stays quiet, and (b) report a
    // parent_origin that disagrees with window.location.origin. Only the
    // parent-origin half of verifyAgainstIdentity should fire — the H3
    // spec above never exercised this branch. Identity is pre-fetched
    // off 127.0.0.1 (route.fetch() in Node can't resolve kb-host.test).
    const real = await (
      await request.get(`http://127.0.0.1:${port()}/api/identity`)
    ).json();
    await page.route("**/api/identity", (route) =>
      route.fulfill({
        json: {
          ...real,
          artifact_host_suffix: ".artifacts.test",
          parent_origin: "http://wrong-parent.example:9999",
        },
      }),
    );

    await page.goto(`http://kb-host.test:${port()}/`);

    // Env-drift warnings moved from an always-visible banner onto the
    // status pill (39cec07) — open it to read the alert.
    await page
      .getByRole("button", { name: /environment warning/i })
      .click();
    const banner = page.getByRole("alert").filter({ hasText: /mismatch/ });
    await expect(banner).toBeVisible();
    await expect(banner).toContainText(/parent origin mismatch/);
  });
});
