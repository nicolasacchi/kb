# Handoff: kb — Knowledge-Base TUI (ratatui)

## Overview
`kb` is a CLI/daemon for indexing local document collections (notes, blogs, code repos) into queryable knowledge bases. This handoff covers its **interactive terminal UI** — a 6-tab dashboard for monitoring indexing runs, browsing sources, drilling into errors, and searching across knowledge bases.

The TUI is launched as `kb tui` (or `kb tui --board` for the read-only stats view) and connects to the local `kb` daemon over Unix socket / SSE for live event streaming.

## About the Design Files
The files in this bundle are **design references rendered in HTML** — they exist solely to communicate intended look, layout, and behavior. They are **not** code to copy. Your job is to recreate these designs in **Rust + ratatui** (with `crossterm` as the backend) using ratatui's idiomatic widget composition. Where the HTML uses CSS for color or geometry, that translates to `Style`/`Color` + `Layout::default().constraints(...)` in ratatui.

## Fidelity
**High-fidelity.** Every glyph, color, border style, and key binding shown is intentional and final. Match the layouts cell-for-cell; ratatui is a fixed-cell medium and the mocks were drawn with that constraint in mind.

---

## Stack

- **Language:** Rust (edition 2021, MSRV 1.78+)
- **TUI:** [`ratatui`](https://crates.io/crates/ratatui) ≥ 0.29, [`crossterm`](https://crates.io/crates/crossterm) ≥ 0.28
- **Async runtime:** `tokio` (multi-thread)
- **Event stream:** `tokio::sync::broadcast` from the daemon's SSE consumer
- **Persistence (config):** TOML via `serde` + `toml`
- **Logging:** `tracing` + `tracing-appender` (file only — never stdout while TUI is up)
- Suggested crates: `ratatui-image` (optional, sources thumbnails), `tui-input` (search/cmd palette buffer), `unicode-width` (column math)

---

## Global chrome (always visible)

Two persistent rows above every view, one row below.

### Header (row 0)
```
│ kb v0.1.0 │ active: work │ ● running :4000 │ uptime 2h 14m │ ⠼ 12/67 rust-learning │
```
- Built with `Paragraph` + `Spans`; segments separated by `│` (`c-faint`).
- `●` is `c-green` when daemon is reachable, `c-yellow` for degraded, `c-red` for disconnected.
- Active source spinner cycles through braille (see Animations).

### Tab strip (row 1)
```
│  1 home   2 sources   3 detail   4 stats   5 errors   6 search │
```
- `Tabs` widget, `highlight_style` = `Style::default().bg(Color::Blue).fg(Color::Rgb(0xfd,0xf6,0xe3))`.
- Numbered labels — `1-6` jumps directly, `h`/`l` cycles.

### Status bar (last row)
```
│ NORMAL · j/k nav · Enter drill · / filter · : cmd · r reindex · ? help · 1-6 tabs │
```
- Mode pill on the left changes color per view (see View specs).
- Right-aligned spinner + count when an indexing run is active.

### Layout constraints
```rust
Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Length(2),  // header + tabs
        Constraint::Min(0),     // active view
        Constraint::Length(1),  // status bar
    ])
```

---

## Views

### V1 — HOME (default tab, `1`)
Three-pane Block layout. Mode pill: `inv-green`.

**Layout:**
```
Horizontal split — sidebar 22 cols | main fill
  Sidebar (Vertical):
    [Sources tree]   List  · BorderType::Plain · title "▌ sources"
    [Run progress]   Gauge + Paragraph
  Main (Vertical):
    [Overview]       Table · 5 cols (kb, source, duration, files, status)
    [Totals]         Paragraph (one line, c-muted)
    [Activity log]   List · auto-tails new SSE events, max 4 visible
```

**Behavior:**
- Sidebar is the focus root on tab open. `j`/`k` moves selection through sources.
- `Enter` drills the highlighted source → tab 3 (DETAIL) with that source loaded.
- Activity log is read-only here; new events push from the bottom and oldest scrolls off.
- Run progress gauge mirrors whatever source is currently indexing — not the highlighted one.

### V2 — SOURCES (`2`)
Single-table k9s discipline. Mode pill: `inv-blue`.

**Layout:**
```
Vertical:
  [Filter strip]   Paragraph — "[1] sources [2] runs [3] errors [4] queries  filter: _  N/N rows"
  [Sources table]  Table · highlight_style bg=selection
  [Detail strip]   Paragraph (4–5 lines about highlighted row: run id, eta, recent files)
```
- Columns: `KB`, `SOURCE`, `FILES`, `OK`, `ERR`, `EDGES`, `LAST`, `STATUS`.
- The four numeric tabs at top (`[1]…[4]`) switch the table's "kind" — same widget, different rows.
- `r` re-runs indexer on highlighted row; `p` toggles pause; `Enter` → V3.

### V3 — DETAIL (`3`)
Master/detail with chart canvas. Mode pill: `inv-violet`.

**Layout:**
```
Vertical:
  [Crumb]      Paragraph — "❯ rust-learning / /home/me/rust · 67 files · ⠼ indexing 12/67 · eta 00:42"
  [Body]       Horizontal split, 54 cols | fill:
    Left:  Table of files (path, mtime, size, indexed, status)
    Right: Vertical split:
      Top    Chart · ratatui::widgets::Chart with one Dataset (latency dots)
      Bottom BarChart · histogram of duration buckets (<100, 100–200, 200–300, 300+)
```
- Selecting a file in the table updates the chart's tooltip dot (highlight that file's latency point).
- `g` opens the neighborhood graph (modal, see Search view's graph block for shape).
- `Esc` returns to V2.

### V4 — STATS (`4`)
Read-only ambient board. Mode pill: `inv-cyan`.

**Layout:**
```
Vertical:
  [KPI row]            4 equal columns, each a Paragraph with a huge bold number + delta line
  [Queries sparkline]  Sparkline (60 cells, last 24h)
  [Throughput row]     Horizontal: Sparkline (38 cells) | "now indexing" Paragraph + Gauge
  [Docs by kb]         BarChart, horizontal, 4 bars
  [Recent strip]       Paragraph — three "[hh:mm:ss] glyph file" entries dot-separated
```
- No interactive selection here. `f` toggles fullscreen (hide chrome).
- `e` exports current snapshot to `kb-stats-<timestamp>.json`.

### V5 — ERRORS (`5`)
Error triage. Mode pill: `inv-red`.

**Layout:**
```
Vertical:
  [Summary]    Paragraph — "⚠ 3 errors · archive / … · most recent 14:30:44 · filter: _"
  [Error list] Table · cols: KIND, PATH, AT, MSG (truncated)
  [Detail]     Vertical of 3 Paragraphs:
    - Traceback (frames as "at fn · path:line")
    - Source context (5 lines around the offending location, with caret underline in c-red)
    - Suggested fix + actions
```
- `r` retries the highlighted error; `x` dismisses; `y` applies the suggested fix; `o` opens in `$EDITOR`.

### V6 — SEARCH (`6`)
Query + results + neighborhood graph. Mode pill: `inv-orange`.

**Layout:**
```
Vertical:
  [Query]      tui_input::Input — "/<query>_  N hits in Xms · across K kbs"
  [Filters]    Two rows of Tabs-styled segmented controls (scope, kind)
  [Results]    List · each row = 2 Spans-rich lines (title + snippet with highlighted match)
  [Graph]      ASCII-drawn neighborhood Paragraph (precomputed; uses box-drawing chars)
```
- `Tab` cycles scope; `Shift+Tab` cycles kind.
- `g` toggles full-screen graph view; arrow keys move focus node.

---

## Modal overlays

Render with `Clear` + a centered `Block`, sized to content.

### Help (`?`)
Single column reference of every keybinding, grouped by `▌ navigation`, `▌ modes`, `▌ actions`. ~45 cols × 25 rows. `?` or `Esc` dismisses.

### Command palette (`:`)
Vim-style colon prompt. `tui_input` for the buffer. Below the buffer, a fuzzy-matched list of completions (verb + target). `Tab` accepts the highlighted completion, `Enter` runs the command, `Esc` dismisses. Verbs: `reindex`, `add`, `remove`, `pause`, `resume`, `export`, `connect`.

---

## Keybindings (canonical table)

| Key | Context | Action |
|---|---|---|
| `1`–`6` | global | jump to tab |
| `h` / `l` | global | previous / next tab |
| `j` / `k` | within list | down / up |
| `Enter` | row selected | drill into row |
| `Esc` | overlay or detail | back / dismiss |
| `/` | global | enter search mode (jumps to V6) |
| `:` | global | open command palette |
| `?` | global | help overlay |
| `r` | row selected | reindex / retry |
| `p` | row selected | pause / resume |
| `g` | row selected | open neighborhood graph |
| `o` | error / file row | open in `$EDITOR` |
| `y` | error w/ fix | apply suggested fix |
| `x` | error row | dismiss |
| `f` | V4 only | toggle fullscreen board |
| `e` | V4 only | export snapshot |
| `Tab` | V6 | cycle scope filter |
| `q` / `Ctrl-C` | global | quit |

---

## Design tokens

### Color palette (Solarized Light)
Map directly to `ratatui::style::Color::Rgb(r, g, b)` (or named ANSI; use Rgb for fidelity):

| Token | Hex | RGB | Usage |
|---|---|---|---|
| `bg` | `#fdf6e3` | `253, 246, 227` | terminal bg (let it default — palette handles it) |
| `bg-alt` | `#eee8d5` | `238, 232, 213` | subtle inset |
| `fg` | `#586e75` | `88, 110, 117` | body text |
| `fg-strong` | `#073642` | `7, 54, 66` | borders, primary text |
| `fg-muted` | `#93a1a1` | `147, 161, 161` | secondary labels |
| `fg-faint` | `#d8d2bd` | `216, 210, 189` | dividers, separators inside cells |
| `yellow` | `#b58900` | `181, 137, 0` | warnings, modified-file watcher |
| `orange` | `#cb4b16` | `203, 75, 22` | search mode pill, errors-tab badge |
| `red` | `#dc322f` | `220, 50, 47` | errors, unreachable, fail glyphs |
| `magenta` | `#d33682` | `211, 54, 130` | reserved |
| `violet` | `#6c71c4` | `108, 113, 196` | active kb, detail mode pill |
| `blue` | `#268bd2` | `38, 139, 210` | active spinner, in-progress glyph, primary tab pill |
| `cyan` | `#2aa198` | `42, 161, 152` | section captions (`▌ overview`), graph nodes, sparklines |
| `green` | `#859900` | `133, 153, 0` | OK glyphs, running state, NORMAL pill |
| `selection` | `#fef0a8` | `254, 240, 168` | row highlight bg (light) |
| `selS` | `#fce98a` | `252, 233, 138` | strong row highlight bg |

Define once in `src/ui/theme.rs`:
```rust
pub const FG_STRONG: Color = Color::Rgb(0x07, 0x36, 0x42);
pub const CYAN:      Color = Color::Rgb(0x2a, 0xa1, 0x98);
// …etc
```

### Typography
Terminal font is the user's monospace; the design assumes JetBrains Mono / SF Mono / equivalent. **Do not** override the user's font — the design relies only on cell width being uniform.

### Box drawing
**Single-line only:** `┌ ┐ └ ┘ ─ │ ├ ┤ ┬ ┴ ┼`. `BorderType::Plain` everywhere. No double-line, no rounded.

### Glyphs
- Spinner: braille frames `⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏` at ~110 ms cadence.
- Status: `✓` ok (green) · `✗` fail (red) · `~` modified (yellow) · `⏸` paused (muted) · `⚠` warn (orange) · `▶` in-flight (blue) · `?` query (cyan).
- Section caption marker: `▌` (cyan).
- Sparkline cells: `▁ ▂ ▃ ▄ ▅ ▆ ▇ █` (use `Sparkline` widget; it picks these for you).
- Bar fill: `█` filled · `░` empty (for the custom Gauge-style bars in the home/detail strips).
- Tree branches: `▾ ▸ ├ │`.

### Spacing
- 1 cell of horizontal padding inside every Block (ratatui's default).
- 1 blank row between major sections inside a panel.
- Section caption (`▌ name`) sits on its own line, not embedded in the border title (matches the mocks).

---

## Animations & live updates

| Element | Cadence | Source |
|---|---|---|
| Header spinner | 110 ms | wall clock — stop when daemon disconnected |
| Run-progress bars | on event | `IndexProgress` SSE event |
| Activity log | on event | `Index*`, `Watch*`, `Query*` SSE events; cap at 200 retained, render last 4 |
| Sparklines (V4) | 1 s | minute-bucketed counters from daemon |
| Uptime | 1 s | wall clock — daemon sends `started_at` once on connect |
| KPI numbers | 1 s | poll `/stats` over the same socket |

Drive the redraw loop with `tokio::select!` over `(sse_event, terminal_input, refresh_tick)`. Don't redraw on every event — coalesce to a 60 Hz frame budget.

---

## State model

```rust
pub struct App {
    pub tab: Tab,                  // Home, Sources, Detail, Stats, Errors, Search
    pub mode: Mode,                // Normal, Search, Command, Help
    pub overlay: Option<Overlay>,  // None, Help, Cmd, Graph
    pub focused_pane: Pane,        // Sidebar, Main, ActivityLog (Home only)

    pub sources: Vec<Source>,
    pub source_table: TableState,
    pub home_sidebar: ListState,

    pub detail: Option<DetailScreen>,    // populated when entering V3
    pub errors: Vec<ErrIssue>,
    pub error_table: TableState,

    pub search_input: Input,             // tui_input
    pub search_results: Vec<Hit>,
    pub search_filters: SearchFilters,

    pub activity: VecDeque<Event>,       // ring buffer, len = 200
    pub stats: StatsSnapshot,
    pub spinner_frame: usize,
    pub connection: ConnectionState,     // Connected, Degraded, Disconnected
}
```

Keep `App` cheap to clone (or wrap heavy fields in `Arc`); the render fn takes `&App`.

---

## Event protocol (daemon → TUI)

SSE channel `/events` emits NDJSON lines:
```json
{"t":"index.start","run":"r-2c1","kb":"rust-learning","src":"/home/me/rust","total":67}
{"t":"index.file","run":"r-2c1","path":"futures.html","ms":188,"edges":5,"ok":true}
{"t":"watch.modify","kb":"work","path":"draft.html"}
{"t":"query","kb":"work","q":"borrow checker","hits":5,"ms":14}
{"t":"error","kind":"parse","kb":"archive","path":"old-blog/2014/broken.html","msg":"…"}
{"t":"connection","state":"degraded"}
```
`tracing` consumes the same stream so logs and TUI agree on ground truth.

---

## File layout (suggested)

```
crates/
  kb-tui/
    Cargo.toml
    src/
      main.rs              # entry, terminal setup/teardown
      app.rs               # App, Mode, Tab, Pane enums + state machine
      event.rs             # SSE consumer + crossterm input -> AppEvent
      ui/
        mod.rs             # render(frame, &App) — top-level layout + chrome
        theme.rs           # color tokens, glyph constants
        chrome.rs          # header, tab strip, status bar
        home.rs            # V1
        sources.rs         # V2
        detail.rs          # V3
        stats.rs           # V4
        errors.rs          # V5
        search.rs          # V6
        overlay/
          help.rs
          cmd.rs
          graph.rs
        widgets/
          spinner.rs       # custom — wraps Paragraph w/ frame counter
          big_number.rs    # large bold figure for V4 KPI cells
```

---

## Open questions for the team
1. Daemon transport — Unix socket vs. TCP-localhost? (Mocks assume `:4000`.)
2. Persistence of TUI state across restarts (last tab, filters) — yes/no?
3. Error "suggested fix" — does the daemon supply this, or does the TUI compute it?
4. Mouse support — out of scope or phase 2? The mocks are keyboard-only.

---

## Files in this bundle
- `kb tui.html` — interactive prototype, 6 tabs, live spinner / log / sparklines. **Primary reference.**
- `Ratatui variations.html` — five static layouts with widget callouts; useful when wiring an individual screen.
- `Approaches.html` — earlier paradigm exploration (k9s / REPL / minimap / board / swim-lane). Background only — the chosen direction is the unified app.

Open each in a browser. The key reference is `kb tui.html` — every design decision in this doc is traceable to one of its tabs.
