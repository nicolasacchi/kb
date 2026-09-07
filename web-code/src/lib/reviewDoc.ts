// The `kbc-review/1` document, as the Review Room's Document tab needs it
// (V73-K2b, design D9/D9-a).
//
// Pure and total. Everything here is a projection of `ReviewDocOut` — no
// fetch, no DOM, no React — so the tab's whole numeric and addressing half is
// unit-testable in the `node` vitest environment, which is the only kind of
// test this SPA has (`vitest.config.ts`).
//
// Three rules the module exists to keep:
//
// **Every count comes off the wire.** `omitted[]`, `revisions`, the lint
// census and the reading order's own `source`/`caption` are RENDERED, never
// recomputed. The one number this file derives is the act/blocking FACETING
// of the findings the wire already sent, and it is derived from those rows
// alone (`findingFacets`) — it is a second VIEW of one list, never a second
// count of the same thing.
//
// **A ref becomes a card only if the daemon minted one.** `cards[]` is keyed
// by the ref body exactly as the author wrote it, and that key is the join.
// A `[[…]]` this side finds but the daemon did not resolve renders as an
// honest "not resolved" chip naming why — never as a card this browser
// invented, and never as silently-dropped text.
//
// **Trust and state are the daemon's words.** `cards::trust_for` is the ONE
// minter (kb-code-server/CLAUDE.md invariant 22(c)); this file maps its four
// states and three tiers to class names and nothing else. `exact` is
// unreachable from a carry on the server, and no arithmetic here can make it
// reachable.
import type {
  ReviewDocCard,
  ReviewDocLintOut,
  ReviewDocOut,
  ReviewFinding,
} from "../api/types";
import { classify, docBody, scanLine, type RefClass } from "./kbcRefs";
import { parseMarkdownLite, type InlineRun, type MarkdownBlock } from "./markdownLite";

/** The four states `review_doc::cards` mints, in the order a legend lists. */
export const CARD_STATES = ["pinned", "carried", "orphan", "inert"] as const;
export type CardState = (typeof CARD_STATES)[number];

/** The three tiers `minimal | standard | full`, weakest first. */
export const DOC_TIERS = ["minimal", "standard", "full"] as const;

/**
 * The closed set of named `blocks:` sections, in `review_doc::BLOCK_NAMES`
 * order — which is the READING order the document declares, not the
 * alphabetical order a `BTreeMap` happens to serialise in. Rendering the
 * wire's key order would silently reorder every document's argument.
 */
export const DOC_BLOCK_NAMES = [
  "context",
  "approach",
  "alternatives_considered",
  "tests",
  "rollout",
  "open_questions",
] as const;

/** The blocks this document carries, in declared order. */
export function orderedBlocks(blocks: Record<string, string>): { name: string; text: string }[] {
  const out: { name: string; text: string }[] = [];
  for (const name of DOC_BLOCK_NAMES) {
    const text = blocks[name];
    if (typeof text === "string" && text.trim() !== "") out.push({ name, text });
  }
  // A name the daemon accepted but this list does not know is rendered LAST
  // rather than dropped: the closed vocabulary lives on the server, and a
  // client that silently hides an unknown section would make a server-side
  // widening invisible.
  for (const name of Object.keys(blocks).sort()) {
    if ((DOC_BLOCK_NAMES as readonly string[]).includes(name)) continue;
    const text = blocks[name];
    if (typeof text === "string" && text.trim() !== "") out.push({ name, text });
  }
  return out;
}

/** A section's heading, in the author's own vocabulary. */
export function blockLabel(name: string): string {
  return name.replace(/_/g, " ");
}

