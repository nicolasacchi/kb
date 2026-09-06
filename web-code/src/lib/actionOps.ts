// V71-E2 — the PURE half of the action menu (D5).
//
// `kbc-actions/1` is server-rendered on purpose: the menu, the drag-select
// pill, the mobile sheet and `kb-code act` must all render the SAME rows, or
// the abstraction is dead (selection-actions-menu.md risk 10). So nothing in
// the SPA composes an action — this module only turns a row the SERVER sent
// into the one thing the client is allowed to decide: where it goes.
//
// Three rules, each pinned by a test:
//
//  1. **`ActionOp` is closed, and the resolver is exhaustive over it.**
//     `resolveOp`'s `default` branch assigns to `never`, so a new variant on
//     the wire fails the SPA BUILD rather than silently doing nothing — the
//     v7.0 dead-surface defect, caught at compile time for once.
//  2. **No second URL grammar.** Every navigation goes through
//     `lib/codeUrl.ts` (root CLAUDE.md #35); the server hands over an
//     ADDRESS and never an href.
//  3. **Nothing auto-navigates.** `ActionRow.auto_navigate` is always
//     `false` on this wire and `isAutoNavigable` restates the rule as a
//     predicate the component reads, so "candidate and likely never
//     auto-navigate" (D5 risk 5) is one function rather than a habit.

import type { ActionOp, ActionRow, ActionTarget, ActionsOut } from "../api/types";
import { codeUrl, permalinkFor, symbolUrl } from "./codeUrl";

/// What the component must actually DO for a chosen row. A closed,
/// component-facing vocabulary — deliberately NOT the wire's `ActionOp`,
/// because "navigate to this href" is a client concept the server has no
/// business naming.
export type ResolvedAction =
  | { kind: "navigate"; href: string; pane: 1 | 2 }
  | { kind: "peek"; peek: string }
  | { kind: "dock"; dock: string }
  | { kind: "search"; query: string }
  | { kind: "clipboard"; text: string; label: string }
  | { kind: "compose"; surface: string }
  | { kind: "collect"; sink: string }
  /// The row named something this client cannot render (a `copy` whose
  /// value only the buffer knows, and no buffer text was supplied). An
  /// honest refusal with a reason, never a silent no-op.
  | { kind: "unavailable"; reason: string };

export interface ResolveCtx {
  repo: string;
  /// `window.location.origin`, injected so this module stays pure.
  origin: string;
  ref?: string;
  /// The selected buffer text, when the surface has one — what a
  /// `copy.snippet` needs and cannot get from the server.
  selectedText?: string;
}

/// `path:a-b` / `path:line` — the header line of a provenance snippet, and
/// the label of a permalink.
export function targetAddress(t: ActionTarget): string {
  if (t.end_line !== undefined && t.line !== undefined && t.end_line > t.line) {
    return `${t.path}:${t.line}-${t.end_line}`;
  }
  return t.line !== undefined ? `${t.path}:${t.line}` : t.path;
}

/// The citable snippet (F6): the lines, under a header naming path, blob sha
/// and range. The sha is what makes the citation survive drift — a snippet
/// with no sha is a quote with no provenance, so it is captioned rather than
/// dropped.
export function snippetWithProvenance(t: ActionTarget, text: string): string {
  const sha = t.blob_sha ? ` @${t.blob_sha}` : " (blob sha unknown)";
  return `// ${targetAddress(t)}${sha}\n${text}`;
}

export function isAutoNavigable(row: ActionRow): boolean {
  // D5 risk 5: the menu is the most likely place to present a `candidate`
  // as a fact. This route never resolves, so nothing here may jump silently.
  return row.auto_navigate === true;
}

