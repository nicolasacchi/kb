/* Main app: header, sidebar, activity log, footer, and keyboard handling. */

const { useState, useEffect, useRef, useMemo, useCallback } = React;

const SPINNER_FRAMES = ["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"];

function useSpinner(active = true, speed = 90) {
  const [i, setI] = useState(0);
  useEffect(() => {
    if (!active) return;
    const t = setInterval(() => setI(x => (x + 1) % SPINNER_FRAMES.length), speed);
    return () => clearInterval(t);
  }, [active, speed]);
  return SPINNER_FRAMES[i];
}

function useUptime() {
  const [now, setNow] = useState(Date.now());
  const [start] = useState(() => Date.now() - (2 * 3600 + 14 * 60) * 1000);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);
  const ms = now - start;
  const h = Math.floor(ms / 3600000);
  const m = Math.floor((ms % 3600000) / 60000);
  return { uptimeStr: `${h}h ${m.toString().padStart(2, "0")}m`, sinceStr: new Date(start).toTimeString().slice(0,5) };
}

const TWEAK_DEFAULTS = /*EDITMODE-BEGIN*/{
  "palette": "solarized-light",
  "borders": "single",
  "logDensity": "medium",
  "view": "status",
  "termSize": "default",
  "connection": "connected"
}/*EDITMODE-END*/;

/* ───────────────────────── HEADER ───────────────────────── */
function Header({ activeKb, connection, uptimeStr, indexing }) {
  const spinner = useSpinner(connection === "connected" && indexing.active);
  const dot = connection === "connected" && !indexing.active ? { ch: "●", color: "var(--green)" }
            : connection === "connected" && indexing.active ? { ch: spinner, color: "var(--blue)" }
            : connection === "degraded" ? { ch: "⚠", color: "var(--yellow)" }
            : { ch: "✗", color: "var(--red)" };
  const stateWord = connection === "connected" ? (indexing.active ? "indexing" : "running")
                  : connection === "degraded" ? "degraded"
                  : "disconnected";
  return (
    <div style={{
      padding: "0 var(--pad)",
      whiteSpace: "pre",
      background: connection === "disconnected" ? "rgba(220,50,47,0.15)" :
                  connection === "degraded" ? "rgba(181,137,0,0.15)" : "transparent",
      borderBottom: "1px solid var(--border)",
      display: "flex",
      alignItems: "center",
      gap: "1.5ch",
      height: "calc(var(--row) + 6px)",
    }}>
      <span style={{ color: "var(--fg-strong)", fontWeight: 500 }}>kb</span>
      <span style={{ color: "var(--fg-muted)" }}>v0.1.0</span>
      <span style={{ color: "var(--fg-faint)" }}>│</span>
      <span style={{ color: "var(--fg-muted)" }}>active:</span>
      <span style={{ color: "var(--violet)" }}>{activeKb}</span>
      <span style={{ color: "var(--fg-faint)" }}>│</span>
      <span style={{ color: dot.color }}>{dot.ch}</span>
      <span style={{ color: "var(--fg-strong)" }}>{stateWord}</span>
      <span style={{ color: "var(--fg-muted)" }}>:4000</span>
      <span style={{ color: "var(--fg-faint)" }}>│</span>
      <span style={{ color: "var(--fg-muted)" }}>uptime {uptimeStr}</span>
      {indexing.active && (
        <>
          <span style={{ color: "var(--fg-faint)" }}>│</span>
          <span style={{ color: "var(--blue)" }}>{spinner}</span>
          <span style={{ color: "var(--fg-strong)" }}>{indexing.k}/{indexing.n}</span>
          <span style={{ color: "var(--fg-muted)" }}>{indexing.kb}</span>
        </>
      )}
      <span style={{ flex: 1 }} />
      {connection === "disconnected" && (
        <span style={{ color: "var(--red)" }}>retry in 4s · [r] now</span>
      )}
      {connection === "degraded" && (
        <span style={{ color: "var(--yellow)" }}>last event 1.8s ago</span>
      )}
    </div>
  );
}

