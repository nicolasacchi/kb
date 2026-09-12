// The TRAIL rail (V74-L3b, D12 + D17) — today's trail, on the Ladder.
//
// This is the operator's OWN read of their own movement record, and the
// daemon serves it over loopback only ("read-tracking never leaves the
// operator's box"). So the honest empty state here is not "no trail" but
// "not readable from this browser", and `useTrails` folds the route's absence
// into exactly that rather than an error toast.
//
// **Every state on a step is the daemon's, computed on that read.** A step
// says `pinned` / `carried` / `orphan` / `inert`; nothing here re-derives one,
// and a `carried` step is NOT re-anchored to a guessed line — a trail records
// where someone went, not a claim about bytes (server invariant 26(c)).
//
// **The FORK chip is the one mutation.** "I went down another path from here"
// is a real thing to want to record, and the daemon models it as a trail whose
// parent and branch point are stored. The chip is ABSENT for a non-loopback
// caller rather than disabled, and a forked trail renders a chip back to its
// parent step — which is what makes the pair legible in the list.
//
// **Purge is behind the ONE confirm host.** It is wholesale and audited on the
// daemon side; on this side it is never a bare button.

import { useCallback, useMemo, useState } from "react";
import { Link } from "react-router";
import type { TrailStepOut, TrailSummary } from "../../api/types";
import { Icon } from "../icons";
import { useConfirm } from "../ConfirmProvider";
import { useForkTrail, usePurgeTrails, useTrail, useTrails } from "../../hooks/useTrails";
import { appendTrail, codeUrl } from "../../lib/codeUrl";
import { toast } from "../../lib/toast";

/// How a `via` reads in a row. The wire vocabulary is snake_case (it has to
/// match `trails::VIA_KINDS`); this is the display twin, and it is the ONLY
/// place the two forms are related — `lib/trail.ts`'s `VIA_LABEL` does the
/// same job for the browser-local ring.
export const SERVER_VIA_LABEL: Readonly<Record<string, string>> = {
  search: "search",
  definition_of: "definition of",
  usage_of: "usage of",
  caller_of: "caller of",
  blame: "blame",
  why: "why",
  story: "story",
  review: "review",
  framework: "framework",
  manual: "manual",
  agent_suggested: "agent suggested",
};

/// A step's one-line label. Total: a step with no path is a hop this daemon
/// recorded without a file, and it says so rather than rendering an empty row.
export function trailStepLabel(step: TrailStepOut): string {
  const where = step.path
    ? step.line_start
      ? `${step.path}:${step.line_start}`
      : step.path
    : "(no file)";
  return step.symbol ? `${where} — ${step.symbol}` : where;
}

/// The dwell, as a human reads it. `0` is a real, honest value (a hop shorter
/// than the daemon's granularity floor records the visit with a zero dwell),
/// so it prints rather than being hidden.
export function dwellLabel(secs: number): string {
  if (secs <= 0) return "under the dwell floor";
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return s === 0 ? `${m}m` : `${m}m ${s}s`;
}

function TrailSteps({
  repo,
  trailId,
  parentOf,
  loopback,
  focusOrdinal,
}: {
  repo: string;
  trailId: string;
  parentOf: TrailSummary | null;
  loopback: boolean;
  focusOrdinal: number | null;
}) {
  const detail = useTrail(repo, trailId);
  const fork = useForkTrail(repo);
  const steps = detail.data?.steps ?? [];

  const forkHere = useCallback(
    (ordinal: number) => {
      fork.mutate(
        { id: trailId, fromOrdinal: ordinal },
        {
          onSuccess: (out) =>
            toast.ok(
              `forked at step ${ordinal} → ${out.id} (${out.steps} step${out.steps === 1 ? "" : "s"} carried)`,
            ),
          onError: (e) => toast.err(e instanceof Error ? e.message : String(e)),
        },
      );
    },
    [fork, trailId],
  );

  if (detail.isLoading) return <p className="kbc-trailrail__hint">Loading…</p>;
  if (detail.error) {
    return <p className="kbc-trailrail__hint">{(detail.error as Error).message}</p>;
  }
  if (steps.length === 0) {
    return <p className="kbc-trailrail__hint">no steps in this trail yet</p>;
  }

  return (
    <>
      {parentOf && (
        <p className="kbc-trailrail__parent" data-kbc-trail-parent={parentOf.id}>
          <Icon.Fork /> forked from <code>{parentOf.id}</code> at step{" "}
          {parentOf.parent_ordinal ?? 0}
        </p>
      )}
      <ol className="kbc-trailrail__steps" data-kbc-trail-steps>
        {steps.map((s) => (
          <li
            key={s.ordinal}
            data-kbc-trail-step={s.ordinal}
            data-kbc-trail-step-state={s.state}
            className={focusOrdinal === s.ordinal ? "is-focused" : undefined}
          >
            <span className="kbc-trailrail__via">{SERVER_VIA_LABEL[s.via] ?? s.via}</span>
            {s.path ? (
              <Link
                className="kbc-trailrail__where"
                to={appendTrail(
                  codeUrl({ repo, path: s.path, line: s.line_start ?? undefined }),
                  { id: trailId, step: s.ordinal, src: "trail" },
                )}
                data-kbc-trail-step-link={s.ordinal}
              >
                {trailStepLabel(s)}
              </Link>
            ) : (
              <span className="kbc-trailrail__where">{trailStepLabel(s)}</span>
            )}
            <span className={`kbc-trailrail__state kbc-trailrail__state--${s.state}`}>
              {s.state}
            </span>
            <span className="kbc-trailrail__dwell">{dwellLabel(s.dwell_secs)}</span>
            {loopback && (
              <button
                type="button"
                className="kbc-trailrail__fork"
                onClick={() => forkHere(s.ordinal)}
                disabled={fork.isPending}
                title="I went down another path from here — records a new trail branching at this step"
                data-kbc-trail-fork={s.ordinal}
              >
                <Icon.Fork /> fork here
              </button>
            )}
          </li>
        ))}
      </ol>
      {detail.data?.notes.map((n, i) => (
        <p className="kbc-trailrail__note" key={i}>
          {n}
        </p>
      ))}
    </>
  );
}

