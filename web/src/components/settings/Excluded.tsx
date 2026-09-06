import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  fetchExclusions,
  includeArtifact,
  type ExclusionEntry,
} from "../../api/client";
import { useKbs } from "../../hooks/useKbs";
import { toast } from "../../lib/toast";
import { Icon } from "../icons";

// Excluded tab (v0.24 X4). Per-kb list of files the operator excluded
// from the index (Card action, reader About row, or `kb exclude`).
// Excluded files stay on disk with their `.review` sidecar + reading
// history intact (KeepUserData cascade, D3); the row-level Include
// button re-indexes them. Mirrors the Quarantine pane's layout, but the
// row set is TanStack-cached under ["exclusions", kb] — the SSE bridge
// (queryClient.ts) invalidates it on artifact.excluded/included, so the
// pane stays live with no local subscribe plumbing (invariant #23).
export default function Excluded() {
  const { data: kbs = [], error } = useKbs();

  return (
    <div className="dash">
      {error != null && (
        <div className="settings__error">
          kbs unreachable: {error instanceof Error ? error.message : String(error)}
        </div>
      )}
      {kbs.map((k) => (
        <KbExcludedCard key={k.name} kb={k.name} />
      ))}
      {kbs.length === 0 && error == null && (
        <div className="settings__hint">no kbs configured</div>
      )}
    </div>
  );
}

function KbExcludedCard({ kb }: { kb: string }) {
  const { data: entries = [] } = useQuery({
    queryKey: ["exclusions", kb] as const,
    queryFn: ({ signal }) => fetchExclusions(kb, signal),
  });
  const [pending, setPending] = useState<Set<string>>(new Set());

  async function includeOne(e: ExclusionEntry) {
    setPending((prev) => new Set(prev).add(e.path));
    try {
      await includeArtifact(kb, e.path);
      // The daemon confirms with artifact.included → the SSE bridge
      // invalidates ["exclusions", kb] and the row leaves; nothing to
      // reconcile locally.
      toast.ok(
        e.present_on_disk
          ? "included — reindexing"
          : "exclusion cleared (file no longer on disk)",
      );
    } catch (err) {
      toast.err(
        `include failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    } finally {
      setPending((prev) => {
        const next = new Set(prev);
        next.delete(e.path);
        return next;
      });
    }
  }

  return (
    <section className="dash__kbcard" aria-label={`exclusions for ${kb}`}>
      <header className="dash__kbcard-head">
        <h3 className="settings__h3">
          {kb}
          <span className="dash__count">{entries.length}</span>
        </h3>
      </header>
      {entries.length === 0 ? (
        <div className="settings__hint">
          no excluded files — exclude from a gallery card or the reader's
          About tab
        </div>
      ) : (
        <table className="dash__table">
          <thead>
            <tr>
              <th>path</th>
              <th>excluded</th>
              <th>note</th>
              <th>file</th>
              <th>actions</th>
            </tr>
          </thead>
          <tbody>
            {entries.map((e) => {
              const isPending = pending.has(e.path);
              return (
                <tr key={e.path} className="errors__row">
                  <td>
                    <span className="dash__path" title={e.artifact_id}>
                      {e.path}
                    </span>
                  </td>
                  <td>{relAge(e.excluded_at)}</td>
                  <td>
                    {e.note ? (
                      <span title={e.note}>{e.note}</span>
                    ) : (
                      <span className="settings__hint">—</span>
                    )}
                  </td>
                  <td>
                    {e.present_on_disk ? (
                      <span title="still on disk — include re-indexes it">
                        <Icon.Doc />
                      </span>
                    ) : (
                      <span
                        className="settings__hint"
                        title="deleted while excluded; include only clears the row"
                      >
                        —
                      </span>
                    )}
                  </td>
                  <td>
                    <button
                      type="button"
                      className="settings__btn settings__btn--sm"
                      disabled={isPending}
                      onClick={() => void includeOne(e)}
                      title="DELETE /api/kb/{kb}/exclusions/{path} — re-include + reindex"
                    >
                      {isPending ? "…" : <><Icon.Refresh aria-hidden="true" /> include</>}
                    </button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </section>
  );
}

function relAge(unix: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unix));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}
