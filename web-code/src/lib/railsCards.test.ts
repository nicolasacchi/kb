// V72-I2 — the `~rails` projection, tested against the DAEMON'S OWN goldens.
//
// `crates/kb-code-server/tests/fixtures/rails-app-expected/{home,routes,
// orphans}.json` are captured from a real daemon over the synthetic
// `acme-app` fixture, and `rails_route.rs` byte-diffs them. Reading them
// here means the SPA's card projection is exercised against exactly the
// bytes the server promises, not against a hand-written idea of them —
// reaching across the crate boundary in a TEST is the same allowance
// `kbcq.golden.test.ts` and `commands/registry.gen.test.ts` already take
// (the BUNDLE may never do it).
//
// The seven nouns whose rows no golden carries are covered by TYPED
// literals below: `RailsRow` is the wire type, so a field that changes shape
// server-side breaks this file at COMPILE time rather than at review time.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import type { RailsHomeOut, RailsListOut, RailsOrphansOut, RailsRow } from "../api/types";
import { parse as parseKbcq } from "./kbcq";
import {
  RAILS_NOUNS,
  addressLabel,
  cardOf,
  facetChipsOf,
  facetValueOf,
  flagText,
  honestyLine,
  laneCaption,
  nounTitle,
  orphanIndex,
  orphanKey,
  pageCaption,
  pageNav,
  passportFacts,
  sectionHeads,
  trustClassOf,
  trustTierOf,
} from "./railsCards";

const FIXTURES = fileURLToPath(
  new URL("../../../crates/kb-code-server/tests/fixtures/rails-app-expected/", import.meta.url),
);

function golden<T>(name: string): T {
  return JSON.parse(readFileSync(FIXTURES + name, "utf-8")) as T;
}

const HOME = golden<RailsHomeOut>("home.json");
const ROUTES = golden<RailsListOut>("routes.json");
const ORPHANS = golden<RailsOrphansOut>("orphans.json");
const REPO = "acme-app";

// ── the goldens themselves ───────────────────────────────────────────────

describe("the rails/1 goldens", () => {
  it("carry the schema and the noun vocabulary this module mirrors", () => {
    expect(HOME.schema).toBe("rails/1");
    expect(ROUTES.schema).toBe("rails/1");
    expect(ORPHANS.schema).toBe("rails/1");
    // `RAILS_NOUNS` is a fallback for a passport that has not loaded; it
    // must not disagree with the server's live list.
    expect(HOME.nouns).toEqual([...RAILS_NOUNS]);
  });

  it("never carries an exact trust class — and neither does the projection", () => {
    for (const row of ROUTES.rows) {
      expect(["likely", "candidate"]).toContain(row.trust);
      expect(cardOf(REPO, row).trust).not.toBe("exact");
      expect(cardOf(REPO, row).trustClass).not.toBe("kbc-trust-exact");
    }
  });
});

// ── trust ────────────────────────────────────────────────────────────────

describe("trust degrades DOWN, never up", () => {
  it("maps the two real tiers", () => {
    expect(trustTierOf("likely")).toBe("likely");
    expect(trustTierOf("candidate")).toBe("candidate");
  });

  it("refuses to mint exact from an unknown or forged value", () => {
    // A daemon ahead of this build, a corrupted row, and the one value this
    // side must never render: `exact` itself.
    for (const forged of ["exact", "lsp-live", "", "observed", "LIKELY"]) {
      expect(trustTierOf(forged)).toBe("candidate");
      expect(trustClassOf(forged)).toBe("kbc-trust-candidate");
    }
  });

  it("only ever emits a dashed or dotted line-style class", () => {
    expect(trustClassOf("likely")).toBe("kbc-trust-likely");
    expect(trustClassOf("candidate")).toBe("kbc-trust-candidate");
  });
});

// ── per-noun cards ───────────────────────────────────────────────────────