/**
 * What one `[[…]]` span in the prose turned out to be, once the daemon's
 * cards are joined onto this side's parse. FOUR outcomes, and each renders
 * differently on purpose:
 *
 * - `card` — the daemon resolved it; render the live card.
 * - `malformed` — it names a kbc scheme and does not parse; render a chip
 *   with the reason (the same failure `doc/lint` reports by line).
 * - `unresolved` — a well-formed ref with no card. Either the read did not
 *   ask for cards, or the daemon's own scanner did not treat this span as a
 *   ref (it sat inside a code construct this renderer does not model). Say
 *   which, rather than inventing a card or hiding the span.
 * - `wikilink` — kb's syntax (root invariant #29); render the literal text.
 */
export type RefSpan =
  | { kind: "card"; body: string; card: ReviewDocCard }
  | { kind: "malformed"; body: string; reason: string }
  | { kind: "unresolved"; body: string; reason: string }
  | { kind: "wikilink"; body: string };

/** One inline run of a rendered document: markdown, or a ref span. */
export type DocRun = InlineRun | { kind: "ref"; span: RefSpan };

/** One block of a rendered document. */
export type DocBlock =
  | { kind: "paragraph"; runs: DocRun[] }
  | { kind: "list"; items: DocRun[][] }
  | { kind: "heading"; level: number; runs: DocRun[] }
  | { kind: "code"; lang: string | null; text: string };

/** `cards[]` indexed by the ref body — the ONE join key. */
export function cardIndex(cards: readonly ReviewDocCard[] | null | undefined): Map<string, ReviewDocCard> {
  const m = new Map<string, ReviewDocCard>();
  for (const c of cards ?? []) if (!m.has(c.ref)) m.set(c.ref, c);
  return m;
}

/**
 * Classify one `[[…]]` body against the cards the daemon sent.
 *
 * `cardsResolved` is the wire's own `cards_resolved`: when it is `false` the
 * read never asked for cards, and saying "the daemon could not resolve this"
 * would be a lie about a request nobody made.
 */
export function refSpanFor(
  body: string,
  cards: Map<string, ReviewDocCard>,
  cardsResolved: boolean,
): RefSpan {
  const cls: RefClass = classify(body);
  if (cls.kind === "wikilink") return { kind: "wikilink", body };
  if (cls.kind === "malformed") return { kind: "malformed", body, reason: cls.reason };
  const card = cards.get(cls.ref.raw);
  if (card) return { kind: "card", body, card };
  return {
    kind: "unresolved",
    body,
    reason: cardsResolved
      ? "the daemon did not mint a card for this ref — it was not scanned as prose (a fenced or otherwise quoted span)"
      : "this read did not ask for cards (`?resolve=true`)",
  };
}

/**
 * Split one markdown inline run at its `[[…]]` spans.
 *
 * `code` runs are returned untouched — a ref inside a code span is a ref
 * being TALKED ABOUT, which is `review_doc::refs`'s own rule and the reason
 * the two sides agree about which spans exist.
 */
export function splitRefRuns(
  run: InlineRun,
  cards: Map<string, ReviewDocCard>,
  cardsResolved: boolean,
): DocRun[] {
  if (run.kind === "code") return [run];
  const found = scanLine(run.text, 1);
  if (found.length === 0) return [run];
  const out: DocRun[] = [];
  const chars = Array.from(run.text);
  let at = 0;
  for (const f of found) {
    // `scanLine` counts columns in CODE POINTS (what an editor shows, and
    // what `refs::scan_line` reports), so the body's own width must be
    // measured the same way — `String.length` is UTF-16 units and would
    // slice one short per astral character, silently eating text after a
    // ref on any line containing an emoji.
    const start = f.col - 1;
    const end = start + Array.from(f.body).length + 4; // `[[` + body + `]]`
    if (start > at) out.push({ kind: run.kind, text: chars.slice(at, start).join("") });
    const span = refSpanFor(f.body, cards, cardsResolved);
    if (span.kind === "wikilink") {
      out.push({ kind: run.kind, text: `[[${f.body}]]` });
    } else {
      out.push({ kind: "ref", span });
    }
    at = end;
  }
  if (at < chars.length) out.push({ kind: run.kind, text: chars.slice(at).join("") });
  return out;
}

