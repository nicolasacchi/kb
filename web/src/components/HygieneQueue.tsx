// MI-W4.4 — the memory hygiene queue: an Anki-style BOUNDED triage list of
// "the memories most worth 90 seconds right now", each with a one-line
// JUSTIFICATION (`kb resurface --explain`'s house style). Purely a VIEW
// over `GET /api/memory/triage` — never derives or stores its own ranking
// (that lives entirely in `kb_core::triage::build_queue`); every action
// below routes through the SAME mutations the main /memory table already
// uses (pin/salience/forget), so acting on an item here is indistinguishable
// from acting on it there.

import { useState } from "react";
import { Link } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  forgetMemory,
  patchMemorySalience,
  pinMemory,
  type MemoryTriageItem,
} from "../api/client";
import { MEMORY_TRIAGE_KEY, useMemoryTriage } from "../hooks/useMemoryTriage";
import { artifactHref } from "../lib/artifactHref";

export default function HygieneQueue() {
  const queryClient = useQueryClient();
  // FIX2 — shared hook (not a component-local `useQuery`) so the /memory
  // route's quadrant scatter can read this SAME response's
  // `high_salience_threshold`/`dormant_days` without a second fetch.
  const q = useMemoryTriage();
  const refresh = () => void queryClient.invalidateQueries({ queryKey: MEMORY_TRIAGE_KEY });

  return (
    <section className="kb-mem__hygiene" data-testid="hygiene-queue">
      <h4>Hygiene queue</h4>
      {q.isPending && <p className="kb-mem__hygiene-loading">loading…</p>}
      {q.isError && (
        <p className="kb-mem__hygiene-err" role="alert">
          triage failed: {String(q.error)}
        </p>
      )}
      {q.isSuccess && q.data.items.length === 0 && (
        <p className="kb-mem__hygiene-empty" data-testid="hygiene-queue-empty">
          queue is empty — nothing needs attention right now.
        </p>
      )}
      {q.isSuccess && q.data.items.length > 0 && (
        <>
          <p className="kb-mem__hygiene-scanned">
            {q.data.scanned} memories scanned, {q.data.items.length} in the queue
          </p>
          <ul className="kb-mem__hygiene-list" data-testid="hygiene-queue-list">
            {q.data.items.map((it) => (
              <HygieneRow key={`${it.kb}:${it.id}`} item={it} onChanged={refresh} />
            ))}
          </ul>
        </>
      )}
    </section>
  );
}

function HygieneRow({
  item,
  onChanged,
}: {
  item: MemoryTriageItem;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = async (label: string, action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      onChanged();
    } catch (e) {
      setError(`${label} failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <li className="kb-mem__hygiene-item" data-testid="hygiene-queue-item" data-id={item.id}>
      <div className="kb-mem__hygiene-item-head">
        <span className="kb-mem__hygiene-urgency" title="urgency">
          {item.urgency.toFixed(2)}
        </span>
        {item.source_relative ? (
          <Link
            className="kb-mem__hygiene-title"
            to={artifactHref(item.kb, item.source_relative)}
          >
            {item.title}
          </Link>
        ) : (
          <span className="kb-mem__hygiene-title">{item.title}</span>
        )}
        <span className="kb-mem__hygiene-kb">{item.kb}</span>
      </div>
      <p className="kb-mem__hygiene-reason" data-testid="hygiene-queue-reason">
        {item.reason}
      </p>
      {error && (
        <p className="kb-mem__hygiene-item-err" role="alert">
          {error}
        </p>
      )}
      <div className="kb-mem__hygiene-actions">
        <button
          type="button"
          disabled={busy}
          onClick={() => run("pin", () => pinMemory(item.kb, item.id))}
        >
          pin
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() =>
            run("salience bump", () =>
              patchMemorySalience(item.kb, item.id, Math.min(1, (item.salience ?? 0.5) + 0.2)),
            )
          }
          title="raise salience by 0.2 — a quick 'this actually matters' nudge"
        >
          +salience
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => run("forget", () => forgetMemory(item.kb, item.id))}
        >
          forget
        </button>
      </div>
    </li>
  );
}
