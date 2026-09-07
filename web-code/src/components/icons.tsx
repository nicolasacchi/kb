import type { SVGProps } from "react";

// F3b/F1 — kb-code's own tiny icon set (there's no shared icon module across
// the two SPAs — see this crate's own build-ownership boundary). Mirrors kb's
// `web/src/components/icons.tsx` visual language (16x16 viewBox, 1.4 stroke,
// currentColor) so the two apps read as the same design family, without
// importing across the app boundary.
type P = SVGProps<SVGSVGElement>;

const base = (size: number) => ({
  viewBox: "0 0 16 16",
  width: size,
  height: size,
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.4,
});

export const Icon = {
  ChevDown: (p: P) => (
    <svg {...base(12)} {...p}>
      <path d="m4 6 4 4 4-4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
  // Outline tab — a simple list glyph (mirrors kb's own `Icon.List`).
  List: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M3 4h10M3 8h10M3 12h10" strokeLinecap="round" />
    </svg>
  ),
  // Provenance tab — a clock (mirrors kb's own `Icon.History`, sans the
  // rewind arrow — kb-code's provenance tab is "what happened", not "go
  // back").
  Clock: (p: P) => (
    <svg {...base(14)} {...p}>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 4.5V8l3 2" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
  // History tab (Wave C) — a clock WITH a rewind arrow, deliberately
  // distinct from the plain `Clock` above (Provenance's "what happened"
  // vs. History's "go back in time" — the two tabs need visually different
  // glyphs even though both are clock-shaped).
  History: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M2.6 8a5.4 5.4 0 1 0 1.6-3.8" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M2.6 3.2v3h3" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M8 5.2V8l2 1.3" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
  // Annotations tab — a note (mirrors kb's own `Icon.Note`).
  Note: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M2.5 3a1 1 0 0 1 1-1h7l3 3v8a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z" />
      <path d="M5 6h6M5 9h6M5 12h4" strokeLinecap="round" />
    </svg>
  ),
  // SH.I1 — synced to web's larger 4→12 geometry (the 3→9 original rendered
  // a 4.5px optical mark at 12px — too faint for a dismiss affordance).
  X: (p: P) => (
    <svg {...base(12)} {...p}>
      <path d="m4 4 8 8M12 4l-8 8" strokeLinecap="round" />
    </svg>
  ),
  // Wave E — the working-set strip's pin toggle (`WorkingSetStrip.tsx`) — a
  // small thumbtack: a round head + a straight point.
  Pin: (p: P) => (
    <svg {...base(12)} {...p}>
      <circle cx="6" cy="4.5" r="2.5" />
      <path d="M6 7v5" strokeLinecap="round" />
    </svg>
  ),
  // F6 — the Search page's EmptyState icon (no-query / no-results) — a
  // plain magnifying glass, mirrors kb's own `Icon.Search` shape without
  // importing across the app boundary (see this module's header doc).
  Search: (p: P) => (
    <svg {...base(16)} {...p}>
      <circle cx="7" cy="7" r="4.5" />
      <path d="m13 13-3-3" strokeLinecap="round" />
    </svg>
  ),
  // F4/F6 — Home's no-repos EmptyState + Commit's unresolvable-sha
  // EmptyState — a simple folder glyph.
  Folder: (p: P) => (
    <svg {...base(16)} {...p}>
      <path d="M2 4.5a1 1 0 0 1 1-1h3l1.4 1.6H13a1 1 0 0 1 1 1V11a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" />
    </svg>
  ),
  // F6 — the Branches page's single-branch EmptyState — a small git-fork
  // glyph (two tips merging into one base).
  Branch: (p: P) => (
    <svg {...base(14)} {...p}>
      <circle cx="4" cy="3.5" r="1.5" />
      <circle cx="4" cy="12.5" r="1.5" />
      <circle cx="11" cy="6.5" r="1.5" />
      <path d="M4 5v5.5M4 8c3 0 4-1 7-1.2" strokeLinecap="round" />
    </svg>
  ),
  // F5 — the mobile-only "reader tools" sheet entry button (`Reader.tsx`'s
  // header, ≤860px): an outer frame with a divider near the right edge,
  // read as "open the side panel" — distinct from `List`'s evenly-spaced
  // full-width rows (reused as-is for the hamburger, since it already IS a
  // 3-line hamburger glyph; adding a near-duplicate here would just be
  // visual noise).
  Panel: (p: P) => (
    <svg {...base(14)} {...p}>
      <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
      <path d="M10 2.5v11" />
    </svg>
  ),
  // V3.N2 — Bookmarks inspector tab: a small flag/bookmark glyph.
  Bookmark: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M4 2.5h8v11l-4-2.5-4 2.5z" strokeLinejoin="round" />
    </svg>
  ),
  // V3.1-H3b — Entity inspector tab: a small diamond/node glyph.
  Entity: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M8 2.5 12.5 8 8 13.5 3.5 8z" strokeLinejoin="round" />
    </svg>
  ),
  Sun: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round">
      <circle cx="8" cy="8" r="2.5" />
      <path d="M8 1.5v1.5M8 13v1.5M1.5 8h1.5M13 8h1.5M3.4 3.4l1 1M11.6 11.6l1 1M3.4 12.6l1-1M11.6 4.4l1-1" />
    </svg>
  ),
  Check: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 8.5l3.5 3.5L13 4.5" />
    </svg>
  ),
  External: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M9 3h4v4M13 3 7.5 8.5M11 9.5V13H3V5h3.5" />
    </svg>
  ),
  More: (p: P) => (
    <svg {...base(14)} {...p}>
      <circle cx="3" cy="8" r="1" />
      <circle cx="8" cy="8" r="1" />
      <circle cx="13" cy="8" r="1" />
    </svg>
  ),
  Chevron: (p: P) => (
    <svg {...base(12)} {...p}>
      <path d="m6 4 4 4-4 4" />
    </svg>
  ),
  Warn: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M7 2.5 1.5 12.5h11z" />
      <path d="M7 6v2.5M7 10.5v.5" />
    </svg>
  ),
  Copy: (p: P) => (
    <svg {...base(14)} {...p}>
      <rect x="5" y="5" width="9" height="9" rx="1" />
      <path d="M3 11V3a1 1 0 0 1 1-1h7" />
    </svg>
  ),
  Plus: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round">
      <path d="M8 3v10M3 8h10" />
    </svg>
  ),
  Comment: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M2 3.5h10a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H6l-3 2.5v-2.5H2a1 1 0 0 1-1-1v-5a1 1 0 0 1 1-1z" />
    </svg>
  ),
  Expand: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M6 2.5H2.5V6M10 2.5h3.5V6M6 13.5H2.5V10M10 13.5h3.5V10" />
    </svg>
  ),
  ArrowLeft: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="m7 4-4 4 4 4M3 8h10" />
    </svg>
  ),
  Terminal: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <rect x="1.5" y="2.5" width="11" height="9" rx="1.2" />
      <path d="M3.5 5.5l1.6 1.5-1.6 1.5" />
      <path d="M6.8 9.2h3" />
    </svg>
  ),
  Grid: (p: P) => (
    <svg {...base(14)} {...p}>
      <rect x="2" y="2" width="5" height="5" rx="0.5" />
      <rect x="9" y="2" width="5" height="5" rx="0.5" />
      <rect x="2" y="9" width="5" height="5" rx="0.5" />
      <rect x="9" y="9" width="5" height="5" rx="0.5" />
    </svg>
  ),
  Graph: (p: P) => (
    <svg {...base(14)} {...p}>
      <circle cx="3.5" cy="3.5" r="1.5" />
      <circle cx="12" cy="6" r="1.5" />
      <circle cx="5" cy="12" r="1.5" />
      <circle cx="13" cy="13" r="1.2" />
      <path d="m4.5 4.5 6 1M4.5 11l6.5 1M5 4.5 5 10.5M11 7l1.5 5" />
    </svg>
  ),
  Tasks: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 3.5l1.2 1.2L6 2.8" />
      <path d="M3 8l1.2 1.2L6 7.3" />
      <path d="M3 12.5l1.2 1.2L6 11.8" />
      <path d="M8.5 4h5M8.5 8.5h5M8.5 13h5" />
    </svg>
  ),
  Pen: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="m11 2 3 3-8 8H3v-3z" />
      <path d="m9.5 3.5 3 3" />
    </svg>
  ),
  Spark: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M8 1v4M8 11v4M1 8h4M11 8h4M3.5 3.5l2 2M10.5 10.5l2 2M3.5 12.5l2-2M10.5 5.5l2-2" />
    </svg>
  ),
  // Compare base⇄head flip: two opposed arrows on offset rows.
  Swap: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 5.5h8M9 3.5l2 2-2 2" />
      <path d="M13 10.5H5M7 8.5l-2 2 2 2" />
    </svg>
  ),
  // Reload/refetch: ~270° arc with an arrowhead at the open end.
  Refresh: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 8a5 5 0 1 1 5-5" />
      <path d="M8 3h3.2v3.2" />
    </svg>
  ),
  // Visibility / viewed: almond outline + pupil.
  Eye: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M1.8 8C3.6 4.8 5.6 3.2 8 3.2s4.4 1.6 6.2 4.8C12.4 11.2 10.4 12.8 8 12.8S3.6 11.2 1.8 8z" />
      <circle cx="8" cy="8" r="1.6" />
    </svg>
  ),
  // Hotspot / heat: teardrop flame + inner tongue.
  Flame: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M8 2C6.4 5 4 6.6 4 9.6a4 4 0 0 0 8 0c0-3-2.4-4.6-4-7.6z" />
      <path d="M8 8.4c-.6 1.1-1.5 1.7-1.5 2.7A1.5 1.5 0 0 0 8 12.6" />
    </svg>
  ),
  // Stacked surfaces / patchsets: three flattened diamonds.
  Layers: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M8 1.5 14 4.5 8 7.5 2 4.5z" />
      <path d="M2 8 8 11 14 8" />
      <path d="M2 11.5 8 14.5 14 11.5" />
    </svg>
  ),
  // Pull request: left rail + elbow into a third node.
  PullRequest: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <circle cx="4" cy="3.5" r="1.5" />
      <circle cx="4" cy="12.5" r="1.5" />
      <circle cx="12" cy="12.5" r="1.5" />
      <path d="M4 5v5.5" />
      <path d="M5.5 3.5H8a4 4 0 0 1 4 4v3.5" />
    </svg>
  ),
  // Review checklist / copied-ok: clipboard with a check inside.
  ClipboardCheck: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <rect x="3.5" y="3.5" width="9" height="11" rx="1" />
      <rect x="5.5" y="1.5" width="5" height="3" rx="0.7" />
      <path d="M5.8 9.2l1.8 1.8 3.2-3.5" />
    </svg>
  ),
  // Side-by-side compare: frame with a CENTER divider (Panel's is off-centre).
  SplitView: (p: P) => (
    <svg {...base(14)} {...p}>
      <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
      <path d="M8 2.5v11" />
    </svg>
  ),
  // Unified/list diff: frame with +/− prefixed text rows.
  UnifiedView: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round">
      <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
      <path d="M4.2 6.2h1.6M5 5.4v1.6M7.2 6.2h4.4" />
      <path d="M4.2 10.2h1.6M7.2 10.2h4.4" />
    </svg>
  ),
  // Inverse of Expand: four corner arrows pointing inward.
  Collapse: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M2.5 6H6V2.5M13.5 6H10V2.5M2.5 10H6V13.5M13.5 10H10V13.5" />
    </svg>
  ),
  // Orphaned comment anchors: chain links pulled apart with a gap slash.
  Unlink: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M6.4 9.6 5 11.1a2.2 2.2 0 1 1-3.1-3.1L3.5 6.4" />
      <path d="M9.6 6.4 11 4.9a2.2 2.2 0 1 1 3.1 3.1L12.5 9.6" />
      <path d="M10.6 4.2 5.4 11.8" />
    </svg>
  ),
  // Create-branch: Branch glyph with the top-right node replaced by a +.
  BranchPlus: (p: P) => (
    <svg {...base(14)} {...p}>
      <circle cx="4" cy="3.5" r="1.5" />
      <circle cx="4" cy="12.5" r="1.5" />
      <path d="M4 5v5.5M4 8c3 0 4-1 7-1.2" strokeLinecap="round" />
      <path d="M11 4.5v4M9 6.5h4" strokeLinecap="round" />
    </svg>
  ),
  // SH.I1 — Browser's "open in reader" links (was a bare ↗ text glyph): an
  // arrow entering a page through its open left edge. Deliberately distinct
  // from External (box + arrow LEAVING) — this one navigates WITHIN the app.
  OpenInReader: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M6.2 2.5h4.3l3 3v8H6.2" />
      <path d="M10.5 2.5v3h3" />
      <path d="M1.5 8h6M5.3 5.8 7.5 8l-2.2 2.2" />
    </svg>
  ),
  // V70-A7 — the theme control's glyph is now STATE-BEARING (recon R11: the
  // Sun was identical in all three states and the only signal was a title
  // attribute). Sun = light, Moon = dark, Contrast = follow the OS. These
  // three names do not exist in web/'s icon set, so the cross-SPA parity
  // golden (`lib/iconParity.test.ts`, which compares SHARED names only)
  // is unaffected.
  Moon: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M13 9.6A5.6 5.6 0 0 1 6.4 3a5.6 5.6 0 1 0 6.6 6.6z" />
    </svg>
  ),
  Contrast: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <circle cx="8" cy="8" r="5.6" />
      <path d="M8 2.4v11.2a5.6 5.6 0 0 0 0-11.2z" fill="currentColor" stroke="none" />
    </svg>
  ),
  Palette: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M8 1.8a6.2 6.2 0 1 0 0 12.4c1 0 1.5-.6 1.5-1.3 0-.8-.7-1.2-.7-1.9 0-.6.5-1 1.2-1h1.3a3 3 0 0 0 2.9-3C14.2 4 11.4 1.8 8 1.8z" />
      <circle cx="5.2" cy="6" r=".9" fill="currentColor" stroke="none" />
      <circle cx="8" cy="4.6" r=".9" fill="currentColor" stroke="none" />
      <circle cx="10.9" cy="6.2" r=".9" fill="currentColor" stroke="none" />
    </svg>
  ),
  // V74-L3b — the three STATE-BEARING glyphs kbc-trail/1's indicator needs
  // (D17: the opt-in must be VISIBLE), plus `Fork` for a trail step's "I
  // went another way from here" chip. Three glyphs rather than one plus a
  // title string, for the same R11 reason the theme control was re-cut in
  // V70-A7: a control whose only signal is a `title` attribute is not an
  // indicator.
  //
  // `Pause` and `Dot` ALREADY EXIST in `web/`'s icon set, so their bodies
  // here are that file's, byte-for-byte — `lib/iconParity.test.ts` compares
  // SHARED names and a "close enough" copy is exactly the drift it exists to
  // catch. `Record` and `Fork` are new names in both senses (absent from
  // `web/`), so the golden does not constrain them.
  Record: (p: P) => (
    <svg {...base(14)} {...p}>
      <circle cx="8" cy="8" r="4.2" fill="currentColor" stroke="none" />
    </svg>
  ),
  Pause: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round">
      <path d="M5.6 3.2v9.6M10.4 3.2v9.6" />
    </svg>
  ),
  Dot: (p: P) => (
    <svg viewBox="0 0 8 8" width="8" height="8" {...p}>
      <circle cx="4" cy="4" r="3" fill="currentColor" />
    </svg>
  ),
  Fork: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M5 2.8v4.4a2 2 0 0 0 2 2h4" />
      <path d="M5 9.2v4" />
      <circle cx="5" cy="2" r="1.4" />
      <circle cx="12.2" cy="9.2" r="1.4" />
      <circle cx="5" cy="14" r="1.4" />
    </svg>
  ),
};
