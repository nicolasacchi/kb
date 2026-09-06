/* Views for the C panel: status / source / search / errors / graph / stats / help / commandOutput */
/* Plus shared row primitives. */

const Row = ({ children, focused, dim, color, bg, onClick, style }) => (
  <div
    onClick={onClick}
    style={{
      display: "flex",
      alignItems: "baseline",
      padding: "0 var(--pad)",
      whiteSpace: "pre",
      color: color || (dim ? "var(--fg-muted)" : "inherit"),
      background: focused ? "var(--selection-strong)" : (bg || "transparent"),
      cursor: onClick ? "pointer" : "default",
      ...style,
    }}
  >
    {children}
  </div>
);

const Tag = ({ children, color }) => (
  <span style={{ color: `var(--${color})` }}>{children}</span>
);

const Sep = ({ ch = "─", color = "fg-faint", n = 1 }) => (
  <span style={{ color: `var(--${color})` }}>{ch.repeat(n)}</span>
);

/* ───────────────────────── STATUS OVERVIEW ───────────────────────── */
function StatusView({ runs, errors, stats, uptime }) {
  return (
    <div style={{ padding: "var(--pad) 0" }}>
      <SectionHeader>recent runs</SectionHeader>
      <div style={{ padding: "0 var(--pad)", color: "var(--fg-muted)" }}>
        {"  kb              source                          duration       files       status"}
      </div>
      <div style={{ padding: "0 var(--pad)", color: "var(--fg-faint)" }}>
        {"  ──              ──────                          ────────       ─────       ──────"}
      </div>
      {runs.map((r, i) => (
        <Row key={i}>
          <span style={{ display: "inline-block", width: "16ch", color: "var(--fg-strong)" }}>{r.kb}</span>
          <span style={{ display: "inline-block", width: "32ch" }}>{r.source}</span>
          <span style={{ display: "inline-block", width: "15ch", color: "var(--fg-muted)" }}>{r.dur}</span>
          <span style={{ display: "inline-block", width: "12ch" }}>{r.files}</span>
          <span style={{
            color: r.status.startsWith("⚠") ? "var(--orange)"
                 : r.status === "⏸" ? "var(--fg-muted)"
                 : r.status === "⠋" ? "var(--blue)"
                 : "var(--green)"
          }}>{r.status}</span>
        </Row>
      ))}

      <SectionHeader>error rollup</SectionHeader>
      <Row>
        <span style={{ color: "var(--orange)" }}>⚠</span>
        <span> {errors.length} unresolved </span>
        <span style={{ color: "var(--fg-muted)" }}>across {new Set(errors.map(e=>e.kb)).size} kbs · top files:</span>
      </Row>
      {errors.slice(0, 3).map((e, i) => (
        <Row key={i} dim>
          <span>  · </span>
          <span style={{ color: "var(--red)" }}>{e.file}</span>
          <span>  </span>
          <span style={{ color: "var(--fg-muted)" }}>{e.phase} — {e.msg.slice(0, 48)}{e.msg.length > 48 ? "…" : ""}</span>
        </Row>
      ))}

      <SectionHeader>per-kb mini-stats</SectionHeader>
      <div style={{ padding: "0 var(--pad)", color: "var(--fg-muted)" }}>
        {"  kb              docs       words         edges       last index    watch"}
      </div>
      <div style={{ padding: "0 var(--pad)", color: "var(--fg-faint)" }}>
        {"  ──              ────       ─────         ─────       ──────────    ─────"}
      </div>
      {stats.perKb.map((k, i) => (
        <Row key={i}>
          <span style={{ display: "inline-block", width: "16ch", color: "var(--fg-strong)" }}>{k.kb}</span>
          <span style={{ display: "inline-block", width: "11ch" }}>{k.docs.toLocaleString()}</span>
          <span style={{ display: "inline-block", width: "14ch" }}>{k.words.toLocaleString()}</span>
          <span style={{ display: "inline-block", width: "12ch" }}>{k.edges.toLocaleString()}</span>
          <span style={{ display: "inline-block", width: "14ch", color: "var(--fg-muted)" }}>{k.lastIndex}</span>
          <span style={{
            color: k.watch === "on" ? "var(--green)" : k.watch === "off" ? "var(--fg-muted)" : "var(--yellow)"
          }}>{k.watch}</span>
        </Row>
      ))}

      <div style={{ padding: "var(--pad)", color: "var(--fg-muted)" }}>
        ── uptime since {uptime} · server localhost:4000 · 1 client connected
      </div>
    </div>
  );
}

