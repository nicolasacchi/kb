/* Mock data + helpers for the kb tui prototype. Exports to window. */

const PALETTES = {
  "solarized-light": {
    name: "solarized light",
    bg: "#fdf6e3", bgAlt: "#eee8d5",
    fg: "#586e75", fgStrong: "#073642", fgMuted: "#93a1a1", fgFaint: "#d8d2bd",
    border: "#d8d2bd",
    yellow: "#b58900", orange: "#cb4b16", red: "#dc322f", magenta: "#d33682",
    violet: "#6c71c4", blue: "#268bd2", cyan: "#2aa198", green: "#859900",
    selection: "#fef0a8", selectionStrong: "#fce98a",
  },
  "tokyo-night": {
    name: "tokyo night",
    bg: "#1a1b26", bgAlt: "#16161e",
    fg: "#a9b1d6", fgStrong: "#c0caf5", fgMuted: "#565f89", fgFaint: "#3b4261",
    border: "#3b4261",
    yellow: "#e0af68", orange: "#ff9e64", red: "#f7768e", magenta: "#bb9af7",
    violet: "#9d7cd8", blue: "#7aa2f7", cyan: "#7dcfff", green: "#9ece6a",
    selection: "#283457", selectionStrong: "#3d59a1",
  },
  "github-dark": {
    name: "github dark",
    bg: "#0d1117", bgAlt: "#161b22",
    fg: "#c9d1d9", fgStrong: "#f0f6fc", fgMuted: "#8b949e", fgFaint: "#30363d",
    border: "#30363d",
    yellow: "#d29922", orange: "#f0883e", red: "#f85149", magenta: "#db61a2",
    violet: "#a371f7", blue: "#58a6ff", cyan: "#79c0ff", green: "#7ee787",
    selection: "#1f2d44", selectionStrong: "#264f78",
  },
  "gruvbox": {
    name: "gruvbox dark",
    bg: "#282828", bgAlt: "#1d2021",
    fg: "#ebdbb2", fgStrong: "#fbf1c7", fgMuted: "#a89984", fgFaint: "#504945",
    border: "#504945",
    yellow: "#fabd2f", orange: "#fe8019", red: "#fb4934", magenta: "#d3869b",
    violet: "#d3869b", blue: "#83a598", cyan: "#8ec07c", green: "#b8bb26",
    selection: "#3c3836", selectionStrong: "#504945",
  },
  "phosphor": {
    name: "phosphor crt",
    bg: "#000000", bgAlt: "#001008",
    fg: "#33ff88", fgStrong: "#aaffcc", fgMuted: "#119955", fgFaint: "#003322",
    border: "#0a5533",
    yellow: "#88ff00", orange: "#ccff44", red: "#ff0044", magenta: "#ff44aa",
    violet: "#aaff66", blue: "#00ffaa", cyan: "#00ffcc", green: "#00ff66",
    selection: "#003322", selectionStrong: "#005544",
  },
};

const SOURCES_INITIAL = [
  {
    kb: "work", expanded: true,
    sources: [
      { path: "/home/me/work-notes",   files: 342, status: "ok",       lastIndex: "5m" },
      { path: "/home/me/team-docs",    files: 89,  status: "ok",       lastIndex: "12m" },
    ],
  },
  {
    kb: "rust-learning", expanded: true,
    sources: [
      { path: "/home/me/rust",         files: 67,  status: "indexing", progress: { k: 12, n: 67 } },
    ],
  },
  {
    kb: "personal", expanded: true,
    sources: [
      { path: "/home/me/personal",     files: 156, status: "ok",       lastIndex: "1h" },
      { path: "/home/me/garden",       files: 12,  status: "paused",   lastIndex: "3h" },
    ],
  },
  {
    kb: "archive", expanded: false,
    sources: [
      { path: "/home/me/old-blog",     files: 1240, status: "warn",    lastIndex: "2d", errors: 3 },
      { path: "/mnt/external/papers",  files: 0,   status: "missing",  lastIndex: "—" },
    ],
  },
];

/* ─── activity log ─────────────────────────────────────── */

const SAMPLE_FILES = [
  "event-sourcing.html", "borrow-checker.html", "lifetimes.html", "async-traits.html",
  "ownership.html", "trait-objects.html", "macros-101.html", "smart-pointers.html",
  "iterators-deep.html", "futures.html", "tokio-basics.html", "pin-projection.html",
  "draft.html", "weekly-review.html", "1-on-1-prep.html", "q4-planning.html",
  "compost.html", "tomatoes-2026.html", "soil-tests.html", "drip-irrigation.html",
];
const QUERIES = [
  "borrow checker", "tokio runtime", "lifetimes elision", "trait objects",
  "weekly review", "q4 planning", "tomato varieties",
];