/**
 * Render one Markdown source into blocks whose inline runs carry ref spans.
 * Headings and fences are ON: a review document's chapters are `##` lines and
 * an agent citing a snippet writes a fence (see `markdownLite`'s options for
 * why the report summary does NOT get them).
 */
export function docBlocks(
  source: string,
  cards: Map<string, ReviewDocCard>,
  cardsResolved: boolean,
): DocBlock[] {
  const md: MarkdownBlock[] = parseMarkdownLite(source, { headings: true, fences: true });
  return md.map((b): DocBlock => {
    switch (b.kind) {
      case "code":
        return b;
      case "list":
        return {
          kind: "list",
          items: b.items.map((it) => it.flatMap((r) => splitRefRuns(r, cards, cardsResolved))),
        };
      case "heading":
        return {
          kind: "heading",
          level: b.level,
          runs: b.runs.flatMap((r) => splitRefRuns(r, cards, cardsResolved)),
        };
      default:
        return {
          kind: "paragraph",
          runs: b.runs.flatMap((r) => splitRefRuns(r, cards, cardsResolved)),
        };
    }
  });
}

/** The document's BODY — everything after the front matter. */
export function bodyOf(doc: ReviewDocOut): string {
  return docBody(doc.doc_md);
}

// --- card presentation -----------------------------------------------------

/** The state's CSS modifier. Trust rides `TrustBadge`'s own line style. */
export function cardStateClass(card: ReviewDocCard): string {
  return `kbc-refcard--${card.state}`;
}

/**
 * The short state word a badge shows. `carried` says so plainly: the
 * card's `caption` carries the how, and this side never summarises it away.
 */
export function cardStateLabel(card: ReviewDocCard): string {
  switch (card.state) {
    case "pinned":
      return "pinned";
    case "carried":
      return "carried";
    case "orphan":
      return "no honest match";
    default:
      return "inert";
  }
}

/**
 * The address a card names, formatted for a human. Never a guess: an orphan
 * has no position (the daemon deliberately does not report one), so it shows
 * the REF the author wrote instead.
 */
export function cardAddress(card: ReviewDocCard): string {
  if (!card.path) return card.ref;
  if (card.line == null) return card.path;
  return card.line_end != null && card.line_end !== card.line
    ? `${card.path}:${card.line}-${card.line_end}`
    : `${card.path}:${card.line}`;
}

/**
 * Where clicking a card goes, or `null` when nothing honest can be built.
 *
 * Four destinations, each through the EXISTING builder (root CLAUDE.md #35 —
 * this side never assembles a second URL grammar):
 *
 * - `hunk:` → the review diff at that file. NOT at the hunk: `?hunk=` takes
 *   a `kbc-hunkid/1` content address, which is computed in the BROWSER from
 *   a parsed diff (`lib/diffHunks.ts`) and is not something a card carries —
 *   naming the file is the honest maximum;
 * - `finding:` → the review's own finding permalink;
 * - anything with a resolved `path` → the reader at that line range;
 * - `gh:`/`kb:` → `null`. They are INERT by construction: kb-code never
 *   calls GitHub and does not own the kb corpus, and a bare `gh:comment/12`
 *   names no host this daemon could resolve. A fabricated link would be the
 *   one thing worse than no link.
 */
export function cardHref(
  card: ReviewDocCard,
  b: {
    codeUrl: (loc: { repo: string; path: string; line?: { start: number; end: number } | number }) => string;
    reviewDiffHref: (repo: string, id: number, file?: string, opts?: { hunk?: string }) => string;
    findingUrl: (repo: string, id: number, slug: string) => string;
  },
  repo: string,
  reviewId: number,
): string | null {
  if (card.state === "orphan" || card.state === "inert") return null;
  if (card.scheme === "finding") {
    const slug = card.ref.slice("finding:".length);
    return b.findingUrl(repo, reviewId, slug);
  }
  if (card.scheme === "hunk" && card.path) {
    return b.reviewDiffHref(repo, reviewId, card.path);
  }
  if (!card.path) return null;
  const line =
    card.line == null
      ? undefined
      : card.line_end != null && card.line_end !== card.line
        ? { start: card.line, end: card.line_end }
        : card.line;
  return b.codeUrl({ repo, path: card.path, line });
}

