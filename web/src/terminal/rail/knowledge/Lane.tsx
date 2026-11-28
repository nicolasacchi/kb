// RAILKNOW — one lane's frame: a header with its freshness + source caption
// and the shared state cards. Each lane owns its own query (no shared
// Promise.all), so one failing lane never touches another.

import type { ReactElement, ReactNode } from "react";
import { StateCard } from "@kb/ui/StateCard";

export type LaneId = "injected" | "written" | "search" | "similar" | "research";

export function LaneFrame({
  lane,
  title,
  fresh,
  source,
  children,
}: {
  lane: LaneId;
  title: string;
  fresh: "From last capture" | "Live search";
  source: string;
  children: ReactNode;
}): ReactElement {
  return (
    <section className="kb-knowrail__lane" data-testid={`kn-lane-${lane}`} aria-label={title}>
      <h3 className="kb-knowrail__h">{title}</h3>
      <p className="kb-knowrail__fresh" data-testid={`kn-fresh-${lane}`}>
        {fresh} · {source}
      </p>
      {children}
    </section>
  );
}

type StateKind = "loading" | "empty" | "error" | "not-captured" | "no-session";

export function LaneState({
  lane,
  kind,
  reason,
  onRetry,
}: {
  lane: LaneId;
  kind: StateKind;
  reason?: string;
  onRetry?: () => void;
}): ReactElement {
  const id = kind === "no-session" ? "empty" : kind;
  let card: ReactElement;
  switch (kind) {
    case "loading":
      card = <StateCard kind="loading" size="inline" title="Loading" />;
      break;
    case "no-session":
      card = (
        <StateCard
          kind="empty"
          size="inline"
          title="No session linked yet"
          reason="This terminal links to a session on the first harness beat."
        />
      );
      break;
    case "not-captured":
      card = <StateCard kind="empty" size="inline" title="Not captured yet" reason="Appears after the next capture" />;
      break;
    case "empty":
      card = <StateCard kind="empty" size="inline" title="Nothing yet" reason={reason} />;
      break;
    case "error":
      card = (
        <StateCard
          kind="degraded"
          size="inline"
          title="Could not load this lane"
          reason={reason}
          actions={onRetry ? [{ label: "Retry", key: "retry", onClick: onRetry }] : undefined}
        />
      );
      break;
  }
  return (
    <div data-testid={`kn-state-${lane}-${id}`} className="kb-knowrail__state">
      {card}
    </div>
  );
}
