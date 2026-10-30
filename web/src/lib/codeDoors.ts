// OL IDEA6 - "the citation is the door". Pure builder for the doors the reader
// posts into an artifact (and the native-note hast pass): one door per code
// reference that has somewhere honest to go in kb-code.
//
// Invariant #2: kb mints no trust class. A door's class comes ONLY from a
// successful codelens/1 payload handed in by the caller (nothing is stored,
// nothing is cached here); with no payload the door is a `hint` - kb's own
// guess, drawn dashed and titled as unverified. Every URL comes from
// `codeLensUrl.ts` (#35); the rules below mirror CodeRefsSection's RefRow so a
// door never disagrees with the Links tab (codeDoors.parity.test.tsx).

import type { CodeRefOut } from "../api/generated/CodeRefOut";
import type { DocLensOut, ResolvedCodeRef } from "../api/doclens";
import { codeReaderUrl, codeSearchUrl } from "./codeLensUrl";
import { validCodeUrl } from "./linkifyCitations";

/// The closed class set. Rendered as a LINE STYLE plus words in the title,
/// never colour alone.
export type DoorClass = "exact" | "moved" | "unsure" | "hint";

export type CodeDoor = {
  ordinal: number;
  raw: string;
  declared: boolean;
  href: string;
  cls: DoorClass;
  title: string;
};

export type DoorCtx = {
  /// `[kb.*] code_url`. Anything that is not an absolute http(s) URL yields no
  /// doors (`validCodeUrl` alone accepts `javascript:`).
  codeUrl: string | null | undefined;
  /// The checkout the doors resolve against (see `doorRepo`), or null.
  repo: string | null;
  /// The lens for `repo`, when it answered.
  lens?: DocLensOut | null;
  /// The error that kept the lens (or the scorecard) from answering.
  lensError?: unknown;
  lensLoading?: boolean;
};

/// A `code_url` safe to build links from: parses, and is http(s).
export function httpCodeUrl(raw: string | null | undefined): string | null {
  const valid = validCodeUrl(raw);
  if (!valid) return null;
  try {
    const u = new URL(valid);
    return u.protocol === "http:" || u.protocol === "https:" ? valid : null;
  } catch {
    return null;
  }
}

/// Which checkout a pane's doors use. The focused pane follows the inspector's
/// view (`?repo=`, then the scorecard pin); an unfocused pane uses the pin only
/// (`?repo=` is the inspector's view of the FOCUSED doc). A checkout is never
/// auto-selected.
export function doorRepo(args: {
  focused: boolean;
  repoParam: string | null | undefined;
  pinnedRepo: string | null | undefined;
}): string | null {
  const pin = args.pinnedRepo || null;
  if (args.focused) return args.repoParam || pin;
  return pin;
}

/// Whether a pane posts doors at all: a valid `code_url`, the entry page (a
/// `?p=` sub-page is a different document the extractor never saw), and not a
/// historical (`?at=`) render.
export function doorsLive(args: {
  codeUrl: string | null | undefined;
  activePage: string;
  historical: boolean;
}): boolean {
  return httpCodeUrl(args.codeUrl) !== null && args.activePage === "" && !args.historical;
}

const LINE_KINDS = new Set(["path_line", "path_range", "path_list"]);

function errorReason(err: unknown): string | null {
  const e = err as (Error & { reason?: string }) | undefined;
  if (e && e.reason) return e.message || e.reason;
  return null;
}

/// Why a hint is unverified, in words (the title's second sentence).
function hintWhy(ctx: DoorCtx): string {
  if (ctx.lensError) {
    const reason = errorReason(ctx.lensError);
    return reason
      ? `kb-code refused: ${reason}`
      : "kb-code did not answer";
  }
  if (ctx.lensLoading) return "not checked yet";
  if (!ctx.repo) return "no checkout chosen";
  return "not checked yet";
}

function lensRowFor(ref: CodeRefOut, lens: DocLensOut | null | undefined): ResolvedCodeRef | null {
  if (!lens) return null;
  const row = lens.refs.find((r) => r.ordinal === ref.ordinal);
  if (!row) return null;
  // A lens computed against an older extraction never lends its class to a
  // different ref.
  if (row.raw !== ref.raw || row.declared !== ref.declared) return null;
  return row;
}

function make(ref: CodeRefOut, href: string, cls: DoorClass, title: string): CodeDoor {
  return { ordinal: ref.ordinal, raw: ref.raw, declared: ref.declared, href, cls, title };
}