function nowHHMMSS(offsetSec = 0) {
  const d = new Date(Date.now() + offsetSec * 1000);
  return d.toTimeString().slice(0, 8);
}

function makeSeedLog() {
  // Build a plausible recent backlog
  const entries = [];
  let t = -180;
  const push = (off, e) => entries.push({ ...e, t: nowHHMMSS(off), id: entries.length });
  push(-180, { kind: "watch", level: "info",  text: 'work / weekly-review.html modified' });
  push(-175, { kind: "index", level: "start", text: 'work / weekly-review.html' });
  push(-174, { kind: "index", level: "ok",    text: 'work / weekly-review.html (98ms, +1 edges)' });
  push(-160, { kind: "query", level: "info",  text: 'work "borrow checker" → 5 hits in 14ms' });
  push(-140, { kind: "watch", level: "warn",  text: 'work / draft.html created' });
  push(-138, { kind: "index", level: "start", text: 'work / draft.html' });
  push(-137, { kind: "index", level: "ok",    text: 'work / draft.html (76ms, +0 edges)' });
  push(-100, { kind: "index", level: "start", text: 'rust-learning / iterators-deep.html' });
  push(-99,  { kind: "index", level: "ok",    text: 'rust-learning / iterators-deep.html (212ms, +5 edges)' });
  push(-80,  { kind: "error", level: "err",   text: 'archive / broken.html: parse failed (line 23)' });
  push(-60,  { kind: "query", level: "info",  text: 'rust-learning "lifetimes" → 12 hits in 22ms' });
  push(-40,  { kind: "index", level: "start", text: 'rust-learning / async-traits.html' });
  push(-39,  { kind: "index", level: "ok",    text: 'rust-learning / async-traits.html (188ms, +2 edges)' });
  push(-20,  { kind: "watch", level: "info",  text: 'rust-learning / pin-projection.html modified' });
  return entries;
}

function randomEvent() {
  const kbs = ["work", "rust-learning", "personal"];
  const kb = kbs[Math.floor(Math.random() * kbs.length)];
  const file = SAMPLE_FILES[Math.floor(Math.random() * SAMPLE_FILES.length)];
  const r = Math.random();
  if (r < 0.45) {
    const ms = 60 + Math.floor(Math.random() * 250);
    const edges = Math.floor(Math.random() * 6);
    return { kind: "index", level: "ok", text: `${kb} / ${file} (${ms}ms, +${edges} edges)` };
  }
  if (r < 0.65) {
    return { kind: "watch", level: "info", text: `${kb} / ${file} modified` };
  }
  if (r < 0.8) {
    const q = QUERIES[Math.floor(Math.random() * QUERIES.length)];
    const hits = 1 + Math.floor(Math.random() * 30);
    const ms = 5 + Math.floor(Math.random() * 40);
    return { kind: "query", level: "info", text: `${kb} "${q}" → ${hits} hits in ${ms}ms` };
  }
  if (r < 0.92) {
    return { kind: "index", level: "start", text: `${kb} / ${file}` };
  }
  return { kind: "error", level: "err", text: `${kb} / broken-${Math.floor(Math.random()*99)}.html: parse failed` };
}

/* ─── source detail mock ───────────────────────────────── */

const SOURCE_DETAIL = {
  "/home/me/rust": {
    breadcrumb: "rust-learning / /home/me/rust",
    files: [
      { path: "ownership.html",        mtime: "2d ago",   size: "12.4 KB", indexed: "5m ago",  status: "✓" },
      { path: "borrow-checker.html",   mtime: "3d ago",   size: "8.1 KB",  indexed: "5m ago",  status: "✓" },
      { path: "lifetimes.html",        mtime: "1w ago",   size: "14.8 KB", indexed: "5m ago",  status: "✓" },
      { path: "trait-objects.html",    mtime: "5d ago",   size: "9.6 KB",  indexed: "5m ago",  status: "✓" },
      { path: "macros-101.html",       mtime: "2w ago",   size: "22.0 KB", indexed: "5m ago",  status: "✓" },
      { path: "smart-pointers.html",   mtime: "10d ago",  size: "11.2 KB", indexed: "5m ago",  status: "✓" },
      { path: "iterators-deep.html",   mtime: "1d ago",   size: "18.7 KB", indexed: "1m ago",  status: "✓" },
      { path: "futures.html",          mtime: "today",    size: "16.3 KB", indexed: "now",     status: "⠋" },
      { path: "async-traits.html",     mtime: "today",    size: "13.9 KB", indexed: "queued",  status: "·" },
      { path: "tokio-basics.html",     mtime: "today",    size: "20.1 KB", indexed: "queued",  status: "·" },
      { path: "pin-projection.html",   mtime: "today",    size: "9.4 KB",  indexed: "queued",  status: "·" },
      { path: "stream.html",           mtime: "1d ago",   size: "7.2 KB",  indexed: "queued",  status: "·" },
      { path: "select-loop.html",      mtime: "4d ago",   size: "5.8 KB",  indexed: "5m ago",  status: "✓" },
      { path: "channels.html",         mtime: "2d ago",   size: "10.5 KB", indexed: "5m ago",  status: "✓" },
      { path: "broken-link.html",      mtime: "3d ago",   size: "1.1 KB",  indexed: "—",       status: "⚠" },
    ],
  },
};