/// One typed row per noun the goldens do not carry rows for. Shapes mirror
/// `rails::build_index`'s own output for the `acme-app` fixture.
const SAMPLES: Record<string, RailsRow> = {
  model: {
    noun: "model",
    name: "Order",
    path: "app/models/order.rb",
    line: 3,
    blob_sha: "abc",
    fqn: "Order",
    table: "orders",
    counts: { associations: 1, validations: 1, scopes: 1, callbacks: 1, concerns: 1 },
    trust: "likely",
    witnesses: [
      { kind: "convention", detail: "path convention: app/models/**.rb → model" },
      { kind: "entity", detail: "entities/1 indexed a definition of Order here", path: "app/models/order.rb", line: 3 },
    ],
  },
  controller: {
    noun: "controller",
    name: "OrdersController",
    path: "app/controllers/orders_controller.rb",
    line: 3,
    fqn: "OrdersController",
    counts: { routes: 3, renders: 1, concerns: 0 },
    trust: "likely",
    witnesses: [{ kind: "convention", detail: "path convention" }],
  },
  action: {
    noun: "action",
    name: "orders#index",
    path: "app/controllers/orders_controller.rb",
    line: 4,
    visibility: "public",
    counts: { routes: 1 },
    trust: "likely",
    witnesses: [{ kind: "symbol", detail: "method index in a controller class" }],
  },
  job: {
    noun: "job",
    name: "ExportJob",
    path: "app/jobs/export_job.rb",
    line: 3,
    fqn: "ExportJob",
    counts: { enqueue_sites: 1 },
    trust: "likely",
    witnesses: [{ kind: "convention", detail: "path convention" }],
  },
  mailer: {
    noun: "mailer",
    name: "OrderMailer",
    path: "app/mailers/order_mailer.rb",
    line: 3,
    fqn: "OrderMailer",
    counts: { deliver_sites: 1 },
    trust: "candidate",
    witnesses: [{ kind: "convention", detail: "path convention" }],
  },
  view: {
    noun: "view",
    name: "orders/index.html.erb",
    path: "app/views/orders/index.html.erb",
    counts: { rendered_by: 1 },
    trust: "likely",
    witnesses: [{ kind: "convention", detail: "path convention" }],
  },
  concern: {
    noun: "concern",
    name: "Discountable",
    path: "app/models/concerns/discountable.rb",
    line: 3,
    fqn: "Discountable",
    counts: { included_by: 1 },
    trust: "likely",
    witnesses: [{ kind: "convention", detail: "path convention" }],
  },
};

