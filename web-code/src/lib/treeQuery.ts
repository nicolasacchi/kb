// V71-F1 — the SPA's half of kbc-tree/1. PURE: no React, no query client,
// no fetch, so every rule below is vitest-covered without a DOM.
//
// ## There is no grammar here, on purpose
//
// kbc-scope/1 is parsed by ONE parser, in the daemon
// (`crates/kb-code-server/src/tree/scope.rs`). kbcq/1 already pays for a
// golden-pinned TS mirror (`lib/kbcq.ts`, invariant 16a) because the search
// box needs to render its own diagnostics before a round trip; the tree box
// does NOT, so adding a second mirror here would buy nothing and cost the
// lock-step. `routeFilterBox` is therefore a SHAPE rule, not a parse: it
// decides which query PARAM the typed text goes into, and when it guesses
// wrong the daemon says so out loud (a refused scope answers with
// `scope_applied: false`, a note naming the token, and the UNSCOPED tree —
// never a silently different set). Getting this heuristic wrong costs one
// caption, never a wrong answer.

import type { TreeRow } from "../api/types";

export type TreeMode = "filter" | "highlight";

export interface TreeBoxRouting {
  /// The text goes to `?scope=` (a kbc-scope/1 expression).
  scope?: string;
  /// The text goes to `?filter=` (a fuzzy name filter, ranked by the
  /// daemon's one matcher).
  filter?: string;
}

/// One box, two destinations. Structured-looking text (`role:spec`,
/// `$generated`, `a && !b`) is a SCOPE; anything else is a fuzzy filter.
export function routeFilterBox(text: string): TreeBoxRouting {
  const t = text.trim();
  if (t === "") return {};
  const structured =
    t.startsWith("$") ||
    t.includes("&&") ||
    t.includes("||") ||
    /(^|\s)!\S/.test(t) ||
    /(^|\s)-?[a-z][a-z_]*:\S/.test(t);
  return structured ? { scope: t } : { filter: t };
}

/// The `expand=` value for a set of open DIRECTORY paths. Group keys are
/// the daemon's own (`d:<path>` for a physical directory) — the SPA never
/// invents a key shape for a projection it did not compute, which is why
/// only the physical view's keys are derivable here; every other view's
/// open groups are carried as the keys the ROWS reported.
export function expandParam(openKeys: Iterable<string>): string {
  return Array.from(new Set(openKeys)).filter(Boolean).join(",");
}

/// The physical-view group key for a directory path.
export function dirKey(path: string): string {
  return `d:${path}`;
}

/// The ancestor chain of `path` (deepest last), file itself excluded.
export function ancestorDirs(path: string): string[] {
  const segments = path.split("/");
  segments.pop();
  const out: string[] = [];
  let acc = "";
  for (const seg of segments) {
    acc = acc === "" ? seg : `${acc}/${seg}`;
    out.push(acc);
  }
  return out;
}

/// Zed's "sticky scroll" for a tree (§1.7 of the evidence report — "in a
/// 5000-file monolith, scrolling a deep subtree without ancestor context is
/// disorienting; this is cheap and nobody else does it well").
///
/// Given the FLAT row list and the index of the first row scrolled into
/// view, return the ancestors that should be pinned above it: walking
/// BACKWARDS from that row, take each row whose depth is strictly less than
/// the shallowest one taken so far. Deepest-last, so the caller renders
/// them top-down. The row at `firstVisible` is never included (it is
/// already on screen).
export function stickyAncestors(rows: readonly TreeRow[], firstVisible: number): TreeRow[] {
  const out: TreeRow[] = [];
  if (firstVisible <= 0 || firstVisible >= rows.length) return out;
  let want = rows[firstVisible].depth;
  for (let i = firstVisible - 1; i >= 0 && want > 0; i--) {
    const r = rows[i];
    if (r.depth < want) {
      out.push(r);
      want = r.depth;
    }
  }
  return out.reverse();
}

