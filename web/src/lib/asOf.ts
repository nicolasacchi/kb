// OL TIME2 — the reader's time machine: pure helpers for `?at=`.
//
// `?at=<unix>` on the reader opens ONE artifact as it stood (the daemon serves
// the older body on the artifact origin). Everything here is a pure function
// of its arguments so the URL handling is testable without a DOM:
//   * `clampAt`            — the daemon answers any instant (the working tree
//                            wins at t >= mtime), so the CLIENT clamps a picked
//                            date to "now" rather than the server refusing it;
//   * `atFromLocalInput` / `localInputFromAt` — the `datetime-local` control's
//                            string <-> unix seconds (local time, minute grain);
//   * `artifactSrcAt`      — the iframe `src` for a historical pane;
//   * `asOfCaption`        — the one honest caption sentence.
// Resolution itself stays in `lib/memento.ts` (the SPA mirror of
// `kb_core::versions::resolve_memento`); nothing here picks a version.

import type { Memento } from "./memento";

/** Never ask for a moment that has not happened yet. */
export function clampAt(at: number, nowSec: number): number {
  return Math.min(Math.trunc(at), Math.trunc(nowSec));
}

const LOCAL_INPUT = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/;

/**
 * A `datetime-local` value (`YYYY-MM-DDTHH:mm`, the user's LOCAL time) as unix
 * seconds, clamped to `nowSec`. TOTAL: anything else (empty, malformed, an
 * impossible date like Feb 30) is `null`.
 */
export function atFromLocalInput(value: string, nowSec: number): number | null {
  const m = LOCAL_INPUT.exec(value.trim());
  if (!m) return null;
  const [y, mo, d, h, mi] = m.slice(1).map(Number);
  const dt = new Date(y, mo - 1, d, h, mi, 0, 0);
  // `new Date` rolls an impossible date over (Feb 30 -> Mar 2); reject it.
  if (
    dt.getFullYear() !== y ||
    dt.getMonth() !== mo - 1 ||
    dt.getDate() !== d ||
    dt.getHours() !== h ||
    dt.getMinutes() !== mi
  ) {
    return null;
  }
  const t = Math.floor(dt.getTime() / 1000);
  return Number.isFinite(t) ? clampAt(t, nowSec) : null;
}

/** Inverse of [`atFromLocalInput`]: unix seconds -> `YYYY-MM-DDTHH:mm` (local). */
export function localInputFromAt(at: number): string {
  const d = new Date(at * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * The historical pane's iframe `src`. Only the ENTRY document is
 * time-travelled (the daemon ignores `at` on sub-pages and assets), so a
 * sub-page keeps the plain URL. Never carries `cm=on`: anchors are
 * cssPath/snippet on the ORIGINAL DOM (invariant #6), and the daemon would
 * not inject the annotator into an old body anyway.
 */
export function artifactSrcAt(
  base: string,
  at: number | null,
  activePage: string,
): string {
  if (at === null || activePage) return base;
  return `${base}?at=${Math.trunc(at)}`;
}

/**
 * The caption on a historical render. A hit ALWAYS names the relation
 * ("nearest earlier version" unless the second matched) and the caveat that
 * relative links resolve to today's siblings (#7); a miss says so and names
 * the floor, never pretending the oldest body is the answer.
 */
export function asOfCaption(m: Memento, fmt: (ts: number) => string): string {
  if (!m.version) {
    return m.oldestTsUnix === null
      ? "no recorded versions to open as of " + fmt(m.atUnix)
      : `nothing is that old: no version before ${fmt(m.oldestTsUnix)}`;
  }
  const rel = m.exact ? "exact version" : "nearest earlier version";
  const body =
    m.version.source === "working"
      ? "today's file (nothing newer was recorded after this instant)"
      : `${rel} (${fmt(m.version.ts_unix)})`;
  return `as of ${fmt(m.atUnix)} — ${body}; relative links resolve to today's siblings`;
}

/** `YYYY-MM-DD HH:mm` in the viewer's local time (the caption's date form). */
export function fmtLocalTs(ts: number): string {
  return localInputFromAt(ts).replace("T", " ");
}
