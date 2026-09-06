import { useMemo, useState } from "react";
import { Link, useParams } from "react-router-dom";
import type { TodoItem } from "../api/types";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useScopes } from "../hooks/useScopes";
import { useTodos } from "../hooks/useTodos";
import { readerUrl } from "../lib/breadcrumbs";
import { speedFilterItems } from "../lib/speedSearch";
import "../styles/todos.css";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

/// Group items by path, preserving first-seen path order.
function groupByPath(items: TodoItem[]): { path: string; items: TodoItem[] }[] {
  const map = new Map<string, TodoItem[]>();
  const order: string[] = [];
  for (const it of items) {
    if (!map.has(it.path)) {
      map.set(it.path, []);
      order.push(it.path);
    }
    map.get(it.path)!.push(it);
  }
  return order.map((path) => ({ path, items: map.get(path)! }));
}

/// Phase N — `/r/:repo/~todos`: TODO/FIXME/… index with marker chips,
/// scope filter, speed-search, and collapsible per-file groups.
export default function Todos() {
  // V70-A6 — root CLAUDE.md #31, ported: this list scrolls the WINDOW, so
  // one offset keyed on the full URL is the whole story. Back onto it lands
  // where the reader left, not where the browser guessed.
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const [marker, setMarker] = useState<string | null>(null);
  const [scope, setScope] = useState<string>("");
  const [filter, setFilter] = useState("");
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});

  const scopesQ = useScopes();
  const scopeNames = useMemo(
    () => Object.keys(scopesQ.data?.scopes ?? {}).sort(),
    [scopesQ.data],
  );

  const todos = useTodos({
    repo,
    marker: marker ?? undefined,
    scope: scope || undefined,
  });

  // Unfiltered fetch for deriving chip set from data (not hardcoded counts).
  const allTodos = useTodos({ repo });

  const markerChips = useMemo(() => {
    const counts = new Map<string, number>();
    for (const it of allTodos.data?.items ?? []) {
      counts.set(it.marker, (counts.get(it.marker) ?? 0) + 1);
    }
    // Stable display order for known markers, then any extras alpha.
    const preferred = ["TODO", "FIXME", "HACK", "XXX", "BUG"];
    const keys = [...counts.keys()].sort((a, b) => {
      const ia = preferred.indexOf(a);
      const ib = preferred.indexOf(b);
      if (ia >= 0 && ib >= 0) return ia - ib;
      if (ia >= 0) return -1;
      if (ib >= 0) return 1;
      return a.localeCompare(b);
    });
    return keys.map((m) => ({ marker: m, count: counts.get(m)! }));
  }, [allTodos.data]);

  const filtered = useMemo(() => {
    const items = todos.data?.items ?? [];
    if (filter.trim() === "") return items;
    return speedFilterItems(items, filter, (it) => `${it.path} ${it.text} ${it.marker}`).map(
      (h) => h.item,
    );
  }, [todos.data, filter]);

  const groups = useMemo(() => groupByPath(filtered), [filtered]);

  function toggleGroup(path: string) {
    setCollapsed((c) => ({ ...c, [path]: !c[path] }));
  }

  return (
    <div className="kbc-todos" id="main" data-kbc-todos>
      <header className="kbc-todos__head">
        <h1 className="kbc-todos__title">TODOs — {repo}</h1>
        <p className="kbc-todos__hint">Comment markers from the live index (TODO / FIXME / …).</p>
      </header>

      {todos.data?.truncated && (
        <div className="kbc-todos__trunc" data-kbc-todos-truncated role="status">
          Showing {todos.data.items.length} of {todos.data.total} — results truncated.
        </div>
      )}

      <div className="kbc-todos__filters">
        <div className="kbc-todos__chips" role="group" aria-label="marker filter">
          <button
            type="button"
            className={"kbc-todos__chip" + (marker === null ? " is-on" : "")}
            onClick={() => setMarker(null)}
            data-kbc-todos-chip="all"
          >
            All
            {allTodos.data && (
              <span className="kbc-todos__chip-n">{allTodos.data.total}</span>
            )}
          </button>
          {markerChips.map(({ marker: m, count }) => (
            <button
              key={m}
              type="button"
              className={"kbc-todos__chip" + (marker === m ? " is-on" : "")}
              onClick={() => setMarker((cur) => (cur === m ? null : m))}
              data-kbc-todos-chip={m}
            >
              {m}
              <span className="kbc-todos__chip-n">{count}</span>
            </button>
          ))}
        </div>
        <div className="kbc-todos__row2">
          <input
            className="kbc-todos__search"
            type="search"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter path + text…"
            aria-label="filter todos"
            data-kbc-todos-filter
          />
          {scopeNames.length > 0 && (
            <select
              className="kbc-todos__scope"
              value={scope}
              onChange={(e) => setScope(e.target.value)}
              aria-label="scope filter"
              data-kbc-todos-scope
            >
              <option value="">All scopes</option>
              {scopeNames.map((name) => (
                <option key={`inc-${name}`} value={name}>
                  include: {name}
                </option>
              ))}
              {scopeNames.map((name) => (
                <option key={`exc-${name}`} value={`!${name}`}>
                  exclude: {name}
                </option>
              ))}
            </select>
          )}
        </div>
      </div>

      {todos.isLoading && <div className="kbc-todos__muted">Loading…</div>}
      {todos.error && (
        <div className="kbc-todos__error">{(todos.error as Error).message}</div>
      )}
      {!todos.isLoading && !todos.error && groups.length === 0 && (
        <EmptyState
          icon={<Icon.List />}
          title="No TODOs"
          hint={
            filter || marker || scope
              ? "Nothing matches the current filters."
              : "No TODO/FIXME/… markers indexed in this repo."
          }
        />
      )}

      <div className="kbc-todos__groups">
        {groups.map((g) => {
          const isCollapsed = !!collapsed[g.path];
          return (
            <section key={g.path} className="kbc-todos__group" data-kbc-todos-group={g.path}>
              <button
                type="button"
                className="kbc-todos__group-head"
                onClick={() => toggleGroup(g.path)}
                aria-expanded={!isCollapsed}
                data-kbc-todos-group-toggle
              >
                <span className="kbc-todos__chev" aria-hidden>
                  <Icon.Chevron className={isCollapsed ? "kbc-twisty" : "kbc-twisty is-open"} />
                </span>
                <span className="kbc-todos__group-path">{g.path}</span>
                <span className="kbc-todos__group-n">{g.items.length}</span>
              </button>
              {!isCollapsed && (
                <ul className="kbc-todos__items">
                  {g.items.map((it) => (
                    <li key={`${it.path}:${it.line}:${it.marker}:${it.text.slice(0, 24)}`}>
                      <Link
                        to={readerUrl(repo, it.path, undefined, it.line)}
                        className="kbc-todos__item"
                        data-kbc-todos-row
                        data-kbc-todos-line={it.line}
                      >
                        <span className="kbc-todos__marker" data-kbc-todos-marker={it.marker}>
                          {it.marker}
                        </span>
                        <span className="kbc-todos__line">:{it.line}</span>
                        <span className="kbc-todos__text">{it.text}</span>
                      </Link>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          );
        })}
      </div>
    </div>
  );
}
