// Pure breadcrumb derivation for the reader's top bar: "repo / dir / dir /
// file.rs", every segment clickable (the repo crumb navigates to the repo
// root tree, each directory crumb to that directory, the last segment is
// the current file/dir and renders non-interactive).

import { codeBasePath, codeUrl, type LineSel, type PaneLoc } from "./codeUrl";

export interface Breadcrumb {
  label: string;
  /// Repo-relative path this crumb navigates to (`""` = repo root). The
  /// repo-name crumb itself always carries `path: ""`.
  path: string;
  isCurrent: boolean;
}

/// `repo` is rendered as the first crumb; `path` (possibly `""` for the
/// repo root) is split on `/` into one crumb per segment. Empty path
/// segments (a stray leading/trailing/doubled slash) are dropped rather
/// than rendered as blank crumbs.
export function buildBreadcrumbs(repo: string, path: string): Breadcrumb[] {
  const segments = path.split("/").filter((s) => s !== "");
  const crumbs: Breadcrumb[] = [{ label: repo, path: "", isCurrent: segments.length === 0 }];
  let acc = "";
  segments.forEach((seg, i) => {
    acc = acc === "" ? seg : `${acc}/${seg}`;
    crumbs.push({ label: seg, path: acc, isCurrent: i === segments.length - 1 });
  });
  return crumbs;
}

/// Build the reader's client-route URL for `repo`/`path`, optionally
/// pinned to `ref` (a branch/tag/sha — omitted means "working tree",
/// mirroring the server's own `GET /api/file` no-`ref` default) and/or a
/// 1-based `line` to scroll to (`Reader.tsx` reads `?line=` into its
/// `gotoLine` state on mount — the Search-Everywhere box's file/symbol/
/// text/semantic lanes are the other callers of this param, landing on a
/// hit's exact line). `path` empty renders the repo-root tree URL. Every
/// clickable surface in the reader (breadcrumbs, the file tree, the ref
/// picker, "back to file" from the diff view, the omnibox/search page)
/// goes through this ONE builder — mirrors kb's own `galleryUrl`
/// discipline (invariant #35 in kb's CLAUDE.md) so the URL grammar stays in
/// one place rather than drifting across components.
///
/// Delegates to `codeUrl.ts` (A2) — this signature and output are
/// preserved byte-for-byte (`breadcrumbs.test.ts` pins it unchanged);
/// `codeUrl.ts` is the actual single builder now, with line-RANGE +
/// (reserved) split-pane support this narrower signature doesn't expose.
export function readerUrl(repo: string, path: string, ref?: string, line?: number): string {
  return codeUrl({ repo, path, ref, line });
}

export interface CrumbHrefOpts {
  ref?: string;
  /// V70-A3S — the CURRENT cursor line, threaded through so a breadcrumb
  /// click doesn't silently drop it (a directory-ancestor crumb just
  /// ignores an inapplicable `line=`, same as any other reader URL with no
  /// matching content).
  line?: LineSel;
  /// V70-A3S — the live Wave E second-pane location (`Reader.tsx`'s
  /// `pane2Loc`), when a split is open. `null`/`undefined` both omit
  /// `pane2=` — `readerUrl`'s narrower 4-arg signature predates pane2
  /// entirely (this module's own doc on `codeUrl.ts` being "the actual
  /// single builder now, with … (reserved) split-pane support this
  /// narrower signature doesn't expose"), so breadcrumbs call `codeUrl`
  /// directly instead of delegating to `readerUrl`.
  pane2?: PaneLoc | null;
}

/// Build ONE breadcrumb segment's href. Same grammar as `readerUrl`, but
/// (V70-A3S) also carries the current line + a live split through — before
/// this, every breadcrumb click silently closed an open Wave E split and
/// dropped the cursor line, since `readerUrl`'s narrower signature has no
/// `pane2` slot at all.
export function crumbHref(repo: string, path: string, opts?: CrumbHrefOpts): string {
  return codeUrl({ repo, path, ref: opts?.ref, line: opts?.line, pane2: opts?.pane2 ?? undefined });
}

/// Build the reader's diff-view URL: same repo/path, plus `from`/`to` query
/// params (`to` omitted = working tree, same convention as `GET /api/diff`).
/// `path` is always a FILE (diff is a per-file view, never a whole-repo
/// one — `~diff` is appended as its own path segment, never merged with an
/// empty `path` into a double slash).
///
/// Delegates its path-segment encoding to `codeUrl.ts`'s `codeBasePath`
/// (the diff view's own `from`/`to` query grammar doesn't fit `codeUrl`'s
/// `ref`/`line`/`pane2` shape, so this stays a distinct builder — but
/// shares the one encoding routine rather than duplicating it).
export function diffUrl(repo: string, path: string, from: string, to?: string): string {
  const params = new URLSearchParams({ from });
  if (to) params.set("to", to);
  return `${codeBasePath(repo, path)}/~diff?${params.toString()}`;
}
