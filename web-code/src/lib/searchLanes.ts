// Search-Everywhere lane vocabulary (W4.3) shared by the Omnibox overlay
// (`components/Omnibox.tsx`) and the full `/search` page
// (`routes/Search.tsx`) - mirrors `crates/kb-code-server/src/search/
// grammar.rs`'s `Lane`/`LANE_ORDER` 1:1 (same six lanes, same fixed order)
// so the SPA never needs to guess section order or invent lane labels of
// its own.

import type { LaneSection, SearchLane, TranscriptHit } from "../api/types";
import { parse as parseKbcq } from "./kbcq";
import { kbLaneState, type KbLaneState } from "./unifiedInbox";

export const LANE_ORDER: SearchLane[] = [
  "files",
  "symbols",
  "text",
  "semantic",
  "sessions",
  "transcripts",
];

export const LANE_LABELS: Record<SearchLane, string> = {
  files: "Files",
  symbols: "Symbols",
  text: "Text",
  semantic: "Semantic",
  sessions: "Sessions",
  transcripts: "Transcripts",
};

/// Defensive re-sort into the canonical lane order. `unified::run` already
/// emits `sections` in this order (see that module's doc), but a one-line
/// resort here makes the fixed contract explicit at the render boundary
/// rather than trusting wire order silently - cheap (at most six elements)
/// and idempotent, so callers may pass either raw or already-ordered
/// sections.
export function orderSections<T extends { lane: string }>(sections: T[]): T[] {
  const rank = new Map(LANE_ORDER.map((lane, i) => [lane, i]));
  return [...sections].sort(
    (a, b) =>
      (rank.get(a.lane as SearchLane) ?? LANE_ORDER.length) -
      (rank.get(b.lane as SearchLane) ?? LANE_ORDER.length),
  );
}

/// kb's own SPA - sessions are digests owned by the OPERATOR'S `kb` daemon
/// (a separate process/corpus entirely, see `search::sessions`'s module
/// doc), never kb-code's own reader. `DEFAULT_KB_SESSION_BASE` matches
/// kb-code's own `[kb_daemon] url` default (`crates/kb-code-server/src/
/// config.rs`'s `KbDaemonSection`) - the right value for local/dev use and
/// for the brief window before the boot identity fetch below resolves. The
/// REAL value arrives from `GET /api/identity`'s `kb_public_url`
/// (`KbDaemonSection::public_base()`) via `hooks/useIdentity.ts`, which
/// calls `setKbSessionBase` once at app boot: the native side-by-side
/// install never needs to (its `public_base()` already resolves to this
/// same default), but a hosted deployment's federation `url` is a
/// container hostname a browser can't load, so the daemon serves a
/// distinct browser-facing `public_url` instead (e.g. `https://kb.example.com`).
const DEFAULT_KB_SESSION_BASE = "http://127.0.0.1:4000";
let kbSessionBase = DEFAULT_KB_SESSION_BASE;

/// V76-R4f — whether the last `setKbSessionBase` call resolved to a usable
/// base. Starts `true` (the pre-V76-R4f assumption: a native side-by-side
/// install, or the brief window before the boot identity fetch lands) and
/// flips to `false` the instant an EXPLICIT empty string arrives — the
/// daemon's own "`[kb_daemon]` is disabled and nothing is configured"
/// signal (`IdentityResponse::kb_public_url`'s doc,
/// `crates/kb-code-server/src/routes.rs`). It never flips back to `true`
/// off a missing/`undefined` value — see `setKbSessionBase`'s own doc for
/// why "absent" and "empty" must stay distinguishable here.
let kbSessionAvailable = true;

/// Set by `hooks/useIdentity.ts` from the boot `GET /api/identity` response
/// - trims a trailing slash so `sessionUrl`'s own `/sessions/...` join
/// never double-slashes regardless of how the operator wrote `public_url`/
/// `url` in `kb-code.toml`.
export function setKbSessionBase(u: string | null | undefined): void {
  // `null`/`undefined`: the field is ABSENT from the response entirely (an
  // older daemon, or the boot fetch hasn't resolved yet) — keep whatever
  // state already holds rather than crashing the boot path.
  if (u == null) return;
  if (u === "") {
    // V76-R4f: an EXPLICIT empty string is meaningfully different from
    // "absent" — it is the daemon's own honest "no kb configured" answer.
    // Falling back to the local-dev default here would silently point
    // every "open in kb"/session link at a daemon that, on a hosted
    // install with `[kb_daemon]` disabled, is not the one actually
    // running — and building a request off "" directly would resolve to a
    // same-origin relative path. Neither happens: the base is left
    // untouched (irrelevant once `kbSessionAvailable` is `false`) and
    // `sessionUrl` refuses to build a link at all.
    kbSessionAvailable = false;
    return;
  }
  kbSessionAvailable = true;
  kbSessionBase = u.replace(/\/$/, "");
}