const SectionHeader = ({ children }) => (
  <div style={{
    padding: "calc(var(--pad) * 1.4) var(--pad) 4px",
    color: "var(--fg-strong)",
    fontWeight: 500,
    letterSpacing: "0.05em",
    textTransform: "lowercase",
  }}>
    {"▌ "}{children}
  </div>
);

/* ───────────────────────── SOURCE DETAIL ───────────────────────── */
function SourceDetailView({ source, focusedFile, onFocusFile, filter }) {
  const detail = SOURCE_DETAIL[source.path] || SOURCE_DETAIL["/home/me/rust"];
  const files = filter
    ? detail.files.filter(f => f.path.toLowerCase().includes(filter.toLowerCase()))
    : detail.files;

  return (
    <div style={{ padding: "var(--pad) 0" }}>
      <Row>
        <span style={{ color: "var(--blue)" }}>❯ </span>
        <span style={{ color: "var(--fg-strong)" }}>{detail.breadcrumb}</span>
        <span style={{ color: "var(--fg-muted)" }}>  ·  {detail.files.length} files{filter ? ` · filter: "${filter}"` : ""}</span>
      </Row>
      <div style={{ padding: "0 var(--pad)", marginTop: "var(--pad)", color: "var(--fg-muted)" }}>
        {"  path                              mtime        size       indexed       status"}
      </div>
      <div style={{ padding: "0 var(--pad)", color: "var(--fg-faint)" }}>
        {"  ────                              ─────        ────       ───────       ──────"}
      </div>
      {files.map((f, i) => (
        <Row key={i} focused={focusedFile === i} onClick={() => onFocusFile && onFocusFile(i)}>
          <span style={{ width: "2ch", display: "inline-block", color: "var(--blue)" }}>{focusedFile === i ? "▶" : " "}</span>
          <span style={{ width: "34ch", display: "inline-block", color: "var(--fg-strong)" }}>{f.path}</span>
          <span style={{ width: "13ch", display: "inline-block", color: "var(--fg-muted)" }}>{f.mtime}</span>
          <span style={{ width: "11ch", display: "inline-block" }}>{f.size}</span>
          <span style={{ width: "14ch", display: "inline-block", color: "var(--fg-muted)" }}>{f.indexed}</span>
          <span style={{
            color: f.status === "⚠" ? "var(--orange)" :
                   f.status === "⠋" ? "var(--blue)" :
                   f.status === "·" ? "var(--fg-muted)" : "var(--green)"
          }}>{f.status}</span>
        </Row>
      ))}
      {files.length === 0 && (
        <Row dim><span>  no files match "{filter}"</span></Row>
      )}
    </div>
  );
}

