// Link peek + reading flow — the PURE link resolver.
//
// One question, one answer: "this href, seen from inside artifact X's
// iframe (or from SPA chrome), points at WHAT?". Every consumer of the
// `kb:link-hover` / `kb:link-open` relay (`components/reader/ArtifactPane.tsx`)
// and the peek card goes through this module, so there is exactly one place
// that knows the three link shapes an artifact can carry:
//
//   1. the SPA permalink grammar  `/a/<kb>/<source-relative>`  (path form)
//   2. an artifact-subdomain URL  `<id>.artifacts.<root>[:port]/…` (id form)
//   3. a plain RELATIVE href       `01-timeline.html`, `../x/y.html`, `#sec`
//
// Pure, LLM-free, no fetch, no DOM: it takes an href string plus a small
// context bag and returns a tagged union. Anything it cannot resolve
// HONESTLY is `external` — never a half-built artifact reference (the same
// totality discipline `lib/paneUrl.ts`'s `parsePane2` follows: a partial
// answer here would point a peek/navigation at the wrong document).
// Golden-pinned in `artifactLinks.test.ts`, like `paneUrl` / `galleryUrl`.
//
// ARTIFACT HOST GRAMMAR v2 — an id-form URL's host label is now
// `<kb_enc>--<id>` (QUALIFIED) rather than a bare `<id>`; see
// `parseArtifactIdFromHost` below for the client-side mirror of the
// server's parser, and `resolveArtifactHref`'s id-form branch for how the
// resolved `kb_enc` feeds a `ResolvedLink`'s `kb` field.
//
// ── why relative hrefs resolve against the SOURCE path, not the URL ───────
//
// An artifact is served at the ROOT of its own subdomain
// (`<id>.artifacts.<root>/`), while its source file lives at
// `pm/00-summary.html` inside the kb. So the browser resolves a relative
// link `01-timeline.html` to `<id>.artifacts.<root>/01-timeline.html` — an
// origin-root path that means `pm/01-timeline.html` in SOURCE space (which
// is exactly what the daemon's own trampoline resolves server-side). Every
// path join below therefore starts from the DIRECTORY of the pane's
// `sourceRelative`, and `..` that would escape above the kb root is
// REJECTED (the URL API silently clamps it, which would turn a traversal
// attempt into a plausible-looking root-level path).

import { encodeKbForHost } from "./artifactHost";

/// Everything the resolver needs to know about where the link was SEEN.
export type ArtifactLinkCtx = {
  /// The kb the observing pane's artifact lives in — the default kb for
  /// every relative/own-origin resolution.
  paneKb: string;
  /// The observing pane's own source-relative path (`pm/00-summary.html`).
  paneSourceRelative: string;
  /// The observing pane's artifact id, when known. Used ONLY to tell "a
  /// URL on my own artifact origin" (resolve against my directory) from "a
  /// URL on some OTHER artifact's origin" (an id reference). Absent ⇒ any
  /// same-suffix origin is treated as the pane's own, which is what the
  /// relay actually produces (the iframe resolves its hrefs against its own
  /// `location.href`).
  paneId?: string | null;
  /// The daemon's authoritative `artifact_host_suffix` (invariant #7 —
  /// runtime config, never hardcoded), e.g. `.artifacts.localhost`.
  hostSuffix: string;
  /// The SPA's own origin (`window.location.origin`). A `/a/<kb>/…`
  /// pathname is only honoured on this origin or on an artifact origin of
  /// this same daemon — never on a third-party host that happens to use
  /// the same path shape.
  spaOrigin: string;
  /// The kb ids this daemon serves (the cached `["kbs"]` query). When
  /// non-empty a `/a/<kb>/…` link naming an unknown kb resolves to
  /// `external`; when empty (kbs not loaded yet) the kb segment is trusted.
  kbIds?: readonly string[];
};

