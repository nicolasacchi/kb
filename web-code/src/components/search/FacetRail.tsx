// V71-D2 — the facet rail that WRITES the query.
//
// Every control here does exactly one thing: hand a new query STRING back to
// the page, built by `lib/kbcqEdit.ts`. Nothing in this component holds
// filter state, because a filter the box does not show is a filter you
// cannot share, cannot paste into `kb-code search`, and cannot undo by
// editing the text — the discipline root CLAUDE.md #35 records for kb's
// `galleryUrl` builder, applied to a grammar instead of a URL.
//
// The counts come from the daemon and are PAGE counts; the rail says so in
// words (the server's own `note`) rather than letting a number imply a
// corpus census it never made.

import type { Facets, ScopesOut } from "../../api/types";
import { setLane, toggleClause, hasClause } from "../../lib/kbcqEdit";
import type { Lane } from "../../lib/kbcq";
import { scopeToClause } from "../../lib/scopeClause";

export interface FacetRailProps {
  query: string;
  facets: Facets | undefined;
  /// `GET /api/scopes` — the named `[scopes]` path-glob sets. Undefined
  /// while loading or when the call failed; the rail simply omits the
  /// section rather than showing an empty promise.
  scopes: ScopesOut | undefined;
  onQuery: (next: string) => void;
}

export default function FacetRail({ query, facets, scopes, onQuery }: FacetRailProps) {
  const scopeNames = Object.keys(scopes?.scopes ?? {}).sort();
  return (
    <aside className="kbc-facets" data-kbc-role="facet-rail" aria-label="facets">
      {facets ? (
        <>
          <p className="kbc-facets__basis">{facets.note}</p>
          {facets.groups.length === 0 && <p className="kbc-facets__empty">Nothing on this page to facet by.</p>}
          {facets.groups.map((g) => (
            <section key={g.field} className="kbc-facets__group" data-kbc-facet={g.field}>
              <h3 className="kbc-facets__label">{g.label}</h3>
              {g.values.map((v) => {
                const active = g.writes === "clause" && hasClause(query, v.clause);
                return (
                  <button
                    key={v.value}
                    type="button"
                    className={`kbc-facets__row${active ? " is-active" : ""}`}
                    data-kbc-clause={v.clause}
                    aria-pressed={g.writes === "clause" ? active : undefined}
                    title={
                      g.writes === "prefix"
                        ? `switch the query to the ${v.value} lane (${v.clause})`
                        : `${active ? "remove" : "add"} ${v.clause}`
                    }
                    onClick={() =>
                      onQuery(
                        g.writes === "prefix"
                          ? setLane(query, v.value as Lane)
                          : toggleClause(query, v.clause),
                      )
                    }
                  >
                    <span className="kbc-facets__value">{v.value === "" ? "(repo root)" : v.value}</span>
                    <span className="kbc-facets__count">{v.count}</span>
                  </button>
                );
              })}
              {g.omitted ? (
                <p className="kbc-facets__omitted">+{g.omitted} more value(s) not shown</p>
              ) : null}
            </section>
          ))}
        </>
      ) : (
        <p className="kbc-facets__empty">Facets are off — Alt-f writes `facets:1` into the query.</p>
      )}

      {scopeNames.length > 0 && (
        <section className="kbc-facets__group" data-kbc-facet="scope">
          <h3 className="kbc-facets__label">Scope</h3>
          {scopeNames.map((name) => {
            const r = scopeToClause(name, scopes?.scopes[name] ?? []);
            if (!r.ok) {
              // An honest refusal, not a disabled button with no reason:
              // kbc-scope/1 (P3) is what will carry these, and until it
              // exists a partial clause would silently over-match.
              return (
                <p key={name} className="kbc-facets__refused" title={r.reason}>
                  {name} — unavailable
                </p>
              );
            }
            const active = hasClause(query, r.clause);
            return (
              <button
                key={name}
                type="button"
                className={`kbc-facets__row${active ? " is-active" : ""}`}
                data-kbc-clause={r.clause}
                aria-pressed={active}
                title={`${active ? "remove" : "add"} ${r.clause}`}
                onClick={() => onQuery(toggleClause(query, r.clause))}
              >
                <span className="kbc-facets__value">{name}</span>
                <span className="kbc-facets__count kbc-facets__count--clause">{r.clause}</span>
              </button>
            );
          })}
        </section>
      )}
    </aside>
  );
}
