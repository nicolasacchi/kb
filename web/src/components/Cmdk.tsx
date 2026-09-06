import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ComponentType,
  type SVGProps,
} from "react";
import { Link, useMatch, useNavigate } from "react-router-dom";
import { Icon } from "./icons";
import { useSearch } from "../hooks/useSearch";
import { useExplicitKb } from "../hooks/useActiveKb";
import {
  fetchKbs,
  fetchRecall,
  type RecallHit,
  type SearchMode,
} from "../api/client";
import { fetchNotesAll, type NoteSummary } from "../api/notes";
import { fetchSessions, type SessionRow } from "../api/sessions";
import {
  fetchHistory,
  recordSearch,
  type HistoryEntry,
} from "../api/history";
import {
  cmdkRowAt,
  cmdkRowBase,
  cmdkTotalRows,
  type CmdkCounts,
} from "./cmdkRows";
import { artifactHref } from "../lib/artifactHref";
import { withKb } from "../lib/navItems";
import { cycleTheme } from "../api/prefs";
import { useIdentity } from "../hooks/useArtifactHost";
import UserChip from "./UserChip";

// X2 — per federated group cap (matches RECENT_VISIBLE feel).
const FED_VISIBLE = 8;

// G8 — last-N recently-opened artifacts shown above the COMMANDS
// section when the search input is empty. Recent dedupes by artifact_id
// (newest visit wins) and the currently-open artifact on the detail
// route is filtered out so users don't see themselves in their own list.
const RECENT_FETCH = 16;
const RECENT_VISIBLE = 8;

function dedupeNewestByArtifact(entries: HistoryEntry[]): HistoryEntry[] {
  const seen = new Set<string>();
  const out: HistoryEntry[] = [];
  for (const e of entries) {
    if (!e.artifact_id || !e.source_relative) continue;
    if (seen.has(e.artifact_id)) continue;
    seen.add(e.artifact_id);
    out.push(e);
  }
  return out;
}

function relativeTime(unixSec: number, nowSec: number): string {
  const diff = Math.max(0, nowSec - unixSec);
  if (diff < 60) return `${Math.floor(diff)}s`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  if (diff < 30 * 86400) return `${Math.floor(diff / 86400)}d`;
  if (diff < 365 * 86400) return `${Math.floor(diff / (30 * 86400))}mo`;
  return `${Math.floor(diff / (365 * 86400))}y`;
}

const MODES: SearchMode[] = ["hybrid", "keyword", "semantic"];

// v0.11 S3 — built-in action rows the palette shows above search hits.
// Match on a substring of the label so typing "mem" surfaces the
// Memory jump; empty query shows them all (the design's primary
// affordance is "type to find what you want, including commands").
type Command = {
  id: string;
  label: string;
  hint: string;
  kbd?: string;
  // SH.I2 — leading row glyph: destinations reuse their navItems icon (kept
  // in lock-step by hand, since Cmdk's COMMANDS list is its own array, not a
  // NAV_ITEMS map), other actions fall back to the generic Icon.Cmd.
  glyph: ComponentType<SVGProps<SVGSVGElement>>;
  run: (deps: CommandDeps) => void;
};
// `kb` is the EXPLICIT active kb (path/query, no first-kb fallback) so the
// go-to commands carry the user's current space — matching the top-button
// strip. Settings stays global (no kb).
type CommandDeps = {
  navigate: ReturnType<typeof useNavigate>;
  onClose: () => void;
  kb: string | null;
};

