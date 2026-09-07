// V72-I2 — the `~rails` surface's whole pure half.
//
// `routes/Rails.tsx` renders rows it did NOT compute, exactly as
// `FileTree.tsx` does for `kbc-tree/1` (web-code/CLAUDE.md, § The file tree):
// every count on screen is `rails/1`'s own (`RailsHomeOut.counts`,
// `RailsListOut.total`, a lane's `total`), and NOTHING here re-derives one
// from `rows.length`. The functions below turn a wire row into the four
// things a card needs — an ADDRESS, a fact list, facet chips and a trust
// LINE STYLE — and nothing else.
//
// Three rules this module exists to make testable:
//
//  1. **A Rails row is never drawn solid.** `rails::noun_trust`'s return type
//     has no `exact` variant, so `trustClassOf` maps anything it does not
//     recognise to `candidate` (dotted) rather than to `exact` (solid) — an
//     unknown tier degrades DOWN. `railsTrustNeverExact` is the assertion
//     that keeps it that way.
//  2. **Every number is captioned or absent.** `pageCaption`/`honestyLine`
//     render the daemon's own `total`/`truncated`/`honesty.reason`; a page
//     that shows fewer rows than the index holds says so, with the reason.
//  3. **A facet chip is a QUERY, not a filter.** Each chip is a `kbcq/1`
//     clause (`model:`, `controller:`, `action:`, `route:`, `job:`,
//     `rails:<noun>`) the results page then owns — the `~rails` page never
//     holds filter state the box cannot show (`routes/Search.tsx`'s "every
//     control WRITES the query" rule, borrowed).
import type {
  RailsHomeOut,
  RailsListOut,
  RailsOrphanLane,
  RailsOrphansOut,
  RailsRow,
} from "../api/types";
import { codeUrl } from "./codeUrl";

/// The noun vocabulary, in the server's own reading order (`rails::NOUNS`).
/// `RailsHomeOut.nouns` is the live copy this module prefers — this constant
/// is the fallback for a passport that has not loaded yet, and the order the
/// section list is built in.
export const RAILS_NOUNS: readonly string[] = [
  "model",
  "controller",
  "action",
  "route",
  "job",
  "mailer",
  "view",
  "concern",
];

/// Section headings. Plain nouns, pluralised once, here — the server sends
/// the singular and the URL segment lives in `api/client.ts`.
const NOUN_TITLE: Readonly<Record<string, string>> = {
  model: "Models",
  controller: "Controllers",
  action: "Controller actions",
  route: "Routes",
  job: "Jobs",
  mailer: "Mailers",
  view: "Views",
  concern: "Concerns",
};

export function nounTitle(noun: string): string {
  return NOUN_TITLE[noun] ?? noun;
}

/// The counts a noun's card puts on its face, in reading order, keyed by the
/// wire's own `counts` map keys (`rails::build_index`). A key the daemon did
/// not send is OMITTED, never rendered as zero — "no associations" and "this
/// build does not report associations" are different facts.
const NOUN_COUNT_KEYS: Readonly<Record<string, readonly string[]>> = {
  model: ["associations", "validations", "scopes", "callbacks", "concerns"],
  controller: ["routes", "renders", "concerns"],
  action: ["routes"],
  route: [],
  job: ["enqueue_sites"],
  mailer: ["deliver_sites"],
  view: ["rendered_by"],
  concern: ["included_by"],
};

const COUNT_LABEL: Readonly<Record<string, string>> = {
  associations: "associations",
  validations: "validations",
  scopes: "scopes",
  callbacks: "callbacks",
  concerns: "concerns",
  routes: "routes",
  renders: "renders",
  enqueue_sites: "enqueue sites",
  deliver_sites: "deliver sites",
  rendered_by: "rendered by",
  included_by: "includers",
};

// ── trust ───────────────────────────────────────────────────────────────

export type RailsTrustTier = "likely" | "candidate";

/// The wire's `trust` → the tier this side draws. `rails/1` can only send
/// `likely` or `candidate`; anything else (a daemon ahead of this build, a
/// corrupted row) degrades DOWN to `candidate`, never up. There is no
/// `exact` branch on purpose — see this module's header.
export function trustTierOf(trust: string): RailsTrustTier {
  return trust === "likely" ? "likely" : "candidate";
}