export type ResolvedLink =
  /// A resolvable artifact, addressed the way the SPA addresses one.
  | { kind: "artifact"; kb: string; sourceRelative: string; sec?: string }
  /// An artifact addressed by ID (a subdomain URL, or a `/a/<kb>/<12hex>`
  /// permalink). `kb` is null when the URL carried no kb segment — the
  /// caller resolves it by trying the pane's kb first, then the rest.
  | { kind: "artifact-id"; kb: string | null; id: string; sec?: string }
  /// The pane's OWN document (a bare `#hash`, a self link, a permalink
  /// naming this very artifact). Nothing to peek, nothing to navigate.
  | { kind: "same-doc" }
  /// Anything else: an off-daemon URL, a `mailto:`, an asset, a traversal
  /// escape, an unknown kb.
  | { kind: "external" };

const EXTERNAL: ResolvedLink = { kind: "external" };
const SAME_DOC: ResolvedLink = { kind: "same-doc" };

/// A bare artifact id as it appears in a permalink slot — kb's ids are
/// 12 lowercase hex chars (`kb_core::ids`).
const BARE_ID_RE = /^[0-9a-f]{12}$/;

/// Exactly 12 lowercase hex chars — the id half of a QUALIFIED host label.
/// `host` is already lowercased by the time this runs, so this is
/// equivalent to the server's "12 lowercase hex chars" wording.
const QUALIFIED_ID_RE = /^[0-9a-f]{12}$/;

/// `kb_enc` grammar: non-empty, starts with `[a-z0-9]`, and every
/// subsequent char is `[a-z0-9-]` — mirrors the server's
/// `[a-z0-9][a-z0-9-]*`.
const KB_ENC_RE = /^[a-z0-9][a-z0-9-]*$/;

/// The result of parsing an artifact-subdomain host, per ARTIFACT HOST
/// GRAMMAR v2.
export type ParsedArtifactHost =
  /// `<kb_enc>--<id>` — unambiguous: split at the RIGHTMOST `--`, `kb_enc`
  /// non-empty and `[a-z0-9][a-z0-9-]*`, `id` exactly 12 lowercase hex.
  | { kind: "qualified"; kbEnc: string; id: string }
  /// Anything else that still passes the legacy character-class checks —
  /// today's behaviour, byte-identical (a pathological/legacy filename-stem
  /// id, or an old-style bare 12-hex id with no `kb_enc--` prefix at all).
  | { kind: "bare"; id: string };

/// Client mirror of `kb_core::iframe::parse_artifact_id` (now ARTIFACT HOST
/// GRAMMAR v2-aware) — the label lives in the FIRST hostname component(s),
/// and the remainder must be exactly the configured suffix. `hostname` is
/// expected WITHOUT a port (`URL.hostname`); a port is stripped defensively
/// anyway, mirroring the Rust helper.
///
/// Parse rule (byte-for-byte the server's): strip `suffix`; if the
/// remaining label ends with `--` + exactly 12 lowercase hex AND the prefix
/// before that RIGHTMOST `--` is non-empty and matches `[a-z0-9][a-z0-9-]*`
/// → `qualified`; otherwise → `bare`, subject to the SAME rejections the
/// pre-v2 parser applied (empty label, leading/trailing dot, `..`, anything
/// outside `[A-Za-z0-9-_.]`) so the SPA can never treat a host the daemon
/// wouldn't dispatch as an artifact origin.
export function parseArtifactIdFromHost(
  hostname: string,
  suffix: string,
): ParsedArtifactHost | null {
  if (!hostname || !suffix) return null;
  const host = hostname.toLowerCase().split(":")[0];
  const sfx = suffix.toLowerCase();
  if (!host.endsWith(sfx)) return null;
  const label = host.slice(0, host.length - sfx.length);
  if (!label) return null;

  // Split at the RIGHTMOST `--` so a `kb_enc` that itself contains `--`
  // (two adjacent encoded underscores) stays deterministic.
  const splitAt = label.lastIndexOf("--");
  if (splitAt > 0) {
    const kbEnc = label.slice(0, splitAt);
    const idPart = label.slice(splitAt + 2);
    if (KB_ENC_RE.test(kbEnc) && QUALIFIED_ID_RE.test(idPart)) {
      return { kind: "qualified", kbEnc, id: idPart };
    }
  }

  // Bare fallthrough — today's behaviour, unchanged.
  if (label.startsWith(".") || label.endsWith(".")) return null;
  if (label.includes("..")) return null;
  if (!/^[A-Za-z0-9\-_.]+$/.test(label)) return null;
  return { kind: "bare", id: label };
}

