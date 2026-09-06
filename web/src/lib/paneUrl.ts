// W3.P-b — the `?pane2=` reader URL grammar.
//
// THE SCOPE RULING (decided, not re-litigated here): the reader's split is a
// TWO-PANE ARTIFACT COMPARE MODE, not an n-pane workspace. Only an ARTIFACT
// may occupy a pane — no search pane, no gallery pane, no list pane. The
// general "any view in any pane" version collides with four app singletons
// (`useScrollRestoration` keys on pathname+search and assumes WINDOW scroll
// — #31; `useRovingCursor` owns ONE unscoped window keydown listener;
// `useDocumentTitle`/`lastGalleryUrl` are app singletons; every route view
// sits behind a `React.lazy` split boundary that exists to keep CodeMirror /
// the atlas out of first paint). So the grammar below can only ever name an
// artifact, and that is deliberate.
//
// The value rides `?pane2=` on the primary artifact's own permalink
// (`/a/:kb/*`), which is what makes the split SHAREABLE and back/forward
// honest: `routes/detail.tsx` derives the second pane PURELY from this param
// (a `useMemo`, no separate "is a split open" React state — the same
// discipline `?view=` already follows, invariant #23).
//
// ── the wire shape ────────────────────────────────────────────────────────
//
//   pane2 := enc(kb) ":" enc(sourceRelative) [ ":" enc(sec) ]
//
// where `enc` is `encodeURIComponent`. `:` is a SAFE separator precisely
// because `encodeURIComponent` escapes it (`%3A`) — every field is escaped,
// so no kb name, path or heading slug can ever forge a field boundary. The
// value returned here is RAW (once-decoded, i.e. what
// `URLSearchParams.get("pane2")` hands back); `artifactHref`'s own
// `encodeURIComponent` puts it on the wire exactly once.
//
// `parsePane2` is TOTAL: anything malformed yields `null`, never a partial
// object. A partial would be worse than nothing here — a half-parsed pane
// location would render an iframe pointed at a wrong-or-empty artifact
// origin, and the exact-origin message guard (`isOriginOfArtifact`) would
// then silently drop everything that pane posts.

export type PaneLoc = {
  /// The kb id the second pane's artifact lives in. Cross-kb splits are
  /// legal (the grammar carries the kb precisely so a compare can straddle
  /// two corpora).
  kb: string;
  /// Source-relative path — the same value the `/a/:kb/*` splat carries.
  sourceRelative: string;
  /// Optional heading-id deep link inside the second pane (the `?sec=`
  /// analogue). Omitted (not `undefined`-valued) when absent so
  /// `formatPane2(parsePane2(v)!) === v` round-trips byte-exactly.
  sec?: string;
};

/// Serialise a pane location into the RAW `?pane2=` value. The caller (or
/// `artifactHref`) is responsible for the single `encodeURIComponent` that
/// puts it on the wire.
export function formatPane2(loc: PaneLoc): string {
  const base = `${encodeURIComponent(loc.kb)}:${encodeURIComponent(loc.sourceRelative)}`;
  return loc.sec ? `${base}:${encodeURIComponent(loc.sec)}` : base;
}

/// Total parser for the RAW `?pane2=` value. Returns `null` for anything
/// that isn't exactly a well-formed 2- or 3-field location:
///   * a non-string / empty input,
///   * the wrong field count (0, 1 or 4+ `:`-separated fields),
///   * an empty kb / path / sec field,
///   * a field that isn't valid percent-encoding (`decodeURIComponent`
///     throws on e.g. `%zz`),
///   * a path that escapes the corpus (`..` segment) or is absolute.
export function parsePane2(value: string | null | undefined): PaneLoc | null {
  if (typeof value !== "string" || value.length === 0) return null;
  const parts = value.split(":");
  if (parts.length < 2 || parts.length > 3) return null;

  let kb: string;
  let sourceRelative: string;
  let sec: string | undefined;
  try {
    kb = decodeURIComponent(parts[0]);
    sourceRelative = decodeURIComponent(parts[1]);
    sec = parts.length === 3 ? decodeURIComponent(parts[2]) : undefined;
  } catch {
    return null;
  }

  if (!kb || !sourceRelative) return null;
  if (parts.length === 3 && !sec) return null;
  // Path hygiene — the pane's iframe src is built from the artifact id the
  // by-path lookup resolves, so a traversal can't reach outside the corpus
  // anyway; rejecting it here keeps the parser's contract ("a PaneLoc names
  // one indexed artifact") honest and the URL non-confusing.
  if (sourceRelative.startsWith("/")) return null;
  if (sourceRelative.split("/").some((seg) => seg === "..")) return null;

  return sec === undefined ? { kb, sourceRelative } : { kb, sourceRelative, sec };
}

/// True when the two locations name the same artifact (kb + path). Used to
/// refuse a split that would put the SAME artifact origin in both panes —
/// two identical origins would make `isOriginOfArtifact` unable to attribute
/// a `kb:scroll`/`kb:reading` beacon to one pane's visit row (#8/#19).
export function samePane(a: PaneLoc, b: PaneLoc): boolean {
  return a.kb === b.kb && a.sourceRelative === b.sourceRelative;
}
