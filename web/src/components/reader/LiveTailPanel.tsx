// W7 (sessions-rethink R15/LF-2) — the Tier-1 live tail panel: a SPA-native
// ticker over `useLiveTail`'s accumulated turns, rendered by
// `ArtifactPane` as reader-column content (ContextBar-adjacent, above the
// iframe — invariant #30: NO new rail icon, NO new sheet). NOT a second
// full transcript renderer — bounded to the last `LIVE_TAIL_MAX_TURNS`
// (200) turns; anything not in the bounded IR kind set the hook emits
// (task boards, side lanes) shows as a count chip, deferring full
// semantics to the captured render once it lands (LF-4's stated scope).
//
// Pinned-to-end (terminal convention): while pinned, new turns auto-scroll
// the panel; ANY upward scroll unpins; a floating "↓ N new" pill re-pins.
// Pin state is component state, never persisted (#23-clean).
//
// XSS: every IR field renders exclusively as React text nodes through JSX
// — no raw-HTML injection path exists in this component.

import { useEffect, useMemo, useRef, useState } from "react";
import type { Turn } from "../../api/generated/Turn";
import type { Item } from "../../api/generated/Item";
import { useLiveTail } from "../../hooks/useLiveTail";
import { harnessGlyph } from "../../lib/sessionChips";

export interface LiveTailPanelProps {
  sessionId: string;
  harness: string;
}

export default function LiveTailPanel({
  sessionId,
  harness,
}: LiveTailPanelProps) {
  const { turns, truncatedAtTop, taskUpdates, live, error, parseFailures } =
    useLiveTail(sessionId, true);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [pinned, setPinned] = useState(true);
  const lastRenderedCount = useRef(0);
  const [newSincePin, setNewSincePin] = useState(0);

  // Pinned-to-end: auto-scroll on new turns while pinned.
  useEffect(() => {
    const grew = turns.length - lastRenderedCount.current;
    lastRenderedCount.current = turns.length;
    if (grew <= 0) return;
    if (pinned) {
      const el = scrollRef.current;
      if (el) el.scrollTop = el.scrollHeight;
    } else {
      setNewSincePin((n) => n + grew);
    }
  }, [turns, pinned]);

  function onScroll() {
    const el = scrollRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
    if (nearBottom && !pinned) {
      setPinned(true);
      setNewSincePin(0);
    } else if (!nearBottom && pinned) {
      setPinned(false);
    }
  }

  function rePinToEnd() {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
    setPinned(true);
    setNewSincePin(0);
  }

  const lastTurn = turns.length > 0 ? turns[turns.length - 1] : null;
  const activity = useMemo(() => currentActivity(lastTurn), [lastTurn]);

  return (
    <div
      className="kb-livetail"
      data-testid="live-tail-panel"
      data-live={live ? "1" : "0"}
    >
      <div className="kb-livetail__head" data-testid="live-tail-header">
        <span className="kb-livetail__dot" aria-hidden>
          ●
        </span>
        <span className="kb-livetail__label">
          {live ? "live — writing now" : "live tail — no output recently"}
        </span>
        <span className="kb-livetail__glyph" title={harness} aria-hidden>
          {harnessGlyph(harness)}
        </span>
        <span className="kb-livetail__count">{turns.length} turn(s)</span>
        {activity && <span className="kb-livetail__activity">{activity}</span>}
        {parseFailures > 0 && (
          <span
            className="kb-livetail__warn"
            title={`${parseFailures} line(s) could not be interpreted this session`}
          >
            ⚠ {parseFailures}
          </span>
        )}
        {taskUpdates.length > 0 && (
          <span
            className="kb-livetail__tasks"
            title={taskUpdates.map((t) => `${t.subject}: ${t.status}`).join("\n")}
          >
            +{taskUpdates.length} task event(s)
          </span>
        )}
      </div>
      {error && (
        <div className="kb-livetail__error" role="status">
          live poll failed: {error} — retrying…
        </div>
      )}
      <div
        className="kb-livetail__body"
        ref={scrollRef}
        onScroll={onScroll}
        data-testid="live-tail-body"
      >
        {truncatedAtTop && (
          <div className="kb-livetail__truncated">
            full transcript below ↑
          </div>
        )}
        {turns.length === 0 && !error && (
          <div className="kb-livetail__empty">
            watching for new activity…
          </div>
        )}
        {turns.map((t) => (
          <TurnCard key={t.id} turn={t} harness={harness} />
        ))}
      </div>
      {!pinned && (
        <button
          type="button"
          className="kb-livetail__pin-pill"
          onClick={rePinToEnd}
          data-testid="live-tail-repin"
        >
          ↓ {newSincePin} new
        </button>
      )}
    </div>
  );
}