export function resolveOp(row: ActionRow, target: ActionTarget, ctx: ResolveCtx): ResolvedAction {
  const op: ActionOp = row.op;
  switch (op.op) {
    case "open":
      return {
        kind: "navigate",
        href: codeUrl({ repo: ctx.repo, path: op.path, ref: ctx.ref, line: op.line }),
        pane: op.pane === 2 ? 2 : 1,
      };
    case "peek":
      return { kind: "peek", peek: op.kind };
    case "dock":
      return { kind: "dock", dock: op.dock };
    case "search":
      return { kind: "search", query: op.query };
    case "compose":
      return { kind: "compose", surface: op.surface };
    case "collect":
      return { kind: "collect", sink: op.sink };
    case "copy": {
      if (op.value !== undefined) {
        return { kind: "clipboard", text: op.value, label: op.what };
      }
      if (op.what === "permalink") {
        // Pinned to the blob the target was read at, when there is one —
        // GitHub's lesson, applied through THE url builder.
        const href = permalinkFor(ctx.origin, {
          repo: ctx.repo,
          path: target.path,
          ref: target.blob_sha ?? ctx.ref,
          line:
            target.end_line !== undefined && target.line !== undefined && target.end_line > target.line
              ? { start: target.line, end: target.end_line }
              : target.line,
        });
        return { kind: "clipboard", text: href, label: "permalink" };
      }
      if (op.what === "snippet") {
        if (!ctx.selectedText) {
          return {
            kind: "unavailable",
            reason: "nothing is selected — a provenance snippet needs the lines it quotes",
          };
        }
        return {
          kind: "clipboard",
          text: snippetWithProvenance(target, ctx.selectedText),
          label: "snippet",
        };
      }
      if (op.what === "sym") {
        if (!target.name) {
          return { kind: "unavailable", reason: "this target has no symbol name" };
        }
        return {
          kind: "clipboard",
          text: symbolUrl(ctx.repo, target.name, {
            fallbackPath: target.path,
            fallbackLine: target.line,
          }),
          label: "sym address",
        };
      }
      return { kind: "unavailable", reason: `no client rendering for copy:${op.what}` };
    }
    default: {
      // Exhaustiveness: a new wire variant fails the BUILD here.
      const never: never = op;
      return never;
    }
  }
}

/// The drag-select pill's rows: the TOP THREE of the same list, derived
/// (D5 — "never hand-picked"). Enabled rows only, in server order, from the
/// groups in server order. Fewer than three is fine; the pill renders what
/// there is and the "…" always opens the full menu.
export const PILL_ROWS = 3;

export function pillRows(out: ActionsOut, limit = PILL_ROWS): ActionRow[] {
  const rows: ActionRow[] = [];
  for (const g of out.groups) {
    for (const a of g.actions) {
      if (!a.enabled) continue;
      rows.push(a);
      if (rows.length >= limit) return rows;
    }
  }
  return rows;
}

/// Type-to-filter inside the menu — a plain substring over the title, the id
/// and the docstring, case-folded. Deliberately NOT a fuzzy matcher: kbcq/1's
/// rule is ONE matcher and it lives server-side (`search::matcher`), so a
/// second ranking implementation here would be exactly what V71-D1
/// deprecated `speedSearch.ts` as a matcher for.
export function filterGroups(out: ActionsOut, q: string): ActionsOut["groups"] {
  const needle = q.trim().toLowerCase();
  if (!needle) return out.groups;
  return out.groups
    .map((g) => ({
      ...g,
      actions: g.actions.filter((a) =>
        `${a.title} ${a.id} ${a.doc}`.toLowerCase().includes(needle),
      ),
    }))
    .filter((g) => g.actions.length > 0);
}

/// The flat keyboard order inside the menu — groups in server order, rows in
/// server order. One function so the arrow keys and the rendered list cannot
/// disagree (the same reason `usages2.ts` has `walkOrder`).
export function menuOrder(groups: ActionsOut["groups"]): ActionRow[] {
  return groups.flatMap((g) => g.actions);
}

/// The accessible name W3C APG asks for, and which no screen reader can
/// infer: context menus are announced as plain dropdowns (`aria-hascontext`
/// never shipped), so the menu states its target explicitly.
export function menuAccessibleName(target: ActionTarget, rowCount: number): string {
  return `Actions for ${target.kind} ${target.label}, ${rowCount} item${rowCount === 1 ? "" : "s"}`;
}
