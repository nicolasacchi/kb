import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Icon } from "./icons";
import { KBC_THEME_FAMILIES, KBC_THEMES } from "../themes/registry.gen";
import {
  applyTheme,
  CONTRAST_MAX,
  CONTRAST_MIN,
  loadContrast,
  loadDensity,
  loadTheme,
  loadThemeFamily,
  previewTheme,
  resolveThemeId,
  setThemePrefs,
  type Appearance,
  type Density,
} from "../lib/prefs";
import "../styles/themepicker.css";

// V70-A7 — the theme picker (D16 feature 3: "a panel showing a real code
// sample PLUS a real diff hunk PLUS a blame gutter PLUS a finding chip, live
// -recolouring on arrow keys; Enter commits, Esc reverts").
//
// The live preview is the point, and it is free: a theme is ONE attribute on
// <html>, so arrowing down the family list repaints the entire app — this
// popover included — in the same frame, with no refetch, no stylesheet swap
// and no CodeMirror rebuild. That is why the sample below is real markup
// using the real `.kbc-hl-*` / diff / trust classes rather than a picture of
// a theme: what you see previewing IS what every surface will render.
//
// Two axes, deliberately separate (see `lib/prefs.ts`): APPEARANCE commits
// immediately (it is the coarse, most-used control, and the mobile NavSheet
// row cycles the same axis), while FAMILY is STAGED — previewed on
// hover/arrow, committed on Enter or click, reverted on Escape or on close.

const APPEARANCES: { value: Appearance; label: string; Glyph: typeof Icon.Sun }[] = [
  { value: "light", label: "Light", Glyph: Icon.Sun },
  { value: "dark", label: "Dark", Glyph: Icon.Moon },
  { value: "system", label: "System", Glyph: Icon.Contrast },
];

export interface ThemePickerProps {
  onClose(): void;
  /// Mobile renders the same component as a bottom sheet (mirrors the
  /// NavSheet), desktop as an anchored popover. Only chrome differs — the
  /// controls, the sample and the keyboard model are identical.
  asSheet?: boolean;
}