function TurnCard({ turn, harness }: { turn: Turn; harness: string }) {
  const who = turn.role === "human" ? "you" : harness;
  return (
    <article
      className={`kb-livetail__turn kb-livetail__turn--${turn.role}`}
      id={turn.id}
      data-testid="live-tail-turn"
    >
      <header className="kb-livetail__turn-head">
        <span className="kb-livetail__turn-who">{who}</span>
        {turn.ts && <span className="kb-livetail__turn-ts">{shortTime(turn.ts)}</span>}
      </header>
      <div className="kb-livetail__turn-items">
        {turn.items.map((item, i) => (
          // eslint-disable-next-line react/no-array-index-key -- items have no stable id of their own
          <ItemLine key={i} item={item} />
        ))}
      </div>
    </article>
  );
}

function ItemLine({ item }: { item: Item }) {
  switch (item.item) {
    case "Prose":
      return <p className="kb-livetail__prose">{item.text}</p>;
    case "Thinking":
      return item.empty ? null : (
        <p className="kb-livetail__thinking">· thinking ({item.len} chars)</p>
      );
    case "ToolCall": {
      const err = item.result?.is_error;
      return (
        <p className="kb-livetail__tool">
          <span className="kb-livetail__tool-name">{item.name}</span>{" "}
          {item.headline}
          {err && <span className="kb-livetail__tool-err"> ✗</span>}
          {!err && item.unpaired && (
            <span className="kb-livetail__tool-unpaired"> …</span>
          )}
        </p>
      );
    }
    case "Command":
      return (
        <p className="kb-livetail__command">
          ⌁ /{item.name} {item.args ?? ""}
        </p>
      );
    case "TaskEvent":
      return (
        <p className="kb-livetail__task">
          ◔ {item.subject} → {item.transition}
        </p>
      );
    case "Decision":
      return (
        <p className="kb-livetail__decision">
          ◆ {item.prompt}
          {item.answer && <> → {item.answer}</>}
        </p>
      );
    case "SystemReminder":
      return <p className="kb-livetail__reminder">ⓘ {item.preview}</p>;
    case "ModeChange":
      return <p className="kb-livetail__mode">⌁ mode → {item.mode}</p>;
    case "TimeGap":
      return <p className="kb-livetail__gap">⋯ {item.secs}s gap</p>;
    case "WorkflowCard":
      return (
        <p className="kb-livetail__workflow">
          ⛭ workflow {item.name ?? item.task_id ?? ""} · {item.phases.length}{" "}
          phase(s)
        </p>
      );
    case "KbCommand":
      return (
        <p className="kb-livetail__kbcmd">
          kb {item.verb} {item.args}
        </p>
      );
    case "MemoryInjection":
      return (
        <p className="kb-livetail__memory">
          ⌁ memories: {item.items.length} injected
        </p>
      );
    case "Raw":
      // Honesty (never silently drop): mirrors the CLI presenter's
      // "? unparsed (...)" line.
      return <p className="kb-livetail__raw">? unparsed ({item.reason})</p>;
    default:
      return null;
  }
}

/// The live header's "current activity" line (cockpit-honest, design
/// archaeology lesson #7): the last item of the last received turn, in
/// compact form. `null` when there's nothing to report yet.
function currentActivity(turn: Turn | null): string | null {
  if (!turn || turn.items.length === 0) return null;
  const last = turn.items[turn.items.length - 1];
  switch (last.item) {
    case "ToolCall":
      return `⚙ ${last.name} ${last.headline}`;
    case "Prose":
      return turn.role === "human" ? "user turn" : "assistant reply";
    case "Thinking":
      return "thinking";
    case "Command":
      return `/${last.name}`;
    default:
      return null;
  }
}

/// `YYYY-MM-DDTHH:MM:SS...` → `HH:MM:SS` (mirrors the CLI presenter's
/// `short_time`). `String.match` (not `RegExp.exec`) is the same
/// capture-group extraction, just spelled the other way.
function shortTime(ts: string): string {
  const m = ts.match(/T(\d{2}:\d{2}:\d{2})/);
  return m ? m[1] : ts;
}