/* ───────────────────────── SIDEBAR ───────────────────────── */
function Sidebar({ kbs, activeKb, focused, focusedRow, onSelect, onToggle }) {
  const spinner = useSpinner(true);
  const rows = [];
  kbs.forEach(g => {
    rows.push({ kind: "kb", kb: g.kb, expanded: g.expanded });
    if (g.expanded) {
      g.sources.forEach(s => rows.push({ kind: "source", kb: g.kb, src: s }));
    }
  });

  return (
    <div style={{ height: "100%", overflow: "auto", padding: "calc(var(--pad) * 0.5) 0" }}>
      {rows.map((r, i) => {
        const isFocused = focused && focusedRow === i;
        if (r.kind === "kb") {
          return (
            <Row key={i} focused={isFocused} onClick={() => onSelect(i)} style={{ paddingTop: i ? 4 : 0 }}>
              <span style={{ color: "var(--fg-muted)", cursor: "pointer" }} onClick={(e) => { e.stopPropagation(); onToggle(r.kb); }}>
                {r.expanded ? "▾ " : "▸ "}
              </span>
              <span style={{ color: r.kb === activeKb ? "var(--violet)" : "var(--fg-strong)", fontWeight: 500 }}>
                {r.kb}
              </span>
              {r.kb === activeKb && <span style={{ color: "var(--violet)" }}>  ←</span>}
            </Row>
          );
        }
        const s = r.src;
        const statusEl = (() => {
          if (s.status === "ok")       return <span style={{ color: "var(--green)" }}>✓</span>;
          if (s.status === "indexing") return <span style={{ color: "var(--blue)" }}>{spinner} {s.progress.k}/{s.progress.n}</span>;
          if (s.status === "warn")     return <span style={{ color: "var(--orange)" }}>⚠ {s.errors}</span>;
          if (s.status === "paused")   return <span style={{ color: "var(--fg-muted)" }}>⏸</span>;
          if (s.status === "missing")  return <span style={{ color: "var(--red)" }}>✗</span>;
          return null;
        })();
        // Compress path: take last 2 segments
        const display = s.path.split("/").slice(-1)[0];
        return (
          <Row key={i} focused={isFocused} onClick={() => onSelect(i)}>
            <span style={{ color: "var(--fg-faint)" }}>{"  ├ "}</span>
            <span style={{ color: "var(--fg)" }}>/{display}</span>
            <span style={{ flex: 1 }} />
            {s.status === "ok" && <span style={{ color: "var(--fg-muted)", fontSize: "0.85em" }}>{s.files}f</span>}
            <span style={{ marginLeft: "1ch" }}>{statusEl}</span>
          </Row>
        );
      })}
    </div>
  );
}

/* ───────────────────────── ACTIVITY LOG ───────────────────────── */
function ActivityLog({ entries, focused, density, filter }) {
  const ref = useRef();
  useEffect(() => {
    if (ref.current) ref.current.scrollTop = ref.current.scrollHeight;
  }, [entries]);

  const filtered = filter && filter !== "all"
    ? entries.filter(e => e.kind === filter)
    : entries;

  if (density === "off") {
    return (
      <div style={{ padding: "var(--pad)", color: "var(--fg-muted)", height: "100%" }}>
        activity log paused · :filter all to resume
      </div>
    );
  }

  const colorFor = (e) => {
    if (e.level === "err")   return "var(--red)";
    if (e.level === "warn")  return "var(--orange)";
    if (e.kind === "query")  return "var(--cyan)";
    if (e.kind === "watch")  return "var(--yellow)";
    if (e.level === "ok")    return "var(--green)";
    if (e.level === "start") return "var(--blue)";
    return "var(--fg)";
  };
  const iconFor = (e) => {
    if (e.kind === "error")  return "✗";
    if (e.kind === "watch")  return e.level === "warn" ? "!" : "~";
    if (e.kind === "query")  return "?";
    if (e.level === "start") return "▶";
    if (e.level === "ok")    return "✓";
    return "·";
  };

  return (
    <div ref={ref} style={{ height: "100%", overflow: "auto", padding: "4px 0" }}>
      {filtered.map((e) => (
        <div key={e.id} style={{ padding: "0 var(--pad)", display: "flex", gap: "1ch", whiteSpace: "pre" }}>
          <span style={{ color: "var(--fg-faint)" }}>[{e.t}]</span>
          <span style={{ color: "var(--fg-muted)", width: "6ch", display: "inline-block" }}>{e.kind}</span>
          <span style={{ color: colorFor(e), width: "1ch", display: "inline-block" }}>{iconFor(e)}</span>
          <span style={{ color: "var(--fg)" }}>{e.text}</span>
        </div>
      ))}
      {filtered.length === 0 && (
        <div style={{ padding: "var(--pad)", color: "var(--fg-muted)" }}>no events match filter "{filter}"</div>
      )}
    </div>
  );
}

