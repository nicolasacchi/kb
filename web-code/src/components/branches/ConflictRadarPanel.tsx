import { ApiError } from "../../api/client";
import { useBranchConflicts } from "../../hooks/useBranchFacts";

// V75-M3 (D15) — the conflict radar.
//
// Three honesty rules, all of them about NOT computing:
//
//  1. **The caption is the server's.** `budget.computed of candidates` is
//     pre-rendered server-side precisely so the CLI and this panel print
//     one sentence; deriving "N of M" here would be a second arithmetic to
//     get wrong.
//  2. **An absent hunk count renders as absent.** Past the probe budget the
//     field is `undefined`, and this panel says "not measured" rather than
//     showing `0` — a zero would read as "a clean file", which is the
//     opposite of what it means.
//  3. **A refusal is RENDERED, not swallowed.** The two the server can
//     return here are the typed `503`
//     (`urn:kb:errors:scratch-unwritable` — the daemon's own state dir, not
//     the repo) and the `400` for `touches:`, which the radar cannot answer
//     without a per-branch diff. Both surface with the daemon's own message.

export interface ConflictRadarPanelProps {
  repo: string;
  against: string;
  /// The kbcq/1 query the page is filtered by, forwarded so "conflicts
  /// among the agent branches" is one address. `touches:` is refused by the
  /// server, which is why this panel renders the refusal instead of
  /// stripping the atom and pretending it applied.
  q?: string;
  onClose: () => void;
}

export default function ConflictRadarPanel({ repo, against, q, onClose }: ConflictRadarPanelProps) {
  const radar = useBranchConflicts(repo, against, { q });

  return (
    <section className="kbc-radar" data-kbc-radar={against} aria-label={`conflict radar against ${against}`}>
      <header className="kbc-radar__head">
        <h2 className="kbc-radar__title">
          Conflict radar <span className="kbc-radar__against">against {against}</span>
        </h2>
        <button type="button" className="kbc-radar__close" onClick={onClose} data-kbc-radar-close>
          Close
        </button>
      </header>

      {radar.isLoading && <p className="kbc-radar__hint">Merging each candidate against {against}…</p>}

      {radar.error && (
        <p className="kbc-radar__refusal" data-kbc-radar-refusal role="status">
          {(radar.error as Error).message}
          {radar.error instanceof ApiError && radar.error.status === 503 && (
            <>
              {" "}
              <span className="kbc-radar__refusal-why">
                The radar writes its would-be merge tree into the daemon&rsquo;s OWN scratch
                directory, never into the repo — so this is about the daemon&rsquo;s state dir,
                not about {repo}.
              </span>
            </>
          )}
        </p>
      )}

      {radar.data && (
        <>
          <p className="kbc-radar__caption" data-kbc-radar-caption>
            {radar.data.caption}
          </p>
          {radar.data.rows.length === 0 && (
            <p className="kbc-radar__hint">
              No candidate branches — every other ref is already contained in {against}.
            </p>
          )}
          <ul className="kbc-radar__rows">
            {radar.data.rows.map((r) => (
              <li
                key={r.full_ref}
                className={`kbc-radar__row ${r.clean ? "kbc-radar__row--clean" : "kbc-radar__row--conflict"}`}
                data-kbc-radar-row={r.branch}
                data-kbc-radar-clean={r.clean ? "1" : "0"}
              >
                <span className="kbc-radar__branch">{r.branch}</span>
                {r.error ? (
                  <span className="kbc-radar__error">{r.error}</span>
                ) : r.clean ? (
                  <span className="kbc-radar__verdict">clean</span>
                ) : (
                  <>
                    <span className="kbc-radar__verdict">
                      {r.conflicts.length} conflicting path(s)
                    </span>
                    <ul className="kbc-radar__paths">
                      {r.conflicts.map((c) => (
                        <li key={c.path} className="kbc-radar__path" data-kbc-radar-path={c.path}>
                          <code>{c.path}</code>{" "}
                          <span className="kbc-radar__kind" data-kbc-radar-kind={c.kind}>
                            {c.kind}
                          </span>{" "}
                          <span className="kbc-radar__hunks">
                            {c.hunks === undefined
                              ? "hunks not measured"
                              : `${c.hunks} hunk(s)`}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
    </section>
  );
}