/// The CSS class carrying the LINE STYLE (`styles/tokens.css`:
/// `.kbc-trust-likely` → dashed, `.kbc-trust-candidate` → dotted). Colour is
/// reinforcement; the dash pattern is the signal.
export function trustClassOf(trust: string): string {
  return `kbc-trust-${trustTierOf(trust)}`;
}

// ── the address ─────────────────────────────────────────────────────────

/// ONE address per card, built through `lib/codeUrl.ts` (root CLAUDE.md #35 —
/// this module never assembles a path). A row always has a `path`; `line` is
/// absent for a template (a file, not a definition site), which is honest:
/// the card opens the file at the top.
export function addressOf(repo: string, row: RailsRow): string {
  return codeUrl({ repo, path: row.path, line: row.line });
}

/// The address as a human reads it — `path:line`, or just `path`.
export function addressLabel(row: RailsRow): string {
  return row.line !== undefined ? `${row.path}:${row.line}` : row.path;
}

// ── facet chips ─────────────────────────────────────────────────────────

export interface RailsFacetChip {
  /// What the chip says.
  label: string;
  /// The `kbcq/1` clause it appends to the search box.
  clause: string;
  /// Why this chip and not a narrower one — rendered as a `title`, so a
  /// generic `rails:<noun>` chip on a mailer explains itself rather than
  /// looking like a missing feature.
  note?: string;
}

/// `kbcq/1`'s own quoting rule (`kbcq.ts`'s private `quoteIfNeeded`),
/// restated here because the chip builder needs it and the parser's copy is
/// not exported. `railsCards.test.ts` re-PARSES every chip it builds rather
/// than trusting this — the same "verify the edit, never trust a substring"
/// discipline `lib/kbcqEdit.ts` states.
function quoteIfNeeded(v: string): string {
  return v.includes(" ") ? `"${v}"` : v;
}

/// The chips one row offers. Always at least the generic `rails:<noun>`;
/// plus the VALUE atom for the five nouns `kbcq/1` gives one
/// (`model`/`controller`/`action`/`route`/`job` — `search::grammar`'s
/// `FILTER_SPECS`). `mailer`/`view`/`concern` have no value atom, and the
/// generic chip's note says so instead of inventing one.
export function facetChipsOf(row: RailsRow): RailsFacetChip[] {
  const chips: RailsFacetChip[] = [];
  const value = facetValueOf(row);
  if (value !== null) {
    chips.push({
      label: `${row.noun}: ${value}`,
      clause: `${row.noun}:${quoteIfNeeded(value)}`,
    });
  }
  chips.push({
    label: `rails: ${row.noun}`,
    clause: `rails:${row.noun}`,
    note:
      value === null
        ? `kbcq/1 has no ${row.noun}: atom — this chip narrows to every ${row.noun} in the repo`
        : `every ${row.noun} in the repo`,
  });
  return chips;
}

/// The value the row's own atom takes, or `null` for a noun with no value
/// atom. `route` is addressable BOTH ways server-side (verb+path OR
/// `controller#action`); the chip prefers the address a reader just read off
/// the card, and falls back to the target when the address is unknown.
export function facetValueOf(row: RailsRow): string | null {
  switch (row.noun) {
    case "model":
    case "controller":
    case "job":
      return row.fqn ?? row.name;
    case "action":
      return row.name;
    case "route": {
      const verb = row.route?.verb;
      const path = row.route?.path;
      if (verb && path) return `${verb} ${path}`;
      const target = row.route?.target ?? "";
      return target === "" ? null : target;
    }
    default:
      return null;
  }
}

// ── the card ────────────────────────────────────────────────────────────

export interface RailsFact {
  label: string;
  value: string;
  /// A fact the daemon FLAGGED (a missing action, an unknown visibility, a
  /// drifted blob) — rendered as a warning rather than a plain line.
  warn?: boolean;
}

export interface RailsCardView {
  noun: string;
  title: string;
  /// The one address this card opens.
  href: string;
  addressLabel: string;
  trust: RailsTrustTier;
  trustClass: string;
  facts: RailsFact[];
  chips: RailsFacetChip[];
  /// The row's own `flags`, verbatim — never summarised away.
  flags: readonly string[];
  witnessCount: number;
}

