// T17 — the Read view: native scrolling and native text selection over a
// snapshot of the panel's own xterm buffer (no round trip, so it works on
// a bad network). The snapshot is taken when Read opens and again on
// "Refresh"; it never follows the stream by itself.

import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { toast } from "../../lib/toast";
import {
  bufferToCopyText,
  centerScrollTop,
  isImageLine,
  linkParts,
  snapshotLines,
  type ReadBuffer,
} from "./readText";
import { takePressedLine } from "./pressedLine";

const MAX_LINES = 20_000;

export interface ReadViewProps {
  /** Reads the buffer at call time (called on open and on Refresh). */
  read: () => ReadBuffer;
  fontSize: number;
  cols: number;
  onWatch: () => void;
}

interface Snapshot {
  lines: string[];
  /** Rows the line cap cut off the front (a buffer row minus this is a `lines` index). */
  dropped: number;
  /** Copy all: the same rows with soft-wrapped continuations joined back. */
  copy: string;
  at: string;
}

function take(read: () => ReadBuffer): Snapshot {
  const buf = read();
  const { lines, dropped } = snapshotLines(buf, { max: MAX_LINES });
  return {
    lines,
    dropped,
    copy: bufferToCopyText(buf, { max: MAX_LINES }),
    at: new Date().toTimeString().slice(0, 8),
  };
}

/**
 * Plain runs stay in one text node (a 20k-line snapshot is a handful of nodes,
 * not tens of thousands); an http(s) URL becomes an anchor (selection still
 * works across it) and an image placeholder line becomes a dim marker.
 */
function renderLines(lines: string[]): ReactNode[] {
  const out: ReactNode[] = [];
  let run = "";
  let key = 0;
  const flush = (): void => {
    if (run !== "") {
      out.push(run);
      run = "";
    }
  };
  for (const line of lines) {
    if (isImageLine(line)) {
      flush();
      // T15: render the picture here once its renderer is available.
      out.push(
        <span key={key++} className="kb-term-read__image">
          {"[image]\n"}
        </span>,
      );
      continue;
    }
    for (const part of linkParts(line)) {
      if (typeof part === "string") {
        run += part;
      } else {
        flush();
        out.push(
          <a key={key++} className="kb-term-read__link" href={part.url} target="_blank" rel="noopener noreferrer">
            {part.url}
          </a>,
        );
      }
    }
    run += "\n";
  }
  flush();
  return out;
}

export function ReadView({ read, fontSize, cols, onWatch }: ReadViewProps) {
  const [snap, setSnap] = useState<Snapshot>(() => take(read));
  const scroller = useRef<HTMLDivElement | null>(null);
  const text = useRef<HTMLPreElement | null>(null);
  const [copied, setCopied] = useState(false);
  // FUPHONELINKS — the row a long press opened Read on, used for the first snapshot only.
  const target = useRef<number | null | undefined>(undefined);
  if (target.current === undefined) target.current = takePressedLine();

  // Start (and restart after Refresh) scrolled to the newest line, except the
  // first snapshot of a long press: centred on the pressed row.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const line = target.current;
    target.current = null;
    const pre = text.current;
    if (line !== null && line !== undefined && pre && snap.lines.length > 0) {
      const idx = Math.min(snap.lines.length - 1, Math.max(0, line - snap.dropped));
      const pad = parseFloat(getComputedStyle(pre).paddingTop) || 0;
      el.scrollTop = centerScrollTop({
        line: idx,
        lines: snap.lines.length,
        contentHeight: pre.scrollHeight,
        viewHeight: el.clientHeight,
        pad,
      });
      return;
    }
    el.scrollTop = el.scrollHeight;
  }, [snap]);

  const copyAll = (): void => {
    // A user action: a failed write is shown (#32), never reported as Copied.
    const write = navigator.clipboard?.writeText(snap.copy);
    if (!write) {
      toast.err("Copy failed: clipboard unavailable");
      return;
    }
    void write.then(
      () => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      },
      () => toast.err("Copy failed: clipboard refused the write"),
    );
  };

  return (
    <div className="kb-term-read" data-testid="kb-term-read" data-kb-term-read="">
      <div className="kb-term-read__bar">
        <span className="kb-term-read__caption" data-testid="kb-term-read-caption">
          Snapshot at {snap.at}
        </span>
        <button
          type="button"
          className="kb-term-phone__btn"
          data-testid="kb-term-read-refresh"
          onClick={() => {
            setCopied(false);
            setSnap(take(read));
          }}
        >
          Refresh
        </button>
        <button
          type="button"
          className="kb-term-phone__btn"
          data-testid="kb-term-read-copy"
          onClick={copyAll}
        >
          {copied ? "Copied" : "Copy all"}
        </button>
        <button
          type="button"
          className="kb-term-phone__btn"
          data-testid="kb-term-read-watch"
          onClick={onWatch}
        >
          Watch
        </button>
      </div>
      <div className="kb-term-read__scroll" ref={scroller} data-kb-term-read-scroll="">
        <pre
          className="kb-term-read__text"
          data-testid="kb-term-read-text"
          ref={text}
          data-kb-term-cols={cols}
          style={{ fontSize: `${fontSize}px` }}
        >
          {renderLines(snap.lines)}
        </pre>
      </div>
    </div>
  );
}
