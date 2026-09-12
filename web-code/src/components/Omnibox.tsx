import { useEffect, useMemo, useReducer, useRef, useState } from "react";
import { useRamp } from "../nav/ramp";
import { useMatch, useNavigate } from "react-router";
import type { TranscriptHit } from "../api/types";
import { readerUrl } from "../lib/breadcrumbs";
import { recordJump } from "../lib/navHistory";
import { initialPaletteState, paletteReducer } from "../lib/paletteReducer";
import { sectionsToRowCounts } from "../lib/omniSearch";
import { applyPrefixChip } from "../lib/prefixChips";
import { fullSearchUrl, orderSections } from "../lib/searchLanes";
import { resolveSearchTarget } from "../lib/searchTargets";
import { useOmniSearch } from "../hooks/useOmniSearch";
import PrefixChips from "./search/PrefixChips";
import SearchSection from "./search/SearchSection";
import { useCommands } from "../commands/CommandRoot";
import {
  commandRows,
  COMMAND_PREFIX,
  isCommandQuery,
  splitRows,
  type CommandRow,
} from "../commands/commandRows";
import { KBC_SCOPES } from "../commands/registry.gen";
import { highlightSegments } from "../lib/speedSearch";
import { useConfirm } from "./ConfirmProvider";
import { toast } from "../lib/toast";

const LIMIT = 8;

export interface OmniboxProps {
  onClose: () => void;
  /// V70-A5 — open straight into COMMAND mode (the `>` prefix pre-filled).
  /// `:` and `Space :` land here; ⌘K still opens in search mode, so the
  /// box's front door is unchanged for everyone who already knows it.
  initialQuery?: string;
}