/**
 * The `.kbc-hl-*` spans for a card's snippet, keyed by the snippet's OWN
 * 1-based line. `null` when the daemon sent none — an unindexed blob answers
 * `null`, and a client-side highlighter would be a second, disagreeing
 * source of truth (`GET /api/file`'s own rule).
 */
export function cardHasHighlights(card: ReviewDocCard): boolean {
  return Array.isArray(card.highlights) && card.highlights.length > 0;
}

/** Every card in `cards[]`, in wire order — the rail's jump list. */
export function cardList(doc: ReviewDocOut | null | undefined): ReviewDocCard[] {
  return doc?.cards ?? [];
}

/** The per-state census of a card list, for the header strip. */
export interface CardCensus {
  total: number;
  pinned: number;
  carried: number;
  orphan: number;
  inert: number;
}

export function cardCensus(cards: readonly ReviewDocCard[]): CardCensus {
  const c: CardCensus = { total: cards.length, pinned: 0, carried: 0, orphan: 0, inert: 0 };
  for (const card of cards) {
    if (card.state === "pinned") c.pinned += 1;
    else if (card.state === "carried") c.carried += 1;
    else if (card.state === "orphan") c.orphan += 1;
    else c.inert += 1;
  }
  return c;
}

/** "12 refs · 8 pinned · 2 carried · 1 no honest match · 1 inert". */
export function cardCensusText(c: CardCensus): string {
  if (c.total === 0) return "no refs";
  const parts = [`${c.total} ref${c.total === 1 ? "" : "s"}`];
  if (c.pinned) parts.push(`${c.pinned} pinned`);
  if (c.carried) parts.push(`${c.carried} carried`);
  if (c.orphan) parts.push(`${c.orphan} no honest match`);
  if (c.inert) parts.push(`${c.inert} inert`);
  return parts.join(" · ");
}

// --- the document's own facts ----------------------------------------------

/**
 * Is this reading order the AUTHOR's or the daemon's? The wire says so
 * (`source`), and the caption beside it is the daemon's own sentence — this
 * side only decides whether to show the "derived" marker.
 */
export function readingOrderIsDerived(doc: ReviewDocOut): boolean {
  return doc.reading_order.source === "derived";
}

/**
 * A pretty label for one entry of `omitted[]`. `blocks.tests` is a named
 * SECTION; the five bare names are the optional front-matter blocks.
 */
export function omittedLabel(name: string): string {
  const named = name.startsWith("blocks.") ? name.slice("blocks.".length) : name;
  return named.replace(/_/g, " ");
}

/** `true` when the entry names a `blocks:` section rather than a top-level block. */
export function omittedIsSection(name: string): boolean {
  return name.startsWith("blocks.");
}

/**
 * The findings-v2 FACETS — a second view of the rows the wire already sent,
 * never a second count. `byAct` preserves first-seen order so two renders of
 * one document agree.
 */
export interface FindingFacets {
  total: number;
  blocking: number;
  byAct: { act: string; count: number }[];
  byCategory: { category: string; count: number }[];
}

export function findingFacets(
  findings: readonly { act?: string; blocking?: boolean; category?: string }[],
): FindingFacets {
  const acts = new Map<string, number>();
  const cats = new Map<string, number>();
  let blocking = 0;
  for (const f of findings) {
    const act = f.act ?? "issue";
    acts.set(act, (acts.get(act) ?? 0) + 1);
    if (f.category) cats.set(f.category, (cats.get(f.category) ?? 0) + 1);
    if (f.blocking) blocking += 1;
  }
  return {
    total: findings.length,
    blocking,
    byAct: [...acts].map(([act, count]) => ({ act, count })),
    byCategory: [...cats].map(([category, count]) => ({ category, count })),
  };
}

