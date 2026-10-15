import type { SVGProps } from "react";

// The ONE icon set for kb (web/) and kb-code (web-code/). 16x16 viewBox,
// 1.4 stroke, currentColor. Union of the two apps' former icons.tsx files:
// every name the two shared had identical shapes, so no glyph was dropped.
// Default render sizes (the `base(N)` argument) are per-name and unchanged
// from what each app rendered before; `createIconSet` lets an app override
// individual names so its existing call sites keep their rendered size.
type P = SVGProps<SVGSVGElement>;

const base = (size: number) => ({
  viewBox: "0 0 16 16",
  width: size,
  height: size,
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.4,
});

/** Default render size (px) of every glyph that is drawn through `base()`. */
export const DEFAULT_ICON_SIZES = {
  Search: 14,
  Grid: 14,
  List: 14,
  Inbox: 14,
  Atlas: 14,
  Press: 14,
  History: 14,
  Settings: 14,
  Chevron: 12,
  ChevDown: 12,
  Arrow: 14,
  ArrowLeft: 14,
  Graph: 14,
  Doc: 14,
  Copy: 14,
  Check: 14,
  Expand: 14,
  External: 14,
  X: 14,
  More: 14,
  Code: 14,
  Spark: 14,
  Table: 14,
  BookOpen: 14,
  Cmd: 12,
  Pen: 14,
  Note: 14,
  Mark: 16,
  Download: 14,
  Share: 14,
  Anchor: 14,
  Bookmark: 14,
  Tasks: 14,
  Sun: 14,
  Brain: 14,
  Warn: 14,
  Terminal: 14,
  Comment: 14,
  Plus: 14,
  Trash: 14,
  Refresh: 14,
  Play: 14,
  Pause: 14,
  Paperclip: 14,
  ImageFrame: 14,
  Link: 14,
  ThumbUp: 14,
  ThumbDown: 14,
  Folder: 14,
  Pin: 12,
  Slate: 14,
  Star: 14,
  Clock: 14,
  File: 14,
  Branch: 14,
  Panel: 14,
  Entity: 14,
  Facts: 14,
  Swap: 14,
  Eye: 14,
  Flame: 14,
  Layers: 14,
  PullRequest: 14,
  ClipboardCheck: 14,
  SplitView: 14,
  UnifiedView: 14,
  Collapse: 14,
  Unlink: 14,
  BranchPlus: 14,
  OpenInReader: 14,
  Moon: 14,
  Contrast: 14,
  Palette: 14,
  Record: 14,
  Fork: 14,
} as const;

export type IconSizeOverrides = Partial<Record<keyof typeof DEFAULT_ICON_SIZES, number>>;

