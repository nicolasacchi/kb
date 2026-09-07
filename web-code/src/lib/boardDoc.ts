// `kbc-canvas/1` — composing the NEXT board document (V74-L2, D10).
//
// There is no partial-patch route. `POST /api/boards/apply` is an idempotent
// upsert of the WHOLE document by slug, so "add this card to that board" means
// "send the board that is on screen, plus one node". This module is that
// composition, and it is pure: it reads a `BoardOut` and an action target and
// returns JSON. It performs no fetch, mints no id the server would reject, and
// — the rule this file exists for — **emits no coordinates**.
//
// **Coordinates.** `boards::lint`'s `coordinates` rule REFUSES `x`/`y`/`w`/`h`/
// `width`/`height`/`position`/`left`/`top`/`coords` anywhere outside the `pins`
// map, scanning the RAW JSON before the typed parse, precisely so an LLM cannot
// learn to emit them. This SPA is held to the same rule and checks itself
// against it (`coordinateKeysOutsidePins`) before sending, so a bug here is a
// refusal in the browser with the offending path named, not a 400 from the
// daemon that a user reads as "the server is broken".
//
// **Round-tripping a node is lossy ON PURPOSE.** `BoardNode` carries the
// daemon's resolution (`state`, `reason`, `address`, `code`, `query_card`,
// `thread`) beside the reference the author wrote. Only the AUTHORED half may
// go back — `NodeIn` is `deny_unknown_fields`, and re-sending a computed field
// would be this SPA claiming a fact it did not establish. `nodeToInput` picks
// exactly the fields the lint's own per-kind table names, and
// `boardDoc.test.ts` walks that table.
//
// **Status.** `apply` may only ever write `pending` or `draft` (D21). A CHANGED
// apply against an ACCEPTED board resets it — a human accepted a specific
// board, not a slug — so `composeAdd` reports that in `statusWillReset` and the
// dialog says so BEFORE the click, rather than the response explaining it
// after.

import type { ActionTarget, BoardEdge, BoardNode, BoardOut, BoardPin } from "../api/types";

export const BOARD_SCHEMA = "kbc-canvas/1";

/// `boards::APPLIABLE_STATUSES`.
export const APPLIABLE_STATUSES = ["pending", "draft"] as const;

/// `boards::lint::COORDINATE_KEYS`, verbatim.
export const COORDINATE_KEYS = [
  "x",
  "y",
  "w",
  "h",
  "width",
  "height",
  "position",
  "left",
  "top",
  "coords",
] as const;

/// `boards::MAX_ID_LEN`.
export const MAX_ID_LEN = 64;

/// `boards::is_valid_id` — 1..=64 chars of `[a-z0-9_-]`, first alphanumeric.
export function isValidBoardId(s: string): boolean {
  if (s.length === 0 || s.length > MAX_ID_LEN) return false;
  if (!/^[a-z0-9]/.test(s)) return false;
  return /^[a-z0-9_-]+$/.test(s);
}

/// Every coordinate-shaped key in `doc` that is NOT under `pins`, as dotted
/// paths. Empty means the document is clean.
///
/// A local mirror of the daemon's raw pre-pass, run before the request rather
/// than instead of it: the server still refuses, and this only means the
/// browser never sends a document it already knows is refused.
export function coordinateKeysOutsidePins(value: unknown, path = ""): string[] {
  const out: string[] = [];
  if (Array.isArray(value)) {
    value.forEach((v, i) => out.push(...coordinateKeysOutsidePins(v, `${path}[${i}]`)));
    return out;
  }
  if (value === null || typeof value !== "object") return out;
  for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
    const child = path ? `${path}.${k}` : k;
    if (k === "pins" && path === "") continue; // the ONE sanctioned geometry
    if ((COORDINATE_KEYS as readonly string[]).includes(k)) out.push(child);
    out.push(...coordinateKeysOutsidePins(v, child));
  }
  return out;
}

// --- the document ---------------------------------------------------------

export interface BoardNodeInput {
  id: string;
  kind: string;
  title?: string;
  body_md?: string;
  group?: string;
  thread_id?: string;
  path?: string;
  symbol?: string;
  range?: [number, number];
  context?: [number, number];
  blob_sha?: string;
  guard_hash?: string;
  query?: string;
  authored_count?: number;
  review?: number;
  patchset?: number;
  hunk?: string;
  finding?: string;
  annotation?: string;
  session?: string;
  turn?: string;
  bookmark?: number;
  members?: string[];
  url?: string;
}

export interface BoardEdgeInput {
  from: string;
  to: string;
  kind: string;
  label?: string;
  provenance?: string;
  trust?: string;
}

export interface BoardDocInput {
  schema: string;
  repo: string;
  slug: string;
  title: string;
  description_md: string;
  status: string;
  authored_ref?: string;
  nodes: BoardNodeInput[];
  edges: BoardEdgeInput[];
  steps: { node: string; caption?: string }[];
  pins: Record<string, BoardPin>;
}

