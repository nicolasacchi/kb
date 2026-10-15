import { useMemo, useState } from "react";
import SharedThemePicker, { type ThemePickerController } from "@kb/ui/ThemePicker";
import {
  applyTheme,
  BUILTIN_FAMILY,
  CONTRAST_MAX,
  CONTRAST_MIN,
  loadContrast,
  loadDensity,
  loadTheme,
  loadThemeFamily,
  previewTheme,
  setThemePrefs,
  type Density,
} from "../lib/prefs";
import "../styles/themesample.css";

// V70-A7 — kb-code's theme picker. The picker itself (family list, appearance
// segment, receipt, keyboard model) is shared with kb and lives in
// `@kb/ui/ThemePicker`; this wrapper binds it to kb-code's prefs module and
// adds the two things only kb-code has: the live code/diff SAMPLE (real
// `.kbc-hl-*` markup, so what previews IS what every surface renders) and
// the contrast + density controls.

export interface ThemePickerProps {
  onClose?(): void;
  /// OL-C8 — embedded in the Settings page instead of floating.
  inline?: boolean;
  /// Mobile renders the same component as a bottom sheet (mirrors the
  /// NavSheet), desktop as an anchored popover.
  asSheet?: boolean;
}

const HOST = { builtinFamily: BUILTIN_FAMILY } as const;

export default function ThemePicker({ onClose, asSheet = false, inline = false }: ThemePickerProps) {
  const controller = useMemo<ThemePickerController>(
    () => ({
      appearance: loadTheme,
      family: loadThemeFamily,
      setAppearance: (theme) => setThemePrefs({ theme }),
      setFamily: (themeFamily) => setThemePrefs({ themeFamily }),
      preview: previewTheme,
      revert: () => applyTheme(loadTheme(), loadThemeFamily()),
    }),
    [],
  );

  return (
    <SharedThemePicker
      controller={controller}
      host={HOST}
      onClose={onClose}
      asSheet={asSheet}
      inline={inline}
      builtinReceipt={
        <>
          <strong>kb-code</strong> · the built-in palette · passes AA as authored
        </>
      }
      sample={<ThemeSample />}
      extras={({ staged, appearance }) => <Controls staged={staged} appearance={appearance} />}
    />
  );
}

function Controls({
  staged,
  appearance,
}: {
  staged: string;
  appearance: "light" | "dark" | "system";
}) {
  const [contrast, setContrast] = useState<number>(() => loadContrast());
  const [density, setDensity] = useState<Density>(() => loadDensity());

  const chooseContrast = (v: number) => {
    setContrast(v);
    setThemePrefs({ contrast: v });
    if (staged !== loadThemeFamily()) previewTheme(staged, appearance);
  };

  const chooseDensity = (d: Density) => {
    setDensity(d);
    setThemePrefs({ density: d });
  };

  return (
    <div className="kbc-themepicker__sliders">
      <label className="kbc-themepicker__slider">
        <span>Contrast</span>
        <input
          type="range"
          min={CONTRAST_MIN}
          max={CONTRAST_MAX}
          step={1}
          value={contrast}
          data-kbc-theme-contrast
          onChange={(e) => chooseContrast(Number(e.target.value))}
        />
        <output>{contrast > 0 ? `+${contrast}` : contrast}</output>
      </label>
      <div className="kbc-themepicker__seg kbc-themepicker__seg--sm" role="group" aria-label="density">
        {(["comfortable", "compact"] as Density[]).map((d) => (
          <button
            key={d}
            type="button"
            className="kbc-themepicker__segbtn"
            data-kbc-theme-density={d}
            aria-pressed={density === d}
            onClick={() => chooseDensity(d)}
          >
            {d}
          </button>
        ))}
      </div>
    </div>
  );
}

/// The preview is REAL markup in the real classes — code in the fifteen
/// `.kbc-hl-*` classes the server's spans map onto, a two-line diff hunk in
/// the diff lane's own classes, a blame age band, a finding chip and a
/// dashed `likely` underline from the Lane Budget's trust vocabulary. A
/// swatch grid would show the palette; this shows the INSTRUMENT.
function ThemeSample() {
  return (
    <div className="kbc-themesample" data-kbc-themesample aria-hidden="true">
      <div className="kbc-themesample__code">
        <div className="kbc-themesample__line">
          <span className="kbc-themesample__gut kbc-themesample__age" data-band="0">
            2d
          </span>
          <code>
            <span className="kbc-hl-keyword">pub fn </span>
            <span className="kbc-hl-function">resolve</span>
            <span className="kbc-hl-punctuation">(</span>
            <span className="kbc-hl-variable">sym</span>
            <span className="kbc-hl-punctuation">: &amp;</span>
            <span className="kbc-hl-type">Symbol</span>
            <span className="kbc-hl-punctuation">) -&gt; </span>
            <span className="kbc-hl-type">Trust</span>
            <span className="kbc-hl-punctuation"> {"{"}</span>
          </code>
        </div>
        <div className="kbc-themesample__line">
          <span className="kbc-themesample__gut kbc-themesample__age" data-band="4">
            3w
          </span>
          <code>
            {"  "}
            <span className="kbc-hl-comment">// an uncertain match is an honest orphan</span>
          </code>
        </div>
        <div className="kbc-themesample__line kbc-themesample__line--del">
          <span className="kbc-themesample__gut kbc-themesample__sign">-</span>
          <code>
            {"  "}
            <span className="kbc-hl-keyword">let </span>
            <span className="kbc-hl-variable">n</span>
            <span className="kbc-hl-operator"> = </span>
            <span className="kbc-hl-number">0</span>
            <span className="kbc-hl-punctuation">;</span>
          </code>
        </div>
        <div className="kbc-themesample__line kbc-themesample__line--add">
          <span className="kbc-themesample__gut kbc-themesample__sign">+</span>
          <code>
            {"  "}
            <span className="kbc-hl-keyword">let </span>
            <span className="kbc-hl-variable">n</span>
            <span className="kbc-hl-operator"> = </span>
            <span className="kbc-hl-string">"exact"</span>
            <span className="kbc-hl-punctuation">.</span>
            <span className="kbc-hl-property">len</span>
            <span className="kbc-hl-punctuation">();</span>
          </code>
        </div>
        <div className="kbc-themesample__line">
          <span className="kbc-themesample__gut kbc-themesample__age" data-band="8">
            1y
          </span>
          <code>
            {"  "}
            <span className="kbc-hl-variable kbc-trust-likely kbc-themesample__usage">
              call_site
            </span>
            <span className="kbc-hl-punctuation">()</span>
          </code>
        </div>
      </div>
      <div className="kbc-themesample__chips">
        <span className="kbc-themesample__chip kbc-themesample__chip--blocker">blocker</span>
        <span className="kbc-themesample__chip kbc-themesample__chip--concern">concern</span>
        <span className="kbc-themesample__chip kbc-themesample__chip--ok">ok</span>
        <span className="kbc-themesample__trust kbc-trust-exact">exact</span>
        <span className="kbc-themesample__trust kbc-trust-likely">likely</span>
        <span className="kbc-themesample__trust kbc-trust-candidate">candidate</span>
      </div>
    </div>
  );
}