/// The Search-Everywhere box's full-screen overlay (W4.3) - the SPA's front
/// door, opened by Cmd/Ctrl+K anywhere (`app.tsx`'s global keydown
/// listener) or the `kbc:omnibox.open` CustomEvent every visible "Search…"
/// affordance dispatches (`Home.tsx`/`Reader.tsx`'s top-bar buttons -
/// mirrors kb's own `kb:capture.open` idiom for a root-mounted overlay
/// reached from several routes).
///
/// **Keyboard**: the input keeps real DOM focus the entire time (never
/// moved to a row) - Up/Down/Tab/Enter are captured on the dialog wrapper
/// via `onKeyDown` and drive a virtual cursor (`lib/paletteReducer.ts`)
/// instead of native browser focus traversal, the same "Tab is repurposed,
/// input never loses focus" shape kb's own `web/src/components/Cmdk.tsx`
/// uses (there: Tab cycles the search MODE; here: Tab moves the active
/// SECTION). Up/Down move the row cursor within the active section only;
/// Enter opens whatever `lib/searchTargets.ts` resolves the cursor to
/// (a lane header -> the full `/search?q=` page; a row -> its own lane-
/// shaped destination); Esc closes an open transcript popover first, else
/// the whole box.
///
/// Repo scoping: opened from inside the reader (`/r/:repo/*`), the search
/// is scoped to that repo (`?repo=` on `GET /api/search`) same as the
/// route itself; opened from Home (or anywhere else), it searches every
/// configured repo.
export default function Omnibox({ onClose, initialQuery = "" }: OmniboxProps) {
  // V70-A6 — the Ramp (§P7): the omnibox is a result surface, so its rows
  // take the same five rungs as every other one.
  const ramp = useRamp({ focusedPane: 1, onNavigated: () => onClose() });
  const [q, setQ] = useState(initialQuery);
  const [popoverHit, setPopoverHit] = useState<TranscriptHit | null>(null);
  const [showUnavailable, setShowUnavailable] = useState(false);
  const [cmdCursor, setCmdCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const navigate = useNavigate();
  const repoMatch = useMatch("/r/:repo/*");
  const repo = repoMatch?.params.repo;
  const bus = useCommands();
  const confirm = useConfirm();

  // V70-A5 — COMMAND MODE. `>` switches the box from six content lanes to the
  // kbc-cmd/1 registry. The search query is suppressed while it is on, so a
  // command-mode keystroke never fires a repo-wide text search.
  const commandMode = isCommandQuery(q);
  const rows = useMemo(
    () => (commandMode ? commandRows(q, bus.scope, bus.ctx, bus.preset) : []),
    [commandMode, q, bus.scope, bus.ctx, bus.preset],
  );
  const { available, unavailable } = useMemo(() => splitRows(rows), [rows]);
  const visibleRows = useMemo(
    () => (showUnavailable ? [...available, ...unavailable] : available),
    [showUnavailable, available, unavailable],
  );
  useEffect(() => {
    setCmdCursor(0);
  }, [q, showUnavailable]);

  const { sections, loading, error } = useOmniSearch(commandMode ? "" : q, repo, LIMIT);
  const [state, dispatch] = useReducer(paletteReducer, initialPaletteState());
  const ordered = useMemo(() => orderSections(sections), [sections]);

  /// Run a palette row. `mutation !== "none"` goes through the ONE
  /// ConfirmProvider (root CLAUDE.md #32) — a palette makes every destructive
  /// verb one fuzzy match away from Enter, and the recon's open question 8
  /// asked for exactly this tier. An unregistered handler is reported, not
  /// swallowed: the row said it was available, so a silent no-op would be the
  /// palette lying.
  async function runCommand(row: CommandRow) {
    if (!row.available) {
      toast.warn(`${row.command.title} — ${row.reason}`);
      return;
    }
    if (row.command.mutation !== "none" || row.command.sideEffect !== "none") {
      const ok = await confirm({
        title: row.command.title,
        body:
          row.command.mutation === "working-tree"
            ? "This touches the working tree."
            : row.command.sideEffect === "remote"
              ? "This talks to a remote."
              : "This writes a kb-code record.",
        confirmLabel: "Run",
        danger: row.command.mutation === "working-tree",
      });
      if (!ok) return;
    }
    onClose();
    if (!bus.run(row.command.id)) {
      toast.warn(`${row.command.title} has no handler on this surface yet`);
    }
  }

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    dispatch({ type: "SET_SECTIONS", sections: sectionsToRowCounts(sections) });
  }, [sections]);

  function activate(target: ReturnType<typeof resolveSearchTarget>) {
    if (!target) return;
    switch (target.kind) {
      case "full-search":
        onClose();
        navigate(fullSearchUrl(q, repo));
        return;
      case "reader":
        // V3.N1 — omnibox navigation is jump-class (file open effect will
        // also see the path change; recording here keeps the destination
        // line accurate even when the Reader effect only has `?line=`).
        recordJump({
          repo: target.repo,
          path: target.path,
          line: target.line ?? 1,
          snippet: "",
        });
        onClose();
        navigate(readerUrl(target.repo, target.path, undefined, target.line));
        return;
      case "external":
        // Opens in a new tab natively - the box stays open (same "a new-tab
        // click shouldn't yank the palette shut" rule kb's own Cmdk applies
        // to a modifier-click).
        window.open(target.href, "_blank", "noreferrer");
        return;
      case "popover":
        setPopoverHit((cur) => (cur?.uuid === target.hit.uuid ? null : target.hit));
        return;
    }
  }

  // V73-K6 — this box is one of `pickers`' several owners of the `palette`
  // scope's rows (`registry.json`'s own `covers` text for that scope: "the
  // omnibox and the transient pick-one-from-a-list overlays" —
  // `StructurePopup.tsx`/`RecentLocations.tsx`/`LineHistoryPopup.tsx` claim
  // `palette.row-next`/`palette.row-prev` too, each independently, which is
  // that scope's own `j`/`k`-only-while-the-filter-is-empty note). Routing
  // this through `commands/dispatch.ts`'s shared `resolve()` was considered
  // and rejected: `commandMode`'s row list is built from `bus.scope`/
  // `bus.ctx` (`commandRows(q, bus.scope, bus.ctx, bus.preset)`, above) —
  // deliberately the UNDERLYING route's scope, so `>` lists that surface's
  // own commands — and publishing `useCommandScope("palette", …)` here
  // would overwrite exactly that value while the box is open, breaking the
  // one property command mode depends on. A direct listener, kept exactly
  // as it already was, is the correct, working owner; only the doc comments
  // below (and the `owner` field on each row) are new.
  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      if (popoverHit) {
        // kbc-owns: "dismiss.popover":
        setPopoverHit(null);
        return;
      }
      // kbc-owns: "dismiss.palette":
      onClose();
      return;
    }
    // Command mode drives its OWN one-section cursor: there are no lanes to
    // Tab between, so Up/Down/Enter is the whole grammar (plus the
    // show-unavailable toggle, which is a registry row like any other).
    if (commandMode) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        // kbc-owns: "palette.row-next": / "palette.row-prev":
        e.preventDefault();
        const delta = e.key === "ArrowDown" ? 1 : -1;
        setCmdCursor((c) => {
          if (visibleRows.length === 0) return 0;
          return (c + delta + visibleRows.length) % visibleRows.length;
        });
        return;
      }
      if (e.key === "Enter") {
        // kbc-owns: "palette.activate":
        e.preventDefault();
        const row = visibleRows[cmdCursor];
        if (row) void runCommand(row);
        return;
      }
      // `Ctrl-H` in the canonical token form == Ctrl+Shift+h (a shifted
      // printable folds into the character — see the registry's
      // `reserved_chords.note`).
      if (e.key === "H" && e.ctrlKey) {
        // kbc-owns: "palette.show-unavailable":
        e.preventDefault();
        setShowUnavailable((v) => !v);
        return;
      }
      return;
    }
    if (e.key === "Tab") {
      // kbc-owns: "palette.section-next": / "palette.section-prev":
      e.preventDefault();
      dispatch({ type: "MOVE_SECTION", delta: e.shiftKey ? -1 : 1 });
      return;
    }
    if (e.key === "ArrowDown") {
      // kbc-owns: "palette.row-next":
      e.preventDefault();
      dispatch({ type: "MOVE_ROW", delta: 1 });
      return;
    }
    if (e.key === "ArrowUp") {
      // kbc-owns: "palette.row-prev":
      e.preventDefault();
      dispatch({ type: "MOVE_ROW", delta: -1 });
      return;
    }
    if (e.key === "Enter") {
      // kbc-owns: "palette.activate":
      e.preventDefault();
      activate(resolveSearchTarget(sections, state.cursor, repo, q));
      return;
    }
  }

  return (
    <div
      className="kbc-omnibox-backdrop"
      role="presentation"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="kbc-omnibox"
        role="dialog"
        aria-modal="true"
        aria-label={commandMode ? "commands" : "search"}
        onKeyDown={onKeyDown}
      >
        <div className="kbc-omnibox__head">
          <input
            ref={inputRef}
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder={
              commandMode
                ? "Run a command…"
                : "Search files, symbols, text, semantic, sessions, transcripts…  (> for commands)"
            }
            className="kbc-omnibox__input"
            aria-label={commandMode ? "command query" : "search query"}
          />
          {loading && (
            <span className="kbc-omnibox__status" aria-live="polite">
              searching…
            </span>
          )}
        </div>
        {!commandMode && (
          <PrefixChips
            onInsert={(chip) => {
              setQ((cur) => applyPrefixChip(cur, chip));
              inputRef.current?.focus();
            }}
          />
        )}
        {commandMode ? (
          <div className="kbc-omnibox__body" role="listbox" aria-label="commands" data-kbc-cmdmode>
            {visibleRows.length === 0 && (
              <div className="kbc-omnibox__hint">No command matches.</div>
            )}
            {visibleRows.map((row, i) => (
              <button
                key={row.command.id}
                type="button"
                role="option"
                aria-selected={i === cmdCursor}
                className={
                  "kbc-omnibox__cmdrow" +
                  (i === cmdCursor ? " is-active" : "") +
                  (row.available ? "" : " is-unavailable")
                }
                data-kbc-cmdrow={row.command.id}
                onMouseEnter={() => setCmdCursor(i)}
                onClick={() => void runCommand(row)}
              >
                <span className="kbc-omnibox__cmdtitle">
                  {highlightSegments(row.command.title, row.ranges).map((seg, si) =>
                    seg.hit ? <mark key={si}>{seg.text}</mark> : <span key={si}>{seg.text}</span>,
                  )}
                </span>
                <span className="kbc-omnibox__cmdgroup">{row.command.group}</span>
                {!row.available && <span className="kbc-omnibox__cmdwhy">{row.reason}</span>}
                {/* Fixed, right-aligned key column — the palette is where a
                    mouse user LEARNS the key, so it is never omitted, only
                    ever empty (palette-only in this preset). */}
                <span className="kbc-omnibox__cmdkey">{row.key ? <kbd>{row.key}</kbd> : null}</span>
              </button>
            ))}
            {unavailable.length > 0 && (
              <button
                type="button"
                className="kbc-omnibox__cmdtoggle"
                data-kbc-cmdtoggle
                aria-expanded={showUnavailable}
                onClick={() => setShowUnavailable((v) => !v)}
              >
                {showUnavailable ? "Hide" : "Show"} unavailable ({unavailable.length}) — why
              </button>
            )}
          </div>
        ) : (
          <div className="kbc-omnibox__body" role="listbox" aria-label="search results">
            {error && (
              <div className="kbc-omnibox__error" role="alert">
                {error}
              </div>
            )}
            {!error && !loading && ordered.length === 0 && (
              <div className="kbc-omnibox__hint">
                Type to search files, symbols, text, semantic, sessions, transcripts.
                <br />
                <span className="kbc-omnibox__legend">
                  <kbd>{COMMAND_PREFIX}</kbd> commands · <kbd>@</kbd> symbols · <kbd>#</kbd> files ·{" "}
                  <kbd>/</kbd> text · <kbd>~</kbd> sessions
                </span>
              </div>
            )}
            {!error &&
              ordered.map((section, i) => (
                <SearchSection
                  key={section.lane}
                  section={section}
                  laneIndex={i}
                  sections={sections}
                  cursor={state.cursor}
                  query={q}
                  repo={repo}
                  expandedTranscriptUuid={popoverHit?.uuid}
                  onHover={(row) => dispatch({ type: "SET_CURSOR", cursor: { section: i, row } })}
                  onNavigate={onClose}
                  onPopover={(hit) => activate({ kind: "popover", hit })}
                  onRamp={(rung, target) => {
                    ramp.activate(rung, target);
                  }}
                />
              ))}
          </div>
        )}
        <div className="kbc-omnibox__foot">
          <span className="kbc-omnibox__keys">
            {commandMode ? (
              <>
                <kbd>↑</kbd>
                <kbd>↓</kbd> row · <kbd>↵</kbd> run · <kbd>Esc</kbd> close
              </>
            ) : (
              <>
                <kbd>↑</kbd>
                <kbd>↓</kbd> row · <kbd>Tab</kbd> section · <kbd>↵</kbd> open · <kbd>Esc</kbd> close
              </>
            )}
          </span>
          {/* The active scope, always visible in command mode: which rows you
              are looking at is a fact about WHERE YOU ARE, and hiding it is
              how a palette starts feeling arbitrary. */}
          {commandMode && (
            <span className="kbc-omnibox__scope" data-kbc-cmdscope={bus.scope}>
              {KBC_SCOPES.find((sc) => sc.id === bus.scope)?.title ?? bus.scope}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