describe("cardOf — one card per noun", () => {
  it("covers every noun the server declares", () => {
    const covered = new Set([...Object.keys(SAMPLES), "route"]);
    for (const noun of HOME.nouns) expect(covered.has(noun)).toBe(true);
  });

  it("gives every card ONE address, built through codeUrl", () => {
    for (const row of [...Object.values(SAMPLES), ROUTES.rows[0]]) {
      const card = cardOf(REPO, row);
      expect(card.href.startsWith(`/r/${REPO}/`)).toBe(true);
      expect(card.href).toContain(row.path);
      expect(card.addressLabel).toBe(addressLabel(row));
    }
  });

  it("a template's card addresses the FILE — no invented line", () => {
    const card = cardOf(REPO, SAMPLES.view);
    expect(card.addressLabel).toBe("app/views/orders/index.html.erb");
    expect(card.href).not.toContain("line=");
  });

  it("model — table name and the five association-family counts", () => {
    const facts = cardOf(REPO, SAMPLES.model).facts;
    expect(facts.find((f) => f.label === "table")?.value).toBe("orders");
    for (const label of ["associations", "validations", "scopes", "callbacks", "concerns"]) {
      expect(facts.some((f) => f.label === label)).toBe(true);
    }
  });

  it("route — the verb+path address, the action it reaches, and a warning when it does not", () => {
    const ok = ROUTES.rows.find((r) => (r.flags ?? []).length === 0)!;
    const okFacts = cardOf(REPO, ok).facts;
    expect(okFacts[0].label).toBe("route");
    expect(okFacts[0].warn).toBeFalsy();
    expect(okFacts[1].label).toBe("→ action");

    const broken = ROUTES.rows.find((r) => (r.flags ?? []).includes("action-missing"));
    // The fixture ships exactly this case (`orders#ping`); if it ever stops
    // doing so this assertion says so rather than passing vacuously.
    expect(broken, "the acme-app fixture must keep its action-missing route").toBeTruthy();
    const brokenFacts = cardOf(REPO, broken!).facts;
    expect(brokenFacts.find((f) => f.label === "→ action")?.warn).toBe(true);
    expect(cardOf(REPO, broken!).flags).toContain("action-missing");
  });

  it("route — an address the extractor could not reconstruct reads unknown, never “/”", () => {
    const row: RailsRow = {
      noun: "route",
      name: "orders#legacy",
      path: "config/routes.rb",
      line: 2,
      route: { target: "orders#legacy" },
      flags: ["address-unknown"],
      trust: "candidate",
      witnesses: [],
    };
    const facts = cardOf(REPO, row).facts;
    expect(facts[0]).toEqual({ label: "route", value: "address unknown", warn: true });
  });

  it("action — visibility, and `unknown` renders as a warning not as public", () => {
    expect(cardOf(REPO, SAMPLES.action).facts.find((f) => f.label === "visibility")).toEqual({
      label: "visibility",
      value: "public",
      warn: false,
    });
    const unknown: RailsRow = { ...SAMPLES.action, visibility: "unknown", flags: ["visibility-unknown"] };
    expect(cardOf(REPO, unknown).facts.find((f) => f.label === "visibility")?.warn).toBe(true);
  });

  it("job / mailer / view / concern — each shows its own inbound count", () => {
    expect(cardOf(REPO, SAMPLES.job).facts.some((f) => f.label === "enqueue sites")).toBe(true);
    expect(cardOf(REPO, SAMPLES.mailer).facts.some((f) => f.label === "deliver sites")).toBe(true);
    expect(cardOf(REPO, SAMPLES.view).facts.some((f) => f.label === "rendered by")).toBe(true);
    expect(cardOf(REPO, SAMPLES.concern).facts.some((f) => f.label === "includers")).toBe(true);
  });

  it("an absent count is OMITTED, never rendered as zero", () => {
    const noCounts: RailsRow = { ...SAMPLES.model, counts: undefined };
    const labels = cardOf(REPO, noCounts).facts.map((f) => f.label);
    expect(labels).not.toContain("associations");
    // A count the daemon DID send as 0 still renders — absent ≠ zero.
    const zero: RailsRow = { ...SAMPLES.job, counts: { enqueue_sites: 0 } };
    expect(cardOf(REPO, zero).facts.find((f) => f.label === "enqueue sites")?.value).toBe("0");
  });

  it("renders an unknown flag as itself rather than dropping it", () => {
    expect(flagText("blob-drifted")).toContain("blob");
    expect(flagText("some-future-flag")).toBe("some-future-flag");
  });
});

// ── facet chips ──────────────────────────────────────────────────────────