/// `]c`/`[c` and `]a`/`[a`: the next row in `dir` from `from` whose facts
/// carry the named lane. Returns `-1` when there is none — the caller
/// toasts "no more changed rows" rather than wrapping silently onto a row
/// the operator has already seen.
export function nextRowWith(
  rows: readonly TreeRow[],
  from: number,
  dir: 1 | -1,
  lane: "change" | "annot",
): number {
  const has = (r: TreeRow): boolean =>
    lane === "change"
      ? r.kind === "file" && !!r.facts?.git
      : r.kind === "file" && (r.facts?.annot ?? 0) > 0;
  for (let i = from + dir; i >= 0 && i < rows.length; i += dir) {
    if (has(rows[i])) return i;
  }
  return -1;
}

/// The `kb-code` command that reproduces what the tree is showing right
/// now. The evidence report's §2.6 rule — "Every action prints (and offers
/// to copy) its CLI equivalent … the UI teaches the CLI" — applied to the
/// view itself, not just to the selection actions.
export function treeCli(args: {
  repo: string;
  view: string;
  scope?: string;
  filter?: string;
  mode?: TreeMode;
  decorate?: string[];
  daemon?: string;
}): string {
  const parts = ["kb-code", "tree", "--repo", shellQuote(args.repo)];
  parts.push("--daemon", args.daemon ?? "http://127.0.0.1:4747");
  parts.push("--view", args.view);
  if (args.scope) parts.push("--scope", shellQuote(args.scope));
  if (args.filter) parts.push("--filter", shellQuote(args.filter));
  if (args.mode && args.mode !== "filter") parts.push("--mode", args.mode);
  if (args.decorate && args.decorate.length > 0) parts.push("--decorate", args.decorate.join(","));
  return parts.join(" ");
}

/// The `kb-code` command for one multi-select ACTION. Every action the tree
/// offers has exactly one of these, and the menu shows it before it runs —
/// so a selection the operator built by clicking is a command they can
/// paste, script, or hand to an agent.
export type SelectionAction = "set" | "board" | "scope" | "review" | "pack";

export function selectionCli(
  action: SelectionAction,
  args: { repo: string; paths: readonly string[]; daemon?: string },
): string {
  const daemon = args.daemon ?? "http://127.0.0.1:4747";
  const paths = args.paths.map(shellQuote).join(" ");
  switch (action) {
    case "set":
      return `kb-code set create <name> --repo ${shellQuote(args.repo)} --daemon ${daemon} ${args.paths
        .map((p) => `--path ${shellQuote(p)}`)
        .join(" ")}`;
    case "board":
      return `kb-code canvas create <name> --repo ${shellQuote(args.repo)} --daemon ${daemon}  # then add ${args.paths.length} card(s)`;
    case "scope":
      return `kb-code scope from-paths ${paths} --repo ${shellQuote(args.repo)} --daemon ${daemon}`;
    case "review":
      return `kb-code review start --repo ${shellQuote(args.repo)} --daemon ${daemon}  # then scope it to these ${args.paths.length} file(s)`;
    case "pack":
      return `kb-code pack --repo ${shellQuote(args.repo)} --daemon ${daemon} ${args.paths
        .map((p) => `--path ${shellQuote(p)}`)
        .join(" ")}`;
  }
}

/// Every selection action, with the honest note on the ones whose CLI is a
/// TEMPLATE rather than a command that runs as printed. An action that
/// cannot be reproduced verbatim says so — the alternative (printing a
/// command that silently does something else) is worse than printing
/// nothing.
export const SELECTION_ACTIONS: {
  id: SelectionAction;
  label: string;
  exact: boolean;
  note?: string;
}[] = [
  { id: "scope", label: "Save as scope…", exact: true },
  { id: "set", label: "Open as reading set…", exact: true },
  { id: "pack", label: "Copy for the agent", exact: true },
  {
    id: "board",
    label: "Send to canvas…",
    exact: false,
    note: "the board is created empty; adding one card per file is a second step",
  },
  {
    id: "review",
    label: "Start a review…",
    exact: false,
    note: "reviews are scoped by REF, not by a file list — the selection narrows what you read, not what the review contains",
  },
];

function shellQuote(s: string): string {
  return /^[A-Za-z0-9_./:@=-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`;
}
