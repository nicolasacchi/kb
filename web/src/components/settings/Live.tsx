import { useEffect, useRef, useState } from "react";
import { KNOWN_EVENT_KINDS, useEventLog } from "../../hooks/useEventLog";
import { Icon } from "../icons";

// Group event kinds for the filter checklist so the picker stays
// scannable. Order is intentional: highest-volume groups at the top
// so a "deselect noisy stuff first" mental model maps to the layout.
const KIND_GROUPS: Array<[string, string[]]> = [
  ["index", ["index.start", "index.file", "index.embedding", "index.complete"]],
  ["artifact", ["artifact.indexed", "artifact.removed"]],
  ["watcher", ["watch.create", "watch.modify", "watch.delete", "watcher.lagged"]],
  ["query", ["query"]],
  ["error", ["error", "error.dismissed", "error.fixed"]],
  ["source", ["source.paused", "source.resumed"]],
  ["atlas", [
    "atlas.recompute.start",
    "atlas.recompute.complete",
    "atlas.recluster.start",
    "atlas.recluster.complete",
  ]],
  ["comments", [
    "comments.updated",
    "comment.anchor_stale",
    "comment.anchor_resolved",
  ]],
  ["history", ["history.recorded"]],
  ["reconcile", ["reconcile.complete"]],
  ["memory", [
    "memory.ingested",
    "memory.stale",
    "memory.resolved",
    "memory.forgotten",
  ]],
  ["metrics", ["metrics.tick"]],
  ["meta", ["lag", "gap"]],
];

const FILTER_KEY = "kb:settings:live:filter";

function loadFilter(): Set<string> {
  try {
    const raw = localStorage.getItem(FILTER_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed)) return new Set(parsed.filter((s) => typeof s === "string"));
    }
  } catch {
    // fallthrough
  }
  // Default: everything except metrics.tick (1Hz; usually pure noise
  // in a live tail unless you're debugging the metrics path).
  return new Set(KNOWN_EVENT_KINDS.filter((k) => k !== "metrics.tick"));
}

function saveFilter(set: Set<string>) {
  try {
    localStorage.setItem(FILTER_KEY, JSON.stringify(Array.from(set)));
  } catch {
    // ignore
  }
}

