import type { SVGProps } from "react";

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
  Search: (p: P) => (
    <svg {...base(14)} {...p}>
      <circle cx="7" cy="7" r="4.5" />
      <path d="m13 13-3-3" strokeLinecap="round" />
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
  List: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M3 4h10M3 8h10M3 12h10" strokeLinecap="round" />
    </svg>
  ),
  Atlas: (p: P) => (
    <svg {...base(14)} {...p} strokeWidth={1.5}>
      <circle cx="4" cy="4" r="1.6" />
      <circle cx="12" cy="5" r="1.6" />
      <circle cx="8" cy="11" r="1.6" />
      <path d="M5 5l5 0M5 5l3 5M11 6l-2 4" strokeLinecap="round" />
    </svg>
  ),
  // W2.5 — the broadsheet "press" gallery view: a front page (rule + columns).
  Press: (p: P) => (
    <svg {...base(14)} {...p} strokeWidth={1.5}>
      <rect x="2" y="3" width="12" height="10" rx="1" />
      <path d="M4.5 6h7M4.5 8.5h3M4.5 10.5h3M9.5 8.5h2M9.5 10.5h2" strokeLinecap="round" />
    </svg>
  ),
  // v0.6+ H5 — gallery's history (timeline) view toggle. A counter-clockwise
  // "rewind" arrow encircling a clock; reads as "go back through what
  // you've done". Same stroke weight as the other view-toggle icons.
  History: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M2.6 8a5.4 5.4 0 1 0 1.6-3.8" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M2.6 3.2v3h3" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M8 5.2V8l2 1.3" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
  // SH.I1 — redesigned from the old 8-tick sunburst (which visually twinned
  // Icon.Sun sitting right beside it in the Header) into three sliders with
  // staggered knobs: reads as "preferences", never as "brightness".
  Settings: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round">
      <path d="M2.5 4.5h4.5M10.2 4.5h3.3" />
      <circle cx="8.6" cy="4.5" r="1.6" />
      <path d="M5.7 8h7.8" />
      <circle cx="4.1" cy="8" r="1.6" />
      <path d="M2.5 11.5h7.8" />
      <circle cx="11.9" cy="11.5" r="1.6" />
    </svg>
  ),
  Chevron: (p: P) => (
    <svg {...base(12)} {...p}>
      <path d="m6 4 4 4-4 4" />
    </svg>
  ),
  ChevDown: (p: P) => (
    <svg {...base(12)} {...p}>
      <path d="m4 6 4 4 4-4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
  Arrow: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="m9 4 4 4-4 4M13 8H3" />
    </svg>
  ),
  ArrowLeft: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="m7 4-4 4 4 4M3 8h10" />
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
  Doc: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M3.5 1.5h6L13 5v9.5H3.5z" />
      <path d="M9.5 1.5V5h3.5" />
    </svg>
  ),
  Copy: (p: P) => (
    <svg {...base(14)} {...p}>
      <rect x="5" y="5" width="9" height="9" rx="1" />
      <path d="M3 11V3a1 1 0 0 1 1-1h7" />
    </svg>
  ),
  // U1 — copy-link "copied ✓" confirmation swap (ContextBar copy-link).
  Check: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 8.5l3.5 3.5L13 4.5" />
    </svg>
  ),
  // U1 — icon-only fullscreen (immersive read) button. Four outward
  // corners = "maximize"; replaces the old ↗ open-in-tab primary slot.
  Expand: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M6 2.5H2.5V6M10 2.5h3.5V6M6 13.5H2.5V10M10 13.5h3.5V10" />
    </svg>
  ),
  // v0.22 — open the BARE artifact (its <id>.artifacts origin) in a new tab:
  // a box with an arrow exiting the top-right corner (the standard "external"
  // glyph). Distinct from Expand (immersive) and Share (static export).
  External: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M9 3h4v4M13 3 7.5 8.5M11 9.5V13H3V5h3.5" />
    </svg>
  ),
  X: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="m4 4 8 8M12 4l-8 8" strokeLinecap="round" />
    </svg>
  ),
  More: (p: P) => (
    <svg {...base(14)} {...p}>
      <circle cx="3" cy="8" r="1" />
      <circle cx="8" cy="8" r="1" />
      <circle cx="13" cy="8" r="1" />
    </svg>
  ),
  Code: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="m5 5-3 3 3 3M11 5l3 3-3 3M9 3 7 13" />
    </svg>
  ),
  Spark: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M8 1v4M8 11v4M1 8h4M11 8h4M3.5 3.5l2 2M10.5 10.5l2 2M3.5 12.5l2-2M10.5 5.5l2-2" />
    </svg>
  ),
  Table: (p: P) => (
    <svg {...base(14)} {...p}>
      <rect x="2" y="3" width="12" height="10" rx="0.5" />
      <path d="M2 7h12M2 10h12M6 7v6M10 7v6" />
    </svg>
  ),
  BookOpen: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M8 4v9M2 3.5h3a2.5 2.5 0 0 1 3 2.5v7c0-1.5-1-2.5-2.5-2.5H2zM14 3.5h-3A2.5 2.5 0 0 0 8 6v7c0-1.5 1-2.5 2.5-2.5H14z" />
    </svg>
  ),
  Cmd: (p: P) => (
    <svg {...base(12)} {...p}>
      <path d="M5 3.5a1.5 1.5 0 1 0 1.5 1.5V11A1.5 1.5 0 1 0 5 12.5h6A1.5 1.5 0 1 0 12.5 11V5A1.5 1.5 0 1 0 11 3.5H5" />
    </svg>
  ),
  Dot: (p: P) => (
    <svg viewBox="0 0 8 8" width="8" height="8" {...p}>
      <circle cx="4" cy="4" r="3" fill="currentColor" />
    </svg>
  ),
  Pen: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="m11 2 3 3-8 8H3v-3z" />
      <path d="m9.5 3.5 3 3" />
    </svg>
  ),
  Note: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M2.5 3a1 1 0 0 1 1-1h7l3 3v8a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z" />
      <path d="M5 6h6M5 9h6M5 12h4" strokeLinecap="round" />
    </svg>
  ),
  Mark: (p: P) => (
    <svg {...base(16)} {...p} strokeWidth={1.6} strokeLinecap="square">
      <path d="M3 2v12M3 8l5-5M3 8l5 5" />
    </svg>
  ),
  // Download — arrow into a tray. Single-artifact + folder-zip download
  // controls (FloatingPill, siblings popover, gallery card/row + header).
  Download: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M8 2v8M5 7l3 3 3-3" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M3 13h10" strokeLinecap="round" />
    </svg>
  ),
  // Share — arrow up out of a tray (the inverse of Download). Publishes
  // an artifact to a gated/public static URL (FloatingPill "Share…").
  Share: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M8 10V2M5 5l3-3 3 3" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M3 9v4h10V9" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  ),
  // v0.10 D2 — chrome additions. Anchor (corkboard pill in the Header),
  // Bookmark (saved-queries button placeholder, lit up in Q4), Sun
  // (theme toggle), Brain (the design's icon for the Memory view in
  // the toggle strip), Warn (status-bar warn cluster).
  Anchor: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <circle cx="8" cy="3" r="1.4" />
      <path d="M8 4.5v9.5" />
      <path d="M3 8.5H2a6 6 0 0 0 12 0h-1" />
      <path d="M5.5 10h5" />
    </svg>
  ),
  Bookmark: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M4 2.5h8v11l-4-2.5-4 2.5z" strokeLinejoin="round" />
    </svg>
  ),
  // Reading-lists view (RL-track). Checklist shape distinguishes the
  // /lists nav glyph from the Bookmark (saved-queries) button living
  // next to it in the Header.
  Tasks: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 3.5l1.2 1.2L6 2.8" />
      <path d="M3 8l1.2 1.2L6 7.3" />
      <path d="M3 12.5l1.2 1.2L6 11.8" />
      <path d="M8.5 4h5M8.5 8.5h5M8.5 13h5" />
    </svg>
  ),
  Sun: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round">
      <circle cx="8" cy="8" r="2.5" />
      <path d="M8 1.5v1.5M8 13v1.5M1.5 8h1.5M13 8h1.5M3.4 3.4l1 1M11.6 11.6l1 1M3.4 12.6l1-1M11.6 4.4l1-1" />
    </svg>
  ),
  Brain: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M8 3a1.8 1.8 0 0 0-3.2-1A1.5 1.5 0 0 0 3 3.6 1.5 1.5 0 0 0 2.4 6.3 1.8 1.8 0 0 0 3.4 9.6 1.8 1.8 0 0 0 6.2 11 1.8 1.8 0 0 0 8 12.5" />
      <path d="M8 3a1.8 1.8 0 0 1 3.2-1A1.5 1.5 0 0 1 13 3.6 1.5 1.5 0 0 1 13.6 6.3 1.8 1.8 0 0 1 12.6 9.6 1.8 1.8 0 0 1 9.8 11 1.8 1.8 0 0 1 8 12.5" />
      <path d="M8 3v9.5" />
    </svg>
  ),
  Warn: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M7 2.5 1.5 12.5h11z" />
      <path d="M7 6v2.5M7 10.5v.5" />
    </svg>
  ),
  // v0.14 S5 — Sessions tab. Terminal silhouette to suggest "recorded
  // agent conversation"; matches Brain (memory) + Tasks (lists)
  // visual weight.
  Terminal: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <rect x="1.5" y="2.5" width="11" height="9" rx="1.2" />
      <path d="M3.5 5.5l1.6 1.5-1.6 1.5" />
      <path d="M6.8 9.2h3" />
    </svg>
  ),
  // Z4 — comments inbox pill (Header) + /inbox empty state. A speech
  // bubble with a tail, same stroke weight as the other chrome glyphs.
  Comment: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M2 3.5h10a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H6l-3 2.5v-2.5H2a1 1 0 0 1-1-1v-5a1 1 0 0 1 1-1z" />
    </svg>
  ),
  // U4 — quick capture's drawer/nav entry (distinct from Share's "arrow out
  // of a tray" and Download's "arrow into a tray with a line": a plain
  // "add new" glyph).
  Plus: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round">
      <path d="M8 3v10M3 8h10" />
    </svg>
  ),
  // SH.I1 — drawn set replacing the unicode/emoji stand-ins that grew across
  // the settings/comment/notes stacks (design-audit 2026-08-20). Same grammar
  // as everything above: 16-unit grid, 1.4 stroke, currentColor, no fill.
  // Outline can: lid, swing-top, tapered body, two ticks. Destructive verbs
  // (drop kb, purge, revoke) — color comes from the call site, never baked in.
  Trash: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M2.5 4.2h11" />
      <path d="M5.7 4.2V3a1 1 0 0 1 1-1h2.6a1 1 0 0 1 1 1v1.2" />
      <path d="M4 4.2l.7 9a1.2 1.2 0 0 0 1.2 1.1h4.2a1.2 1.2 0 0 0 1.2-1.1l.7-9" />
      <path d="M6.6 7v4.4M9.4 7v4.4" />
    </svg>
  ),
  // Reload/refetch — ported verbatim from web-code's set (same design family).
  Refresh: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 8a5 5 0 1 1 5-5" />
      <path d="M8 3h3.2v3.2" />
    </svg>
  ),
  // Resume/pause pair for Live tail + source toggles (was ▶/⏸ dingbats).
  Play: (p: P) => (
    <svg {...base(14)} {...p} strokeLinejoin="round">
      <path d="M5.2 3.2v9.6l7.6-4.8z" />
    </svg>
  ),
  Pause: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round">
      <path d="M5.6 3.2v9.6M10.4 3.2v9.6" />
    </svg>
  ),
  // Attachment paperclip (was the 📎 emoji across the composer stack).
  Paperclip: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="m14.2 7.5-6 6a4 4 0 0 1-5.7-5.7l6-6a2.7 2.7 0 0 1 3.8 3.8l-6 6A1.35 1.35 0 0 1 4.4 9.7l5.6-5.6" />
    </svg>
  ),
  // Picture frame + horizon (was 🖼; composer attach + broken-image fallback).
  ImageFrame: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <rect x="2" y="3" width="12" height="10" rx="1" />
      <circle cx="5.8" cy="6.3" r="1.1" />
      <path d="m3.6 11.5 3-3 2.2 2.2 2.3-2.5 1.3 1.3" />
    </svg>
  ),
  // Chain link (was 🔗 in the markdown toolbar). Whole links only — the
  // broken-anchor semantics live in web-code's Unlink, not here.
  Link: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M6.7 8.7a3.3 3.3 0 0 0 5 .3l2-2a3.3 3.3 0 0 0-4.7-4.7L7.8 3.5" />
      <path d="M9.3 7.3a3.3 3.3 0 0 0-5-.3l-2 2a3.3 3.3 0 0 0 4.7 4.7l1.2-1.2" />
    </svg>
  ),
  // Quick-reply agree/disagree (was 👍/👎 emoji in QuickButtons).
  ThumbUp: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M9.3 6V3.3a2 2 0 0 0-2-2L4.7 7.3v7.2h7.4a1.4 1.4 0 0 0 1.4-1.2l.9-5.8A1.4 1.4 0 0 0 13 6z" />
      <path d="M4.7 14.5H3a1.4 1.4 0 0 1-1.4-1.4V8.7A1.4 1.4 0 0 1 3 7.3h1.7" />
    </svg>
  ),
  ThumbDown: (p: P) => (
    <svg {...base(14)} {...p} strokeLinecap="round" strokeLinejoin="round">
      <path d="M9.3 10v2.7a2 2 0 0 1-2 2L4.7 8.7V1.5h7.4a1.4 1.4 0 0 1 1.4 1.2l.9 5.8A1.4 1.4 0 0 1 13 10z" />
      <path d="M4.7 1.5H3A1.4 1.4 0 0 0 1.6 2.9v4.4A1.4 1.4 0 0 0 3 8.7h1.7" />
    </svg>
  ),
  // Folder — ported verbatim from web-code's set (settings table cells).
  Folder: (p: P) => (
    <svg {...base(14)} {...p}>
      <path d="M2 4.5a1 1 0 0 1 1-1h3l1.4 1.6H13a1 1 0 0 1 1 1V11a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" />
    </svg>
  ),
  // Pin — ported verbatim from web-code's set (note/notepad pin toggles;
  // was the 📌 emoji).
  Pin: (p: P) => (
    <svg {...base(12)} {...p}>
      <circle cx="6" cy="4.5" r="2.5" />
      <path d="M6 7v5" strokeLinecap="round" />
    </svg>
  ),
  // SL4 — the slate: a blackboard in a frame with two chalked lines. Used by
  // the nav entry and the /slates EmptyState; the board's own kind glyphs
  // are emoji from lib/slateGlyphs.ts, not this set.
  Slate: (p: P) => (
    <svg {...base(14)} {...p} strokeLinejoin="round">
      <rect x="1.5" y="2.5" width="13" height="10" rx="1.5" />
      <path d="M4 6h8M4 9h5" strokeLinecap="round" />
    </svg>
  ),
  // Star — saved/favourite badges (was the ★ text glyph in saved-queries +
  // global-memory chips).
  Star: (p: P) => (
    <svg {...base(14)} {...p} strokeLinejoin="round">
      <path d="M8 1.8l1.9 3.9 4.3.6-3.1 3 .7 4.2L8 11.5l-3.8 2 .7-4.2-3.1-3 4.3-.6z" />
    </svg>
  ),
};
