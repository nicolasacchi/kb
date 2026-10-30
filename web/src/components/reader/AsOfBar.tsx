// OL TIME2 — the caption strip on a HISTORICAL render (`?at=<unix>`).
//
// The daemon serves the artifact body as it stood; this strip is the honest
// label for that: which moment was asked for, which version answered it
// (nearest earlier, unless the second matched), and the sibling caveat. It
// also owns the two exits: "then | now" (a split holding the SAME artifact at
// its working version in the second pane) and "back to today".
//
// It renders only while `?at=` is present, so the default reader is
// byte-identical. The resolution is the SPA mirror of the daemon's
// (`lib/memento.ts`) over the `["versions", kb, id]` timeline the rail
// already holds; no extra fetch.

import { useMemo } from "react";
import { useVersions } from "../../hooks/useVersions";
import { resolveMemento, type Memento } from "../../lib/memento";
import { asOfCaption, fmtLocalTs } from "../../lib/asOf";

export function AsOfBarView({
  memento,
  loading,
  split,
  onSplit,
  onToday,
}: {
  memento: Memento | null;
  loading: boolean;
  /** "open" = offer then|now; "close" = the split is open, offer to close it. */
  split?: "open" | "close";
  onSplit?: () => void;
  onToday: () => void;
}) {
  const miss = memento !== null && memento.version === null;
  return (
    <div
      className={`asof-bar${miss ? " asof-bar--miss" : ""}`}
      role="status"
      data-testid="asof-bar"
    >
      <span className="asof-bar__msg" data-testid="asof-caption">
        {memento
          ? asOfCaption(memento, fmtLocalTs)
          : loading
            ? "opening an earlier version…"
            : "this artifact has no recorded versions"}
      </span>
      <span className="asof-bar__acts">
        {split && onSplit && (
          <button
            type="button"
            className="asof-bar__btn"
            data-testid="asof-split"
            onClick={onSplit}
            title={
              split === "open"
                ? "show this moment beside the current file"
                : "close the then | now split"
            }
          >
            {split === "open" ? "then | now" : "close split"}
          </button>
        )}
        <button
          type="button"
          className="asof-bar__btn"
          data-testid="asof-today"
          onClick={onToday}
        >
          back to today
        </button>
      </span>
    </div>
  );
}

export default function AsOfBar({
  kb,
  id,
  at,
  split,
  onSplit,
  onToday,
}: {
  kb: string;
  id: string;
  at: number;
  split?: "open" | "close";
  onSplit?: () => void;
  onToday: () => void;
}) {
  const { versions, loading, error } = useVersions(kb, id, true);
  const memento = useMemo(
    () => (loading || error ? null : resolveMemento(versions, at)),
    [versions, at, loading, error],
  );
  return (
    <AsOfBarView
      memento={memento}
      loading={loading}
      split={split}
      onSplit={onSplit}
      onToday={onToday}
    />
  );
}