/// The door for ONE ref, or null when the ref has nowhere honest to go.
export function codeDoorFor(ref: CodeRefOut, ctx: DoorCtx): CodeDoor | null {
  const codeUrl = httpCodeUrl(ctx.codeUrl);
  if (!codeUrl || !ref.raw) return null;
  if (ref.kind === "issue" || ref.kind === "external") return null;

  const row = lensRowFor(ref, ctx.lens);
  if (row) {
    if (row.kind === "issue" || row.path_state === "external") return null;
    const repoName = ctx.lens ? ctx.lens.repo.name : (ctx.repo ?? "");
    if (row.path_state === "present" && row.reader) {
      const href = codeReaderUrl(codeUrl, row.reader.repo, row.reader.path, row.reader.line);
      const where = row.reader.line
        ? `${row.reader.path}:${row.reader.line}`
        : row.reader.path;
      if (!LINE_KINDS.has(row.kind) || row.line_state === "confirmed") {
        return make(ref, href, "exact", `${where} - verified in ${row.reader.repo}; opens in kb-code`);
      }
      if (row.line_state === "drifted") {
        return make(
          ref,
          href,
          "moved",
          `${where} - the document's line moved; opens at the line kb-code found in ${row.reader.repo}`,
        );
      }
      return make(
        ref,
        href,
        "unsure",
        `${where} - the file exists in ${row.reader.repo} but its line is not verified; opens in kb-code`,
      );
    }
    if (row.path_state === "ambiguous" || row.path_state === "absent") {
      if (!row.search) return null;
      const href = codeSearchUrl(codeUrl, row.search.repo, row.search.q);
      const title =
        row.path_state === "ambiguous"
          ? `${row.candidate_count} candidates in ${repoName || row.search.repo} - opens a kb-code search`
          : `not found in ${repoName || row.search.repo}${row.note ? ` - ${row.note}` : ""} - opens a kb-code search`;
      return make(ref, href, "unsure", title);
    }
    if (
      (row.symbol_state === "hit_unique" || row.symbol_state === "hit_container_matched") &&
      row.symbol_hits.length === 1
    ) {
      const hit = row.symbol_hits[0];
      return make(
        ref,
        codeReaderUrl(codeUrl, repoName, hit.path, hit.line_start),
        "exact",
        `${hit.path}:${hit.line_start} - the one match in ${repoName}; opens in kb-code`,
      );
    }
    // A row that kb-code did answer but that has no honest target (an
    // ambiguous symbol lists its hits in the Links tab; an unmatched one has
    // nothing): no door - never a fabricated hint over a real answer.
    return null;
  }

  // No usable lens row: kb's own hint. The door is kb-code's `#file` search.
  if (!ref.path_hint) return null;
  const href = codeSearchUrl(codeUrl, ctx.repo ?? undefined, "#" + ref.path_hint);
  return make(
    ref,
    href,
    "hint",
    `${ref.path_hint} - kb's hint, not verified (${hintWhy(ctx)}); pick a checkout in Links to verify. Opens a kb-code search`,
  );
}

/// All doors for a document's refs, ordered by ordinal.
export function buildCodeDoors(refs: readonly CodeRefOut[], ctx: DoorCtx): CodeDoor[] {
  if (!httpCodeUrl(ctx.codeUrl)) return [];
  const out: CodeDoor[] = [];
  for (const ref of [...refs].sort((a, b) => a.ordinal - b.ordinal)) {
    const d = codeDoorFor(ref, ctx);
    if (d) out.push(d);
  }
  return out;
}

// --- native notes (hast) -----------------------------------------------------

type HastNode = {
  type?: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
};

function hastText(node: HastNode): string {
  if (node.type === "text") return node.value ?? "";
  return (node.children ?? []).map(hastText).join("");
}

/// Rehype pass for the read-only native-note render. react-markdown drops raw
/// HTML, so there are no declared (`data-kb-ref`) keys; matching is the same
/// text-key rule as the runtime: walk `code` in document order, skip a `code`
/// nested in another, pop the lowest ordinal per trimmed text. A match inside
/// a link CONSUMES its ordinal but is not wrapped. Wrapping is safe here: a
/// native note carries file-scope comments only (no css-path anchors).
///
/// Passed as `[rehypeCodeDoors, doors]` (the unified attacher gotcha
/// documented in NoteMarkdown.tsx).
export function rehypeCodeDoors(doors: readonly CodeDoor[] | null | undefined) {
  return (tree: HastNode) => {
    if (!doors || doors.length === 0) return;
    const queues = new Map<string, CodeDoor[]>();
    for (const d of [...doors].sort((a, b) => a.ordinal - b.ordinal)) {
      if (d.declared) continue;
      const q = queues.get(d.raw) ?? [];
      q.push(d);
      queues.set(d.raw, q);
    }
    const walk = (node: HastNode, inLink: boolean, inCode: boolean) => {
      if (!node.children) return;
      const next: HastNode[] = [];
      for (const child of node.children) {
        if (child.type !== "element") {
          next.push(child);
          continue;
        }
        if (child.tagName === "code" && !inCode) {
          const key = hastText(child).trim();
          const door = key ? queues.get(key)?.shift() : undefined;
          if (door && !inLink) {
            next.push({
              type: "element",
              tagName: "a",
              properties: {
                href: door.href,
                target: "_blank",
                rel: ["noopener", "noreferrer"],
                title: door.title,
                className: ["kb-coderef-door", `kb-coderef-door--${door.cls}`],
                dataKbCoderef: String(door.ordinal),
              },
              children: [child],
            });
          } else {
            next.push(child);
          }
          continue;
        }
        walk(child, inLink || child.tagName === "a", inCode || child.tagName === "code");
        next.push(child);
      }
      node.children = next;
    };
    walk(tree, false, false);
  };
}