/// Resolve a QUALIFIED label's `kb_enc` against the known kb list — each
/// encoded the same way (`encodeKbForHost`, shared with `artifactOrigin` so
/// the two directions of the grammar never drift). A UNIQUE match is a real
/// answer; zero or multiple matches (kb list not loaded yet, or two kb
/// names that happen to encode to the same label) return null — the caller
/// falls back to `kb: null` and its existing pane-then-ladder resolution,
/// exactly as a bare/legacy id does today.
function resolveKbFromEnc(
  kbEnc: string,
  kbIds: readonly string[],
): string | null {
  const matches = kbIds.filter(
    (k) => encodeKbForHost(k).toLowerCase() === kbEnc,
  );
  return matches.length === 1 ? matches[0] : null;
}

function decodeSegment(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/// `#why-pin` → `why-pin`; empty/absent hash ⇒ undefined. Decoded, because
/// `artifactHref` re-encodes `sec` on the way out (double-encoding would
/// produce a heading id that matches nothing).
function secOf(hash: string): string | undefined {
  if (!hash || hash === "#") return undefined;
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!raw) return undefined;
  return decodeSegment(raw);
}

/// The directory segments of a source-relative path (`pm/a/b.html` → `["pm","a"]`).
function dirSegments(sourceRelative: string): string[] {
  const parts = sourceRelative.split("/").filter((s) => s !== "");
  parts.pop();
  return parts;
}

/// Join a path that is relative to `baseDir` (already-decoded segments),
/// rejecting any `..` that would climb above the kb root. Returns null for
/// an escape or an empty result.
function joinUnderRoot(baseDir: string[], path: string): string | null {
  const out = [...baseDir];
  for (const rawSeg of path.split("/")) {
    const seg = decodeSegment(rawSeg);
    if (seg === "" || seg === ".") continue;
    if (seg === "..") {
      if (out.length === 0) return null; // escape above the kb root
      out.pop();
      continue;
    }
    out.push(seg);
  }
  if (out.length === 0) return null;
  return out.join("/");
}

/// An absolute-path (`/foo/bar.html`) href inside an artifact addresses the
/// artifact ORIGIN's root, which is the artifact's own directory in source
/// space — so it joins from the pane's directory exactly like a relative
/// one. (Both go through `joinUnderRoot`; only the leading empty segment
/// differs, and that is skipped.)
function resolveInPaneDir(
  path: string,
  sec: string | undefined,
  ctx: ArtifactLinkCtx,
): ResolvedLink {
  const rel = joinUnderRoot(dirSegments(ctx.paneSourceRelative), path);
  if (!rel) return EXTERNAL;
  if (rel === ctx.paneSourceRelative) return SAME_DOC;
  return sec
    ? { kind: "artifact", kb: ctx.paneKb, sourceRelative: rel, sec }
    : { kind: "artifact", kb: ctx.paneKb, sourceRelative: rel };
}

/// `/a/<kb>/<rest…>` on a trusted origin. Returns null when the pathname
/// isn't the permalink shape at all (the caller falls through to the other
/// forms); returns `external` when it IS the shape but names a kb this
/// daemon doesn't serve.
function matchPermalink(
  url: URL,
  ctx: ArtifactLinkCtx,
): ResolvedLink | null {
  const segs = url.pathname.split("/").filter((s) => s !== "");
  if (segs.length < 3 || segs[0] !== "a") return null;
  const kb = decodeSegment(segs[1]);
  const rest = segs
    .slice(2)
    .map(decodeSegment)
    .join("/");
  if (!kb || !rest) return EXTERNAL;
  const known = ctx.kbIds ?? [];
  if (known.length > 0 && !known.includes(kb)) return EXTERNAL;
  const sec = secOf(url.hash);
  if (BARE_ID_RE.test(rest)) {
    return sec ? { kind: "artifact-id", kb, id: rest, sec } : { kind: "artifact-id", kb, id: rest };
  }
  if (kb === ctx.paneKb && rest === ctx.paneSourceRelative) return SAME_DOC;
  return sec
    ? { kind: "artifact", kb, sourceRelative: rest, sec }
    : { kind: "artifact", kb, sourceRelative: rest };
}