/// `flags` → the sentence a reader needs. An unknown flag is rendered as
/// ITSELF (the tree's "render the note verbatim" rule) rather than dropped.
const FLAG_TEXT: Readonly<Record<string, string>> = {
  "blob-drifted": "the lens read a blob that is no longer this file's live one",
  "action-missing": "no public action of that name was found in the target controller",
  "controller-missing": "the target controller file was not found in the index",
  "address-unknown": "this edge carries no verb/path — unknown, not “/”",
  "visibility-unknown": "past this request’s visibility read budget — not assumed public",
  partial: "a partial template",
};

export function flagText(flag: string): string {
  return FLAG_TEXT[flag] ?? flag;
}

/// Project ONE wire row into the card the section renders. Pure; called per
/// render; nothing cached (the whole `rails/1` posture).
export function cardOf(repo: string, row: RailsRow): RailsCardView {
  const facts: RailsFact[] = [];
  if (row.noun === "model" && row.table !== undefined) {
    facts.push({ label: "table", value: row.table });
  }
  if (row.noun === "route" && row.route) {
    const verb = row.route.verb;
    const path = row.route.path;
    facts.push({
      label: "route",
      value: verb && path ? `${verb} ${path}` : "address unknown",
      warn: !(verb && path),
    });
    facts.push({
      label: "→ action",
      value: row.route.target === "" ? "unknown" : row.route.target,
      warn:
        (row.flags ?? []).includes("action-missing") ||
        (row.flags ?? []).includes("controller-missing"),
    });
  }
  if (row.noun === "action" && row.visibility !== undefined) {
    facts.push({
      label: "visibility",
      value: row.visibility,
      warn: row.visibility === "unknown",
    });
  }
  if (row.fqn !== undefined && row.noun !== "action") {
    facts.push({ label: "constant", value: row.fqn });
  }
  for (const key of NOUN_COUNT_KEYS[row.noun] ?? []) {
    const n = row.counts?.[key];
    // Absent ≠ zero. A key the daemon did not send is a fact this build does
    // not report, and inventing a 0 for it is the exact over-claim the
    // honesty rules forbid.
    if (n === undefined) continue;
    facts.push({ label: COUNT_LABEL[key] ?? key, value: String(n) });
  }
  return {
    noun: row.noun,
    title: row.name,
    href: addressOf(repo, row),
    addressLabel: addressLabel(row),
    trust: trustTierOf(row.trust),
    trustClass: trustClassOf(row.trust),
    facts,
    chips: facetChipsOf(row),
    flags: row.flags ?? [],
    witnessCount: row.witnesses.length,
  };
}

// ── honesty ─────────────────────────────────────────────────────────────

export interface HonestyLine {
  state: string;
  text: string;
}

/// The `honesty` block as ONE line. `ok` produces `null` — a state with
/// nothing to say should say nothing, not "ok".
export function honestyLine(h: { state: string; reason?: string } | undefined): HonestyLine | null {
  if (!h) return null;
  if (h.state === "ok") return null;
  const reason = h.reason ?? "";
  switch (h.state) {
    case "empty":
      return { state: "empty", text: reason === "" ? "nothing to show" : reason };
    case "partial":
      return {
        state: "partial",
        text:
          reason === ""
            ? "this page is partial — a budget bit and the daemon did not name it"
            : `partial — ${reason}`,
      };
    case "error":
      return { state: "error", text: reason === "" ? "error" : reason };
    default:
      // An unknown state renders as itself: a vocabulary this build does
      // not know is still a fact the reader should see.
      return { state: h.state, text: reason === "" ? h.state : `${h.state} — ${reason}` };
  }
}

/// "Showing 1–25 of 312" — built ONLY from the daemon's own `offset`/
/// `returned`/`total`. `truncated` gets its own clause, so a capped page can
/// never read as a complete one.
export function pageCaption(list: Pick<RailsListOut, "offset" | "returned" | "total" | "truncated">): string {
  if (list.total === 0) return "0 rows";
  const first = list.offset + 1;
  const last = list.offset + list.returned;
  const base = `Showing ${first}–${last} of ${list.total}`;
  return list.truncated ? `${base} — more rows past this page` : base;
}

