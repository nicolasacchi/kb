import { useCallback, useEffect, useState, type ReactNode } from "react";

export type TabSpec = {
  id: string;
  label: string;
  badge?: ReactNode;
  body: () => ReactNode;
};

const STORE_KEY = "kb:settings:tab";

function readHashTab(tabs: TabSpec[]): string | null {
  const h = window.location.hash.replace(/^#/, "");
  return tabs.some((t) => t.id === h) ? h : null;
}

function readStoredTab(tabs: TabSpec[]): string | null {
  try {
    const v = localStorage.getItem(STORE_KEY);
    return v && tabs.some((t) => t.id === v) ? v : null;
  } catch {
    return null;
  }
}

// Tabbed shell for the operator dashboard. URL hash drives the active
// tab so deep links work and the back button moves between tabs; on a
// fresh visit (no hash) we restore the last tab from localStorage and
// fall back to the first tab in the list. The tab body is rendered
// lazily — `body()` is called only for the active tab, so dormant tabs
// don't open SSE streams or fetch state.
export default function Tabs({
  tabs,
  label,
}: {
  tabs: TabSpec[];
  label: string;
}) {
  const [active, setActiveState] = useState<string>(
    () => readHashTab(tabs) ?? readStoredTab(tabs) ?? tabs[0]?.id ?? "",
  );

  // Follow the URL hash when the user uses bf/fwd or types a hash.
  useEffect(() => {
    const onHash = () => {
      const next = readHashTab(tabs);
      if (next && next !== active) setActiveState(next);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, [tabs, active]);

  // Persist the latest active tab + reflect it into the URL. `replace`
  // (not push) keeps tab clicks from polluting browser history with one
  // entry per click; bf/fwd between tabs is still possible because the
  // user can return to a deep link via the explicit URL.
  const setActive = useCallback((id: string) => {
    setActiveState(id);
    try {
      localStorage.setItem(STORE_KEY, id);
    } catch {
      // ignore quota / private-mode failures — non-fatal
    }
    const url = new URL(window.location.href);
    url.hash = id;
    window.history.replaceState(null, "", url.toString());
  }, []);

  const body = tabs.find((t) => t.id === active)?.body;

  return (
    <>
      <div className="settings__tabs" role="tablist" aria-label={label}>
        {tabs.map((t) => (
          <button
            key={t.id}
            role="tab"
            id={`settings-tab-${t.id}`}
            aria-selected={t.id === active}
            aria-controls={`settings-panel-${t.id}`}
            tabIndex={t.id === active ? 0 : -1}
            className={`settings__tab ${t.id === active ? "is-active" : ""}`}
            onClick={() => setActive(t.id)}
            onKeyDown={(e) => {
              const i = tabs.findIndex((x) => x.id === active);
              if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
                e.preventDefault();
                const dir = e.key === "ArrowRight" ? 1 : -1;
                const next = tabs[(i + dir + tabs.length) % tabs.length];
                if (next) {
                  setActive(next.id);
                  document.getElementById(`settings-tab-${next.id}`)?.focus();
                }
              } else if (e.key === "Home") {
                e.preventDefault();
                const first = tabs[0];
                if (first) {
                  setActive(first.id);
                  document.getElementById(`settings-tab-${first.id}`)?.focus();
                }
              } else if (e.key === "End") {
                e.preventDefault();
                const last = tabs[tabs.length - 1];
                if (last) {
                  setActive(last.id);
                  document.getElementById(`settings-tab-${last.id}`)?.focus();
                }
              }
            }}
          >
            <span>{t.label}</span>
            {t.badge != null && <span className="settings__tab-badge">{t.badge}</span>}
          </button>
        ))}
      </div>
      <div
        role="tabpanel"
        id={`settings-panel-${active}`}
        aria-labelledby={`settings-tab-${active}`}
        className="settings__panel"
      >
        {body ? body() : null}
      </div>
    </>
  );
}
