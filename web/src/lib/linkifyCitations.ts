// CT-B5 — linkify inert code citations (a sha, a `path/to/file.rs:401`) in
// operator-read prose: memory summaries, comment bodies, session decisions.
// Client-side only, no daemon changes — a citation becomes a deep link INTO
// kb-code's own SPA, never a resolved-against-a-repo fact kb itself computed
// (kb has no working tree; that's the whole point of invariant #2/DCB).
//
// REDUCED scope vs. the coderefs/1 closed grammar (`kb_core::coderefs`):
// this mirrors ONLY the extension whitelist (below) plus a single `:LINE`
// suffix, not the full grammar (segment-shape regexes, `Namespace::Class`/
// `Class#method` symbol forms, gem/vendor `external` classification,
// GitHub-issue parsing). Those forms carry a resolved kb-code trust class
// server-side (path_state/line_state); a freeform prose mention has none of
// that machinery behind it, so this stays a conservative "point toward",
// never a claim of resolution.
//
// No repo context: unlike coderefs/1 (which resolves through a per-doc
// `code_refs` table against a human-picked checkout), a citation floating in
// memory/comment/session prose carries no repo at all. `crates/kb-core/src/
// config.rs`'s `KbSection.code_url` doc comment rules this out explicitly —
// a singular repo name is deliberately NOT config-pinned ("the repo LIST
// comes from kb-code's own scorecard, picked per read-time by a human,
// never pinned in config") — so every link here routes through kb-code's
// repo-less `/search` page (`codeLensUrl.ts`'s `codeSearchUrl`, `repo`
// omitted) rather than a speculative `/r/{repo}/{path}` reader deep link.

import { codeSearchUrl } from "./codeLensUrl";

// --- extension whitelist -------------------------------------------------

/// Hand-copied mirror of `CODE_EXTENSIONS` in
/// `crates/kb-core/src/coderefs.rs` — kept in lock-step BY HAND (same
/// posture as `codeLensUrl.ts`'s own hand-copied mirror of web-code's
/// encoding rules; the DCB grammar is Rust-only, there is no shared npm
/// package to import it from). If the Rust list changes, update this one.
export const CODE_EXTENSIONS: readonly string[] = [
  // compound (matching is longest-suffix, so these win over their own tails)
  "js.erb",
  "html.erb",
  "json.erb",
  "css.erb",
  "turbo_stream.erb",
  // ruby / rails
  "rb",
  "rake",
  "gemspec",
  "erb",
  "haml",
  "slim",
  // js / ts
  "js",
  "mjs",
  "cjs",
  "jsx",
  "ts",
  "tsx",
  "vue",
  "svelte",
  // other languages
  "py",
  "go",
  "rs",
  "java",
  "kt",
  "swift",
  "php",
  "c",
  "h",
  "cpp",
  "hpp",
  // shells
  "sh",
  "fish",
  "ps1",
  // data / config / infra
  "yml",
  "yaml",
  "toml",
  "json",
  "xml",
  "proto",
  "graphql",
  "conf",
  "ini",
  "lock",
  "tf",
  // styles + assets
  "css",
  "scss",
  "sass",
  "less",
  "svg",
  "txt",
];

/// Longest whitelisted extension suffix of `body` (mirrors
/// `kb_core::coderefs::longest_code_ext`'s longest-suffix-wins rule, e.g.
/// `.js.erb` beats `.erb`), or `null` when none matches.
function longestCodeExt(body: string): string | null {
  let best: string | null = null;
  for (const ext of CODE_EXTENSIONS) {
    if (
      body.length > ext.length + 1 &&
      body[body.length - ext.length - 1] === "." &&
      body.endsWith(ext) &&
      (best === null || ext.length > best.length)
    ) {
      best = ext;
    }
  }
  return best;
}