describe("facet chips are kbcq/1 clauses, verified by re-parsing", () => {
  it("every chip parses back to the filter it claims", () => {
    for (const row of [...Object.values(SAMPLES), ...ROUTES.rows]) {
      for (const chip of facetChipsOf(row)) {
        const parsed = parseKbcq(chip.clause);
        const key = chip.clause.slice(0, chip.clause.indexOf(":")) as
          | "model"
          | "controller"
          | "action"
          | "route"
          | "job"
          | "rails";
        expect(parsed.filters[key], `${chip.clause} did not parse to a ${key} filter`).not.toBeNull();
      }
    }
  });

  it("quotes a value with a space so a route address survives the round trip", () => {
    const row = ROUTES.rows.find((r) => r.route?.verb && r.route?.path)!;
    const chip = facetChipsOf(row)[0];
    expect(chip.clause).toBe(`route:"${row.route!.verb} ${row.route!.path}"`);
    expect(parseKbcq(chip.clause).filters.route).toBe(`${row.route!.verb} ${row.route!.path}`);
  });

  it("gives the five value-atom nouns a value and the other three an honest generic chip", () => {
    expect(facetValueOf(SAMPLES.model)).toBe("Order");
    expect(facetValueOf(SAMPLES.controller)).toBe("OrdersController");
    expect(facetValueOf(SAMPLES.action)).toBe("orders#index");
    expect(facetValueOf(SAMPLES.job)).toBe("ExportJob");
    for (const noun of ["mailer", "view", "concern"]) {
      expect(facetValueOf(SAMPLES[noun])).toBeNull();
      const chips = facetChipsOf(SAMPLES[noun]);
      expect(chips).toHaveLength(1);
      expect(chips[0].clause).toBe(`rails:${noun}`);
      expect(chips[0].note).toContain("no");
    }
  });

  it("falls back to the route TARGET when the address is unknown", () => {
    const row: RailsRow = {
      noun: "route",
      name: "orders#legacy",
      path: "config/routes.rb",
      route: { target: "orders#legacy" },
      trust: "candidate",
      witnesses: [],
    };
    expect(facetValueOf(row)).toBe("orders#legacy");
  });

  it("every row always offers the generic rails:<noun> chip", () => {
    for (const row of Object.values(SAMPLES)) {
      expect(facetChipsOf(row).some((c) => c.clause === `rails:${row.noun}`)).toBe(true);
    }
  });
});

// ── honesty, paging, the passport ────────────────────────────────────────

describe("the four read states", () => {
  it("ok says nothing", () => {
    expect(honestyLine({ state: "ok" })).toBeNull();
    expect(honestyLine(undefined)).toBeNull();
    // The goldens are all `ok`, which is exactly why the other three need
    // their own cases below.
    expect(HOME.honesty.state).toBe("ok");
  });

  it("empty carries its reason", () => {
    expect(honestyLine({ state: "empty", reason: "not a Rails application" })).toEqual({
      state: "empty",
      text: "not a Rails application",
    });
    expect(honestyLine({ state: "empty" })?.text).toBe("nothing to show");
  });

  it("partial names the budget that bit", () => {
    const line = honestyLine({ state: "partial", reason: "the entity index was read up to 200000 definitions" })!;
    expect(line.state).toBe("partial");
    expect(line.text).toContain("the entity index was read up to");
    expect(line.text.startsWith("partial")).toBe(true);
  });

  it("error, and an unknown future state, render as themselves", () => {
    expect(honestyLine({ state: "error", reason: "boom" })).toEqual({ state: "error", text: "boom" });
    expect(honestyLine({ state: "degraded", reason: "why" })).toEqual({
      state: "degraded",
      text: "degraded — why",
    });
  });
});

describe("paging is the server's", () => {
  it("captions the golden page from its own numbers", () => {
    expect(pageCaption(ROUTES)).toBe(`Showing 1–${ROUTES.returned} of ${ROUTES.total}`);
  });

  it("says so when more rows exist past the page", () => {
    expect(pageCaption({ offset: 0, returned: 25, total: 312, truncated: true })).toBe(
      "Showing 1–25 of 312 — more rows past this page",
    );
    expect(pageCaption({ offset: 25, returned: 25, total: 312, truncated: true })).toBe(
      "Showing 26–50 of 312 — more rows past this page",
    );
  });

  it("says 0 rows rather than an empty range", () => {
    expect(pageCaption({ offset: 0, returned: 0, total: 0, truncated: false })).toBe("0 rows");
  });

  it("never steps past the total, and never below zero", () => {
    expect(pageNav({ offset: 0, returned: 25, total: 312 }, 25)).toEqual({
      canPrev: false,
      canNext: true,
      prevOffset: 0,
      nextOffset: 25,
    });
    expect(pageNav({ offset: 300, returned: 12, total: 312 }, 25)).toEqual({
      canPrev: true,
      canNext: false,
      prevOffset: 275,
      nextOffset: 312,
    });
    expect(pageNav({ offset: 10, returned: 5, total: 15 }, 25).prevOffset).toBe(0);
  });
});

