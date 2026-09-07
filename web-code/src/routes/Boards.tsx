// `/r/{repo}/~boards` — the kbc-canvas/1 board list (V74-L2, D10).
//
// **How this coexists with `~canvas`.** V3.4-C2's working-set canvas is a
// DIFFERENT thing that happens to share a word: a `canvas_sets` row whose
// payload is opaque by contract, holding free-form symbol fragments a human
// dragged into place. `boards/mod.rs` froze it beside kbc-canvas/1 rather than
// migrating it ("the `/api/usages` → `/api/usages/2` treatment"), because a
// board must be re-resolvable and an opaque payload cannot be. So the two stay
// two: this page owns `~boards`, `routes/Canvas.tsx` still owns `~canvas`
// unchanged, and the ONE place they meet is the "Legacy" section at the bottom
// of this list — one link, captioned with what the difference is. Neither page
// links into the other's data, and nothing was migrated.
//
// Every count in a row is the daemon's (`BoardSummary`), and the sweep summary
// is fetched only when asked (`useBoardSweep`'s `enabled`), because a sweep
// executes every query card on every board.

import { useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useBoardSweep, useBoards } from "../hooks/useBoards";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";
import { boardHref, parseBoardsStatus } from "../lib/boardsUrl";
import { canvasPageUrl } from "../lib/codeUrl";
import { relativeTime } from "../lib/format";
import "../styles/boards.css";

export default function Boards() {
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const [params, setParams] = useSearchParams();
  const boards = useBoards(repo);
  const statuses = boards.data?.statuses_available ?? [];
  const status = parseBoardsStatus(params.get("status"), statuses);
  const filtered = useBoards(repo, status);
  const [sweeping, setSweeping] = useState(false);
  const sweep = useBoardSweep(repo, undefined, sweeping);

  const list = (status ? filtered.data : boards.data)?.boards ?? [];
  const query = status ? filtered : boards;

  return (
    <div className="kbc-boards" data-kbc-boards>
      <header className="kbc-boards__head">
        <h1 className="kbc-boards__title">Boards — {repo}</h1>
        <p className="kbc-boards__hint">
          A board is an ordered set of REFERENCES into this repo — never a copy of it. Every card
          is re-resolved on every read, and an orphan is shown with its last-known text rather than
          dropped.
        </p>
        <div className="kbc-boards__controls">
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
              data-kbc-boards-status
            >
              <option value="">every status</option>
              {statuses.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            onClick={() => setSweeping((v) => !v)}
            data-kbc-boards-sweep
            title="Re-resolve every node of every board and report drift. A read: it never repairs what it finds."
          >
            <Icon.Refresh /> {sweeping ? "Hide drift" : "Check drift"}
          </button>
        </div>
      </header>

      {sweeping && (
        <section className="kbc-boards__sweep" data-kbc-boards-sweep-panel>
          {sweep.isLoading ? (
            <p>Sweeping — every query card is executed, so this is slower than a page load…</p>
          ) : sweep.error ? (
            <p className="kbc-boards__error">{(sweep.error as Error).message}</p>
          ) : sweep.data ? (
            <>
              <p data-kbc-boards-sweep-summary>
                {sweep.data.checked} board{sweep.data.checked === 1 ? "" : "s"} checked —{" "}
                {sweep.data.drifted ? "drift found" : "no drift"}
              </p>
              <ul>
                {sweep.data.boards
                  .filter((b) => b.drifted)
                  .map((b) => (
                    <li key={b.slug} data-kbc-boards-sweep-row={b.slug}>
                      <Link to={boardHref(repo, b.slug)}>{b.title}</Link> — {b.orphans} orphan
                      {b.orphans === 1 ? "" : "s"} · {b.carried} carried · {b.query_deltas} query
                      delta{b.query_deltas === 1 ? "" : "s"} · {b.stale_pins} stale pin
                      {b.stale_pins === 1 ? "" : "s"}
                    </li>
                  ))}
              </ul>
            </>
          ) : null}
        </section>
      )}

      {query.isLoading ? (
        <div className="kbc-reader__hint">Loading boards…</div>
      ) : query.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error">
          {(query.error as Error).message}
        </div>
      ) : list.length === 0 ? (
        <EmptyState
          icon={<Icon.Grid />}
          title="No boards yet"
          hint="A board never starts empty — add a card from the reader's action panel (.) and pick “New board…”, or run kb-code canvas apply -f board.json."
        />
      ) : (
        <ul className="kbc-boards__list" data-kbc-boards-list>
          {list.map((b) => (
            <li className="kbc-boards__row" key={b.slug} data-kbc-boards-row={b.slug}>
              <Link className="kbc-boards__row-name" to={boardHref(repo, b.slug)} data-kbc-boards-row-link={b.slug}>
                {b.title}
              </Link>
              <span
                className={`kbc-boards__status kbc-boards__status--${b.status}`}
                data-kbc-boards-row-status={b.status}
              >
                {b.status}
              </span>
              <span className="kbc-boards__row-meta">
                {b.nodes} node{b.nodes === 1 ? "" : "s"} · {b.edges} edge{b.edges === 1 ? "" : "s"} ·{" "}
                {b.steps} step{b.steps === 1 ? "" : "s"} · rev {b.revision} · updated{" "}
                {relativeTime(b.updated_unix)}
              </span>
            </li>
          ))}
        </ul>
      )}

      <footer className="kbc-boards__legacy" data-kbc-boards-legacy>
        <h2>Legacy</h2>
        <p>
          <Link to={canvasPageUrl(repo)} data-kbc-boards-legacy-link>
            Working-set canvas
          </Link>{" "}
          — the older free-form surface (a <code>canvas_sets</code> row with an opaque payload).
          It is frozen, not migrated: its payload cannot be re-resolved, which is the whole point
          of a board. Nothing here reads it and nothing there reads this.
        </p>
      </footer>
    </div>
  );
}