/* ───────────────────────── FOOTER ───────────────────────── */
function Footer({ mode, focusedPanel, commandText, searchText, onCommandChange, onCommandSubmit, suggestions }) {
  const inputRef = useRef();
  useEffect(() => {
    if ((mode === "command" || mode === "search") && inputRef.current) {
      inputRef.current.focus();
    }
  }, [mode]);

  const modeColor = mode === "command" ? "var(--violet)"
                  : mode === "search"  ? "var(--cyan)"
                  : mode === "visual"  ? "var(--orange)"
                  : "var(--green)";
  const modeLabel = mode.toUpperCase();
  const modeBox = (
    <span style={{
      background: modeColor,
      color: "var(--bg)",
      padding: "0 1ch",
      fontWeight: 700,
      letterSpacing: "0.1em",
    }}>{" " + modeLabel + " "}</span>
  );

  if (mode === "command" || mode === "search") {
    const prefix = mode === "command" ? ":" : "/";
    return (
      <div style={{ borderTop: "1px solid var(--border)" }}>
        {mode === "command" && suggestions && suggestions.length > 0 && (
          <div style={{ background: "var(--bg-alt)", padding: "2px var(--pad)", borderBottom: "1px solid var(--border)", display: "flex", gap: "2ch", flexWrap: "wrap" }}>
            {suggestions.map((s, i) => (
              <span key={i} style={{ color: i === 0 ? "var(--fg-strong)" : "var(--fg-muted)" }}>
                {i === 0 && <span style={{ color: "var(--violet)" }}>▸ </span>}{s}
              </span>
            ))}
          </div>
        )}
        <div style={{ padding: "0 var(--pad)", display: "flex", alignItems: "center", gap: "1ch", height: "calc(var(--row) + 6px)" }}>
          {modeBox}
          <span style={{ color: modeColor }}>{prefix}</span>
          <input
            ref={inputRef}
            value={mode === "command" ? commandText : searchText}
            onChange={e => onCommandChange(e.target.value)}
            onKeyDown={e => {
              if (e.key === "Enter") onCommandSubmit();
              if (e.key === "Tab" && suggestions && suggestions.length) {
                e.preventDefault();
                onCommandChange(suggestions[0]);
              }
            }}
            spellCheck={false}
            autoComplete="off"
            style={{
              flex: 1,
              border: "none",
              outline: "none",
              background: "transparent",
              color: "var(--fg-strong)",
              font: "inherit",
              padding: 0,
            }}
          />
          <span style={{ color: "var(--fg-muted)" }}>
            {mode === "command" ? "Tab complete · Enter run · Esc cancel" : "Esc cancel"}
          </span>
        </div>
      </div>
    );
  }

  // Normal / visual mode hints
  const hints = mode === "visual"
    ? [["j/k", "extend"], ["r", "reindex"], ["d", "delete"], ["y", "yank ids"], ["Esc", "cancel"]]
    : focusedPanel === "sidebar"
      ? [["j/k", "navigate"], ["Enter", "drill in"], ["r", "reindex"], ["i", "incr"], ["p", "pause"], ["+", "add"], ["l", "→ main"], ["?", "help"]]
      : focusedPanel === "main"
        ? [["j/k", "navigate"], ["Enter", "open"], ["/", "filter"], ["g", "graph"], ["s", "stats"], ["e", "errors"], ["h", "← side"], ["?", "help"]]
        : [["j/k", "scroll"], ["G", "tail"], [":", "filter"], ["k", "↑ main"], ["?", "help"]];

  return (
    <div style={{
      borderTop: "1px solid var(--border)",
      padding: "0 var(--pad)",
      display: "flex",
      alignItems: "center",
      gap: "1ch",
      height: "calc(var(--row) + 6px)",
      whiteSpace: "pre",
    }}>
      {modeBox}
      <span style={{ color: "var(--fg-muted)" }}>·</span>
      {hints.map(([k, d], i) => (
        <React.Fragment key={i}>
          <span style={{ color: "var(--orange)" }}>{k}</span>
          <span style={{ color: "var(--fg-muted)" }}>{d}</span>
          {i < hints.length - 1 && <span style={{ color: "var(--fg-faint)" }}>·</span>}
        </React.Fragment>
      ))}
      <span style={{ flex: 1 }} />
      <span style={{ color: "var(--fg-muted)" }}>focus: </span>
      <span style={{ color: "var(--cyan)" }}>{focusedPanel}</span>
      <span style={{ color: "var(--fg-faint)" }}>·</span>
      <span style={{ color: "var(--orange)" }}>q</span>
      <span style={{ color: "var(--fg-muted)" }}>quit</span>
    </div>
  );
}

