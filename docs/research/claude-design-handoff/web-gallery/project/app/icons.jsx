// Inline SVG icons — small, line-style, currentColor
const Icon = {
  Search: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><circle cx="7" cy="7" r="4.5"/><path d="m10.5 10.5 3 3"/></svg>,
  Grid: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><rect x="2" y="2" width="5" height="5" rx="0.5"/><rect x="9" y="2" width="5" height="5" rx="0.5"/><rect x="2" y="9" width="5" height="5" rx="0.5"/><rect x="9" y="9" width="5" height="5" rx="0.5"/></svg>,
  List: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="M3 4h10M3 8h10M3 12h10"/></svg>,
  Settings: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><circle cx="8" cy="8" r="2"/><path d="M8 1v2M8 13v2M1 8h2M13 8h2M3 3l1.4 1.4M11.6 11.6 13 13M3 13l1.4-1.4M11.6 4.4 13 3"/></svg>,
  Chevron: (p) => <svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="m6 4 4 4-4 4"/></svg>,
  ChevDown: (p) => <svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="m4 6 4 4 4-4"/></svg>,
  Arrow: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="m9 4 4 4-4 4M13 8H3"/></svg>,
  ArrowLeft: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="m7 4-4 4 4 4M3 8h10"/></svg>,
  Graph: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><circle cx="3.5" cy="3.5" r="1.5"/><circle cx="12" cy="6" r="1.5"/><circle cx="5" cy="12" r="1.5"/><circle cx="13" cy="13" r="1.2"/><path d="m4.5 4.5 6 1M4.5 11l6.5 1M5 4.5 5 10.5M11 7l1.5 5"/></svg>,
  Doc: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="M3.5 1.5h6L13 5v9.5H3.5z"/><path d="M9.5 1.5V5h3.5"/></svg>,
  Copy: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><rect x="5" y="5" width="9" height="9" rx="1"/><path d="M3 11V3a1 1 0 0 1 1-1h7"/></svg>,
  X: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="m4 4 8 8M12 4l-8 8"/></svg>,
  More: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><circle cx="3" cy="8" r="1"/><circle cx="8" cy="8" r="1"/><circle cx="13" cy="8" r="1"/></svg>,
  Code: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="m5 5-3 3 3 3M11 5l3 3-3 3M9 3 7 13"/></svg>,
  Spark: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="M8 1v4M8 11v4M1 8h4M11 8h4M3.5 3.5l2 2M10.5 10.5l2 2M3.5 12.5l2-2M10.5 5.5l2-2"/></svg>,
  Table: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><rect x="2" y="3" width="12" height="10" rx="0.5"/><path d="M2 7h12M2 10h12M6 7v6M10 7v6"/></svg>,
  BookOpen: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="M8 4v9M2 3.5h3a2.5 2.5 0 0 1 3 2.5v7c0-1.5-1-2.5-2.5-2.5H2zM14 3.5h-3A2.5 2.5 0 0 0 8 6v7c0-1.5 1-2.5 2.5-2.5H14z"/></svg>,
  Cmd: (p) => <svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="M5 3.5a1.5 1.5 0 1 0 1.5 1.5V11A1.5 1.5 0 1 0 5 12.5h6A1.5 1.5 0 1 0 12.5 11V5A1.5 1.5 0 1 0 11 3.5H5"/></svg>,
  Dot: (p) => <svg viewBox="0 0 8 8" width="8" height="8" {...p}><circle cx="4" cy="4" r="3" fill="currentColor"/></svg>,
  Pen: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="m11 2 3 3-8 8H3v-3z"/><path d="m9.5 3.5 3 3"/></svg>,
  Note: (p) => <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.4" {...p}><path d="M2.5 3a1 1 0 0 1 1-1h7l3 3v8a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z"/><path d="M5 6h6M5 9h6M5 12h4"/></svg>,
};

window.Icon = Icon;