/// The reference fields each kind may carry, `boards::lint`'s own per-kind
/// table (required ∪ optional). Anything outside it is REFUSED by the lint as
/// "another kind's reference field", so re-sending one would break a
/// round-trip that otherwise works.
export const REF_FIELDS_BY_KIND: Record<string, readonly (keyof BoardNodeInput)[]> = {
  code: ["path", "range", "symbol", "context", "blob_sha", "guard_hash"],
  note: [],
  query: ["query", "authored_count"],
  hunk: ["review", "patchset", "hunk", "path"],
  finding: ["review", "finding"],
  annotation: ["annotation"],
  turn: ["session", "turn"],
  bookmark: ["bookmark"],
  group: ["members"],
  link: ["url"],
};

/// One resolved node back into the AUTHORED node the daemon would accept.
export function nodeToInput(node: BoardNode): BoardNodeInput {
  const out: BoardNodeInput = { id: node.id, kind: node.kind };
  if (node.title != null) out.title = node.title;
  if (node.body_md != null) out.body_md = node.body_md;
  if (node.group != null) out.group = node.group;
  // The thread is echoed as an OBJECT (`{id, resolved, replies}`); only its id
  // is authored state, and `thread_id` is what the document calls it.
  if (node.thread?.id) out.thread_id = node.thread.id;
  const allowed = REF_FIELDS_BY_KIND[node.kind] ?? [];
  for (const field of allowed) {
    const v = (node as unknown as Record<string, unknown>)[field as string];
    if (v !== undefined && v !== null) {
      (out as unknown as Record<string, unknown>)[field as string] = v;
    }
  }
  return out;
}

export function edgeToInput(edge: BoardEdge): BoardEdgeInput {
  const out: BoardEdgeInput = { from: edge.from, to: edge.to, kind: edge.kind };
  if (edge.label != null) out.label = edge.label;
  if (edge.provenance) out.provenance = edge.provenance;
  // The lint REFUSES a trust class on an authored edge; a derived one must
  // carry the class it inherited.
  if (edge.provenance === "derived" && edge.trust != null) out.trust = edge.trust;
  return out;
}

/// The status an apply may write for this board: its own, when that is one of
/// the two appliable ones, else `pending`.
export function appliableStatus(status: string): string {
  return (APPLIABLE_STATUSES as readonly string[]).includes(status) ? status : "pending";
}

/// The board on screen, as a document — the identity round trip every
/// composition starts from.
export function boardToDoc(board: BoardOut): BoardDocInput {
  return {
    schema: BOARD_SCHEMA,
    repo: board.repo,
    slug: board.slug,
    title: board.title,
    description_md: board.description_md,
    status: appliableStatus(board.status),
    ...(board.authored_ref ? { authored_ref: board.authored_ref } : {}),
    nodes: board.nodes.map(nodeToInput),
    edges: board.edges.map(edgeToInput),
    steps: board.steps.map((s) => ({ node: s.node, ...(s.caption ? { caption: s.caption } : {}) })),
    pins: { ...board.pins },
  };
}

// --- minting a node from an action target ---------------------------------

/// A slug-safe, deterministic node id, de-duplicated against `taken`.
export function mintNodeId(seed: string, taken: Iterable<string>): string {
  const used = new Set(taken);
  const base =
    seed
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+/, "")
      .replace(/-+$/, "")
      .slice(0, MAX_ID_LEN - 4) || "n";
  const start = /^[a-z0-9]/.test(base) ? base : `n-${base}`.slice(0, MAX_ID_LEN - 4);
  if (!used.has(start) && isValidBoardId(start)) return start;
  for (let i = 2; i < 1000; i += 1) {
    const candidate = `${start}-${i}`;
    if (!used.has(candidate) && isValidBoardId(candidate)) return candidate;
  }
  // 998 collisions on one seed is not a state this reaches; refusing beats
  // returning an id the server will reject.
  throw new Error(`could not mint a node id for ${seed}`);
}

export interface TargetNodeResult {
  node: BoardNodeInput;
  /// What the composition could not establish, in words. Rendered in the
  /// dialog BEFORE the apply, never discovered afterwards.
  notes: string[];
}

/// One actions/1 target as a board node.
///
/// Every target kind the panel serves becomes a `code` node, because that is
/// what all five of them ARE: a path plus a range in the working tree. The
/// only variation is how much of the range is known, and where it is not, this
/// says so rather than inventing one.
export function nodeFromTarget(
  target: ActionTarget,
  taken: Iterable<string>,
): TargetNodeResult {
  const notes: string[] = [];
  const start = target.line;
  const end = target.end_line;
  let range: [number, number];
  if (start === undefined) {
    range = [1, 1];
    notes.push(
      "this target names a file with no line, so the card anchors at line 1 — widen it on the board",
    );
  } else {
    range = [start, end !== undefined && end > start ? end : start];
  }
  const base = target.path.split("/").pop() || target.path;
  const seed = target.name ? `${base}-${target.name}` : `${base}-${range[0]}`;
  const node: BoardNodeInput = {
    id: mintNodeId(seed, taken),
    kind: "code",
    title: target.name ?? `${base}:${range[0]}`,
    path: target.path,
    range,
  };
  if (target.name) node.symbol = target.name;
  // The blob the target was READ at, when a read happened. It is what turns
  // the card from "these lines" into a verifiable claim about specific bytes;
  // absent, the daemon records the current blob at apply time and says so.
  if (target.blob_sha) node.blob_sha = target.blob_sha;
  else notes.push("no blob sha on this target — the daemon will pin the current one at apply time");
  if (target.exists === false) {
    notes.push("this path is not in the repo index — the card will be an honest orphan");
  }
  return { node, notes };
}