export interface PageNav {
  canPrev: boolean;
  canNext: boolean;
  prevOffset: number;
  nextOffset: number;
}

/// Server-side paging arithmetic, kept in one place so no component does it
/// inline. `nextOffset` never runs past `total`.
export function pageNav(list: Pick<RailsListOut, "offset" | "returned" | "total">, limit: number): PageNav {
  const nextOffset = list.offset + Math.max(list.returned, 0);
  return {
    canPrev: list.offset > 0,
    canNext: nextOffset < list.total,
    prevOffset: Math.max(0, list.offset - limit),
    nextOffset,
  };
}

// ── the passport ────────────────────────────────────────────────────────

export interface PassportFact {
  label: string;
  value: string;
  note?: string;
}

/// The passport's fact list — framework detection, version, lens freshness,
/// Zeitwerk. Every value is the daemon's; `rails_version` absent renders as
/// "unknown", which is what the wire means by omitting it.
export function passportFacts(home: RailsHomeOut): PassportFact[] {
  const facts: PassportFact[] = [
    {
      label: "Rails",
      value: home.rails_version ?? "unknown",
      note:
        home.version_source !== undefined
          ? `resolved from ${home.version_source}`
          : "no Gemfile.lock or Gemfile declared a version",
    },
    {
      label: "lens edges",
      value: String(home.lens.edges_total),
      note: `${home.lens.grammar_version} over ${home.lens.source_files} source file(s)`,
    },
  ];
  if (home.lens.stale_source_files > 0) {
    facts.push({
      label: "stale sources",
      value: String(home.lens.stale_source_files),
      note: "the lens has not caught up with these files’ live blobs",
    });
  }
  if (home.lens.orphan_source_files > 0) {
    facts.push({
      label: "orphan sources",
      value: String(home.lens.orphan_source_files),
      note: "the lens produced edges for paths the mirror index no longer has",
    });
  }
  facts.push({
    label: "Zeitwerk",
    value: home.zeitwerk.state,
    note: home.zeitwerk.reason,
  });
  facts.push({ label: "index generation", value: String(home.lens.generation) });
  return facts;
}

/// The section list, in the passport's own noun order, each with the TRUE
/// total the passport reports. A noun the passport did not count is omitted
/// rather than shown as 0.
export interface RailsSectionHead {
  noun: string;
  title: string;
  total: number;
}

export function sectionHeads(home: RailsHomeOut | undefined): RailsSectionHead[] {
  const order = home?.nouns?.length ? home.nouns : RAILS_NOUNS;
  const out: RailsSectionHead[] = [];
  for (const noun of order) {
    const total = home?.counts?.[noun];
    if (total === undefined) continue;
    out.push({ noun, title: nounTitle(noun), total });
  }
  return out;
}

// ── orphans ─────────────────────────────────────────────────────────────

/// The key a row and an orphan row are matched on. Path + name, because two
/// nouns can share a path (a controller and its actions) and two paths can
/// share a name (`_row.html.erb` under two view dirs).
export function orphanKey(row: { name: string; path: string }): string {
  return `${row.path} ${row.name}`;
}

/// key → the lane TITLES that listed it. A Map (not a Set) so the badge can
/// say WHICH lane flagged the row — "orphan" alone is the verdict this
/// report refuses to be.
export function orphanIndex(report: RailsOrphansOut | undefined): Map<string, string[]> {
  const out = new Map<string, string[]>();
  if (!report) return out;
  for (const lane of report.lanes) {
    for (const row of lane.rows) {
      const k = orphanKey(row);
      const cur = out.get(k);
      if (cur) cur.push(lane.title);
      else out.set(k, [lane.title]);
    }
  }
  return out;
}

/// A lane's own caption: its `state`/`reason` plus the truncation fact.
/// `why` is rendered separately and VERBATIM by the component — never folded
/// in here, so it cannot be summarised away.
export function laneCaption(lane: RailsOrphanLane): string {
  const parts: string[] = [];
  if (lane.state !== "ok") {
    parts.push(lane.reason ?? lane.state);
  }
  if (lane.truncated) {
    parts.push(`showing ${lane.returned} of ${lane.total} — the lane is capped`);
  } else if (lane.total > 0) {
    parts.push(`${lane.total} row(s)`);
  }
  return parts.join(" · ");
}
