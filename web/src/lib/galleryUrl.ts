// v0.22 — the single source of truth for "reader → gallery" deep-links.
//
// Every clickable metadata atom in the reader (a tag pill, the category,
// each folder breadcrumb segment, the mtime "modified around" pivot, the
// "Explore from here" facet chips) routes through `galleryUrl()` so the
// gallery's URL grammar lives in ONE place and can't drift. It mirrors
// `navItems.withKb` but for the full filter axis set.
//
// The gallery (route "/") reads each of these params (see `gallery.tsx`):
//   kb        — pins the active corpus (the gallery is kb-scoped)
//   tags      — csv, any-of
//   category  — exact kb-category (v0.22 server filter)
//   folder    — descendant-inclusive prefix
//   folder_exact — `1` when true AND folder is set (exact folder only;
//               v0.33 Y1). Absent = today's descendant-inclusive default.
//   from/to   — absolute mtime window, unix seconds (v0.22 server filter)
//   read      — csv read-state facet (W1.A server filter; see routes/docs.rs)
//   ids       — csv id-set filter (W2.3a atlas-lasso / working-set pivot;
//               see kb_core::docs_query's `ids` gate + routes/docs.rs)
//   sort/dir  — recent|indexed|created|title|words · asc|desc
//
// Empty / null / undefined values are omitted so the landing URL only
// carries the axes the caller actually set. A bare `galleryUrl(kb)` is
// the unfiltered kb gallery (what a root-level "(root)" folder links to).

export type GallerySort = "recent" | "indexed" | "created" | "title" | "words";

/// W1.A — the docs-gallery read-state facet token. Mirrors the server's
/// `ReadToken` (`routes/docs.rs`): `never-opened` means the artifact was
/// never opened AND carries no list read-override (id absent from the
/// reading rollup); the other three are `kb_core::lists::ReadState`
/// equality on a row that IS present in the rollup.
export type ReadFacet = "never-opened" | "unread" | "in_progress" | "read";

export type GalleryFilters = {
  tags?: string[];
  category?: string | null;
  folder?: string | null;
  /// v0.33 Y1 — exact-folder only (`folder_exact=1`). Meaningful only with
  /// a non-empty `folder`; false/absent keeps descendant-inclusive matching.
  folderExact?: boolean;
  /// mtime window bounds in unix seconds (inclusive); either may stand alone.
  from?: number | null;
  to?: number | null;
  /// W1.A — csv, order-preserved (any-of).
  read?: ReadFacet[];
  /// W2.3a — csv id-set membership filter (the atlas lasso / working-set
  /// gallery pivot). Order-preserved, any-of; empty/absent = no constraint.
  ids?: string[];
  sort?: GallerySort;
  dir?: "asc" | "desc";
};

export function galleryUrl(kb: string | null, f: GalleryFilters = {}): string {
  const p = new URLSearchParams();
  if (kb) p.set("kb", kb);
  if (f.tags && f.tags.length > 0) p.set("tags", f.tags.join(","));
  if (f.category) p.set("category", f.category);
  // A root-level doc (folder === "") links to the unfiltered kb gallery —
  // there is no "root only" filter, so an empty folder is intentionally
  // dropped (descendant-inclusive matching makes "" mean "everything").
  if (f.folder) p.set("folder", f.folder);
  // After `folder` so existing goldens stay byte-identical when the flag
  // is absent/false (same pattern as `read`/`ids` — only serialised when on).
  if (f.folderExact && f.folder) p.set("folder_exact", "1");
  if (f.from != null) p.set("from", String(Math.floor(f.from)));
  if (f.to != null) p.set("to", String(Math.floor(f.to)));
  if (f.read && f.read.length > 0) p.set("read", f.read.join(","));
  if (f.ids && f.ids.length > 0) p.set("ids", f.ids.join(","));
  if (f.sort) p.set("sort", f.sort);
  if (f.dir) p.set("dir", f.dir);
  const qs = p.toString();
  return qs ? `/?${qs}` : "/";
}