export interface ComposeAddResult {
  doc: BoardDocInput;
  nodeId: string;
  notes: string[];
  /// `true` when this apply will move an accepted/archived board back to
  /// `pending` (D21). The dialog says so before the click.
  statusWillReset: boolean;
  /// Coordinate-shaped keys this composition would have sent. ALWAYS empty for
  /// a document this module built; checked rather than asserted, because the
  /// point of the rule is that it holds even when the code is wrong.
  coordinateViolations: string[];
}

/// Add one target to an EXISTING board.
export function composeAdd(board: BoardOut, target: ActionTarget): ComposeAddResult {
  const doc = boardToDoc(board);
  const { node, notes } = nodeFromTarget(
    target,
    doc.nodes.map((n) => n.id),
  );
  doc.nodes.push(node);
  return {
    doc,
    nodeId: node.id,
    notes,
    statusWillReset: !(APPLIABLE_STATUSES as readonly string[]).includes(board.status),
    coordinateViolations: coordinateKeysOutsidePins(doc),
  };
}

/// Add one target to a board that does not exist yet. A board never starts
/// empty (the research's own finding about every canvas that failed) — the
/// entry action always brings content.
export function composeNew(
  repo: string,
  slug: string,
  title: string,
  target: ActionTarget,
): ComposeAddResult {
  const { node, notes } = nodeFromTarget(target, []);
  const doc: BoardDocInput = {
    schema: BOARD_SCHEMA,
    repo,
    slug,
    title,
    description_md: "",
    status: "pending",
    nodes: [node],
    edges: [],
    steps: [],
    pins: {},
  };
  return {
    doc,
    nodeId: node.id,
    notes,
    statusWillReset: false,
    coordinateViolations: coordinateKeysOutsidePins(doc),
  };
}

/// Pin or unpin ONE node. This is the only door through which a coordinate
/// ever reaches the daemon, and it is explicit by construction: a drag does
/// not write one, "pin here" does.
export function composePin(
  board: BoardOut,
  nodeId: string,
  pin: BoardPin | null,
): { doc: BoardDocInput; coordinateViolations: string[] } {
  const doc = boardToDoc(board);
  if (pin) doc.pins[nodeId] = { x: Math.round(pin.x), y: Math.round(pin.y) };
  else delete doc.pins[nodeId];
  return { doc, coordinateViolations: coordinateKeysOutsidePins(doc) };
}

/// The lint report's one-line summary, in the daemon's own words. Rendered
/// verbatim on a refusal — this side never rewrites a rule's message, because
/// `rule` is the stable id an agent branches on and the message is what tells a
/// human which node to fix.
export function lintSummary(findings: { rule: string; at?: string | null; message: string }[]): string[] {
  return findings.map((f) => (f.at ? `[${f.rule}] ${f.at}: ${f.message}` : `[${f.rule}] ${f.message}`));
}

/// Record a node's THREAD parent on the board.
///
/// A node thread REUSES the annotations store (D10) — the parent is an
/// ordinary annotation and the board only stores its id. The parent is created
/// first, through the existing `POST /api/annotations` path, and this
/// composition is the second half: writing `thread_id` onto the node so the
/// link survives a reload. If the apply is refused (a bearer caller), the
/// annotation still exists at its own path and the panel says the LINK could
/// not be recorded — the comment is never lost to a failed bookkeeping write.
export function composeThread(
  board: BoardOut,
  nodeId: string,
  threadId: string,
): { doc: BoardDocInput; coordinateViolations: string[] } {
  const doc = boardToDoc(board);
  const node = doc.nodes.find((n) => n.id === nodeId);
  if (node) node.thread_id = threadId;
  return { doc, coordinateViolations: coordinateKeysOutsidePins(doc) };
}

/// Where a node's thread can honestly be anchored, or `null`.
///
/// An annotation is anchored at a PATH and a LINE (`routes::CreateAnnotationBody`
/// requires both for a parent). A `code` node has them; a `hunk` node has a
/// path when its author gave one. Every other kind addresses something that is
/// not a working-tree position, and inventing one so a button could be enabled
/// is exactly the fabricated address this codebase refuses elsewhere — so the
/// affordance says why instead.
export function threadAnchorFor(
  node: BoardNode,
): { path: string; line: number; lineEnd?: number } | null {
  if (node.kind === "code" && node.code) {
    const [a, b] = node.code.range;
    return { path: node.code.path, line: a, ...(b > a ? { lineEnd: b } : {}) };
  }
  if (node.path && node.range) {
    const [a, b] = node.range;
    return { path: node.path, line: a, ...(b > a ? { lineEnd: b } : {}) };
  }
  return null;
}