const COMMANDS: Command[] = [
  {
    id: "go-gallery",
    label: "Gallery — recent artifacts",
    hint: "/",
    kbd: "g g",
    glyph: Icon.Grid,
    run: ({ navigate, onClose, kb }) => {
      navigate(withKb("/", kb));
      onClose();
    },
  },
  {
    id: "go-atlas",
    label: "Atlas — constellation view",
    hint: "/?view=atlas",
    kbd: "g a",
    glyph: Icon.Atlas,
    run: ({ navigate, onClose, kb }) => {
      navigate(withKb("/?view=atlas", kb));
      onClose();
    },
  },
  {
    id: "go-memory",
    label: "Memory — agent recall corpus",
    hint: "/memory",
    kbd: "g m",
    glyph: Icon.Brain,
    run: ({ navigate, onClose, kb }) => {
      navigate(withKb("/memory", kb));
      onClose();
    },
  },
  {
    id: "go-history",
    label: "History — visits timeline",
    hint: "/?view=history",
    kbd: "g h",
    glyph: Icon.History,
    run: ({ navigate, onClose, kb }) => {
      navigate(withKb("/?view=history", kb));
      onClose();
    },
  },
  {
    id: "go-anchors",
    label: "Anchors — corkboard + stale",
    hint: "/anchors",
    // Not one of navItems' 10 view-toggle destinations — reuses the same
    // Icon.Anchor the Header's AnchorPill / ContextBar anchor button already
    // use for this exact route, rather than falling back to the generic
    // Icon.Cmd for a row that IS a place, not an action.
    glyph: Icon.Anchor,
    run: ({ navigate, onClose, kb }) => {
      navigate(withKb("/anchors", kb));
      onClose();
    },
  },
  {
    id: "add-to-list",
    label: "Add this artifact to a reading list…",
    hint: "detail view",
    glyph: Icon.Cmd,
    run: ({ onClose }) => {
      // Only the detail route's ContextBar AddToListButton listens; on
      // other routes this fizzles (the g t / toc-spy.advance precedent).
      window.dispatchEvent(new CustomEvent("kb:add-to-list.open"));
      onClose();
    },
  },
  {
    id: "go-lists",
    label: "Lists — reading queues",
    hint: "/lists",
    kbd: "g l",
    glyph: Icon.Tasks,
    run: ({ navigate, onClose, kb }) => {
      navigate(withKb("/lists", kb));
      onClose();
    },
  },
  {
    id: "go-sessions",
    label: "Sessions — captured agent transcripts",
    hint: "/sessions",
    kbd: "g s",
    glyph: Icon.Terminal,
    run: ({ navigate, onClose, kb }) => {
      navigate(withKb("/sessions", kb));
      onClose();
    },
  },
  {
    id: "go-settings",
    label: "Settings — operator dashboard",
    hint: "/settings",
    kbd: "g ,",
    // Same reasoning as go-anchors — reuses the Header's own Icon.Settings
    // for this destination rather than the generic command glyph.
    glyph: Icon.Settings,
    run: ({ navigate, onClose }) => {
      navigate("/settings");
      onClose();
    },
  },
  {
    id: "capture",
    label: "Capture files or text…",
    hint: "upload .md/.html into a kb",
    glyph: Icon.Cmd,
    run: ({ onClose }) => {
      // App owns the CaptureSheet's open state; it's not (and shouldn't be)
      // mounted per-route, so reach it the same way "add-to-list" does —
      // a CustomEvent App.tsx listens for at the root.
      window.dispatchEvent(new CustomEvent("kb:capture.open"));
      onClose();
    },
  },
  {
    id: "cycle-theme",
    label: "Toggle theme",
    hint: "dark → light → system",
    glyph: Icon.Cmd,
    run: ({ onClose }) => {
      cycleTheme();
      onClose();
    },
  },
  {
    id: "keyboard-shortcuts",
    label: "Keyboard shortcuts (?)",
    hint: "chords, marks, registers, hints — the full cheat sheet",
    kbd: "?",
    glyph: Icon.Cmd,
    run: ({ onClose }) => {
      // Same channel Header's "?" icon button uses — HotkeyRoot owns the
      // actual helpOpen state (SH.D.4).
      window.dispatchEvent(new CustomEvent("kb:keyhelp.toggle"));
      onClose();
    },
  },
];

