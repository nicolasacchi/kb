import { useState } from "react";
import { Icon } from "../icons";
import { useSavedQueries } from "../../hooks/useSavedQueries";
import { useRecentQueries } from "../../hooks/useRecentQueries";
import { useUrl } from "../../hooks/useUrl";
import type { SearchMode } from "../../api/client";

// FS8 — saved searches + recent queries, in a dropdown under the search
// box. Because the whole faceted search state lives in the URL, a saved
// search is just its querystring: useSavedQueries snapshots
// `location.search` + pathname (localStorage + the daemon's canonical
// store) and restoring is one navigate. Recents come from the per-kb
// queries ring. No new persistence layer.

type Props = {
  kb?: string;
  onApplyQuery: (q: string, mode?: SearchMode) => void;
};

export default function SearchSavedBar({ kb, onApplyQuery }: Props) {
  const [open, setOpen] = useState(false);
  const { queries: saved, save, remove } = useSavedQueries();
  const recents = useRecentQueries(kb, open);
  const { navigate } = useUrl();

  const onSave = () => {
    const name = window.prompt("Save this search as…");
    if (name && name.trim()) save(name.trim());
  };

  return (
    <div className="kb-search-saved">
      <button
        type="button"
        className="kb-search-saved__btn"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <Icon.Star aria-hidden="true" /> Saved · Recent
      </button>
      {open && (
        <>
          <button
            type="button"
            className="kb-search-saved__backdrop"
            aria-hidden="true"
            tabIndex={-1}
            onClick={() => setOpen(false)}
          />
          <div className="kb-search-saved__panel" role="menu">
            <div className="kb-search-saved__sect">
              <span>Saved</span>
              <button
                type="button"
                className="kb-search-saved__save"
                onClick={onSave}
              >
                + save current
              </button>
            </div>
            {saved.length === 0 ? (
              <div className="kb-search-saved__empty">none saved yet</div>
            ) : (
              saved.map((s) => (
                <div key={s.name} className="kb-search-saved__row">
                  <button
                    type="button"
                    className="kb-search-saved__apply"
                    onClick={() => {
                      setOpen(false);
                      navigate(`${s.path}${s.search}`);
                    }}
                  >
                    {s.name}
                  </button>
                  <button
                    type="button"
                    className="kb-search-saved__x"
                    aria-label={`delete ${s.name}`}
                    onClick={() => remove(s.name)}
                  >
                    <Icon.X />
                  </button>
                </div>
              ))
            )}
            {recents.length > 0 && (
              <>
                <div className="kb-search-saved__sect">
                  <span>Recent</span>
                </div>
                {recents.map((r, i) => (
                  <button
                    key={`${r.q}-${i}`}
                    type="button"
                    className="kb-search-saved__apply kb-search-saved__recent"
                    onClick={() => {
                      setOpen(false);
                      onApplyQuery(r.q, r.mode as SearchMode);
                    }}
                  >
                    <span className="kb-search-saved__q">{r.q}</span>
                    <span className="kb-search-saved__hits">{r.hits}</span>
                  </button>
                ))}
              </>
            )}
          </div>
        </>
      )}
    </div>
  );
}
