// MI-W4.3 — a compact vertical DAG for ONE fact's supersede chain: each
// node labelled with title + date + its own inline decay sparkline, arrows
// to what it superseded, forgotten nodes visually distinct, the overlap
// window between a fact and its corrector shaded (TimeArcs idiom —
// duration, not just order). Deliberately small: 2-6 nodes; a longer chain
// truncates with a count rather than growing toward a hairball
// (`lib/lineageDisplay.ts`'s job, never this component's own logic).

import { useEffect, useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchMemoryLineage, type MemoryLineageNode } from "../api/client";
import { useMemoryPolicy } from "../hooks/useMemoryPolicy";
import { planLineageDisplay } from "../lib/lineageDisplay";
import { computeOverlapWindow, ongoingOverlapDays } from "../lib/lineageOverlap";
import DecaySparkline from "./DecaySparkline";
import { Icon } from "./icons";
import IdsGalleryChip from "./IdsGalleryChip";

function formatDate(unix: number | null | undefined): string {
  if (unix == null) return "?";
  return new Date(unix * 1000).toISOString().slice(0, 10);
}

function Node({ node, dropThreshold }: { node: MemoryLineageNode; dropThreshold: number | null }) {
  return (
    <div
      className={`kb-lineage__node${node.forgotten ? " kb-lineage__node--forgotten" : ""}`}
      data-testid="lineage-node"
      data-id={node.id}
    >
      <div className="kb-lineage__node-head">
        <span className="kb-lineage__node-title">{node.title}</span>
        {node.forgotten && <span className="kb-lineage__node-tag">forgotten</span>}
      </div>
      <div className="kb-lineage__node-meta">
        <span className="kb-lineage__node-date">{formatDate(node.created_unix)}</span>
        <code className="kb-lineage__node-id">{node.id}</code>
      </div>
      <DecaySparkline
        salience={node.salience}
        ageDays={node.age_days}
        decayK={node.decay_k}
        floor={dropThreshold}
        pinned={node.pinned}
        width={100}
        height={26}
      />
    </div>
  );
}

/** The shaded overlap bar between an older node and the newer one that
 * superseded it — TimeArcs idiom: duration, not just order. */
function OverlapBadge({
  older,
  newer,
  nowUnix,
}: {
  older: MemoryLineageNode;
  newer: MemoryLineageNode;
  nowUnix: number;
}) {
  const overlapWindow = computeOverlapWindow(
    { createdUnix: older.created_unix ?? null, forgotten: older.forgotten },
    { createdUnix: newer.created_unix ?? null, forgotten: newer.forgotten },
  );
  if (!overlapWindow) return <span className="kb-lineage__arrow">↓ supersedes</span>;
  const days = ongoingOverlapDays(overlapWindow, nowUnix);
  return (
    <span className="kb-lineage__overlap" data-testid="lineage-overlap">
      {overlapWindow.ongoing ? (
        <span className="kb-lineage__overlap-bar kb-lineage__overlap-bar--ongoing" title={`still overlapping, ${Math.round(days ?? 0)}d and counting`}>
          ↓ overlapping {Math.round(days ?? 0)}d and counting
        </span>
      ) : (
        <span className="kb-lineage__overlap-bar kb-lineage__overlap-bar--resolved" title="the outdated fact was eventually forgotten">
          ↓ resolved (forgotten)
        </span>
      )}
    </span>
  );
}

export default function LineageViewer({
  kb,
  id,
  onClose,
}: {
  kb: string;
  id: string;
  onClose: () => void;
}) {
  const dlgRef = useRef<HTMLDialogElement | null>(null);
  useEffect(() => {
    const dlg = dlgRef.current;
    if (dlg && !dlg.open) dlg.showModal();
  }, []);

  const { dropThreshold } = useMemoryPolicy();
  const q = useQuery({
    queryKey: ["memory-lineage", kb, id],
    queryFn: ({ signal }) => fetchMemoryLineage(kb, id, signal),
  });

  const nowUnix = Math.floor(Date.now() / 1000);

  return (
    <dialog
      ref={dlgRef}
      className="kb-lineage"
      data-testid="lineage-viewer"
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
    >
      <header className="kb-lineage__head">
        <h2>Lineage</h2>
        <button type="button" className="kb-lineage__close" onClick={onClose} aria-label="close">
          <Icon.X />
        </button>
      </header>
      <div className="kb-lineage__body">
        {q.isPending && <p className="kb-lineage__loading">loading…</p>}
        {q.isError && (
          <p className="kb-lineage__error" role="alert">
            lineage failed: {String(q.error)}
          </p>
        )}
        {q.isSuccess && (
          <LineageChain kb={kb} data={q.data} dropThreshold={dropThreshold} nowUnix={nowUnix} />
        )}
      </div>
    </dialog>
  );
}

function LineageChain({
  kb,
  data,
  dropThreshold,
  nowUnix,
}: {
  kb: string;
  data: {
    start: MemoryLineageNode;
    supersedes_chain: MemoryLineageNode[];
    superseded_by_chain: MemoryLineageNode[];
  };
  dropThreshold: number | null;
  nowUnix: number;
}) {
  const plan = planLineageDisplay(data.supersedes_chain, data.superseded_by_chain);
  // Render newest → oldest top to bottom: the superseded_by chain reversed
  // (furthest-newest first), then start, then the supersedes chain as-is.
  const newerTopDown = [...plan.shownNewer].reverse();
  // CT-B3 — the WHOLE chain (not just the display-truncated slice above),
  // single kb by construction (a lineage chain never crosses corpora).
  const familyIds = [
    data.start.id,
    ...data.supersedes_chain.map((n) => n.id),
    ...data.superseded_by_chain.map((n) => n.id),
  ];

  return (
    <div className="kb-lineage__chain" data-testid="lineage-chain">
      <div className="kb-lineage__pivot">
        <IdsGalleryChip
          kb={kb}
          ids={familyIds}
          label={`view family (${familyIds.length}) in gallery`}
          testId="lineage-gallery-pivot"
        />
      </div>
      {plan.newerTruncated > 0 && (
        <p className="kb-lineage__truncated" data-testid="lineage-truncated-newer">
          +{plan.newerTruncated} more (newer)
        </p>
      )}
      {newerTopDown.map((n, i) => {
        // The node BELOW this one in render order (closer to start) is the
        // one it directly supersedes; index i corresponds to
        // newerTopDown[i+1] or, at the end, `start`.
        const below = newerTopDown[i + 1];
        return (
          <div key={n.id}>
            <Node node={n} dropThreshold={dropThreshold} />
            <OverlapBadge older={below ?? data.start} newer={n} nowUnix={nowUnix} />
          </div>
        );
      })}
      <div data-testid="lineage-start">
        <Node node={data.start} dropThreshold={dropThreshold} />
      </div>
      {plan.shownOlder.map((n, i) => {
        const parent = i === 0 ? data.start : plan.shownOlder[i - 1];
        return (
          <div key={n.id}>
            <OverlapBadge older={n} newer={parent} nowUnix={nowUnix} />
            <Node node={n} dropThreshold={dropThreshold} />
          </div>
        );
      })}
      {plan.olderTruncated > 0 && (
        <p className="kb-lineage__truncated" data-testid="lineage-truncated-older">
          +{plan.olderTruncated} more (older)
        </p>
      )}
      {data.supersedes_chain.length === 0 && (
        <p className="kb-lineage__none">(this memory supersedes nothing)</p>
      )}
      {data.superseded_by_chain.length === 0 && (
        <p className="kb-lineage__none">(nothing supersedes this memory)</p>
      )}
    </div>
  );
}
