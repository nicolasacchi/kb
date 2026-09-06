// Pure mirror of the server's wikilink grammar (`kb_core::links` — comrak's
// `wikilinks_title_after_pipe`: target before the pipe, alias after) + the
// `normalize_target` ladder. The SPA's native note view (`NoteMarkdown`'s
// `rehypeWikilinks`) resolves a note's `[[…]]` against the map the server
// already computed (`NoteDetail.links`), so the rendered link and the edge
// the indexer recorded address the same artifact. Same dual-render contract
// as callouts (server-side comrak ⇄ a SPA hast pass), pinned by
// `wikilink.test.ts`.
//
// The render path leaves an unresolved `[[…]]` as a muted span (a "not yet
// created" / pending affordance), never an error — matching Obsidian and the
// server's `state: "dangling" | "ambiguous"`.

import { artifactHref } from "./artifactHref";
import type { ResolvedLink } from "../api/notes";

// `[[` + inner (no brackets / newlines) + `]]`. The inner splits on the first
// `|` into target | alias. Matches comrak: a target may carry a `#section`
// anchor (stripped by normalizeTarget for resolution, kept for display).
// Global so matchAll walks every occurrence in a text node.
const WIKILINK_RE = /\[\[([^[\]\n]+)\]\]/g;

export type ParsedWikilink = { target: string; alias?: string };

/// Split a `[[…]]` inner into target + optional alias, trimmed. An empty
/// target → null (left literal). A SECOND pipe → null too: comrak
/// (`wikilinks_title_after_pipe`) rejects `[[a|b|c]]` entirely, so to stay in
/// lock-step with the server (invariant #29) the SPA must also leave it
/// literal rather than render a target the server never linked. Pinned by
/// `kb_core::links::double_pipe_is_not_a_wikilink`.
export function parseWikilink(inner: string): ParsedWikilink | null {
  const pipe = inner.indexOf("|");
  const rawTarget = (pipe === -1 ? inner : inner.slice(0, pipe)).trim();
  if (!rawTarget) return null;
  if (pipe === -1) return { target: rawTarget };
  const rest = inner.slice(pipe + 1);
  if (rest.includes("|")) return null; // a 2nd pipe — comrak rejects it
  return { target: rawTarget, alias: rest.trim() || undefined };
}

/// Mirror `kb_core::links::normalize_target`: drop a `#fragment`, a leading
/// `/`, and Windows separators; trim. The result is the key into the server's
/// resolution map (whose keys are `normalizeTarget(link.target)`).
export function normalizeTarget(target: string): string {
  const noFrag = target.split("#")[0]?.trim() ?? "";
  return noFrag.replace(/^\/+/, "").replace(/\\/g, "/");
}

// Minimal hast node shape we build/walk — kept local + loose, like the sibling
// rehypeCallouts/rehypeTaskIndex passes in NoteMarkdown.
type HastNode = {
  type?: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
};

/// What a resolver returns for one normalized target.
export type WikiResolved = {
  state: "resolved" | "dangling" | "ambiguous";
  href?: string;
  title?: string;
};

/// Build a resolver from the server's `NoteDetail.links` (raw targets +
/// resolution). Unknown targets (e.g. a live edit preview with no map) →
/// null → rendered as a pending/unresolved span.
export function makeResolver(
  links: ResolvedLink[] | undefined,
): (target: string) => WikiResolved | null {
  const map = new Map<string, ResolvedLink>();
  for (const l of links ?? []) map.set(normalizeTarget(l.target), l);
  return (target: string) => {
    const l = map.get(target);
    if (!l) return null;
    if (l.state === "resolved" && l.source_relative) {
      return {
        state: "resolved",
        href: artifactHref(l.kb, l.source_relative),
        title: l.title,
      };
    }
    return { state: l.state as WikiResolved["state"], title: l.title };
  };
}

/// Replace every `[[…]]` in a text string with hast nodes: surrounding text
/// kept verbatim, each wikilink an `<a class="kb-wikilink">` (resolved) or a
/// `<span class="kb-wikilink kb-wikilink--unresolved">` (dangling / ambiguous
/// / no map). An empty target is left literal.
export function splitWikilinks(
  text: string,
  resolve: (target: string) => WikiResolved | null,
): HastNode[] {
  const out: HastNode[] = [];
  let last = 0;
  for (const m of text.matchAll(WIKILINK_RE)) {
    const parsed = parseWikilink(m[1]);
    const start = m.index ?? 0;
    if (!parsed) continue; // empty target → leave the `[[…]]` literal
    if (start > last) out.push({ type: "text", value: text.slice(last, start) });
    const norm = normalizeTarget(parsed.target);
    const r = resolve(norm);
    const display = parsed.alias || r?.title || parsed.target;
    if (r && r.state === "resolved" && r.href) {
      out.push({
        type: "element",
        tagName: "a",
        properties: {
          className: ["kb-wikilink"],
          href: r.href,
          dataWikilink: "true",
          title: r.title ?? parsed.target,
        },
        children: [{ type: "text", value: display }],
      });
    } else {
      const title =
        r?.state === "ambiguous"
          ? `“${parsed.target}” matches more than one artifact`
          : `No artifact named “${parsed.target}” yet`;
      out.push({
        type: "element",
        tagName: "span",
        properties: { className: ["kb-wikilink", "kb-wikilink--unresolved"], title },
        children: [{ type: "text", value: display }],
      });
    }
    last = start + m[0].length;
  }
  if (last < text.length) out.push({ type: "text", value: text.slice(last) });
  return out;
}

/// Rehype pass: walk the hast tree and rewrite `[[…]]` inside ordinary text
/// into wikilink elements, skipping `code` / `pre` (literal) and `a` (don't
/// nest a link in a link). StrictMode-safe (no shared mutable counter).
export function rehypeWikilinks(resolve: (target: string) => WikiResolved | null) {
  const SKIP = new Set(["code", "pre", "a"]);
  return (tree: HastNode) => {
    const walk = (node: HastNode, skip: boolean) => {
      if (!node.children) return;
      const childSkip = skip || (node.type === "element" && SKIP.has(node.tagName ?? ""));
      const next: HastNode[] = [];
      for (const child of node.children) {
        if (
          !childSkip &&
          child.type === "text" &&
          typeof child.value === "string" &&
          child.value.includes("[[")
        ) {
          next.push(...splitWikilinks(child.value, resolve));
        } else {
          walk(child, childSkip);
          next.push(child);
        }
      }
      node.children = next;
    };
    walk(tree, false);
  };
}