/// The kb-session lane's own degrade state — the SAME shape
/// `lib/unifiedInbox.ts`'s `kbLaneState` already gives the Inbox "From kb"
/// lane (and the shape `LaneSection.unavailable_reason` gives every search
/// lane), reused rather than re-invented. `sessionUrl` consults this
/// itself (never a same-origin-relative or stale-default href once this
/// reports `"unavailable"`); a caller that wants to render an honest
/// reason alongside a hidden link reads it directly.
export function kbSessionState(): KbLaneState {
  return kbLaneState({
    available: kbSessionAvailable,
    reason: kbSessionAvailable ? null : "disabled",
  });
}

/// `kb` (optional) scopes the deep link with `?kb=<corpus>` — kb's own
/// session view otherwise falls back to whichever corpus its SPA has
/// active, which need not be the one the session actually lives in
/// (`provenance::why::AttributionOut.kb` / `WhySession.kb`, when resolved).
/// Absent stays the pre-existing unscoped link, byte-identical.
///
/// `null` (V76-R4f) means the kb-session lane is unavailable
/// (`kbSessionState()`) — every caller must treat that as "no link", never
/// build an href off it (a stray `${sessionUrl(...)}` in a template literal
/// would otherwise silently interpolate the string `"null"`).
export function sessionUrl(sessionId: string, kb?: string): string | null {
  if (!kbSessionAvailable) return null;
  const base = `${kbSessionBase}/sessions/${encodeURIComponent(sessionId)}`;
  return kb ? `${base}?kb=${encodeURIComponent(kb)}` : base;
}

/// `TranscriptHit` (`api/types.ts`, mirroring `crates/kb-code-server/src/
/// transcripts/search.rs`) carries no file/path field today - every
/// transcript row currently opens the detail popover, never the reader.
/// This stays a NAMED seam (not an inlined `null`) so a future server
/// field extending `TranscriptHit` only needs a one-line change here, not a
/// hunt through every row-click handler.
export function transcriptReaderPath(_hit: TranscriptHit): string | null {
  return null;
}

/// The `repo:<name>` a query scopes itself to, if any — the ONE place the
/// SPA needs it: the text lane's row targets carry no `repo` field of their
/// own (`search::text`'s single-repo-per-request design, see
/// `search::unified`'s module doc), so building a `readerUrl` for a
/// text-lane hit has to recover the repo when the search isn't already
/// route-scoped to one.
///
/// V71-D1 — this used to be a hand-rolled `^repo:(.+)$` scan over the
/// whitespace tokens, documented as "a small UI-only re-implementation, not
/// a full grammar parse". kbcq/1 retires it: `lib/kbcq.ts` is the mirror of
/// the daemon's own parser, golden-pinned against it case-for-case, so the
/// SPA now answers this question with the SAME grammar the daemon runs —
/// including the cases the old regex got wrong (a quoted `"repo:x"` phrase
/// is a search TERM, not a filter; `-repo:x` is a diagnostic, not a scope).
export function extractRepoFilter(query: string): string | undefined {
  return parseKbcq(query).filters.repo ?? undefined;
}

/// Build the shareable full-page search URL (`routes/Search.tsx`) - the
/// omnibox's Enter-on-header target, and the chip row's "See all" escape
/// hatch.
export function fullSearchUrl(q: string, repo?: string): string {
  const params = new URLSearchParams();
  if (q) params.set("q", q);
  if (repo) params.set("repo", repo);
  const qs = params.toString();
  return qs ? `/search?${qs}` : "/search";
}

/// Row count for keyboard-navigation purposes - a `pending` or
/// `unavailable_reason` section shows a skeleton/muted note in the body but
/// has nothing SELECTABLE yet, so it contributes zero rows (only its
/// header stays reachable via Tab). The `text` lane's `results` is grouped
/// per FILE server-side (`TextFileResult[]`); this counts the FLATTENED
/// per-match row count `lib/searchRows.ts`'s `flattenTextRows` actually
/// renders, not the file count.
export function laneRowCount(section: LaneSection): number {
  if (section.pending || section.unavailable_reason) return 0;
  if (!Array.isArray(section.results)) return 0;
  if (section.lane === "text") {
    return section.results.reduce(
      (n: number, file: { matches?: unknown[] }) =>
        n + (Array.isArray(file.matches) ? file.matches.length : 0),
      0,
    );
  }
  return section.results.length;
}