/* ───────────────────────── PANEL WRAPPER ───────────────────────── */
function Panel({ title, children, focused, borderStyle, style, scroll = true }) {
  const borderProp = borderStyle === "double"
    ? "2px double var(--border)"
    : "1px solid var(--border)";
  const radius = borderStyle === "rounded" ? 8 : 0;
  return (
    <div style={{
      border: borderProp,
      borderRadius: radius,
      position: "relative",
      background: "var(--bg)",
      boxShadow: focused ? "inset 0 0 0 1px var(--blue)" : "none",
      overflow: "hidden",
      display: "flex",
      flexDirection: "column",
      ...style,
    }}>
      {title && (
        <div style={{
          position: "absolute",
          top: -1,
          left: 12,
          padding: "0 6px",
          background: "var(--bg)",
          color: focused ? "var(--blue)" : "var(--fg-muted)",
          fontSize: "0.85em",
          letterSpacing: "0.05em",
          textTransform: "lowercase",
          transform: "translateY(-50%)",
          zIndex: 1,
        }}>
          {focused ? `▌ ${title}` : title}
        </div>
      )}
      <div style={{
        flex: 1,
        overflow: scroll ? "auto" : "hidden",
        minHeight: 0,
      }}>{children}</div>
    </div>
  );
}

/* ───────────────────────── APP ───────────────────────── */
function App() {
  const [t, setTweak] = useTweaks(TWEAK_DEFAULTS);
  const palette = PALETTES[t.palette] || PALETTES["solarized-light"];

  // Inject palette into CSS vars
  useEffect(() => {
    const r = document.documentElement.style;
    r.setProperty("--bg", palette.bg);
    r.setProperty("--bg-alt", palette.bgAlt);
    r.setProperty("--fg", palette.fg);
    r.setProperty("--fg-strong", palette.fgStrong);
    r.setProperty("--fg-muted", palette.fgMuted);
    r.setProperty("--fg-faint", palette.fgFaint);
    r.setProperty("--border", palette.border);
    r.setProperty("--yellow", palette.yellow);
    r.setProperty("--orange", palette.orange);
    r.setProperty("--red", palette.red);
    r.setProperty("--magenta", palette.magenta);
    r.setProperty("--violet", palette.violet);
    r.setProperty("--blue", palette.blue);
    r.setProperty("--cyan", palette.cyan);
    r.setProperty("--green", palette.green);
    r.setProperty("--selection", palette.selection);
    r.setProperty("--selection-strong", palette.selectionStrong);
  }, [t.palette, palette]);

  const [kbs, setKbs] = useState(SOURCES_INITIAL);
  const [sidebarRow, setSidebarRow] = useState(0);
  const flatSidebar = useMemo(() => {
    const out = [];
    kbs.forEach(g => {
      out.push({ kind: "kb", kb: g.kb });
      if (g.expanded) g.sources.forEach(s => out.push({ kind: "source", kb: g.kb, src: s }));
    });
    return out;
  }, [kbs]);

  const [activeKb, setActiveKb] = useState("work");
  const [focusedPanel, setFocusedPanel] = useState("main"); // sidebar | main | log
  const [view, setView] = useState(t.view); // status | source | search | errors | graph | stats | help
  useEffect(() => setView(t.view), [t.view]);
  const [drilledSource, setDrilledSource] = useState(null);
  const [mode, setMode] = useState("normal"); // normal | command | search | visual
  const [commandText, setCommandText] = useState("");
  const [searchText, setSearchText] = useState("");
  const [logFilter, setLogFilter] = useState("all");
  const [searchFocused, setSearchFocused] = useState(0);
  const [fileFocused, setFileFocused] = useState(0);
  const [showHelp, setShowHelp] = useState(false);
  const [showGraph, setShowGraph] = useState(false);
  const [activeQuery, setActiveQuery] = useState(SEARCH_RESULTS.query);

  // Activity log
  const [log, setLog] = useState(() => makeSeedLog());
  const { uptimeStr, sinceStr } = useUptime();

  // Connection: from tweaks
  const connection = t.connection || "connected";

  // Indexing state (from sidebar's rust-learning source)
  const indexing = useMemo(() => {
    for (const g of kbs) {
      for (const s of g.sources) {
        if (s.status === "indexing") return { active: true, kb: g.kb, k: s.progress.k, n: s.progress.n };
      }
    }
    return { active: false };
  }, [kbs]);

  // Density-driven event simulator
  useEffect(() => {
    if (connection !== "connected") return;
    if (t.logDensity === "off") return;
    const interval = t.logDensity === "lively" ? 700 : 2400;
    const tick = setInterval(() => {
      setLog(prev => {
        const e = randomEvent();
        const next = [...prev, { ...e, t: nowHHMMSS(), id: prev.length ? prev[prev.length-1].id + 1 : 0 }];
        return next.slice(-200);
      });
      // Advance indexing
      setKbs(prev => prev.map(g => ({
        ...g,
        sources: g.sources.map(s => {
          if (s.status === "indexing") {
            const k = Math.min(s.progress.n, s.progress.k + 1);
            if (k >= s.progress.n) return { ...s, status: "ok", lastIndex: "now" };
            return { ...s, progress: { ...s.progress, k } };
          }
          return s;
        }),
      })));
    }, interval);
    return () => clearInterval(tick);
  }, [t.logDensity, connection]);

  /* ─── keyboard ─── */
  const onKey = useCallback((e) => {
    // Ignore typing in inputs
    const tag = (e.target && e.target.tagName) || "";
    if (tag === "INPUT" || tag === "TEXTAREA") {
      if (e.key === "Escape") {
        setMode("normal");
        setCommandText("");
        e.preventDefault();
      }
      return;
    }
    if (mode !== "normal" && mode !== "visual") return;

    if (e.key === "Escape") {
      setShowHelp(false); setShowGraph(false);
      if (drilledSource) setDrilledSource(null);
      setMode("normal");
      return;
    }
    if (e.key === "?") { setShowHelp(s => !s); return; }
    if (e.key === ":") { e.preventDefault(); setMode("command"); setCommandText(""); return; }
    if (e.key === "/") { e.preventDefault(); setMode("search"); setSearchText(""); return; }
    if (e.key === "g") { setShowGraph(true); return; }
    if (e.key === "s") { setView("stats"); setTweak("view", "stats"); return; }
    if (e.key === "e") { setView("errors"); setTweak("view", "errors"); return; }
    if (e.key === "h" || e.key === "ArrowLeft")  { setFocusedPanel(p => p === "main" ? "sidebar" : p === "log" ? "main" : "sidebar"); return; }
    if (e.key === "l" || e.key === "ArrowRight") { setFocusedPanel(p => p === "sidebar" ? "main" : p === "main" ? "main" : "main"); return; }
    if (e.key === "k" || e.key === "ArrowUp") {
      if (focusedPanel === "sidebar") setSidebarRow(r => Math.max(0, r - 1));
      else if (focusedPanel === "main" && view === "search") setSearchFocused(r => Math.max(0, r - 1));
      else if (focusedPanel === "main" && view === "source") setFileFocused(r => Math.max(0, r - 1));
      else if (focusedPanel === "log") setFocusedPanel("main");
      return;
    }
    if (e.key === "j" || e.key === "ArrowDown") {
      if (focusedPanel === "sidebar") setSidebarRow(r => Math.min(flatSidebar.length - 1, r + 1));
      else if (focusedPanel === "main" && view === "search") setSearchFocused(r => Math.min(SEARCH_RESULTS.hits.length - 1, r + 1));
      else if (focusedPanel === "main" && view === "source") {
        const detail = SOURCE_DETAIL[(drilledSource && drilledSource.src.path)] || SOURCE_DETAIL["/home/me/rust"];
        setFileFocused(r => Math.min(detail.files.length - 1, r + 1));
      }
      else if (focusedPanel === "main") setFocusedPanel("log");
      return;
    }
    if (e.key === "Enter") {
      if (focusedPanel === "sidebar") {
        const row = flatSidebar[sidebarRow];
        if (!row) return;
        if (row.kind === "kb") {
          setKbs(prev => prev.map(g => g.kb === row.kb ? { ...g, expanded: !g.expanded } : g));
        } else {
          setDrilledSource(row);
          setView("source"); setTweak("view", "source");
          setFocusedPanel("main");
        }
      }
      if (focusedPanel === "main" && view === "search") {
        const hit = SEARCH_RESULTS.hits[searchFocused];
        const url = `https://en.wikipedia.org/wiki/${encodeURIComponent(hit.title.replace(/\.html$/, "").replace(/-/g, "_"))}`;
        try { window.open(url, "_blank", "noopener,noreferrer"); } catch (_) {}
        setLog(prev => [...prev, { kind: "query", level: "info", text: `xdg-open: ${hit.title} → browser`, t: nowHHMMSS(), id: (prev[prev.length-1]?.id ?? 0) + 1 }]);
      }
      if (focusedPanel === "sidebar" && flatSidebar[sidebarRow]?.kind === "source") {
        // covered by drill-down above; no-op
      }
      return;
    }
    if (e.key === "o") {
      let url = "https://en.wikipedia.org/wiki/Knowledge_base";
      let label = "active kb";
      if (focusedPanel === "main" && view === "search") {
        const hit = SEARCH_RESULTS.hits[searchFocused];
        url = `https://en.wikipedia.org/wiki/${encodeURIComponent(hit.title.replace(/\.html$/, "").replace(/-/g, "_"))}`;
        label = hit.title;
      }
      try { window.open(url, "_blank", "noopener,noreferrer"); } catch (_) {}
      setLog(prev => [...prev, { kind: "query", level: "info", text: `xdg-open: ${label} → browser`, t: nowHHMMSS(), id: (prev[prev.length-1]?.id ?? 0) + 1 }]);
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      const order = kbs.map(g => g.kb);
      const i = order.indexOf(activeKb);
      setActiveKb(order[(i + 1) % order.length]);
      return;
    }
  }, [mode, focusedPanel, sidebarRow, flatSidebar, view, drilledSource, kbs, activeKb, searchFocused, setTweak]);

  useEffect(() => {
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onKey]);

  /* ─── command tab-completion suggestions ─── */
  const ALL_COMMANDS = [
    "reindex", "source add", "source rm", "source pause", "source resume",
    "kb add", "kb rm", "kb switch", "kb ls",
    "watch pause", "watch resume",
    "filter index", "filter query", "filter watch", "filter error", "filter all",
    "open", "graph", "stats", "export json", "export csv", "export md",
    "server reload", "q",
  ];
  const suggestions = useMemo(() => {
    if (mode !== "command") return [];
    const q = commandText.trim();
    if (!q) return ALL_COMMANDS.slice(0, 5);
    return ALL_COMMANDS.filter(c => c.startsWith(q)).slice(0, 6);
  }, [commandText, mode]);

  const runCommand = () => {
    const c = commandText.trim();
    if (!c) { setMode("normal"); return; }
    const id = (log[log.length - 1]?.id ?? 0) + 1;
    setLog(prev => [...prev, { kind: "query", level: "info", text: `:${c}`, t: nowHHMMSS(), id }]);
    if (c === "stats") { setView("stats"); setTweak("view", "stats"); }
    else if (c === "errors" || c === "e") { setView("errors"); setTweak("view", "errors"); }
    else if (c.startsWith("filter ")) setLogFilter(c.split(" ")[1]);
    else if (c === "graph") setShowGraph(true);
    else if (c === "q") { /* no-op in prototype */ }
    else if (c.startsWith("kb switch ")) {
      const name = c.split(" ").pop();
      if (kbs.some(g => g.kb === name)) setActiveKb(name);
    }
    setMode("normal"); setCommandText("");
  };

  const runSearch = () => {
    const q = searchText.trim();
    if (q) {
      setActiveQuery(q);
      setView("search"); setTweak("view", "search");
      setLog(prev => {
        const id = (prev[prev.length-1]?.id ?? 0) + 1;
        return [...prev, { kind: "query", level: "info", text: `${activeKb} "${q}" → 5 hits in 14ms`, t: nowHHMMSS(), id }];
      });
    }
    setMode("normal"); setSearchText("");
  };

  /* ─── terminal size ─── */
  const sizePresets = {
    cramped: { width: 760, height: 460 },
    default: { width: 1180, height: 720 },
    wide:    { width: 1480, height: 900 },
  };
  const isCramped = t.termSize === "cramped";

  const mainView = (() => {
    if (view === "source" && drilledSource) {
      return <SourceDetailView
        source={drilledSource.src}
        focusedFile={fileFocused}
        onFocusFile={setFileFocused}
        filter={null}
      />;
    }
    if (view === "search")  return <SearchResultsView query={activeQuery} focused={searchFocused} />;
    if (view === "errors")  return <ErrorsView errors={ERRORS_LIST} />;
    if (view === "stats")   return <StatsView stats={STATS} />;
    if (view === "help")    return <HelpView />;
    return <StatusView runs={RECENT_RUNS} errors={ERRORS_LIST} stats={STATS} uptime={sinceStr} />;
  })();

  const titleFor = (() => {
    if (view === "source" && drilledSource) return `source · ${drilledSource.src.path.split("/").pop()}`;
    if (view === "search") return `search · "${activeQuery}"`;
    if (view === "errors") return "errors";
    if (view === "stats")  return "stats";
    if (view === "help")   return "help";
    return "overview";
  })();

  return (
    <div style={{
      display: "flex",
      flexDirection: "column",
      height: "100vh",
      maxWidth: t.termSize === "default" ? "100vw" : sizePresets[t.termSize].width,
      maxHeight: t.termSize === "default" ? "100vh" : sizePresets[t.termSize].height,
      margin: t.termSize === "default" ? 0 : "auto",
      border: t.termSize === "default" ? "none" : "1px solid var(--border)",
      borderRadius: t.termSize === "default" ? 0 : 6,
      overflow: "hidden",
      position: "relative",
    }}>
      <Header
        activeKb={activeKb}
        connection={connection}
        uptimeStr={uptimeStr}
        indexing={indexing}
      />

      <div style={{
        flex: 1,
        display: "grid",
        gridTemplateColumns: isCramped ? "180px 1fr" : "240px 1fr",
        gap: "var(--pad)",
        padding: "var(--pad)",
        minHeight: 0,
        opacity: connection === "disconnected" ? 0.55 : 1,
        filter: connection === "disconnected" ? "saturate(0.4)" : "none",
      }}>
        <Panel title="sources" focused={focusedPanel === "sidebar"} borderStyle={t.borders}>
          <Sidebar
            kbs={kbs}
            activeKb={activeKb}
            focused={focusedPanel === "sidebar"}
            focusedRow={sidebarRow}
            onSelect={(i) => { setSidebarRow(i); setFocusedPanel("sidebar"); }}
            onToggle={(name) => setKbs(prev => prev.map(g => g.kb === name ? { ...g, expanded: !g.expanded } : g))}
          />
        </Panel>

        <div style={{ display: "grid", gridTemplateRows: isCramped ? "1fr 110px" : "1fr 170px", gap: "var(--pad)", minHeight: 0 }}>
          <Panel title={titleFor} focused={focusedPanel === "main"} borderStyle={t.borders} style={{ minHeight: 0 }}>
            {mainView}
          </Panel>
          <Panel title={`activity log${logFilter !== "all" ? ` · ${logFilter}` : ""}`} focused={focusedPanel === "log"} borderStyle={t.borders} style={{ minHeight: 0 }}>
            <ActivityLog entries={log} focused={focusedPanel === "log"} density={t.logDensity} filter={logFilter} />
          </Panel>
        </div>
      </div>

      <Footer
        mode={mode}
        focusedPanel={focusedPanel}
        commandText={commandText}
        searchText={searchText}
        suggestions={suggestions}
        onCommandChange={(v) => mode === "command" ? setCommandText(v) : setSearchText(v)}
        onCommandSubmit={() => mode === "command" ? runCommand() : runSearch()}
      />

      {/* Help overlay */}
      {showHelp && (
        <Modal onClose={() => setShowHelp(false)} title="help">
          <HelpView />
        </Modal>
      )}
      {showGraph && (
        <Modal onClose={() => setShowGraph(false)} title="graph">
          <GraphView />
        </Modal>
      )}

      {/* Disconnected banner overlay */}
      {connection === "disconnected" && (
        <div style={{
          position: "absolute",
          top: "calc(var(--row) + 6px)",
          left: 0, right: 0,
          background: "var(--red)",
          color: palette.bg,
          padding: "4px var(--pad)",
          textAlign: "center",
          fontWeight: 500,
          letterSpacing: "0.05em",
        }}>
          ✗  disconnected from kb-server@localhost:4000  ·  retry in 4s  ·  panels showing last-known state
        </div>
      )}

      <Tweaks t={t} setTweak={setTweak} />
    </div>
  );
}