/** Build the icon map, optionally overriding the default size of some names. */
export function createIconSet(overrides: IconSizeOverrides = {}) {
  const sz = (name: keyof typeof DEFAULT_ICON_SIZES): number =>
    overrides[name] ?? DEFAULT_ICON_SIZES[name];
  return {
    // F6 — the Search page's EmptyState icon (no-query / no-results) — a
    // plain magnifying glass, mirrors kb's own `Icon.Search` shape without
    // importing across the app boundary (see this module's header doc).
    Search: (p: P) => (
      <svg {...base(sz("Search"))} {...p}>
        <circle cx="7" cy="7" r="4.5" />
        <path d="m13 13-3-3" strokeLinecap="round" />
      </svg>
    ),
    Grid: (p: P) => (
      <svg {...base(sz("Grid"))} {...p}>
        <rect x="2" y="2" width="5" height="5" rx="0.5" />
        <rect x="9" y="2" width="5" height="5" rx="0.5" />
        <rect x="2" y="9" width="5" height="5" rx="0.5" />
        <rect x="9" y="9" width="5" height="5" rx="0.5" />
      </svg>
    ),
    // Outline tab — a simple list glyph (mirrors kb's own `Icon.List`).
    List: (p: P) => (
      <svg {...base(sz("List"))} {...p}>
        <path d="M3 4h10M3 8h10M3 12h10" strokeLinecap="round" />
      </svg>
    ),
    // OL-P1 — a tray: open-top box with the shelf notch, distinct from `List`
    // (which is also the hamburger).
    Inbox: (p: P) => (
      <svg {...base(sz("Inbox"))} {...p}>
        <path d="M2 9.5l1.7-5.6A1 1 0 0 1 4.65 3.2h6.7a1 1 0 0 1 .95.7L14 9.5v2.7a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V9.5z" strokeLinejoin="round" />
        <path d="M2 9.5h3.2a1 1 0 0 1 1 .9 1.8 1.8 0 0 0 3.6 0 1 1 0 0 1 1-.9H14" strokeLinejoin="round" />
      </svg>
    ),
    Atlas: (p: P) => (
      <svg {...base(sz("Atlas"))} {...p} strokeWidth={1.5}>
        <circle cx="4" cy="4" r="1.6" />
        <circle cx="12" cy="5" r="1.6" />
        <circle cx="8" cy="11" r="1.6" />
        <path d="M5 5l5 0M5 5l3 5M11 6l-2 4" strokeLinecap="round" />
      </svg>
    ),
    // W2.5 — the broadsheet "press" gallery view: a front page (rule + columns).
    Press: (p: P) => (
      <svg {...base(sz("Press"))} {...p} strokeWidth={1.5}>
        <rect x="2" y="3" width="12" height="10" rx="1" />
        <path d="M4.5 6h7M4.5 8.5h3M4.5 10.5h3M9.5 8.5h2M9.5 10.5h2" strokeLinecap="round" />
      </svg>
    ),
    // History tab (Wave C) — a clock WITH a rewind arrow, deliberately
    // distinct from the plain `Clock` above (Provenance's "what happened"
    // vs. History's "go back in time" — the two tabs need visually different
    // glyphs even though both are clock-shaped).
    History: (p: P) => (
      <svg {...base(sz("History"))} {...p}>
        <path d="M2.6 8a5.4 5.4 0 1 0 1.6-3.8" strokeLinecap="round" strokeLinejoin="round" />
        <path d="M2.6 3.2v3h3" strokeLinecap="round" strokeLinejoin="round" />
        <path d="M8 5.2V8l2 1.3" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    ),
    // SH.I1 — redesigned from the old 8-tick sunburst (which visually twinned
    // Icon.Sun sitting right beside it in the Header) into three sliders with
    // staggered knobs: reads as "preferences", never as "brightness".
    Settings: (p: P) => (
      <svg {...base(sz("Settings"))} {...p} strokeLinecap="round">
        <path d="M2.5 4.5h4.5M10.2 4.5h3.3" />
        <circle cx="8.6" cy="4.5" r="1.6" />
        <path d="M5.7 8h7.8" />
        <circle cx="4.1" cy="8" r="1.6" />
        <path d="M2.5 11.5h7.8" />
        <circle cx="11.9" cy="11.5" r="1.6" />
      </svg>
    ),
    Chevron: (p: P) => (
      <svg {...base(sz("Chevron"))} {...p}>
        <path d="m6 4 4 4-4 4" />
      </svg>
    ),
    ChevDown: (p: P) => (
      <svg {...base(sz("ChevDown"))} {...p}>
        <path d="m4 6 4 4 4-4" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    ),
    Arrow: (p: P) => (
      <svg {...base(sz("Arrow"))} {...p}>
        <path d="m9 4 4 4-4 4M13 8H3" />
      </svg>
    ),
    ArrowLeft: (p: P) => (
      <svg {...base(sz("ArrowLeft"))} {...p}>
        <path d="m7 4-4 4 4 4M3 8h10" />
      </svg>
    ),
    Graph: (p: P) => (
      <svg {...base(sz("Graph"))} {...p}>
        <circle cx="3.5" cy="3.5" r="1.5" />
        <circle cx="12" cy="6" r="1.5" />
        <circle cx="5" cy="12" r="1.5" />
        <circle cx="13" cy="13" r="1.2" />
        <path d="m4.5 4.5 6 1M4.5 11l6.5 1M5 4.5 5 10.5M11 7l1.5 5" />
      </svg>
    ),
    Doc: (p: P) => (
      <svg {...base(sz("Doc"))} {...p}>
        <path d="M3.5 1.5h6L13 5v9.5H3.5z" />
        <path d="M9.5 1.5V5h3.5" />
      </svg>
    ),
    Copy: (p: P) => (
      <svg {...base(sz("Copy"))} {...p}>
        <rect x="5" y="5" width="9" height="9" rx="1" />
        <path d="M3 11V3a1 1 0 0 1 1-1h7" />
      </svg>
    ),
    Check: (p: P) => (
      <svg {...base(sz("Check"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 8.5l3.5 3.5L13 4.5" />
      </svg>
    ),
    Expand: (p: P) => (
      <svg {...base(sz("Expand"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M6 2.5H2.5V6M10 2.5h3.5V6M6 13.5H2.5V10M10 13.5h3.5V10" />
      </svg>
    ),
    External: (p: P) => (
      <svg {...base(sz("External"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M9 3h4v4M13 3 7.5 8.5M11 9.5V13H3V5h3.5" />
      </svg>
    ),
    // SH.I1 — synced to web's larger 4→12 geometry (the 3→9 original rendered
    // a 4.5px optical mark at 12px — too faint for a dismiss affordance).
    X: (p: P) => (
      <svg {...base(sz("X"))} {...p}>
        <path d="m4 4 8 8M12 4l-8 8" strokeLinecap="round" />
      </svg>
    ),
    More: (p: P) => (
      <svg {...base(sz("More"))} {...p}>
        <circle cx="3" cy="8" r="1" />
        <circle cx="8" cy="8" r="1" />
        <circle cx="13" cy="8" r="1" />
      </svg>
    ),
    Code: (p: P) => (
      <svg {...base(sz("Code"))} {...p}>
        <path d="m5 5-3 3 3 3M11 5l3 3-3 3M9 3 7 13" />
      </svg>
    ),
    Spark: (p: P) => (
      <svg {...base(sz("Spark"))} {...p}>
        <path d="M8 1v4M8 11v4M1 8h4M11 8h4M3.5 3.5l2 2M10.5 10.5l2 2M3.5 12.5l2-2M10.5 5.5l2-2" />
      </svg>
    ),
    Table: (p: P) => (
      <svg {...base(sz("Table"))} {...p}>
        <rect x="2" y="3" width="12" height="10" rx="0.5" />
        <path d="M2 7h12M2 10h12M6 7v6M10 7v6" />
      </svg>
    ),
    BookOpen: (p: P) => (
      <svg {...base(sz("BookOpen"))} {...p}>
        <path d="M8 4v9M2 3.5h3a2.5 2.5 0 0 1 3 2.5v7c0-1.5-1-2.5-2.5-2.5H2zM14 3.5h-3A2.5 2.5 0 0 0 8 6v7c0-1.5 1-2.5 2.5-2.5H14z" />
      </svg>
    ),
    Cmd: (p: P) => (
      <svg {...base(sz("Cmd"))} {...p}>
        <path d="M5 3.5a1.5 1.5 0 1 0 1.5 1.5V11A1.5 1.5 0 1 0 5 12.5h6A1.5 1.5 0 1 0 12.5 11V5A1.5 1.5 0 1 0 11 3.5H5" />
      </svg>
    ),
    Dot: (p: P) => (
      <svg viewBox="0 0 8 8" width="8" height="8" {...p}>
        <circle cx="4" cy="4" r="3" fill="currentColor" />
      </svg>
    ),
    Pen: (p: P) => (
      <svg {...base(sz("Pen"))} {...p}>
        <path d="m11 2 3 3-8 8H3v-3z" />
        <path d="m9.5 3.5 3 3" />
      </svg>
    ),
    // Annotations tab — a note (mirrors kb's own `Icon.Note`).
    Note: (p: P) => (
      <svg {...base(sz("Note"))} {...p}>
        <path d="M2.5 3a1 1 0 0 1 1-1h7l3 3v8a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z" />
        <path d="M5 6h6M5 9h6M5 12h4" strokeLinecap="round" />
      </svg>
    ),
    Mark: (p: P) => (
      <svg {...base(sz("Mark"))} {...p} strokeWidth={1.6} strokeLinecap="square">
        <path d="M3 2v12M3 8l5-5M3 8l5 5" />
      </svg>
    ),
    // Download — arrow into a tray. Single-artifact + folder-zip download
    // controls (FloatingPill, siblings popover, gallery card/row + header).
    Download: (p: P) => (
      <svg {...base(sz("Download"))} {...p}>
        <path d="M8 2v8M5 7l3 3 3-3" strokeLinecap="round" strokeLinejoin="round" />
        <path d="M3 13h10" strokeLinecap="round" />
      </svg>
    ),
    // Share — arrow up out of a tray (the inverse of Download). Publishes
    // an artifact to a gated/public static URL (FloatingPill "Share…").
    Share: (p: P) => (
      <svg {...base(sz("Share"))} {...p}>
        <path d="M8 10V2M5 5l3-3 3 3" strokeLinecap="round" strokeLinejoin="round" />
        <path d="M3 9v4h10V9" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    ),
    // v0.10 D2 — chrome additions. Anchor (corkboard pill in the Header),
    // Bookmark (saved-queries button placeholder, lit up in Q4), Sun
    // (theme toggle), Brain (the design's icon for the Memory view in
    // the toggle strip), Warn (status-bar warn cluster).
    Anchor: (p: P) => (
      <svg {...base(sz("Anchor"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <circle cx="8" cy="3" r="1.4" />
        <path d="M8 4.5v9.5" />
        <path d="M3 8.5H2a6 6 0 0 0 12 0h-1" />
        <path d="M5.5 10h5" />
      </svg>
    ),
    // V3.N2 — Bookmarks inspector tab: a small flag/bookmark glyph.
    Bookmark: (p: P) => (
      <svg {...base(sz("Bookmark"))} {...p}>
        <path d="M4 2.5h8v11l-4-2.5-4 2.5z" strokeLinejoin="round" />
      </svg>
    ),
    Tasks: (p: P) => (
      <svg {...base(sz("Tasks"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 3.5l1.2 1.2L6 2.8" />
        <path d="M3 8l1.2 1.2L6 7.3" />
        <path d="M3 12.5l1.2 1.2L6 11.8" />
        <path d="M8.5 4h5M8.5 8.5h5M8.5 13h5" />
      </svg>
    ),
    Sun: (p: P) => (
      <svg {...base(sz("Sun"))} {...p} strokeLinecap="round">
        <circle cx="8" cy="8" r="2.5" />
        <path d="M8 1.5v1.5M8 13v1.5M1.5 8h1.5M13 8h1.5M3.4 3.4l1 1M11.6 11.6l1 1M3.4 12.6l1-1M11.6 4.4l1-1" />
      </svg>
    ),
    Brain: (p: P) => (
      <svg {...base(sz("Brain"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M8 3a1.8 1.8 0 0 0-3.2-1A1.5 1.5 0 0 0 3 3.6 1.5 1.5 0 0 0 2.4 6.3 1.8 1.8 0 0 0 3.4 9.6 1.8 1.8 0 0 0 6.2 11 1.8 1.8 0 0 0 8 12.5" />
        <path d="M8 3a1.8 1.8 0 0 1 3.2-1A1.5 1.5 0 0 1 13 3.6 1.5 1.5 0 0 1 13.6 6.3 1.8 1.8 0 0 1 12.6 9.6 1.8 1.8 0 0 1 9.8 11 1.8 1.8 0 0 1 8 12.5" />
        <path d="M8 3v9.5" />
      </svg>
    ),
    Warn: (p: P) => (
      <svg {...base(sz("Warn"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M7 2.5 1.5 12.5h11z" />
        <path d="M7 6v2.5M7 10.5v.5" />
      </svg>
    ),
    Terminal: (p: P) => (
      <svg {...base(sz("Terminal"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <rect x="1.5" y="2.5" width="11" height="9" rx="1.2" />
        <path d="M3.5 5.5l1.6 1.5-1.6 1.5" />
        <path d="M6.8 9.2h3" />
      </svg>
    ),
    Comment: (p: P) => (
      <svg {...base(sz("Comment"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M2 3.5h10a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H6l-3 2.5v-2.5H2a1 1 0 0 1-1-1v-5a1 1 0 0 1 1-1z" />
      </svg>
    ),
    Plus: (p: P) => (
      <svg {...base(sz("Plus"))} {...p} strokeLinecap="round">
        <path d="M8 3v10M3 8h10" />
      </svg>
    ),
    // SH.I1 — drawn set replacing the unicode/emoji stand-ins that grew across
    // the settings/comment/notes stacks (design-audit 2026-08-20). Same grammar
    // as everything above: 16-unit grid, 1.4 stroke, currentColor, no fill.
    // Outline can: lid, swing-top, tapered body, two ticks. Destructive verbs
    // (drop kb, purge, revoke) — color comes from the call site, never baked in.
    Trash: (p: P) => (
      <svg {...base(sz("Trash"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M2.5 4.2h11" />
        <path d="M5.7 4.2V3a1 1 0 0 1 1-1h2.6a1 1 0 0 1 1 1v1.2" />
        <path d="M4 4.2l.7 9a1.2 1.2 0 0 0 1.2 1.1h4.2a1.2 1.2 0 0 0 1.2-1.1l.7-9" />
        <path d="M6.6 7v4.4M9.4 7v4.4" />
      </svg>
    ),
    // Reload/refetch: ~270° arc with an arrowhead at the open end.
    Refresh: (p: P) => (
      <svg {...base(sz("Refresh"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 8a5 5 0 1 1 5-5" />
        <path d="M8 3h3.2v3.2" />
      </svg>
    ),
    // Resume/pause pair for Live tail + source toggles (was ▶/⏸ dingbats).
    Play: (p: P) => (
      <svg {...base(sz("Play"))} {...p} strokeLinejoin="round">
        <path d="M5.2 3.2v9.6l7.6-4.8z" />
      </svg>
    ),
    Pause: (p: P) => (
      <svg {...base(sz("Pause"))} {...p} strokeLinecap="round">
        <path d="M5.6 3.2v9.6M10.4 3.2v9.6" />
      </svg>
    ),
    // Attachment paperclip (was the 📎 emoji across the composer stack).
    Paperclip: (p: P) => (
      <svg {...base(sz("Paperclip"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="m14.2 7.5-6 6a4 4 0 0 1-5.7-5.7l6-6a2.7 2.7 0 0 1 3.8 3.8l-6 6A1.35 1.35 0 0 1 4.4 9.7l5.6-5.6" />
      </svg>
    ),
    // Picture frame + horizon (was 🖼; composer attach + broken-image fallback).
    ImageFrame: (p: P) => (
      <svg {...base(sz("ImageFrame"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <rect x="2" y="3" width="12" height="10" rx="1" />
        <circle cx="5.8" cy="6.3" r="1.1" />
        <path d="m3.6 11.5 3-3 2.2 2.2 2.3-2.5 1.3 1.3" />
      </svg>
    ),
    // Chain link (was 🔗 in the markdown toolbar). Whole links only — the
    // broken-anchor semantics live in web-code's Unlink, not here.
    Link: (p: P) => (
      <svg {...base(sz("Link"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M6.7 8.7a3.3 3.3 0 0 0 5 .3l2-2a3.3 3.3 0 0 0-4.7-4.7L7.8 3.5" />
        <path d="M9.3 7.3a3.3 3.3 0 0 0-5-.3l-2 2a3.3 3.3 0 0 0 4.7 4.7l1.2-1.2" />
      </svg>
    ),
    // Quick-reply agree/disagree (was 👍/👎 emoji in QuickButtons).
    ThumbUp: (p: P) => (
      <svg {...base(sz("ThumbUp"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M9.3 6V3.3a2 2 0 0 0-2-2L4.7 7.3v7.2h7.4a1.4 1.4 0 0 0 1.4-1.2l.9-5.8A1.4 1.4 0 0 0 13 6z" />
        <path d="M4.7 14.5H3a1.4 1.4 0 0 1-1.4-1.4V8.7A1.4 1.4 0 0 1 3 7.3h1.7" />
      </svg>
    ),
    ThumbDown: (p: P) => (
      <svg {...base(sz("ThumbDown"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M9.3 10v2.7a2 2 0 0 1-2 2L4.7 8.7V1.5h7.4a1.4 1.4 0 0 1 1.4 1.2l.9 5.8A1.4 1.4 0 0 1 13 10z" />
        <path d="M4.7 1.5H3A1.4 1.4 0 0 0 1.6 2.9v4.4A1.4 1.4 0 0 0 3 8.7h1.7" />
      </svg>
    ),
    // F4/F6 — Home's no-repos EmptyState + Commit's unresolvable-sha
    // EmptyState — a simple folder glyph.
    Folder: (p: P) => (
      <svg {...base(sz("Folder"))} {...p}>
        <path d="M2 4.5a1 1 0 0 1 1-1h3l1.4 1.6H13a1 1 0 0 1 1 1V11a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" />
      </svg>
    ),
    // Wave E — the working-set strip's pin toggle (`WorkingSetStrip.tsx`) — a
    // small thumbtack: a round head + a straight point.
    Pin: (p: P) => (
      <svg {...base(sz("Pin"))} {...p}>
        <circle cx="6" cy="4.5" r="2.5" />
        <path d="M6 7v5" strokeLinecap="round" />
      </svg>
    ),
    // SL4 — the slate: a blackboard in a frame with two chalked lines. Used by
    // the nav entry and the /slates EmptyState; the board's own kind glyphs
    // are emoji from lib/slateGlyphs.ts, not this set.
    Slate: (p: P) => (
      <svg {...base(sz("Slate"))} {...p} strokeLinejoin="round">
        <rect x="1.5" y="2.5" width="13" height="10" rx="1.5" />
        <path d="M4 6h8M4 9h5" strokeLinecap="round" />
      </svg>
    ),
    // Star — saved/favourite badges (was the ★ text glyph in saved-queries +
    // global-memory chips).
    Star: (p: P) => (
      <svg {...base(sz("Star"))} {...p} strokeLinejoin="round">
        <path d="M8 1.8l1.9 3.9 4.3.6-3.1 3 .7 4.2L8 11.5l-3.8 2 .7-4.2-3.1-3 4.3-.6z" />
      </svg>
    ),
    // Provenance tab — a clock (mirrors kb's own `Icon.History`, sans the
    // rewind arrow — kb-code's provenance tab is "what happened", not "go
    // back").
    Clock: (p: P) => (
      <svg {...base(sz("Clock"))} {...p}>
        <circle cx="8" cy="8" r="6" />
        <path d="M8 4.5V8l3 2" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    ),
    // V76-R2b — generic file-kind fallback when syntax/1 has no lang for the path.
    File: (p: P) => (
      <svg {...base(sz("File"))} {...p}>
        <path d="M4 2.5h5.5L12 5.5V13a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V3.5a1 1 0 0 1 1-1z" />
        <path d="M9.5 2.5V5.5H12" />
      </svg>
    ),
    // F6 — the Branches page's single-branch EmptyState — a small git-fork
    // glyph (two tips merging into one base).
    Branch: (p: P) => (
      <svg {...base(sz("Branch"))} {...p}>
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
      <svg {...base(sz("Panel"))} {...p}>
        <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
        <path d="M10 2.5v11" />
      </svg>
    ),
    // V3.1-H3b — Entity inspector tab: a small diamond/node glyph.
    Entity: (p: P) => (
      <svg {...base(sz("Entity"))} {...p}>
        <path d="M8 2.5 12.5 8 8 13.5 3.5 8z" strokeLinejoin="round" />
      </svg>
    ),
    // V76-R3a — Facts tab: three bars, a histogram of claims about a file.
    Facts: (p: P) => (
      <svg {...base(sz("Facts"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 12V8M7 12V4M11 12V6" />
      </svg>
    ),
    // Compare base⇄head flip: two opposed arrows on offset rows.
    Swap: (p: P) => (
      <svg {...base(sz("Swap"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 5.5h8M9 3.5l2 2-2 2" />
        <path d="M13 10.5H5M7 8.5l-2 2 2 2" />
      </svg>
    ),
    // Visibility / viewed: almond outline + pupil.
    Eye: (p: P) => (
      <svg {...base(sz("Eye"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M1.8 8C3.6 4.8 5.6 3.2 8 3.2s4.4 1.6 6.2 4.8C12.4 11.2 10.4 12.8 8 12.8S3.6 11.2 1.8 8z" />
        <circle cx="8" cy="8" r="1.6" />
      </svg>
    ),
    // Hotspot / heat: teardrop flame + inner tongue.
    Flame: (p: P) => (
      <svg {...base(sz("Flame"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M8 2C6.4 5 4 6.6 4 9.6a4 4 0 0 0 8 0c0-3-2.4-4.6-4-7.6z" />
        <path d="M8 8.4c-.6 1.1-1.5 1.7-1.5 2.7A1.5 1.5 0 0 0 8 12.6" />
      </svg>
    ),
    // Stacked surfaces / patchsets: three flattened diamonds.
    Layers: (p: P) => (
      <svg {...base(sz("Layers"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M8 1.5 14 4.5 8 7.5 2 4.5z" />
        <path d="M2 8 8 11 14 8" />
        <path d="M2 11.5 8 14.5 14 11.5" />
      </svg>
    ),
    // Pull request: left rail + elbow into a third node.
    PullRequest: (p: P) => (
      <svg {...base(sz("PullRequest"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <circle cx="4" cy="3.5" r="1.5" />
        <circle cx="4" cy="12.5" r="1.5" />
        <circle cx="12" cy="12.5" r="1.5" />
        <path d="M4 5v5.5" />
        <path d="M5.5 3.5H8a4 4 0 0 1 4 4v3.5" />
      </svg>
    ),
    // Review checklist / copied-ok: clipboard with a check inside.
    ClipboardCheck: (p: P) => (
      <svg {...base(sz("ClipboardCheck"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <rect x="3.5" y="3.5" width="9" height="11" rx="1" />
        <rect x="5.5" y="1.5" width="5" height="3" rx="0.7" />
        <path d="M5.8 9.2l1.8 1.8 3.2-3.5" />
      </svg>
    ),
    // Side-by-side compare: frame with a CENTER divider (Panel's is off-centre).
    SplitView: (p: P) => (
      <svg {...base(sz("SplitView"))} {...p}>
        <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
        <path d="M8 2.5v11" />
      </svg>
    ),
    // Unified/list diff: frame with +/− prefixed text rows.
    UnifiedView: (p: P) => (
      <svg {...base(sz("UnifiedView"))} {...p} strokeLinecap="round">
        <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
        <path d="M4.2 6.2h1.6M5 5.4v1.6M7.2 6.2h4.4" />
        <path d="M4.2 10.2h1.6M7.2 10.2h4.4" />
      </svg>
    ),
    // Inverse of Expand: four corner arrows pointing inward.
    Collapse: (p: P) => (
      <svg {...base(sz("Collapse"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M2.5 6H6V2.5M13.5 6H10V2.5M2.5 10H6V13.5M13.5 10H10V13.5" />
      </svg>
    ),
    // Orphaned comment anchors: chain links pulled apart with a gap slash.
    Unlink: (p: P) => (
      <svg {...base(sz("Unlink"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M6.4 9.6 5 11.1a2.2 2.2 0 1 1-3.1-3.1L3.5 6.4" />
        <path d="M9.6 6.4 11 4.9a2.2 2.2 0 1 1 3.1 3.1L12.5 9.6" />
        <path d="M10.6 4.2 5.4 11.8" />
      </svg>
    ),
    // Create-branch: Branch glyph with the top-right node replaced by a +.
    BranchPlus: (p: P) => (
      <svg {...base(sz("BranchPlus"))} {...p}>
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
      <svg {...base(sz("OpenInReader"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M6.2 2.5h4.3l3 3v8H6.2" />
        <path d="M10.5 2.5v3h3" />
        <path d="M1.5 8h6M5.3 5.8 7.5 8l-2.2 2.2" />
      </svg>
    ),
    // V70-A7 — the theme control's glyph is now STATE-BEARING (recon R11: the
    // Sun was identical in all three states and the only signal was a title
    // attribute). Sun = light, Moon = dark, Contrast = follow the OS. These
    // three names were originally kb-code only.
    Moon: (p: P) => (
      <svg {...base(sz("Moon"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M13 9.6A5.6 5.6 0 0 1 6.4 3a5.6 5.6 0 1 0 6.6 6.6z" />
      </svg>
    ),
    Contrast: (p: P) => (
      <svg {...base(sz("Contrast"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <circle cx="8" cy="8" r="5.6" />
        <path d="M8 2.4v11.2a5.6 5.6 0 0 0 0-11.2z" fill="currentColor" stroke="none" />
      </svg>
    ),
    Palette: (p: P) => (
      <svg {...base(sz("Palette"))} {...p} strokeLinecap="round" strokeLinejoin="round">
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
    // `Pause` and `Dot` were already in kb's set; `Record` and `Fork` were
    // originally kb-code only.
    Record: (p: P) => (
      <svg {...base(sz("Record"))} {...p}>
        <circle cx="8" cy="8" r="4.2" fill="currentColor" stroke="none" />
      </svg>
    ),
    Fork: (p: P) => (
      <svg {...base(sz("Fork"))} {...p} strokeLinecap="round" strokeLinejoin="round">
        <path d="M5 2.8v4.4a2 2 0 0 0 2 2h4" />
        <path d="M5 9.2v4" />
        <circle cx="5" cy="2" r="1.4" />
        <circle cx="12.2" cy="9.2" r="1.4" />
        <circle cx="5" cy="14" r="1.4" />
      </svg>
    ),
  };
}

export const Icon = createIconSet();