/* ───────────────────────── SEARCH RESULTS ───────────────────────── */
function SearchResultsView({ query, focused }) {
  const data = { ...SEARCH_RESULTS, query: query || SEARCH_RESULTS.query };
  return (
    <div style={{ padding: "var(--pad) 0" }}>
      <Row>
        <span style={{ color: "var(--blue)" }}>{"⌕ "}</span>
        <span style={{ color: "var(--fg-strong)" }}>"{data.query}"</span>
        <span style={{ color: "var(--fg-muted)" }}>  </span>
        <span style={{
          padding: "0 6px",
          background: "var(--bg-alt)",
          color: "var(--violet)",
          borderRadius: 2,
        }}>{data.mode}</span>
        <span style={{ color: "var(--fg-muted)" }}>  · {data.hits.length} hits in 14ms · all kbs</span>
      </Row>
      <div style={{ height: "var(--pad)" }} />
      {data.hits.map((h, i) => (
        <div
          key={i}
          style={{
            padding: "calc(var(--pad) * 0.5) var(--pad)",
            background: focused === i ? "var(--selection)" : "transparent",
            borderLeft: focused === i ? "2px solid var(--blue)" : "2px solid transparent",
          }}
        >
          <div>
            <span style={{ color: "var(--fg-muted)", display: "inline-block", width: "3ch" }}>{h.rank}.</span>
            <span style={{ color: "var(--fg-strong)" }}>{h.title}</span>
            <span style={{ color: "var(--fg-muted)" }}>  ·  </span>
            <span style={{ color: "var(--cyan)" }}>{h.kb}</span>
            <span style={{ color: "var(--fg-muted)" }}>  ·  score </span>
            <span style={{ color: "var(--green)" }}>{h.score.toFixed(2)}</span>
          </div>
          <div style={{ color: "var(--fg-muted)", paddingLeft: "3ch", textWrap: "pretty", whiteSpace: "normal" }}>
            <Snippet text={h.snippet} />
          </div>
        </div>
      ))}
    </div>
  );
}

function Snippet({ text }) {
  // **bold** as highlight
  const parts = text.split(/(\*\*[^*]+\*\*)/g);
  return parts.map((p, i) =>
    p.startsWith("**")
      ? <span key={i} style={{ background: "var(--selection-strong)", color: "var(--fg-strong)" }}>{p.slice(2, -2)}</span>
      : <span key={i}>{p}</span>
  );
}