// Live tab — SSE firehose tail. Filter checklist (group-collapsible),
// pause/resume the buffer, clear it, and an auto-scroll-to-bottom
// that respects user scroll-back (when the user scrolls up, we stop
// jumping). 500-entry ring keeps the DOM bounded.
export default function Live() {
  const [paused, setPaused] = useState(false);
  const [kindFilter, setKindFilter] = useState<Set<string>>(loadFilter);

  useEffect(() => {
    saveFilter(kindFilter);
  }, [kindFilter]);

  const { entries, clear } = useEventLog({
    enabled: true,
    paused,
    kindFilter,
  });

  // Auto-follow-tail: scroll the table container to the bottom when
  // new entries land, unless the user has scrolled up (`stuck` is
  // true when they're within 30px of the bottom).
  const tailRef = useRef<HTMLDivElement | null>(null);
  const stickRef = useRef(true);
  useEffect(() => {
    const el = tailRef.current;
    if (!el) return;
    if (stickRef.current) el.scrollTop = el.scrollHeight;
  }, [entries.length]);

  function toggleKind(k: string) {
    setKindFilter((prev) => {
      const next = new Set(prev);
      if (next.has(k)) next.delete(k);
      else next.add(k);
      return next;
    });
  }
  function toggleGroup(kinds: string[]) {
    setKindFilter((prev) => {
      const next = new Set(prev);
      const allOn = kinds.every((k) => next.has(k));
      if (allOn) {
        for (const k of kinds) next.delete(k);
      } else {
        for (const k of kinds) next.add(k);
      }
      return next;
    });
  }

  const visibleCount = entries.length;
  return (
    <div className="dash live">
      <div className="live__chrome">
        <div className="live__filter" aria-label="event kind filter">
          {KIND_GROUPS.map(([group, kinds]) => {
            const allOn = kinds.every((k) => kindFilter.has(k));
            const someOn = kinds.some((k) => kindFilter.has(k));
            return (
              <details key={group} className="live__group">
                <summary>
                  <input
                    type="checkbox"
                    aria-label={`toggle group ${group}`}
                    checked={allOn}
                    ref={(el) => {
                      if (el) el.indeterminate = !allOn && someOn;
                    }}
                    onChange={(e) => {
                      e.stopPropagation();
                      toggleGroup(kinds);
                    }}
                    // Stop the click from toggling the <details>.
                    onClick={(e) => e.stopPropagation()}
                  />
                  <span>{group}</span>
                  <span className="live__group-n">{kinds.length}</span>
                </summary>
                <div className="live__group-body">
                  {kinds.map((k) => (
                    <label key={k} className="live__kind">
                      <input
                        type="checkbox"
                        checked={kindFilter.has(k)}
                        onChange={() => toggleKind(k)}
                      />
                      <code>{k}</code>
                    </label>
                  ))}
                </div>
              </details>
            );
          })}
        </div>

        <div className="live__actions">
          <button
            type="button"
            className={`settings__btn ${paused ? "is-active" : ""}`}
            onClick={() => {
              setPaused((p) => !p);
            }}
            aria-pressed={paused}
          >
            {paused ? (
              <><Icon.Play aria-hidden="true" /> resume</>
            ) : (
              <><Icon.Pause aria-hidden="true" /> pause</>
            )}
          </button>
          <button type="button" className="settings__btn" onClick={clear}>
            <Icon.X aria-hidden="true" /> clear
          </button>
          <span className="live__count">
            {visibleCount} / 500 {paused && "(paused)"}
          </span>
        </div>
      </div>

      <div
        className="live__tail"
        ref={tailRef}
        onScroll={(e) => {
          const el = e.currentTarget;
          stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
      >
        {entries.length === 0 ? (
          <div className="settings__hint">
            no events yet — interact with the daemon (open an artifact, run a
            search) to see frames here.
          </div>
        ) : (
          <table className="dash__table live__table">
            <thead>
              <tr>
                <th>time</th>
                <th>kind</th>
                <th>payload</th>
              </tr>
            </thead>
            <tbody>
              {entries.map((e) => (
                <tr key={e.seq}>
                  <td className="live__time">{fmtMs(e.at)}</td>
                  <td>
                    <code className={kindClass(e.kind)}>{e.kind}</code>
                  </td>
                  <td>
                    <PayloadCell payload={e.payload} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

function fmtMs(at: number): string {
  const d = new Date(at);
  return d.toLocaleTimeString(undefined, { hour12: false }) + "." +
    String(d.getMilliseconds()).padStart(3, "0");
}

function kindClass(kind: string): string {
  if (kind === "error" || kind === "watcher.lagged" || kind === "lag" || kind === "gap")
    return "is-err";
  if (kind.endsWith(".start")) return "is-info";
  if (kind.endsWith(".complete")) return "is-ok";
  return "";
}

function PayloadCell({ payload }: { payload: Record<string, unknown> }) {
  // Render an inline summary: pull a handful of well-known keys to the
  // front (kb, path, run, q, hits, ms…) and fall back to a JSON tail
  // for the rest. Keeps the table dense yet still informative.
  const keysFront = [
    "kb",
    "path",
    "run",
    "q",
    "hits",
    "ms",
    "mode",
    "kind",
    "id",
    "artifact_id",
    "src",
  ];
  const entries: Array<[string, unknown]> = [];
  for (const k of keysFront) {
    if (k in payload) entries.push([k, payload[k]]);
  }
  for (const k of Object.keys(payload)) {
    if (!keysFront.includes(k)) entries.push([k, payload[k]]);
  }
  return (
    <span className="live__payload">
      {entries.map(([k, v], i) => (
        <span key={k}>
          <span className="live__key">{k}=</span>
          <span className="live__val">{fmtVal(v)}</span>
          {i < entries.length - 1 && " "}
        </span>
      ))}
    </span>
  );
}

function fmtVal(v: unknown): string {
  if (v == null) return "—";
  if (typeof v === "string") return v.length > 64 ? v.slice(0, 64) + "…" : v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  try {
    const s = JSON.stringify(v);
    return s.length > 64 ? s.slice(0, 64) + "…" : s;
  } catch {
    return "?";
  }
}
