import { useEffect, useMemo, useRef, useState } from "react";
import ThemePicker, { type ThemePickerController } from "@kb/ui/ThemePicker";
import { applyTheme } from "@kb/ui/themes/apply";
import {
  applyPrefs,
  loadPrefs,
  patchSettingsDebounced,
  savePrefs,
  type Density,
  type Home,
  type Prefs,
  type Theme,
} from "../../api/prefs";
import DaemonsManager from "../DaemonsManager";

const DENSITIES: Density[] = ["compact", "comfy", "spacious"];
const THEMES: Theme[] = ["light", "dark", "system"];
// W3.M-d — where a bare "/" lands. "grid" first AND default: map-home is an
// evidence-gated promotion (see `Home` in api/prefs.ts for the flip
// criterion), so this control is the deliberate opt-in, not a suggestion.
const HOMES: { value: Home; label: string; hint: string }[] = [
  { value: "grid", label: "grid", hint: "the artifact grid (default)" },
  {
    value: "map",
    label: "map",
    hint: "the atlas as a home, with the selection projected as a list",
  },
];

// Browser-scope preferences: theme (appearance + palette family)/density (persisted to
// localStorage + PATCH'd to /api/settings) plus the daemon URL list.
// Distinct from the dashboard tabs (which control the daemon's state)
// because these settings are per-user-per-browser, not per-daemon.
export default function Preferences() {
  const [prefs, setPrefs] = useState<Prefs>(loadPrefs);

  // The shared picker reads and writes through this controller; the ref keeps
  // it pointing at the CURRENT prefs without re-creating it per render.
  const prefsRef = useRef(prefs);
  prefsRef.current = prefs;
  const controller = useMemo<ThemePickerController>(
    () => ({
      appearance: () => prefsRef.current.theme,
      family: () => prefsRef.current.themeFamily,
      setAppearance: (theme) => setPrefs((p) => ({ ...p, theme })),
      setFamily: (themeFamily) => setPrefs((p) => ({ ...p, themeFamily })),
      preview: (family, appearance) => applyTheme(appearance, family),
      revert: () => applyPrefs(loadPrefs()),
    }),
    [],
  );

  useEffect(() => {
    applyPrefs(prefs);
    savePrefs(prefs);
    patchSettingsDebounced(prefs);
  }, [prefs]);

  return (
    <>
      <section className="settings__section" aria-label="appearance">
        <h2 className="settings__h2">appearance</h2>

        <div className="settings__field">
          <label className="settings__label" htmlFor="theme-select">
            theme
          </label>
          <select
            id="theme-select"
            value={prefs.theme}
            onChange={(e) => setPrefs({ ...prefs, theme: e.target.value as Theme })}
          >
            {THEMES.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </select>
        </div>

        {/* OL F2 — the shared theme picker replaces the old accent swatches:
            palette FAMILY (kb-classic first) + appearance. Browser-local; the
            daemon copy still carries theme/density only. Re-keyed on the
            appearance select above so both always show the same value. */}
        <div className="settings__field" style={{ alignItems: "flex-start" }}>
          <span className="settings__label">palette</span>
          <ThemePicker
            key={prefs.theme}
            inline
            leadFirst
            controller={controller}
          />
        </div>

        <div className="settings__field">
          <span className="settings__label">density</span>
          <div className="settings__buttons" role="radiogroup" aria-label="density">
            {DENSITIES.map((d) => (
              <button
                key={d}
                role="radio"
                aria-checked={prefs.density === d}
                className={`settings__btn ${prefs.density === d ? "is-active" : ""}`}
                onClick={() => setPrefs({ ...prefs, density: d })}
              >
                {d}
              </button>
            ))}
          </div>
        </div>

        {/* W3.M-d — cold-entry home. Browser-local (localStorage), NOT sent
            to the daemon and with no `kb` CLI verb — the same recorded
            exemption theme/accent/density/lastKb already carry. */}
        <div className="settings__field">
          <span className="settings__label">home</span>
          <div className="settings__buttons" role="radiogroup" aria-label="home">
            {HOMES.map((h) => (
              <button
                key={h.value}
                role="radio"
                aria-checked={(prefs.home ?? "grid") === h.value}
                title={h.hint}
                className={`settings__btn ${(prefs.home ?? "grid") === h.value ? "is-active" : ""}`}
                onClick={() => setPrefs({ ...prefs, home: h.value })}
                data-kb-act={`home-${h.value}`}
              >
                {h.label}
              </button>
            ))}
          </div>
        </div>
        <p className="settings__hint">
          Which view a bare <code>/</code> opens. The map home is opt-in — it
          is also reachable any time as <code>/?shell=map</code>, and the
          local-only feature census (the <code>census</code> tab) is what
          decides whether it is ever promoted to the default.
        </p>
        <div className="settings__field">
          <label className="settings__label" htmlFor="pinch-zoom">
            Pinch to change Library view
          </label>
          <input
            id="pinch-zoom"
            type="checkbox"
            checked={prefs.pinchZoom !== false}
            onChange={(e) => setPrefs({ ...prefs, pinchZoom: e.target.checked })}
          />
        </div>
        <p className="settings__hint">
          Ctrl-wheel or a trackpad pinch over the Library steps Atlas, Index and
          Shelf. Off leaves pinch to the browser; <code>-</code> and <code>=</code> still work.
        </p>
      </section>

      <DaemonsManager />
    </>
  );
}