/* ───────────────────────── ERRORS VIEW ───────────────────────── */
function ErrorsView({ errors }) {
  const grouped = {};
  for (const e of errors) {
    grouped[e.run] = grouped[e.run] || [];
    grouped[e.run].push(e);
  }
  return (
    <div style={{ padding: "var(--pad) 0" }}>
      <Row>
        <span style={{ color: "var(--red)" }}>{"✗ "}</span>
        <span style={{ color: "var(--fg-strong)" }}>{errors.length} errors</span>
        <span style={{ color: "var(--fg-muted)" }}> across {Object.keys(grouped).length} runs · group by run · </span>
        <span style={{ color: "var(--fg-muted)" }}>[Enter] stacktrace · [d] dismiss · [r] retry</span>
      </Row>
      <div style={{ height: "calc(var(--pad) * 0.6)" }} />
      {Object.entries(grouped).map(([run, list]) => (
        <div key={run} style={{ paddingBottom: "var(--pad)" }}>
          <Row>
            <span style={{ color: "var(--fg-muted)" }}>▾ </span>
            <span style={{ color: "var(--violet)" }}>run {run}</span>
            <span style={{ color: "var(--fg-muted)" }}>  · {list[0].kb} · {list.length} error{list.length > 1 ? "s" : ""}</span>
          </Row>
          {list.map((e, i) => (
            <div key={i} style={{ padding: "0 var(--pad) 0 calc(var(--pad) + 2ch)" }}>
              <div>
                <span style={{ color: "var(--red)" }}>{"✗ "}</span>
                <span style={{ color: "var(--fg-strong)" }}>{e.file}</span>
                <span style={{ color: "var(--fg-muted)" }}>   {e.t}</span>
              </div>
              <div style={{ color: "var(--fg-muted)", paddingLeft: "2ch" }}>
                <span style={{ color: "var(--orange)" }}>[{e.phase}] </span>{e.msg}
              </div>
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}

/* ───────────────────────── GRAPH (ASCII OVERLAY) ───────────────────────── */
function GraphView() {
  // ASCII neighborhood, center node "B" = borrow-checker.html
  const lines = [
    "                                                                               ",
    "                          ┌─ in ─────────────────────────┐                    ",
    "                          │                              │                    ",
    "         ┌─────┐       ┌──┴──┐                         ┌─┴───┐                ",
    "         │  O  │  →    │  L  │  ←──────────────┐       │  W  │                ",
    "         └─────┘       └──┬──┘                 │       └─────┘                ",
    "         ownership        │                    │       weekly-review          ",
    "                          ▼                    │                              ",
    "                       ┌─────┐                 │                              ",
    "                       │  B  │  ←──── center                                  ",
    "                       │  ●  │     borrow-checker                             ",
    "                       └──┬──┘                                                ",
    "                          │                                                   ",
    "                ┌─ out ───┴──────────┐                                        ",
    "                │                    │                                        ",
    "             ┌──▼──┐              ┌──▼──┐                                    ",
    "             │  S  │              │  T  │                                    ",
    "             └─────┘              └─────┘                                    ",
    "             smart-pointers       trait-objects                              ",
    "                                                                              ",
  ];
  return (
    <div style={{ padding: "var(--pad)" }}>
      <Row>
        <span style={{ color: "var(--violet)" }}>graph </span>
        <span style={{ color: "var(--fg-strong)" }}>borrow-checker.html</span>
        <span style={{ color: "var(--fg-muted)" }}>  · 1-hop neighborhood · 4 in / 2 out · ←↑→↓ traverse</span>
      </Row>
      <pre style={{
        margin: "var(--pad) 0 0 0",
        fontFamily: "inherit",
        fontSize: "inherit",
        lineHeight: "var(--row)",
        color: "var(--fg-strong)",
      }}>
        {lines.join("\n")}
      </pre>
      <div style={{ padding: "var(--pad)", color: "var(--fg-muted)", borderTop: "1px solid var(--fg-faint)" }}>
        <div>legend</div>
        <div>  <span style={{ color: "var(--violet)" }}>B</span> borrow-checker.html (center)</div>
        <div>  <span style={{ color: "var(--blue)" }}>L</span> lifetimes.html  ·  <span style={{ color: "var(--blue)" }}>O</span> ownership.html  ·  <span style={{ color: "var(--blue)" }}>W</span> weekly-review.html</div>
        <div>  <span style={{ color: "var(--cyan)" }}>S</span> smart-pointers.html  ·  <span style={{ color: "var(--cyan)" }}>T</span> trait-objects.html</div>
      </div>
    </div>
  );
}

/* ───────────────────────── STATS ───────────────────────── */
function StatsView({ stats }) {
  return (
    <div style={{ padding: "var(--pad) 0" }}>
      <SectionHeader>totals</SectionHeader>
      <div style={{ display: "grid", gridTemplateColumns: "repeat(5, 1fr)", padding: "0 var(--pad)", gap: "var(--pad)" }}>
        {[
          ["kbs",     stats.totals.kbs],
          ["sources", stats.totals.sources],
          ["docs",    stats.totals.docs.toLocaleString()],
          ["words",   stats.totals.words.toLocaleString()],
          ["edges",   stats.totals.edges.toLocaleString()],
        ].map(([k, v]) => (
          <div key={k} style={{ borderLeft: "2px solid var(--cyan)", padding: "0 0 0 var(--pad)" }}>
            <div style={{ color: "var(--fg-muted)" }}>{k}</div>
            <div style={{ color: "var(--fg-strong)", fontSize: "1.5em", lineHeight: "1.5em" }}>{v}</div>
          </div>
        ))}
      </div>

      <SectionHeader>per-kb</SectionHeader>
      <div style={{ padding: "0 var(--pad)", color: "var(--fg-muted)" }}>
        {"  kb              docs       words         edges       last index    watch"}
      </div>
      <div style={{ padding: "0 var(--pad)", color: "var(--fg-faint)" }}>
        {"  ──              ────       ─────         ─────       ──────────    ─────"}
      </div>
      {stats.perKb.map((k, i) => (
        <Row key={i}>
          <span style={{ display: "inline-block", width: "16ch", color: "var(--fg-strong)" }}>{k.kb}</span>
          <span style={{ display: "inline-block", width: "11ch" }}>{k.docs.toLocaleString()}</span>
          <span style={{ display: "inline-block", width: "14ch" }}>{k.words.toLocaleString()}</span>
          <span style={{ display: "inline-block", width: "12ch" }}>{k.edges.toLocaleString()}</span>
          <span style={{ display: "inline-block", width: "14ch", color: "var(--fg-muted)" }}>{k.lastIndex}</span>
          <span style={{
            color: k.watch === "on" ? "var(--green)" : k.watch === "off" ? "var(--fg-muted)" : "var(--yellow)"
          }}>{k.watch}</span>
        </Row>
      ))}

      <SectionHeader>histogram · doc count by kb</SectionHeader>
      <div style={{ padding: "0 var(--pad)" }}>
        {stats.perKb.map((k, i) => {
          const max = Math.max(...stats.perKb.map(x => x.docs));
          const w = Math.round((k.docs / max) * 50);
          return (
            <div key={i} style={{ display: "flex", gap: "1ch" }}>
              <span style={{ width: "16ch", color: "var(--fg-strong)" }}>{k.kb}</span>
              <span style={{ color: "var(--cyan)" }}>{"█".repeat(w)}</span>
              <span style={{ color: "var(--fg-muted)" }}>{k.docs}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/* ───────────────────────── HELP OVERLAY ───────────────────────── */
function HelpView() {
  const groups = [
    ["navigation", [
      ["j / k / ↑ ↓", "navigate items in focused panel"],
      ["h / l / ← →", "switch between panels"],
      ["Tab",         "switch active kb"],
      ["Enter",       "drill in / open in browser"],
      ["Esc",         "back out of view"],
    ]],
    ["actions", [
      ["r",       "reindex selected source/file"],
      ["R",       "reindex active kb"],
      ["Ctrl+R",  "reindex all kbs"],
      ["p",       "pause / resume watcher"],
      ["o",       "open in browser (xdg-open)"],
      ["c",       "copy id / path"],
    ]],
    ["views", [
      ["g",       "graph for selected"],
      ["G",       "gaps view"],
      ["s",       "stats view"],
      ["e",       "errors view"],
      ["?",       "this help"],
    ]],
    ["modes", [
      ["/",       "search / filter current view"],
      [":",       "command mode (tab-completes)"],
      ["v",       "visual mode (multi-select)"],
      ["q",       "quit"],
      ["Ctrl+C",  "force quit"],
    ]],
  ];
  return (
    <div style={{ padding: "var(--pad)", display: "grid", gridTemplateColumns: "1fr 1fr", gap: "calc(var(--pad) * 2)" }}>
      {groups.map(([name, rows]) => (
        <div key={name}>
          <div style={{ color: "var(--violet)", marginBottom: 4 }}>▌ {name}</div>
          {rows.map(([k, d], i) => (
            <div key={i} style={{ display: "flex", padding: "1px 0" }}>
              <span style={{ display: "inline-block", width: "14ch", color: "var(--orange)" }}>{k}</span>
              <span style={{ color: "var(--fg)" }}>{d}</span>
            </div>
          ))}
        </div>
      ))}
      <div style={{ gridColumn: "1 / -1", color: "var(--fg-muted)", paddingTop: "var(--pad)", borderTop: "1px solid var(--fg-faint)" }}>
        kb tui · v0.1.0 · press <span style={{ color: "var(--orange)" }}>?</span> or <span style={{ color: "var(--orange)" }}>Esc</span> to dismiss
      </div>
    </div>
  );
}

Object.assign(window, {
  Row, Tag, Sep, SectionHeader,
  StatusView, SourceDetailView, SearchResultsView,
  ErrorsView, GraphView, StatsView, HelpView,
});