/** "3 findings · 1 blocking · 2 issue · 1 question". */
export function findingFacetText(f: FindingFacets): string {
  if (f.total === 0) return "no findings";
  const parts = [`${f.total} finding${f.total === 1 ? "" : "s"}`];
  if (f.blocking > 0) parts.push(`${f.blocking} blocking`);
  for (const a of f.byAct) parts.push(`${a.count} ${a.act}`);
  return parts.join(" · ");
}

/**
 * The `kb-code review compose` line the AGENT would run.
 *
 * D22's local-canonical ruling is unchanged: every authoring surface is
 * loopback-only, so the SPA shows the command rather than offering to run it
 * (the same CLI-parity posture `lib/searchHistory.ts` records for a saved
 * search). `--doc` names a file the reader must write; there is deliberately
 * no attempt to guess its name.
 */
export function composeCommandLine(reviewId: number, tier?: string): string {
  const t = tier && tier !== "standard" ? ` --tier ${tier}` : "";
  return `kb-code review compose ${reviewId} --doc review.md${t}`;
}

/** The `kb-code review doc` line — the read this tab renders. */
export function docCommandLine(reviewId: number, ps?: number): string {
  const p = ps == null ? "" : ` --ps ${ps}`;
  return `kb-code review doc ${reviewId}${p} --resolve`;
}

// --- lint ------------------------------------------------------------------

/** "2 errors · 1 warning" — or the honest all-clear. */
export function lintCensusText(lint: ReviewDocLintOut | null | undefined): string {
  if (!lint) return "";
  if (lint.rows.length === 0) return "lints clean";
  const parts: string[] = [];
  if (lint.errors) parts.push(`${lint.errors} error${lint.errors === 1 ? "" : "s"}`);
  if (lint.warnings) parts.push(`${lint.warnings} warning${lint.warnings === 1 ? "" : "s"}`);
  if (lint.infos) parts.push(`${lint.infos} info${lint.infos === 1 ? "" : "s"}`);
  return parts.join(" · ");
}

/** The severity's CSS modifier — one home for the three lint weights. */
export function lintSeverityClass(severity: string): string {
  return `kbc-doclint__row--${severity === "error" || severity === "warning" ? severity : "info"}`;
}

/** "did you mean …" — never an applied fix, always the author's own words. */
export function lintCandidatesText(candidates: readonly string[] | undefined): string {
  if (!candidates || candidates.length === 0) return "";
  return `did you mean ${candidates.map((c) => `[[${c}]]`).join(" or ")}?`;
}

// --- the findings a document declares --------------------------------------

/**
 * `superseded_by` as a TOMBSTONE: the row is still here, still readable, and
 * names its successor. Mirrors the MI-W2.3 soft-forget posture the crate
 * applies to `review_findings.superseded` — a re-compose that says something
 * different must not destroy what a human formed a disposition against.
 */
export function findingTombstoneText(f: { superseded?: boolean; superseded_by?: string | null }): string | null {
  if (!f.superseded && !f.superseded_by) return null;
  return f.superseded_by ? `superseded by ${f.superseded_by}` : "superseded";
}

/** The act's CSS modifier. `act` is a LABEL — it never orders anything. */
export function actChipClass(act: string | undefined): string {
  return `kbc-finding__act kbc-finding__act--${act ?? "issue"}`;
}

/** A finding's secondary refs, or `[]`. Absent ≠ empty — see the wire's doc. */
export function findingCites(f: Pick<ReviewFinding, "cites">): string[] {
  return Array.isArray(f.cites) ? f.cites : [];
}
