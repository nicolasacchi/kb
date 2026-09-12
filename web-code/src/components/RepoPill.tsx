import { useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import { useActiveRepo, useExplicitRepo } from "../hooks/useActiveRepo";
import { useRepos } from "../hooks/useRepos";
import { readerUrl } from "../lib/breadcrumbs";
import { Icon } from "./icons";

// F1 — the repo-scope pill, replacing the old `RepoPicker` box. The
// operator's explicit ask: "remove the top left box to change the codebase,
// make it more similar to kb" — this mirrors kb's own workspace pill
// (`web/src/components/chrome/Header.tsx`'s `KbSelector`, root CLAUDE.md
// invariant #33): a small pill in the shared TopBar showing the active repo,
// dimmed + "click to pick one" on an unscoped view (Home, all-repo
// /search), opening a dropdown that lists every configured repo (name, file/
// symbol counts, watcher badge — the data the old `RepoPicker` showed) and
// navigates into it.
export default function RepoPill() {
  const explicit = useExplicitRepo();
  const active = useActiveRepo();
  const { data, isLoading, error } = useRepos();
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onDocClick(e: MouseEvent) {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const repos = data?.repos ?? [];
  const scoped = explicit !== null;
  const label = active ?? (isLoading ? "…" : "—");

  return (
    <div className="kbc-repopill-wrap" ref={wrapRef}>
      {/* SH.C2 (audit P1) — once repo-scoped there was no chrome path back
          to the Home fleet dashboard; the 'R' badge is now its own escape
          hatch, always live regardless of scope/popover state. The
          name+chevron button below is otherwise byte-identical (same
          `data-kbc-repopill` / `data-scope` the e2e suite drives) and keeps
          toggling the popover. */}
      <Link
        to="/"
        className="kbc-repopill-home"
        data-kbc-repopill-home
        aria-label="All repos"
        title="All repos"
      >
        <span className="kbc-repopill-r" aria-hidden="true">
          R
        </span>
      </Link>
      <button
        type="button"
        className={"kbc-repopill" + (scoped ? "" : " kbc-repopill--unscoped")}
        data-kbc-repopill
        data-scope={scoped ? "one" : "unscoped"}
        onClick={() => setOpen((o) => !o)}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={scoped ? undefined : "no repo pinned — click to pick one"}
      >
        <span className="kbc-repopill-name">{label}</span>
        <Icon.ChevDown />
      </button>
      {open && (
        <ul className="kbc-repopopover" role="listbox" data-kbc-repopopover>
          <li>
            <Link
              className="kbc-repopopover__item kbc-repopopover__item--home"
              to="/"
              onClick={() => setOpen(false)}
              data-kbc-repopill-allrepos
            >
              <span className="kbc-repopopover__name">All repos</span>
              <span className="kbc-repopopover__meta">Home / fleet dashboard</span>
            </Link>
          </li>
          {error ? (
            <li className="kbc-repopopover__empty">Failed to load repos</li>
          ) : repos.length === 0 ? (
            <li className="kbc-repopopover__empty">
              No repos configured — add one under <code>[[repos]]</code> in kb-code.toml.
            </li>
          ) : (
            repos.map((r) => (
              <li key={r.name}>
                <Link
                  className={"kbc-repopopover__item" + (r.name === active ? " is-on" : "")}
                  to={readerUrl(r.name, "")}
                  onClick={() => setOpen(false)}
                  data-kbc-repopill-item={r.name}
                >
                  <span className="kbc-repopopover__name">{r.name}</span>
                  <span className="kbc-repopopover__meta">
                    {r.file_count} files · {r.symbol_count} symbols
                  </span>
                  <span
                    className={`kbc-repopopover__badge kbc-repopopover__badge--${r.watcher}`}
                    data-kbc-watcher-state={r.watcher}
                    title={`live-mirror watcher: ${r.watcher}`}
                  >
                    {r.watcher}
                  </span>
                </Link>
              </li>
            ))
          )}
        </ul>
      )}
    </div>
  );
}
