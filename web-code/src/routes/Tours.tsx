// `/r/{repo}/~tours` — the kbc-tour/1 tour list (V74-L3b, D12 + D10).
//
// **How this coexists with `~sets/{id}/~tour`.** Phase E4's "tour mode" walks
// ONE reading set's ordered spans; it stays exactly where it is. A kbc-tour/1
// tour is a different object: a document on the Ladder, authored by an agent
// or recorded from navigation, whose every step is RE-RESOLVED on every read.
// The two share the word and nothing else, and neither page reads the other's
// data — the same coexistence `~boards` has with `~canvas`, for the same
// reason (`routes/Boards.tsx`'s own header).
//
// Every count in a row is the daemon's (`TourSummary`). Note there is ONE
// count, not two: a tour's node count and its step count are the same number
// by construction (its nodes ARE its steps), so the daemon reports one and
// this page renders one — two fields that must agree is the drift this
// codebase keeps designing out.

import { Link, useParams, useSearchParams } from "react-router";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useTours } from "../hooks/useTours";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";
import { boardsPageUrl } from "../lib/codeUrl";
import { parseToursStatus, tourHref } from "../lib/toursUrl";
import { relativeTime } from "../lib/format";
import "../styles/tours.css";

export default function Tours() {
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const [params, setParams] = useSearchParams();
  const all = useTours(repo);
  const statuses = all.data?.statuses_available ?? [];
  const status = parseToursStatus(params.get("status"), statuses);
  const filtered = useTours(repo, status);

  const query = status ? filtered : all;
  const list = query.data?.tours ?? [];

  return (
    <div className="kbc-tours" data-kbc-tours>
      <header className="kbc-tours__head">
        <h1 className="kbc-tours__title">Tours — {repo}</h1>
        <p className="kbc-tours__hint">
          A tour is a walk through references into this repo — prose, a blob and a camera per
          step. Every step is re-resolved on every read, and a step whose code moved says{" "}
          <em>carried</em> rather than pretending its line number still means what it meant.
        </p>
        <div className="kbc-tours__controls">
          <label>
            <span>Status</span>
            <select
              value={status ?? ""}
              onChange={(e) => {
                const next = new URLSearchParams(params);
                if (e.target.value) next.set("status", e.target.value);
                else next.delete("status");
                setParams(next, { replace: true });
              }}
              aria-label="filter by status"
              data-kbc-tours-status
            >
              <option value="">every status</option>
              {statuses.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          </label>
        </div>
      </header>

      {query.isLoading ? (
        <div className="kbc-reader__hint">Loading tours…</div>
      ) : query.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error" data-kbc-tours-error>
          {(query.error as Error).message}
        </div>
      ) : list.length === 0 ? (
        <EmptyState
          icon={<Icon.Spark />}
          title="No tours yet"
          hint="Record one from where you have been — walk a few files, then Space k r to start and Space k s to stop — or apply a document with kb-code tour apply -f tour.json."
        />
      ) : (
        <ul className="kbc-tours__list" data-kbc-tours-list>
          {list.map((t) => (
            <li className="kbc-tours__row" key={t.slug} data-kbc-tours-row={t.slug}>
              <Link
                className="kbc-tours__row-name"
                to={tourHref(repo, t.slug)}
                data-kbc-tours-row-link={t.slug}
              >
                {t.title}
              </Link>
              <span
                className={`kbc-tours__status kbc-tours__status--${t.status}`}
                data-kbc-tours-row-status={t.status}
              >
                {t.status}
              </span>
              <span className="kbc-tours__row-meta">
                {t.steps} step{t.steps === 1 ? "" : "s"} · rev {t.revision} · updated{" "}
                {relativeTime(t.updated_unix)}
              </span>
            </li>
          ))}
        </ul>
      )}

      <footer className="kbc-tours__sibling" data-kbc-tours-sibling>
        <h2>Nearby</h2>
        <p>
          <Link to={boardsPageUrl(repo)} data-kbc-tours-boards-link>
            Boards
          </Link>{" "}
          — the same reference model, laid out rather than walked. A tour <em>is</em> a board
          whose nodes are its steps (the daemon stores them in one table), which is why a card
          resolves identically on both surfaces.
        </p>
      </footer>
    </div>
  );
}
