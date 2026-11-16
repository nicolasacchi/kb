// T17 — the Read view: native scrolling and native text selection over a
// snapshot of the panel's own xterm buffer (no round trip, so it works on
// a bad network). The snapshot is taken when Read opens and again on
// "Refresh"; it never follows the stream by itself.

import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { bufferToLines, isImageLine, type ReadBuffer } from "./readText";

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
  at: string;
}

function take(read: () => ReadBuffer): Snapshot {
  return {
    lines: bufferToLines(read(), { max: MAX_LINES }),
    at: new Date().toTimeString().slice(0, 8),
  };
}

/** Plain runs stay in one text node; an image placeholder line becomes a dim marker. */
function renderLines(lines: string[]): ReactNode[] {
  const out: ReactNode[] = [];
  let run: string[] = [];
  let key = 0;
  const flush = (): void => {
    if (run.length > 0) {
      out.push(run.join("\n") + "\n");
      run = [];
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
    } else {
      run.push(line);
    }
  }
  flush();
  return out;
}

export function ReadView({ read, fontSize, cols, onWatch }: ReadViewProps) {
  const [snap, setSnap] = useState<Snapshot>(() => take(read));
  const scroller = useRef<HTMLDivElement | null>(null);

  // Start (and restart after Refresh) scrolled to the newest line.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [snap]);

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
          onClick={() => setSnap(take(read))}
        >
          Refresh
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
          data-kb-term-cols={cols}
          style={{ fontSize: `${fontSize}px` }}
        >
          {renderLines(snap.lines)}
        </pre>
      </div>
    </div>
  );
}
