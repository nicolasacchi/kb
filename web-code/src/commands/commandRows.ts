// V70-A5 — the command palette's row model (pure).
//
// The recon's G1: "the ⌘K box cannot run an action, open a page, or toggle a
// setting… In an IDE this is the single most-used affordance." This module is
// the answer's data half: given a query, the active scope and the live
// context, produce the ordered rows the Omnibox renders in command mode —
// including the ones that are NOT available here, with the reason why, so the
// palette can be honest instead of simply hiding half the product.
//
// It reuses `lib/speedSearch.ts`'s matcher (the same one behind the tree and
// rail filters) rather than adding a twelfth ranking function; the only
// palette-specific bit is WHAT gets matched: title, `aka` synonyms, and the
// id, joined so one pass covers all three ("viewed" finds
// `diff.toggle-viewed`, and so does "seen").

import { matchSpeedSearch, type MatchRange } from "../lib/speedSearch";
import { displayKey, evalWhen, type Ctx } from "./dispatch";
import {
  KBC_COMMANDS,
  KBC_SCOPES,
  type KbcCommand,
  type KbcPreset,
  type KbcScope,
} from "./registry.gen";

/// The `>` prefix puts the box in command mode. It collides with nothing in
/// `lib/prefixChips.ts`'s grammar (`@ # / ?nl ~ ~~`), which is why it — and
/// not a second global chord — is the door.
export const COMMAND_PREFIX = ">";

export interface CommandRow {
  command: KbcCommand;
  /// The key for the ACTIVE preset, right-aligned in the palette's fixed
  /// column. `null` = palette-only in this preset (never vim's key silently
  /// borrowed — presets are columns).
  key: string | null;
  /// Highlight ranges into `command.title`. Ranges always describe the
  /// RENDERED string: a row matched only through an `aka` synonym or its id
  /// highlights nothing, rather than painting characters the query never
  /// touched.
  ranges: MatchRange[];
  rank: number;
  available: boolean;
  /// Why the row is unavailable, in the operator's terms. Empty when it is.
  reason: string;
}

/// True when `q` puts the box in command mode, i.e. starts with `>`.
export function isCommandQuery(q: string): boolean {
  return q.trimStart().startsWith(COMMAND_PREFIX);
}

/// The needle inside a command-mode query (`"> go home"` → `"go home"`).
export function commandNeedle(q: string): string {
  const t = q.trimStart();
  return t.startsWith(COMMAND_PREFIX) ? t.slice(COMMAND_PREFIX.length).trim() : t.trim();
}

function scopeTitle(scope: KbcScope): string {
  return KBC_SCOPES.find((s) => s.id === scope)?.title ?? scope;
}

/// Why this row cannot run right now. One sentence, naming the missing
/// condition rather than the predicate — "not here: the disposition menu is
/// closed" beats "when: diff.menu".
function unavailableReason(c: KbcCommand, scope: KbcScope, ctx: Ctx): string {
  if (c.lifecycle !== "shipped") {
    return c.note?.split(".")[0] ?? "planned — declared, not yet wired";
  }
  if (c.scope !== scope && c.scope !== "global") {
    return `only on ${scopeTitle(c.scope)}`;
  }
  if (!evalWhen(c.when, ctx)) {
    return `needs: ${c.when}`;
  }
  return "";
}

/// Build the palette's rows. `available` rows come first, ranked by the
/// matcher; unavailable ones follow (the palette hides them behind a "show
/// unavailable (N) — why" toggle) in the same ranked order, so revealing them
/// never reshuffles the list the operator was already reading.
export function commandRows(
  query: string,
  scope: KbcScope,
  ctx: Ctx,
  preset: KbcPreset,
): CommandRow[] {
  const needle = commandNeedle(query);
  const rows: CommandRow[] = [];
  for (const command of KBC_COMMANDS) {
    // Rank on the BEST of title / synonyms / id — an operator who types
    // "seen" should find "Toggle viewed on this file" as readily as one who
    // types "viewed" — with a small per-field penalty so a title hit wins a
    // tie against a synonym, and a synonym against an id.
    const haystacks = [command.title, ...command.aka, command.id];
    let rank: number | null = null;
    for (let i = 0; i < haystacks.length; i++) {
      const m = matchSpeedSearch(haystacks[i], needle);
      if (!m) continue;
      const r = m.rank + i * 10;
      if (rank === null || r < rank) rank = r;
    }
    if (rank === null) continue;
    // Highlight the TITLE independently of what ranked the row: the ranges
    // describe the string actually on screen, so a synonym match paints
    // nothing rather than characters the query never touched.
    const titleMatch = needle === "" ? null : matchSpeedSearch(command.title, needle);
    const best = { rank, ranges: titleMatch?.ranges ?? [] };
    const reason = unavailableReason(command, scope, ctx);
    rows.push({
      command,
      key: displayKey(command, preset),
      ranges: best.ranges,
      rank: best.rank,
      available: reason === "",
      reason,
    });
  }
  rows.sort((a, b) => {
    if (a.available !== b.available) return a.available ? -1 : 1;
    if (a.rank !== b.rank) return a.rank - b.rank;
    return a.command.id.localeCompare(b.command.id);
  });
  return rows;
}

/// The two slices the palette renders. Split here (not in the component) so
/// the count in "show unavailable (N)" and the list it reveals can never
/// disagree.
export function splitRows(rows: CommandRow[]): { available: CommandRow[]; unavailable: CommandRow[] } {
  return {
    available: rows.filter((r) => r.available),
    unavailable: rows.filter((r) => !r.available),
  };
}

/// `?cmd=<id>` deep links (§P2). A link may AUTO-EXECUTE only a command that
/// reads: `mutation: none` AND `side_effect: none`. Anything else pre-fills
/// the palette instead, so a pasted URL can never write to a review or the
/// working tree on someone else's behalf.
export function deepLinkDisposition(
  id: string,
): { kind: "run"; command: KbcCommand } | { kind: "prefill"; command: KbcCommand; reason: string } | null {
  const command = KBC_COMMANDS.find((c) => c.id === id);
  if (!command) return null;
  if (command.lifecycle !== "shipped") {
    return { kind: "prefill", command, reason: "planned — not yet wired" };
  }
  if (command.mutation !== "none") {
    return { kind: "prefill", command, reason: `writes (${command.mutation}) — confirm first` };
  }
  if (command.sideEffect !== "none") {
    return { kind: "prefill", command, reason: `has a ${command.sideEffect} side effect — confirm first` };
  }
  return { kind: "run", command };
}