export interface TrailRailProps {
  repo: string;
  loopback: boolean;
  /// A step the reader asked to re-focus (the linked-tab chip's return).
  focus?: { id: string; ordinal: number } | null;
}

export default function TrailRail({ repo, loopback, focus }: TrailRailProps) {
  const trails = useTrails(repo);
  const purge = usePurgeTrails(repo);
  const confirm = useConfirm();
  const list = useMemo(() => trails.data?.trails ?? [], [trails.data]);
  // The chip's target wins over the default (today's trail) so following one
  // actually lands on the step it names.
  const [selected, setSelected] = useState<string | null>(null);
  const current = focus?.id ?? selected ?? list[0]?.id ?? null;
  const currentRow = list.find((t) => t.id === current) ?? null;
  const parentRow = currentRow?.parent_id
    ? (list.find((t) => t.id === currentRow.parent_id) ?? {
        ...currentRow,
        id: currentRow.parent_id,
      })
    : null;

  const doPurge = useCallback(async () => {
    const ok = await confirm({
      title: "Purge every trail in this repo?",
      body: "This deletes the movement record — trails and their steps — wholesale, and it is audited. Your own dissent notes on an authored trail SURVIVE: they are your words, not derived data.",
      confirmLabel: "Purge",
      danger: true,
    });
    if (!ok) return;
    purge.mutate(
      {},
      {
        onSuccess: (out) =>
          toast.ok(`purged ${out.trails} trail${out.trails === 1 ? "" : "s"} and ${out.steps} steps`),
        onError: (e) => toast.err(e instanceof Error ? e.message : String(e)),
      },
    );
  }, [confirm, purge]);

  return (
    <div className="kbc-trailrail" data-kbc-trail-rail>
      <div className="kbc-trailrail__head">
        <span className="kbc-trailrail__mode" data-kbc-trail-rail-mode={trails.data?.mode ?? "off"}>
          {trails.data?.mode ?? "off"}
        </span>
        {loopback && list.length > 0 && (
          <button
            type="button"
            onClick={() => void doPurge()}
            disabled={purge.isPending}
            data-kbc-trail-purge
            title="Delete every trail in this repo. Wholesale and audited."
          >
            Purge…
          </button>
        )}
      </div>

      {trails.data?.notes.map((n, i) => (
        <p className="kbc-trailrail__note" key={i} data-kbc-trail-rail-note>
          {n}
        </p>
      ))}

      {list.length === 0 ? (
        <p className="kbc-trailrail__hint" data-kbc-trail-rail-empty>
          nothing recorded here yet
        </p>
      ) : (
        <>
          {list.length > 1 && (
            <label className="kbc-trailrail__pick">
              <span>Trail</span>
              <select
                value={current ?? ""}
                onChange={(e) => setSelected(e.target.value)}
                data-kbc-trail-pick
              >
                {list.map((t) => (
                  <option key={t.id} value={t.id}>
                    {t.title ?? t.day ?? t.id} — {t.step_count} step
                    {t.step_count === 1 ? "" : "s"}
                    {t.origin === "authored" ? " (authored)" : ""}
                    {t.parent_id ? " (fork)" : ""}
                  </option>
                ))}
              </select>
            </label>
          )}
          {current && (
            <TrailSteps
              repo={repo}
              trailId={current}
              parentOf={parentRow}
              loopback={loopback}
              focusOrdinal={focus?.id === current ? focus.ordinal : null}
            />
          )}
        </>
      )}
    </div>
  );
}
