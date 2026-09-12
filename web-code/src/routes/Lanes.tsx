import { useParams } from "react-router";
import EmptyState from "../components/EmptyState";
import { useLanes } from "../hooks/useLanes";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";
import { formatLastIngest, isFamilyTemplate } from "../lib/lanes";
import "../styles/lanes.css";

/// V76-R3a — `/r/:repo/~lanes`: the aug-lane/1 registry dock.
///
/// Mounted the way every other repo-scoped sentinel is (§D1: a lazy route
/// in `app.tsx` beside `~todos`/`~rails`, NOT a second app shell and not a
/// new Desk center mode). Numbers on this page are the daemon's: fact/run
/// counts and last ingest come off `GET /api/lanes`. A family template is
/// shown and labelled, never treated as addressable.
export default function Lanes() {
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const q = useLanes(repo);

  if (q.isLoading) {
    return (
      <div className="kbc-lanes" data-kbc-lanes data-kbc-lanes-state="loading">
        <h1 className="kbc-lanes__title">Lanes</h1>
        <p className="kbc-lanes__lede">Loading…</p>
      </div>
    );
  }
  if (q.error) {
    return (
      <div className="kbc-lanes" data-kbc-lanes data-kbc-lanes-state="error">
        <EmptyState
          title="Could not load lanes"
          hint={q.error instanceof Error ? q.error.message : "request failed"}
        />
      </div>
    );
  }
  const lanes = q.data?.lanes ?? [];
  const unknown = q.data?.unknown_enabled ?? [];
  if (lanes.length === 0) {
    return (
      <div className="kbc-lanes" data-kbc-lanes data-kbc-lanes-state="empty">
        <EmptyState title="No lanes registered" hint="This daemon shipped no aug-lane/1 registry rows." />
      </div>
    );
  }

  return (
    <div className="kbc-lanes" data-kbc-lanes data-kbc-lanes-state="ready">
      <h1 className="kbc-lanes__title">Lanes</h1>
      <p className="kbc-lanes__lede">
        aug-lane/1 registry for {repo}. A lane is enabled only by{" "}
        <code>[lanes] enabled</code> — never by this page.
      </p>
      {unknown.length > 0 && (
        <p className="kbc-lanes__unknown" data-kbc-lanes-unknown>
          unknown in [lanes] enabled: {unknown.join(", ")}
        </p>
      )}
      <table className="kbc-lanes__table">
        <thead>
          <tr>
            <th>id</th>
            <th>kind</th>
            <th>enabled</th>
            <th>facts</th>
            <th>runs</th>
            <th>last ingest</th>
            <th>retention</th>
          </tr>
        </thead>
        <tbody>
          {lanes.map((lane) => (
            <tr
              key={lane.id}
              data-kbc-lanes-row={lane.id}
              data-kbc-lanes-enabled={lane.enabled ? "1" : "0"}
              data-kbc-lanes-family={lane.family ? "1" : undefined}
            >
              <td className="kbc-lanes__id">{lane.id}</td>
              <td>{isFamilyTemplate(lane) ? "template" : lane.kind}</td>
              <td>{isFamilyTemplate(lane) ? "—" : lane.enabled ? "on" : "off"}</td>
              <td>{lane.facts ?? "—"}</td>
              <td>{lane.runs ?? "—"}</td>
              <td>{formatLastIngest(lane.last_ingest_at)}</td>
              <td>{lane.retention_days}d</td>
            </tr>
          ))}
        </tbody>
      </table>
      {lanes.map(
        (lane) =>
          lane.note && (
            <p key={`${lane.id}-note`} className="kbc-lanes__note" data-kbc-lanes-note={lane.id}>
              {lane.id}: {lane.note}
            </p>
          ),
      )}
    </div>
  );
}