export default function ThemePicker({ onClose, asSheet = false }: ThemePickerProps) {
  const committedFamily = loadThemeFamily();
  const [appearance, setAppearance] = useState<Appearance>(() => loadTheme());
  const [staged, setStaged] = useState<string>(committedFamily);
  const [contrast, setContrast] = useState<number>(() => loadContrast());
  const [density, setDensity] = useState<Density>(() => loadDensity());
  const committedRef = useRef(committedFamily);

  const families = useMemo(
    () => KBC_THEME_FAMILIES.map((f) => f.family),
    [],
  );

  /// Revert to whatever is actually persisted. Used by Escape, by an outside
  /// click and by unmount — three paths, ONE revert, so a preview can never
  /// leak past the popover's lifetime.
  const revert = useCallback(() => {
    applyTheme(loadTheme(), loadThemeFamily());
  }, []);

  const stage = useCallback(
    (family: string, nextAppearance: Appearance = appearance) => {
      setStaged(family);
      previewTheme(family, nextAppearance);
    },
    [appearance],
  );

  const commit = useCallback(
    (family: string) => {
      committedRef.current = family;
      setThemePrefs({ themeFamily: family });
    },
    [],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        revert();
        onClose();
        return;
      }
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const i = families.indexOf(staged);
        const next =
          families[
            (i + (e.key === "ArrowDown" ? 1 : families.length - 1) + families.length) %
              families.length
          ];
        stage(next);
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        commit(staged);
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [families, staged, stage, commit, revert, onClose]);

  // Unmount safety. Every dismissal path — Escape, the ✕, an outside click,
  // a route change — lands here, so a staged-but-uncommitted preview can
  // never outlive the popover. Re-applying the PERSISTED prefs is correct
  // whether or not anything was staged (it is idempotent), which is why
  // there is one branch and not two.
  useEffect(() => {
    return () => {
      applyTheme(loadTheme(), loadThemeFamily());
    };
  }, []);

  const chooseAppearance = (a: Appearance) => {
    setAppearance(a);
    setThemePrefs({ theme: a });
    // Keep any staged (uncommitted) family visible under the new appearance.
    if (staged !== loadThemeFamily()) previewTheme(staged, a);
  };

  const chooseContrast = (v: number) => {
    setContrast(v);
    setThemePrefs({ contrast: v });
    if (staged !== loadThemeFamily()) previewTheme(staged, appearance);
  };

  const chooseDensity = (d: Density) => {
    setDensity(d);
    setThemePrefs({ density: d });
  };

  const stagedId = resolveThemeId(
    staged,
    appearance,
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia("(prefers-color-scheme: light)").matches
      : false,
  );
  const stagedEntry = stagedId ? KBC_THEMES.find((t) => t.id === stagedId) : undefined;

  return (
    <div
      className={`kbc-themepicker${asSheet ? " kbc-themepicker--sheet" : ""}`}
      data-kbc-themepicker
      role="dialog"
      aria-modal={asSheet ? true : undefined}
      aria-label="theme"
    >
      <div className="kbc-themepicker__head">
        <span className="kbc-themepicker__title">Theme</span>
        <button
          type="button"
          className="kbc-iconbtn"
          onClick={() => {
            revert();
            onClose();
          }}
          aria-label="close theme picker"
        >
          <Icon.X />
        </button>
      </div>

      <div className="kbc-themepicker__seg" role="group" aria-label="appearance">
        {APPEARANCES.map(({ value, label, Glyph }) => (
          <button
            key={value}
            type="button"
            className="kbc-themepicker__segbtn"
            data-kbc-theme-appearance={value}
            aria-pressed={appearance === value}
            onClick={() => chooseAppearance(value)}
          >
            <Glyph />
            {label}
          </button>
        ))}
      </div>

      <ul className="kbc-themepicker__list" role="listbox" aria-label="theme family">
        {KBC_THEME_FAMILIES.map((f) => {
          const on = staged === f.family;
          const member =
            f.members.find((m) => m.appearance === (appearance === "system" ? "dark" : appearance)) ??
            f.members[0];
          return (
            <li key={f.family}>
              <button
                type="button"
                className="kbc-themepicker__row"
                data-kbc-theme-family={f.family}
                role="option"
                aria-selected={on}
                onMouseEnter={() => stage(f.family)}
                onFocus={() => stage(f.family)}
                onClick={() => {
                  stage(f.family);
                  commit(f.family);
                  onClose();
                }}
              >
                <span className="kbc-themepicker__swatches" aria-hidden="true">
                  <i style={{ background: member.bg }} />
                  <i style={{ background: member.accent }} />
                  <i style={{ background: member.ink }} />
                </span>
                <span className="kbc-themepicker__name">{f.familyName}</span>
                <span className="kbc-themepicker__variants">
                  {f.members.length === 1 ? f.members[0].appearance : `${f.members.length} variants`}
                </span>
                {on && (
                  <span className="kbc-themepicker__check">
                    <Icon.Check />
                  </span>
                )}
              </button>
            </li>
          );
        })}
      </ul>

      {/* The receipt. A DERIVED value is never presented as an authored one:
          `repaired` is how many roles the AA pass had to move off the
          vendor's own hex to clear 4.5:1, and it is stated, not hidden. */}
      <p className="kbc-themepicker__receipt">
        {stagedEntry ? (
          <>
            <strong>{stagedEntry.name}</strong> · {stagedEntry.license}
            {stagedEntry.repaired > 0
              ? ` · ${stagedEntry.repaired} role${stagedEntry.repaired === 1 ? "" : "s"} nudged for AA`
              : " · passes AA as authored"}
          </>
        ) : (
          <>
            <strong>kb-code</strong> · the built-in palette · passes AA as authored
          </>
        )}
      </p>

      <ThemeSample />

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

      <p className="kbc-themepicker__hint">↑↓ preview · Enter apply · Esc revert</p>
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