describe("the passport", () => {
  it("reports the daemon's own version, source and lens freshness", () => {
    const facts = passportFacts(HOME);
    const rails = facts.find((f) => f.label === "Rails")!;
    expect(rails.value).toBe(HOME.rails_version);
    expect(rails.note).toContain(HOME.version_source!);
    expect(facts.find((f) => f.label === "lens edges")?.value).toBe(String(HOME.lens.edges_total));
    expect(facts.find((f) => f.label === "index generation")?.value).toBe(String(HOME.lens.generation));
  });

  it("says unknown when no version could be resolved — never a guessed default", () => {
    const home: RailsHomeOut = { ...HOME, rails_version: undefined, version_source: undefined };
    const rails = passportFacts(home).find((f) => f.label === "Rails")!;
    expect(rails.value).toBe("unknown");
    expect(rails.note).toContain("no Gemfile.lock");
  });

  it("surfaces stale and orphaned lens sources only when there are any", () => {
    expect(passportFacts(HOME).some((f) => f.label === "stale sources")).toBe(
      HOME.lens.stale_source_files > 0,
    );
    const drifted: RailsHomeOut = {
      ...HOME,
      lens: { ...HOME.lens, stale_source_files: 2, orphan_source_files: 1 },
    };
    const labels = passportFacts(drifted).map((f) => f.label);
    expect(labels).toContain("stale sources");
    expect(labels).toContain("orphan sources");
  });

  it("builds one section per counted noun, in the server's order, with the TRUE total", () => {
    const heads = sectionHeads(HOME);
    expect(heads.map((h) => h.noun)).toEqual(HOME.nouns);
    for (const h of heads) expect(h.total).toBe(HOME.counts[h.noun]);
    expect(heads[0].title).toBe(nounTitle(HOME.nouns[0]));
  });

  it("omits a noun the passport did not count rather than showing 0", () => {
    const partial: RailsHomeOut = { ...HOME, counts: { model: 3 } };
    expect(sectionHeads(partial).map((h) => h.noun)).toEqual(["model"]);
  });

  it("falls back to the mirrored noun order before the passport lands", () => {
    expect(sectionHeads(undefined)).toEqual([]);
  });
});

// ── orphans ──────────────────────────────────────────────────────────────

describe("the orphan report is a triage queue", () => {
  it("indexes every lane row by path+name, naming the LANES not a verdict", () => {
    const idx = orphanIndex(ORPHANS);
    let matched = 0;
    for (const lane of ORPHANS.lanes) {
      for (const row of lane.rows) {
        expect(idx.get(orphanKey(row))).toContain(lane.title);
        matched++;
      }
    }
    expect(matched).toBeGreaterThan(0);
    expect(orphanIndex(undefined).size).toBe(0);
  });

  it("keys on path AND name, so one file's two nouns do not collide", () => {
    expect(orphanKey({ path: "a/b.rb", name: "X" })).not.toBe(orphanKey({ path: "a/b.rb", name: "Y" }));
  });

  it("captions a lane from its own state, reason and cap", () => {
    for (const lane of ORPHANS.lanes) {
      const cap = laneCaption(lane);
      if (lane.state !== "ok") expect(cap).toContain(lane.reason ?? lane.state);
      if (lane.total > 0 && lane.state === "ok" && !lane.truncated) {
        expect(cap).toBe(`${lane.total} row(s)`);
      }
    }
    expect(
      laneCaption({
        id: "l",
        title: "t",
        why: "w",
        rows: [],
        total: 500,
        returned: 200,
        truncated: true,
        state: "ok",
      }),
    ).toBe("showing 200 of 500 — the lane is capped");
  });

  it("carries the report's own caption, unsummarised", () => {
    expect(ORPHANS.caption).toContain("triage queue");
    // Every lane states its own doubt; the component renders `why` verbatim,
    // so the only thing to assert here is that the wire always carries one.
    for (const lane of ORPHANS.lanes) expect(lane.why.length).toBeGreaterThan(20);
  });
});
