import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  fetchSlo,
  snapshotSlo,
  type SloIndicator,
  type SloReport,
} from "../../api/client";
import { useKbs } from "../../hooks/useKbs";
import { toast } from "../../lib/toast";
import { Icon } from "../icons";

// CT-F5 — corpus-health SLOs, one card per kb.
//
// SURFACED, NEVER ENFORCED. This panel is the whole consumer surface: no
// other view badges on a warn, nothing retries, nothing is invalidated by a
// missed target. An SLO here is a number you read, and the daemon's
// behaviour is identical whether every indicator is green or every one is a
// warn.
//
// Data pattern: invariant #23's documented no-SSE-tie exception (the daycard
// / live-tail / presence set) — there is no `slo.*` event to invalidate on
// (nothing writes an SLO except a read), so these queries carry a finite
// staleTime plus an explicit refresh button rather than joining the SSE
// invalidation bridge. Emitting an event per computed report would be an
// event storm describing nothing that changed.
const SLO_STALE_MS = 60_000;

export default function Slo() {
  const { data: kbs = [], error } = useKbs();

  return (
    <div className="dash">
      <p className="settings__hint">
        Per-corpus health indicators computed from tables kb already keeps —
        code-ref path shape, orphan <code>kb_session</code> docs, recall-ledger
        parse failures, and capture freshness. Targets come from{" "}
        <code>[kb.&lt;name&gt;.slo]</code> in kb.toml; every key is optional.
      </p>
      <p className="settings__hint">
        Surfaced, never enforced: nothing changes behaviour on a missed target.
        An indicator with no target is still measured — its status just reads{" "}
        <em>unknown</em>, because there is nothing to judge it against. A value
        of <em>—</em> means the inputs genuinely aren't there; it never means
        zero.
      </p>
      {error != null && (
        <div className="settings__error">
          kbs unreachable:{" "}
          {error instanceof Error ? error.message : String(error)}
        </div>
      )}
      {kbs.map((k) => (
        <KbSloCard key={k.name} kb={k.name} />
      ))}
      {kbs.length === 0 && error == null && (
        <div className="settings__hint">no kbs configured</div>
      )}
    </div>
  );
}

function KbSloCard({ kb }: { kb: string }) {
  const qc = useQueryClient();
  const [saving, setSaving] = useState(false);
  const { data, error, isFetching } = useQuery<SloReport>({
    queryKey: ["slo", kb] as const,
    queryFn: ({ signal }) => fetchSlo(kb, signal),
    staleTime: SLO_STALE_MS,
  });

  async function takeSnapshot() {
    setSaving(true);
    try {
      const res = await snapshotSlo(kb);
      // The route echoes the report it stored, so seeding the cache with it
      // shows exactly what landed rather than a second, slightly-later read.
      qc.setQueryData(["slo", kb], res.report);
      toast.ok(`snapshot appended — ${res.appended} indicator rows`);
    } catch (e) {
      toast.err(
        `snapshot failed: ${e instanceof Error ? e.message : String(e)}`,
      );
    } finally {
      setSaving(false);
    }
  }

  const warn = data?.warn_count ?? 0;

  return (
    <section className="dash__kbcard" aria-label={`corpus-health SLOs for ${kb}`}>
      <header className="dash__kbcard-head">
        <h3 className="settings__h3">
          {kb}
          {warn > 0 && (
            <span className="dash__count" title="indicators missing their target">
              {warn} warn
            </span>
          )}
        </h3>
        <div className="dash__actions">
          <button
            type="button"
            className="settings__btn settings__btn--sm"
            disabled={isFetching}
            onClick={() => void qc.invalidateQueries({ queryKey: ["slo", kb] })}
            title="GET /api/kb/{kb}/slo — recompute now"
          >
            <Icon.Refresh aria-hidden="true" /> refresh
          </button>
          <button
            type="button"
            className="settings__btn settings__btn--sm"
            disabled={saving || !data}
            onClick={() => void takeSnapshot()}
            title="POST /api/kb/{kb}/slo/snapshot — append this reading to the append-only log"
          >
            {saving ? "…" : "snapshot"}
          </button>
        </div>
      </header>
      {error != null ? (
        <div className="settings__error">
          slo unreachable:{" "}
          {error instanceof Error ? error.message : String(error)}
        </div>
      ) : !data ? (
        <div className="settings__hint">loading…</div>
      ) : (
        <table className="dash__table">
          <thead>
            <tr>
              <th>indicator</th>
              <th>value</th>
              <th>target</th>
              <th>status</th>
            </tr>
          </thead>
          <tbody>
            {data.indicators.map((i) => (
              <SloRow key={i.key} i={i} />
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

function SloRow({ i }: { i: SloIndicator }) {
  return (
    <tr className="errors__row">
      <td>
        <span title={i.key}>{i.label}</span>
        <div className="settings__hint">{i.detail}</div>
      </td>
      <td>{fmtValue(i.value, i.unit)}</td>
      <td>
        {i.target == null ? (
          <span className="settings__hint" title="no target in [kb.*.slo]">
            —
          </span>
        ) : (
          <span
            title={
              i.direction === "higher_is_better"
                ? "target is a minimum"
                : "target is a maximum"
            }
          >
            {i.direction === "higher_is_better" ? "min " : "max "}
            {fmtValue(i.target, i.unit)}
          </span>
        )}
      </td>
      <td>
        <span
          className={`kb-slo__status kb-slo__status--${i.status}`}
          title={
            i.status === "unknown"
              ? i.value == null
                ? "not measurable — the inputs aren't there"
                : "measured, but no target is configured to judge it against"
              : undefined
          }
        >
          {i.status}
        </span>
      </td>
    </tr>
  );
}

/// `null` renders as `—`, never `0`: the two are different facts, and the
/// whole `unknown` status exists to keep them apart.
function fmtValue(v: number | null, unit: string): string {
  if (v == null) return "—";
  if (unit === "percent") return `${v}%`;
  if (unit === "hours") return `${v}h`;
  return `${v}`;
}