/// The reduced path production: whitelisted extension, non-empty stem, no
/// directory-only ref, no `..` (path traversal / elision — a simplified
/// stand-in for the Rust grammar's `HARD_REJECT_SUBSTRINGS`; most of that
/// list is moot here since [`TOKEN_CHARS`] already excludes every other
/// disqualifying character), and — mirroring the Rust W1.gate — the whole
/// basename isn't itself just a compound extension's own name (bare
/// `html.erb` is author shorthand, not a citation to a file named that).
function parsePathToken(body: string): { path: string } | null {
  if (!body || body.includes("..") || body.startsWith("/") || body.endsWith("/")) {
    return null;
  }
  const ext = longestCodeExt(body);
  if (!ext) return null;
  const segs = body.split("/");
  const base = segs[segs.length - 1];
  if (base.length <= ext.length + 1) return null;
  if (CODE_EXTENSIONS.includes(base)) return null;
  return { path: body };
}

// --- sha shape -------------------------------------------------------------

/// Lengths a bare hex run is treated as an unambiguous git sha shape.
/// **12 is deliberately excluded**: kb artifact ids are ALSO 12 lowercase-hex
/// characters, so a bare 12-hex token is genuinely ambiguous (could be
/// either) — the honest call is to skip it entirely rather than guess.
/// Lengths 13-39 are excluded too — not a shape either grammar claims, kept
/// out rather than over-matching.
const SHA_LINK_LENGTHS: ReadonlySet<number> = new Set([7, 8, 9, 10, 11, 40]);

const SHA_SHAPE_RE = /^[0-9a-f]+$/;

// --- tokenizer ---------------------------------------------------------

/// Path/sha candidate token chars: word chars plus the punctuation a real
/// path/sha can legitimately contain (`.`/`/`/`-`). Deliberately narrow —
/// every `HARD_REJECT_SUBSTRINGS` character (`(){}[]$@"'` etc., see
/// `kb_core::coderefs`) is already outside this class, so a candidate can
/// never straddle one. Global flag required for `matchAll` below.
const TOKEN_RE = /[A-Za-z0-9_][A-Za-z0-9_./-]*/g;

/// Trailing characters peeled off a candidate before classifying it —
/// sentence furniture (`memory.rs.`, `(see app.rb)`, `a1b2c3d,`) that the
/// greedy [`TOKEN_RE`] class otherwise swallows. Peeled chars are put back
/// as plain text, never silently dropped.
const TRAILING_PUNCT = new Set([".", ",", ";", ":", ")", "]", "-", "/"]);

/// A path citation's optional `:LINE` suffix — single line only (v1 MVP,
/// matching `codeReaderUrl`'s own "start line only" scope in
/// `codeLensUrl.ts`) — never `:A-B`/`:A,B-C,D` ranges.
const LINE_SUFFIX_RE = /^:(\d+)/;

export type CitationSegment =
  | { kind: "text"; text: string }
  | { kind: "sha"; text: string; href: string }
  | { kind: "path"; text: string; href: string; line: number | null };

/// `PreviewInspector.tsx`'s W1.D.R #11 check, factored out: `code_url` is
/// operator-entered `kb.toml` config, not a wire type the daemon validates.
/// `new URL()` requires an absolute URL with a scheme — a value that can't
/// parse is treated the same as "unset" (never a same-origin fetch/link).
export function validCodeUrl(raw: string | null | undefined): string | null {
  if (!raw) return null;
  try {
    new URL(raw);
    return raw;
  } catch {
    return null;
  }
}