function Modal({ title, onClose, children }) {
  return (
    <div
      onClick={onClose}
      style={{
        position: "absolute", inset: 0,
        background: "rgba(0,0,0,0.35)",
        display: "flex", alignItems: "center", justifyContent: "center",
        zIndex: 30,
      }}
    >
      <div
        onClick={e => e.stopPropagation()}
        style={{
          background: "var(--bg)",
          border: "2px solid var(--violet)",
          minWidth: "min(720px, 90vw)",
          maxWidth: "95vw",
          maxHeight: "85vh",
          overflow: "auto",
          position: "relative",
        }}
      >
        <div style={{
          padding: "4px var(--pad)",
          background: "var(--violet)",
          color: "var(--bg)",
          fontWeight: 700,
          letterSpacing: "0.1em",
          textTransform: "uppercase",
          display: "flex",
          justifyContent: "space-between",
        }}>
          <span>▌ {title}</span>
          <span style={{ cursor: "pointer", opacity: 0.85 }} onClick={onClose}>[Esc] close</span>
        </div>
        {children}
      </div>
    </div>
  );
}

/* ───────────────────────── TWEAKS ───────────────────────── */
function Tweaks({ t, setTweak }) {
  return (
    <TweaksPanel title="tweaks">
      <TweakSection label="palette">
        <TweakSelect
          label="theme"
          value={t.palette}
          onChange={v => setTweak("palette", v)}
          options={[
            { value: "solarized-light", label: "solarized light" },
            { value: "tokyo-night",     label: "tokyo night" },
            { value: "github-dark",     label: "github dark" },
            { value: "gruvbox",         label: "gruvbox dark" },
            { value: "phosphor",        label: "phosphor crt" },
          ]}
        />
      </TweakSection>
      <TweakSection label="connection">
        <TweakRadio
          value={t.connection}
          onChange={v => setTweak("connection", v)}
          options={[
            { value: "connected",    label: "connected" },
            { value: "degraded",     label: "degraded" },
            { value: "disconnected", label: "offline" },
          ]}
        />
      </TweakSection>
      <TweakSection label="borders">
        <TweakRadio
          value={t.borders}
          onChange={v => setTweak("borders", v)}
          options={[
            { value: "single",  label: "single" },
            { value: "double",  label: "double" },
            { value: "rounded", label: "rounded" },
          ]}
        />
      </TweakSection>
      <TweakSection label="activity log">
        <TweakRadio
          value={t.logDensity}
          onChange={v => setTweak("logDensity", v)}
          options={[
            { value: "off",     label: "off" },
            { value: "medium",  label: "medium" },
            { value: "lively",  label: "lively" },
          ]}
        />
      </TweakSection>
      <TweakSection label="main view">
        <TweakSelect
          label="show"
          value={t.view}
          onChange={v => setTweak("view", v)}
          options={[
            { value: "status", label: "status overview" },
            { value: "source", label: "source detail" },
            { value: "search", label: "search results" },
            { value: "errors", label: "errors" },
            { value: "stats",  label: "stats" },
            { value: "help",   label: "help" },
          ]}
        />
      </TweakSection>
      <TweakSection label="terminal size">
        <TweakRadio
          value={t.termSize}
          onChange={v => setTweak("termSize", v)}
          options={[
            { value: "cramped", label: "80×24" },
            { value: "default", label: "fit" },
            { value: "wide",    label: "wide" },
          ]}
        />
      </TweakSection>
    </TweaksPanel>
  );
}

ReactDOM.createRoot(document.getElementById("root")).render(<App />);