// Cmd+K command palette / search modal. Three modes cycled by Tab:
// hybrid (BM25 + vector RRF, default), keyword (BM25 only), semantic
// (vector only). semantic + hybrid require an embedding_model on the
// kb; the daemon returns 400 if not, which we show inline.
//
// Trigger: ⌘K or Ctrl+K. Esc closes. Enter on a result navigates to
// the detail view; ↑↓ moves the selection.
//
// Self-contained — owns its own input ref, focus management, and
// keyboard handlers. No portal: rendered inline at the App root with
// position: fixed.
export default function Cmdk({
  kb: kbProp,
  onClose,
}: {
  kb?: string;
  onClose: () => void;
}) {
  const [q, setQ] = useState("");
  const [mode, setMode] = useState<SearchMode>("hybrid");
  const [cursor, setCursor] = useState(0);
  const [fallbackKb, setFallbackKb] = useState<string | undefined>(undefined);
  const [recents, setRecents] = useState<HistoryEntry[]>([]);
  // X2 — federated groups. notes + sessions are bounded lists fetched ONCE on
  // open and filtered client-side as the user types; memory is an embedding
  // recall, re-fetched per (debounced) query. All three show only when the user
  // has typed — the empty palette stays recents + commands.
  const [allNotes, setAllNotes] = useState<NoteSummary[]>([]);
  const [allSessions, setAllSessions] = useState<SessionRow[]>([]);
  const [memories, setMemories] = useState<RecallHit[]>([]);
  const identity = useIdentity();
  const me = identity?.user;
  const inputRef = useRef<HTMLInputElement | null>(null);
  // No useFocusTrap here on purpose: Cmdk's onKey already `preventDefault()`s
  // Tab (it cycles the search mode), so the browser never moves focus out of
  // the dialog — a trap would only double-act at the list boundary.
  const navigate = useNavigate();
  const kb = kbProp ?? fallbackKb;
  // Explicit active kb (no first-kb fallback) for the go-to commands, so a
  // jump from a reader keeps that artifact's space and an unscoped view
  // stays kb-less. `kb` above (resolved) still scopes search + result links.
  const explicitKb = useExplicitKb();
  const result = useSearch(q, mode, kb);
  // Detect the detail route so we can exclude the currently-open
  // artifact from Recent. `useMatch` returns null off-route.
  const detailMatch = useMatch("/a/:kb/*");
  const currentKb = detailMatch?.params.kb ?? null;
  const currentRel = detailMatch?.params["*"] ?? null;
  // Frozen at modal open — Recent rows say "2m ago" relative to when
  // the user popped the palette, not a live ticker.
  const openedAtRef = useRef(Math.floor(Date.now() / 1000));

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    if (!kb) return;
    let alive = true;
    const ctl = new AbortController();
    fetchHistory(kb, {
      limit: RECENT_FETCH,
      kind: "open",
      signal: ctl.signal,
    })
      .then((entries) => {
        if (!alive) return;
        setRecents(dedupeNewestByArtifact(entries));
      })
      .catch(() => {
        // History is best-effort — a failure here just leaves Recent
        // empty and the rest of the palette works.
        if (alive) setRecents([]);
      });
    return () => {
      alive = false;
      ctl.abort();
    };
  }, [kb]);

  // X2 — fetch the notes + sessions lists once when the palette opens (bounded,
  // best-effort; a failure just leaves that group empty). They're filtered
  // client-side below as the user types, so no per-keystroke refetch.
  useEffect(() => {
    const ctl = new AbortController();
    fetchNotesAll(undefined, ctl.signal)
      .then((r) => setAllNotes(r.notes))
      .catch(() => setAllNotes([]));
    fetchSessions(ctl.signal)
      .then(setAllSessions)
      .catch(() => setAllSessions([]));
    return () => ctl.abort();
  }, []);

  // X2 — memory recall is a real (embedding) search, so it re-runs per query.
  // 80ms debounce + AbortController, mirroring useSearch; scope=all federates
  // across corpora ("find any memory"). A modal-lifecycle fetch that does NOT
  // ride the query cache / SSE bridge — the documented #23 Cmdk carve-out, same
  // as useSearch here.
  useEffect(() => {
    const needle = q.trim();
    if (!needle) {
      setMemories([]);
      return;
    }
    const ctl = new AbortController();
    const t = setTimeout(() => {
      fetchRecall({ q: needle, scope: "all", limit: FED_VISIBLE }, ctl.signal)
        .then((r) => {
          if (!ctl.signal.aborted) setMemories(r.hits.slice(0, FED_VISIBLE));
        })
        .catch(() => {
          if (!ctl.signal.aborted) setMemories([]);
        });
    }, 80);
    return () => {
      clearTimeout(t);
      ctl.abort();
    };
  }, [q]);

  useEffect(() => {
    // When the App-level URL has no ?kb=, pick the first configured kb
    // so Enter on a result still routes to /a/{kb}/{id}. This fetch is
    // cheap (and cached by the browser between modal opens via etag).
    if (!kbProp) {
      fetchKbs()
        .then((ks) => {
          if (ks.length > 0) setFallbackKb(ks[0].name);
        })
        .catch(() => {
          // The error is surfaced via the search hook below; here we
          // silently leave fallbackKb undefined and Enter is a no-op.
        });
    }
  }, [kbProp]);

  useEffect(() => {
    setCursor(0);
  }, [q, mode]);

  const cycleMode = useCallback(() => {
    setMode((m) => MODES[(MODES.indexOf(m) + 1) % MODES.length]);
  }, []);

  // Track F — escalate to the full search page, carrying the typed query,
  // the active kb, and the current mode. The "type fast in the popup, then
  // go deep" bridge: ⌘/Ctrl+↵ or the footer link.
  const goToFullSearch = useCallback(() => {
    const p = new URLSearchParams();
    if (q.trim()) p.set("q", q);
    if (kb) p.set("kb", kb);
    if (mode !== "hybrid") p.set("mode", mode);
    onClose();
    navigate(`/search${p.toString() ? `?${p.toString()}` : ""}`);
  }, [q, kb, mode, onClose, navigate]);

  // S3 — filter built-in commands by substring of label + hint. Empty
  // query → show all.
  const visibleCommands = useMemo(() => {
    const needle = q.trim().toLowerCase();
    if (!needle) return COMMANDS;
    return COMMANDS.filter(
      (c) =>
        c.label.toLowerCase().includes(needle) ||
        c.hint.toLowerCase().includes(needle),
    );
  }, [q]);

  // G8 — Recent shows only when the user has typed nothing. Excludes
  // the currently-open artifact (same kb + source_relative). The list
  // was already deduped + filtered at fetch time.
  const visibleRecents = useMemo<HistoryEntry[]>(() => {
    if (q.trim() !== "") return [];
    const filtered = recents.filter((r) => {
      if (!r.source_relative) return false;
      if (currentKb && currentRel && kb === currentKb) {
        return r.source_relative !== currentRel;
      }
      return true;
    });
    return filtered.slice(0, RECENT_VISIBLE);
  }, [q, recents, currentKb, currentRel, kb]);

  // X2 — notes + sessions filtered by the typed needle (substring on the human
  // label), capped, shown only when the user has typed.
  const visibleNotes = useMemo<NoteSummary[]>(() => {
    const needle = q.trim().toLowerCase();
    if (!needle) return [];
    return allNotes
      .filter((n) =>
        (n.title || n.source_relative || "").toLowerCase().includes(needle),
      )
      .slice(0, FED_VISIBLE);
  }, [q, allNotes]);
  const visibleSessions = useMemo<SessionRow[]>(() => {
    const needle = q.trim().toLowerCase();
    if (!needle) return [];
    return allSessions
      .filter((s) =>
        (s.title || s.first_user_prompt || "").toLowerCase().includes(needle),
      )
      .slice(0, FED_VISIBLE);
  }, [q, allSessions]);

  // Combined nav cursor — recents, commands, hits, then the federated groups
  // (notes, memory, sessions). The flat cursor indexes the concatenation; the
  // tested cmdkRows geometry is the single source of truth for both ↑↓/Enter
  // and the per-row highlight so they can never disagree.
  const counts: CmdkCounts = useMemo(
    () => ({
      recents: visibleRecents.length,
      commands: visibleCommands.length,
      hits: result.hits.length,
      notes: visibleNotes.length,
      memories: memories.length,
      sessions: visibleSessions.length,
    }),
    [
      visibleRecents.length,
      visibleCommands.length,
      result.hits.length,
      visibleNotes.length,
      memories.length,
      visibleSessions.length,
    ],
  );
  const totalRows = cmdkTotalRows(counts);

  const onKey = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        cycleMode();
        return;
      }
      // ⌘/Ctrl+↵ escalates to the full search page (before the plain-Enter
      // open-result handler below).
      if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        goToFullSearch();
        return;
      }
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setCursor((c) => Math.min(c + 1, totalRows - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setCursor((c) => Math.max(c - 1, 0));
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        const at = cmdkRowAt(cursor, counts);
        if (!at) return;
        switch (at.kind) {
          case "recent": {
            const r = visibleRecents[at.idx];
            if (r?.source_relative && kb) {
              onClose();
              navigate(artifactHref(kb, r.source_relative));
            }
            return;
          }
          case "command":
            visibleCommands[at.idx]?.run({ navigate, onClose, kb: explicitKb });
            return;
          case "hit": {
            const hit = result.hits[at.idx];
            if (hit && kb) {
              // v0.6+ H4 — record the query as a deliberate intent (Enter /
              // click-result, not every keystroke). Server-side dedups within
              // 5s so spurious double-fires are harmless.
              void recordSearch(kb, q);
              onClose();
              navigate(artifactHref(kb, hit.source_relative));
            }
            return;
          }
          case "note": {
            const n = visibleNotes[at.idx];
            if (n) {
              onClose();
              navigate(artifactHref(n.kb, n.source_relative));
            }
            return;
          }
          case "memory": {
            const m = memories[at.idx];
            if (m) {
              onClose();
              navigate(artifactHref(m.kb, m.source_relative));
            }
            return;
          }
          case "session": {
            const s = visibleSessions[at.idx];
            if (s) {
              onClose();
              navigate(`/memory?session=${encodeURIComponent(s.session_id)}`);
            }
            return;
          }
        }
      }
    },
    [
      counts,
      cursor,
      cycleMode,
      explicitKb,
      goToFullSearch,
      kb,
      memories,
      navigate,
      onClose,
      q,
      result.hits,
      totalRows,
      visibleCommands,
      visibleNotes,
      visibleRecents,
      visibleSessions,
    ],
  );

  return (
    <div
      className="cmdk-backdrop"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      role="presentation"
    >
      <div
        className="cmdk"
        role="dialog"
        aria-modal="true"
        aria-label="search"
        onKeyDown={onKey}
      >
        <div className="cmdk__head">
          <input
            ref={inputRef}
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Search artifacts (Tab to cycle modes, Esc to close)"
            className="cmdk__input"
            aria-label="search query"
          />
          <button
            className="cmdk__mode"
            onClick={cycleMode}
            aria-label={`mode: ${mode}; click to cycle`}
            title="Tab to cycle"
          >
            {mode}
          </button>
        </div>
        <div className="cmdk__body" role="listbox" aria-label="search results">
          {visibleRecents.length > 0 && (
            <>
              <div className="cmdk__section cmdk__section--recent">Recent</div>
              {visibleRecents.map((r, i) => {
                const rowIdx = i;
                const rel = r.source_relative ?? "";
                const title = r.title || rel.split("/").pop() || rel;
                const when = relativeTime(r.updated_at, openedAtRef.current);
                return kb && rel ? (
                  <Link
                    key={`recent-${r.id}`}
                    to={artifactHref(kb, rel)}
                    role="option"
                    aria-selected={rowIdx === cursor}
                    className={`cmdk__hit cmdk__hit--recent ${rowIdx === cursor ? "is-active" : ""}`}
                    onMouseEnter={() => setCursor(rowIdx)}
                    onClick={(e) => {
                      if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0)
                        return;
                      onClose();
                    }}
                  >
                    <span className="cmdk__hit-title">
                      <Icon.Doc aria-hidden="true" /> {title}
                      <UserChip
                        user={r.user}
                        me={me}
                        className="cmdk__hit-user"
                      />
                    </span>
                    <span className="cmdk__hit-path" title={rel}>
                      <span className="cmdk__recent-when">{when}</span>
                      {rel}
                    </span>
                  </Link>
                ) : null;
              })}
            </>
          )}
          {visibleCommands.length > 0 && (
            <>
              <div className="cmdk__section">Commands</div>
              {visibleCommands.map((c, i) => {
                const rowIdx = visibleRecents.length + i;
                const Glyph = c.glyph;
                return (
                  <button
                    key={c.id}
                    type="button"
                    role="option"
                    aria-selected={rowIdx === cursor}
                    className={`cmdk__hit cmdk__hit--cmd ${rowIdx === cursor ? "is-active" : ""}`}
                    onMouseEnter={() => setCursor(rowIdx)}
                    onClick={() => c.run({ navigate, onClose, kb: explicitKb })}
                  >
                    <span className="cmdk__hit-title">
                      <Glyph aria-hidden="true" /> {c.label}
                    </span>
                    <span className="cmdk__hit-path">
                      {c.kbd ? <kbd>{c.kbd}</kbd> : null}
                      {c.kbd ? " " : ""}
                      {c.hint}
                    </span>
                  </button>
                );
              })}
              {result.hits.length > 0 && (
                <div className="cmdk__section">Search results</div>
              )}
            </>
          )}
          {result.error && (
            <div className="cmdk__error" role="alert">
              {result.error}
            </div>
          )}
          {result.loading && <div className="cmdk__hint">searching…</div>}
          {!result.loading &&
            !result.error &&
            q &&
            result.hits.length === 0 &&
            visibleNotes.length === 0 &&
            memories.length === 0 &&
            visibleSessions.length === 0 && (
              <div className="cmdk__hint">no matches</div>
            )}
          {result.hits.map((h, i) => {
            const rowIdx = visibleRecents.length + visibleCommands.length + i;
            return kb ? (
              // Real anchor so ctrl/⌘/middle-click opens the result in a
              // new kb tab natively. recordSearch still fires (same H4
              // intent signal as Enter); onClose only on an unmodified
              // primary click so a new-tab click leaves the palette open.
              <Link
                key={h.id}
                to={artifactHref(kb, h.source_relative)}
                role="option"
                aria-selected={rowIdx === cursor}
                className={`cmdk__hit ${rowIdx === cursor ? "is-active" : ""}`}
                onMouseEnter={() => setCursor(rowIdx)}
                onClick={(e) => {
                  void recordSearch(kb, q);
                  if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0)
                    return;
                  onClose();
                }}
              >
                <span className="cmdk__hit-title">
                  <Icon.Doc aria-hidden="true" /> {h.title || "(untitled)"}
                </span>
                {/* D8 — `h.path` is the absolute on-disk path the indexer
                    stored; never render it. `source_relative` is the same
                    kb-relative path `artifactHref` above already navigates
                    with. */}
                <span className="cmdk__hit-path" title={h.source_relative}>
                  {h.source_relative}
                </span>
              </Link>
            ) : (
              <button
                key={h.id}
                role="option"
                aria-selected={rowIdx === cursor}
                className={`cmdk__hit ${rowIdx === cursor ? "is-active" : ""}`}
                onMouseEnter={() => setCursor(rowIdx)}
                disabled
              >
                <span className="cmdk__hit-title">
                  <Icon.Doc aria-hidden="true" /> {h.title || "(untitled)"}
                </span>
                {/* D8 — `h.path` is the absolute on-disk path the indexer
                    stored; never render it. `source_relative` is the same
                    kb-relative path `artifactHref` above already navigates
                    with. */}
                <span className="cmdk__hit-path" title={h.source_relative}>
                  {h.source_relative}
                </span>
              </button>
            );
          })}
          {visibleNotes.length > 0 && (
            <>
              <div className="cmdk__section">Notes</div>
              {visibleNotes.map((n, i) => {
                const rowIdx = cmdkRowBase("note", counts) + i;
                return (
                  <Link
                    key={`note-${n.kb}-${n.id}`}
                    to={artifactHref(n.kb, n.source_relative)}
                    role="option"
                    aria-selected={rowIdx === cursor}
                    className={`cmdk__hit cmdk__hit--note ${rowIdx === cursor ? "is-active" : ""}`}
                    onMouseEnter={() => setCursor(rowIdx)}
                    onClick={(e) => {
                      if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0)
                        return;
                      onClose();
                    }}
                  >
                    <span className="cmdk__hit-title">
                      <Icon.Note aria-hidden="true" /> {n.title || n.source_relative}
                    </span>
                    <span className="cmdk__hit-path" title={n.source_relative}>
                      {n.kb} · {n.source_relative}
                    </span>
                  </Link>
                );
              })}
            </>
          )}
          {memories.length > 0 && (
            <>
              <div className="cmdk__section">Memory</div>
              {memories.map((m, i) => {
                const rowIdx = cmdkRowBase("memory", counts) + i;
                return (
                  <Link
                    key={`mem-${m.kb}-${m.id}`}
                    to={artifactHref(m.kb, m.source_relative)}
                    role="option"
                    aria-selected={rowIdx === cursor}
                    className={`cmdk__hit cmdk__hit--memory ${rowIdx === cursor ? "is-active" : ""}`}
                    onMouseEnter={() => setCursor(rowIdx)}
                    onClick={(e) => {
                      if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0)
                        return;
                      onClose();
                    }}
                  >
                    <span className="cmdk__hit-title">
                      <Icon.Brain aria-hidden="true" /> {m.title || "(untitled)"}
                    </span>
                    <span className="cmdk__hit-path" title={m.source_relative}>
                      {m.kb} · {m.source_relative}
                    </span>
                  </Link>
                );
              })}
            </>
          )}
          {visibleSessions.length > 0 && (
            <>
              <div className="cmdk__section">Sessions</div>
              {visibleSessions.map((s, i) => {
                const rowIdx = cmdkRowBase("session", counts) + i;
                const label =
                  s.title ||
                  s.first_user_prompt ||
                  `session ${s.session_id.slice(0, 8)}`;
                return (
                  <Link
                    key={`sess-${s.session_id}`}
                    to={`/memory?session=${encodeURIComponent(s.session_id)}`}
                    role="option"
                    aria-selected={rowIdx === cursor}
                    className={`cmdk__hit cmdk__hit--session ${rowIdx === cursor ? "is-active" : ""}`}
                    onMouseEnter={() => setCursor(rowIdx)}
                    onClick={(e) => {
                      if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0)
                        return;
                      onClose();
                    }}
                  >
                    <span className="cmdk__hit-title">
                      <Icon.Terminal aria-hidden="true" /> {label}
                    </span>
                    <span className="cmdk__hit-path">
                      {s.message_count} msg
                      {s.memory_count > 0 ? ` · ${s.memory_count} mem` : ""}
                    </span>
                  </Link>
                );
              })}
            </>
          )}
        </div>
        <div className="cmdk__foot">
          {/* N1 — announce result counts to assistive tech as they land.
              role=status + aria-live=polite means a screen-reader user hears
              "5 results · 12ms" without the count being a visible-only cue. */}
          <span
            className="cmdk__counter"
            role="status"
            aria-live="polite"
            aria-atomic="true"
          >
            {result.hits.length > 0 &&
              `${result.hits.length} result${result.hits.length === 1 ? "" : "s"} · ${result.ms}ms${result.cacheHit ? " · cached" : ""}`}
          </span>
          <button
            type="button"
            className="cmdk__seeall"
            onClick={goToFullSearch}
            title="Open the full search page with this query (⌘↵)"
          >
            See all in Search →
          </button>
          <span className="cmdk__keys">
            <kbd>↑</kbd>
            <kbd>↓</kbd> nav · <kbd>↵</kbd> open · <kbd>⌘↵</kbd> all ·{" "}
            <kbd>Tab</kbd> mode · <kbd>Esc</kbd> close
          </span>
        </div>
      </div>
    </div>
  );
}