/// Scan `text` for sha/path citations, returning it as an ordered list of
/// plain-text and link segments. `codeUrl` is the (already `validCodeUrl`-
/// checked, or not — this re-validates) `[kb.*] code_url` for the kb the
/// text came from; `null`/invalid ⇒ `text` passes through UNCHANGED as a
/// single text segment (no daemon call, no partial linkification).
export function linkifyCitations(
  text: string,
  codeUrl: string | null | undefined,
): CitationSegment[] {
  const base = validCodeUrl(codeUrl);
  if (!base || !text) return [{ kind: "text", text }];

  const segments: CitationSegment[] = [];
  let cursor = 0;
  for (const match of text.matchAll(TOKEN_RE)) {
    const start = match.index ?? 0;
    // Already swallowed by a previous path citation's `:LINE` suffix.
    if (start < cursor) continue;

    const raw = match[0];
    let end = start + raw.length;

    let body = raw;
    while (body.length > 1 && TRAILING_PUNCT.has(body[body.length - 1])) {
      body = body.slice(0, -1);
      end -= 1;
    }

    let seg: CitationSegment | null = null;

    if (SHA_SHAPE_RE.test(body) && SHA_LINK_LENGTHS.has(body.length)) {
      seg = {
        kind: "sha",
        text: body,
        href: codeSearchUrl(base, undefined, body),
      };
    } else {
      const parsed = parsePathToken(body);
      if (parsed) {
        let line: number | null = null;
        const lm = LINE_SUFFIX_RE.exec(text.slice(end));
        if (lm) {
          line = Number(lm[1]);
          end += lm[0].length;
        }
        seg = {
          kind: "path",
          text: text.slice(start, end),
          href: codeSearchUrl(base, undefined, `#${parsed.path}`),
          line,
        };
      }
    }

    if (seg) {
      if (start > cursor) {
        segments.push({ kind: "text", text: text.slice(cursor, start) });
      }
      segments.push(seg);
      cursor = end;
    }
  }
  if (cursor < text.length) segments.push({ kind: "text", text: text.slice(cursor) });
  return segments.length > 0 ? segments : [{ kind: "text", text }];
}

// --- rehype pass (comment bodies) ---------------------------------------

// hast node shape we touch. Kept local + loose, same posture as
// `wikilink.ts`'s and `NoteMarkdown.tsx`'s own local `HastNode` — a
// three/four-field walk doesn't need a real hast type dependency.
type HastNode = {
  type?: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
};

function citationHastNodes(text: string, codeUrl: string | null): HastNode[] {
  const segments = linkifyCitations(text, codeUrl);
  if (segments.length === 1 && segments[0].kind === "text") {
    return [{ type: "text", value: text }];
  }
  return segments.map((seg) =>
    seg.kind === "text"
      ? { type: "text", value: seg.text }
      : {
          type: "element",
          tagName: "a",
          properties: {
            className: ["kb-citation-link"],
            href: seg.href,
            target: "_blank",
            rel: "noopener noreferrer",
            title:
              seg.kind === "sha"
                ? "look up this commit in kb-code"
                : "look up this file in kb-code (repo not pinned — pick one on the search page)",
          },
          children: [{ type: "text", value: seg.text }],
        },
  );
}

/// Rehype pass for `CommentBody`'s Markdown render: walk the hast tree and
/// rewrite citations inside ordinary text into link elements, skipping
/// `code`/`pre` (literal — a fenced/inline code span is not prose) and `a`
/// (never nest a link in a link) — same SKIP set + walk shape as
/// `wikilink.ts`'s `rehypeWikilinks`. `codeUrl` a no-op (tree untouched)
/// when absent/invalid, so passing this plugin unconditionally is safe.
///
/// unified attacher gotcha (documented in `NoteMarkdown.tsx` against
/// `rehypeWikilinks`): this function IS the attacher and must be passed as
/// `[rehypeCitations, codeUrl]` in a `rehypePlugins` array, never
/// pre-applied (`rehypeCitations(codeUrl)`) — unified would then treat the
/// returned transformer itself as the attacher and invoke it with no tree.
export function rehypeCitations(codeUrl: string | null | undefined) {
  const base = validCodeUrl(codeUrl);
  const SKIP = new Set(["code", "pre", "a"]);
  return (tree: HastNode) => {
    if (!base) return;
    const walk = (node: HastNode, skip: boolean) => {
      if (!node.children) return;
      const childSkip = skip || (node.type === "element" && SKIP.has(node.tagName ?? ""));
      const next: HastNode[] = [];
      for (const child of node.children) {
        if (!childSkip && child.type === "text" && typeof child.value === "string") {
          next.push(...citationHastNodes(child.value, base));
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