/// Resolve one href seen from `ctx`. TOTAL — every input yields exactly one
/// of the four cases, and anything ambiguous degrades to `external`.
export function resolveArtifactHref(
  href: string,
  ctx: ArtifactLinkCtx,
): ResolvedLink {
  const raw = (href ?? "").trim();
  if (!raw) return EXTERNAL;

  let url: URL | null = null;
  try {
    url = new URL(raw);
  } catch {
    url = null;
  }

  if (url) {
    // `mailto:`, `javascript:`, `data:`, `tel:` … — never an artifact.
    if (url.protocol !== "http:" && url.protocol !== "https:") return EXTERNAL;
    const parsedHost = parseArtifactIdFromHost(url.hostname, ctx.hostSuffix);
    const artifactId = parsedHost?.id ?? null;
    // ARTIFACT HOST GRAMMAR v2 — a QUALIFIED label resolves its kb by
    // matching `kb_enc` against the known kb list; a bare/legacy label (or
    // an unresolved kb_enc) carries no kb, exactly like an id-only link did
    // before the grammar existed.
    const linkKb =
      parsedHost?.kind === "qualified"
        ? resolveKbFromEnc(parsedHost.kbEnc, ctx.kbIds ?? [])
        : null;
    const onDaemon = artifactId !== null || url.origin === ctx.spaOrigin;
    // (1) the permalink grammar — honoured on the SPA origin AND on an
    // artifact origin of this daemon (an artifact that hardcodes the SPA
    // path resolves it against its own origin), never on a foreign host.
    if (onDaemon) {
      const permalink = matchPermalink(url, ctx);
      if (permalink) return permalink;
    }
    // (2) an artifact-subdomain URL.
    if (artifactId !== null) {
      const sec = secOf(url.hash);
      const path = url.pathname.replace(/^\/+/, "");
      // "Own origin" = this pane's own artifact origin — checked by id, and
      // (when the label was QUALIFIED) also by kb, so a FOREIGN kb's
      // artifact that happens to reuse this pane's 12-hex id is never
      // mistaken for a same-doc/own-directory reference. A bare/legacy
      // label carries no kb to check, so it degrades to the pre-v2,
      // id-only comparison.
      const sameId =
        ctx.paneId == null || ctx.paneId.toLowerCase() === artifactId.toLowerCase();
      const sameKb =
        parsedHost?.kind !== "qualified" ||
        encodeKbForHost(ctx.paneKb).toLowerCase() === parsedHost.kbEnc;
      const isOwnOrigin = sameId && sameKb;
      if (!path) {
        if (isOwnOrigin) return SAME_DOC;
        return sec
          ? { kind: "artifact-id", kb: linkKb, id: artifactId, sec }
          : { kind: "artifact-id", kb: linkKb, id: artifactId };
      }
      // A path UNDER an artifact origin is relative to that artifact's own
      // directory. We can only map it when the origin is the pane's own
      // (which is what the hover/click relay produces — the iframe resolves
      // its hrefs against its own location); a sub-path under a FOREIGN
      // artifact origin is an asset/page we cannot honestly name, so it
      // stays external rather than silently opening that artifact's root.
      if (!isOwnOrigin) return EXTERNAL;
      return resolveInPaneDir(path, sec, ctx);
    }
    // (3) anything else on the SPA origin (or off-daemon entirely).
    return EXTERNAL;
  }

  // A RELATIVE href (`01-timeline.html`, `../x.html`, `#sec`, `?q=1`).
  //
  // Split by hand rather than through `new URL(raw, base)`: the URL API
  // CLAMPS a `..` that climbs past the base's root, which would silently
  // turn `../../../secrets.html` into a plausible root-level path instead
  // of the traversal escape it is. `joinUnderRoot` rejects it instead.
  const hashIdx = raw.indexOf("#");
  const sec = secOf(hashIdx >= 0 ? raw.slice(hashIdx) : "");
  let path = hashIdx >= 0 ? raw.slice(0, hashIdx) : raw;
  const queryIdx = path.indexOf("?");
  if (queryIdx >= 0) path = path.slice(0, queryIdx);
  // A bare `#hash` (or `?query`) addresses the pane's own document.
  if (!path) return SAME_DOC;
  return resolveInPaneDir(path, sec, ctx);
}