const SEARCH_RESULTS = {
  query: "borrow checker",
  mode: "k+v",
  hits: [
    { rank: 1, title: "borrow-checker.html",   kb: "rust-learning", score: 0.94, snippet: "…the **borrow checker** enforces aliasing-XOR-mutability at compile time, preventing data races…" },
    { rank: 2, title: "lifetimes.html",        kb: "rust-learning", score: 0.81, snippet: "…lifetimes are how the **borrow checker** reasons about reference validity across function calls…" },
    { rank: 3, title: "ownership.html",        kb: "rust-learning", score: 0.72, snippet: "…ownership is the foundation; the **borrow** **checker** is the runtime that enforces it…" },
    { rank: 4, title: "smart-pointers.html",   kb: "rust-learning", score: 0.58, snippet: "…Rc and RefCell let you sidestep parts of the **borrow checker** by deferring to runtime…" },
    { rank: 5, title: "weekly-review.html",    kb: "work",          score: 0.41, snippet: "…spent half of friday wrestling the **borrow checker**; pair-debugged with sam, see notes…" },
  ],
};

const RECENT_RUNS = [
  { kb: "rust-learning", source: "/home/me/rust",       dur: "in progress",  files: "12/67", status: "⠋" },
  { kb: "work",          source: "/home/me/work-notes", dur: "5m 12s",       files: "342",   status: "✓" },
  { kb: "work",          source: "/home/me/team-docs",  dur: "1m 04s",       files: "89",    status: "✓" },
  { kb: "personal",      source: "/home/me/personal",   dur: "2m 41s",       files: "156",   status: "✓" },
  { kb: "personal",      source: "/home/me/garden",     dur: "—",            files: "12",    status: "⏸" },
  { kb: "archive",       source: "/home/me/old-blog",   dur: "8m 19s",       files: "1240",  status: "⚠ 3" },
];

const ERRORS_LIST = [
  { run: "r-8a3", kb: "archive",       file: "old-blog/2014/broken.html",         phase: "parse",   msg: "unexpected </p> at line 23, col 4",         t: nowHHMMSS(-80) },
  { run: "r-8a3", kb: "archive",       file: "old-blog/2014/missing-front.html",  phase: "frontmatter", msg: "expected `---` delimiter, got EOF",       t: nowHHMMSS(-78) },
  { run: "r-8a3", kb: "archive",       file: "old-blog/2015/encoding.html",       phase: "read",    msg: "invalid utf-8 sequence at byte 1284",        t: nowHHMMSS(-77) },
  { run: "r-2c1", kb: "rust-learning", file: "rust/draft-incomplete.html",        phase: "embed",   msg: "embedding model timeout after 30s",         t: nowHHMMSS(-300) },
];

const STATS = {
  totals: { docs: 1906, words: 1_240_318, edges: 8_412, kbs: 4, sources: 7 },
  perKb: [
    { kb: "work",          docs: 431,  words: 312_004, edges: 2_104, lastIndex: "5m ago",  watch: "on" },
    { kb: "rust-learning", docs: 67,   words: 184_220, edges: 1_801, lastIndex: "now",     watch: "on" },
    { kb: "personal",      docs: 168,  words: 220_188, edges: 1_245, lastIndex: "1h ago",  watch: "mixed" },
    { kb: "archive",       docs: 1240, words: 523_906, edges: 3_262, lastIndex: "2d ago",  watch: "off" },
  ],
};

Object.assign(window, {
  PALETTES, SOURCES_INITIAL, SOURCE_DETAIL, SEARCH_RESULTS,
  RECENT_RUNS, ERRORS_LIST, STATS, makeSeedLog, randomEvent, nowHHMMSS,
});
