import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Icon } from "./icons";
import { KBC_THEME_FAMILIES, KBC_THEMES } from "./themes/registry.gen";
import {
  prefersLight,
  resolveThemeId,
  type ThemeAppearance,
  type ThemeHostOptions,
} from "./themes/apply";
import "./themepicker.css";

// V70-A7 / OL F2 — the theme picker, shared by both apps (D16 feature 3: a
// real sample, live-recolouring on arrow keys; Enter commits, Esc reverts).
//
// The live preview is free: a theme is ONE attribute on <html>, so arrowing
// down the family list repaints the whole app, this popover included, in the
// same frame, with no refetch and no stylesheet swap.
//
// Two axes, deliberately separate: APPEARANCE commits immediately (the coarse,
// most-used control), while FAMILY is STAGED, previewed on hover/arrow and
// committed on Enter or click, reverted on Escape or on close.
//
// The component owns no storage. The host hands in a `controller` (how to
// persist + apply, how to preview, how to revert), so kb-code's prefs blob and
// kb's prefs blob are each written by their own module. Host-specific pieces
// ride slots: `sample` (kb-code's code/diff preview) and `extras` (kb-code's
// contrast + density controls).

const APPEARANCES: { value: ThemeAppearance; label: string; Glyph: typeof Icon.Sun }[] = [
  { value: "light", label: "Light", Glyph: Icon.Sun },
  { value: "dark", label: "Dark", Glyph: Icon.Moon },
  { value: "system", label: "System", Glyph: Icon.Contrast },
];

export interface ThemePickerController {
  /// The PERSISTED appearance / family (what Escape reverts to).
  appearance(): ThemeAppearance;
  family(): string;
  /// Persist + apply. Appearance commits immediately; family only on commit.
  setAppearance(appearance: ThemeAppearance): void;
  setFamily(family: string): void;
  /// Flip the attributes WITHOUT persisting (a preview).
  preview(family: string, appearance: ThemeAppearance): void;
  /// Re-apply the persisted prefs: the single revert path.
  revert(): void;
}

export interface ThemePickerProps {
  controller: ThemePickerController;
  /// Host engine options (kb-code's built-in family has no registry attribute).
  host?: ThemeHostOptions;
  onClose?(): void;
  /// Mobile: the same component as a bottom sheet. Only chrome differs.
  asSheet?: boolean;
  /// Embedded in a page (Settings) instead of floating: no global key
  /// handling, no close button, no hint line, rows commit on click and stay.
  inline?: boolean;
  /// List registry families flagged `lead` first (kb lists `kb-classic` first).
  leadFirst?: boolean;
  /// The receipt line when the staged family has no registry entry (kb-code's
  /// built-in family).
  builtinReceipt?: ReactNode;
  /// Host-specific live sample, rendered under the receipt.
  sample?: ReactNode;
  /// Host-specific controls, rendered under the sample. Gets the staged
  /// family so a control change can re-preview it.
  extras?: (ctx: { staged: string; appearance: ThemeAppearance }) => ReactNode;
}

export default function ThemePicker({
  controller,
  host,
  onClose,
  asSheet = false,
  inline = false,
  leadFirst = false,
  builtinReceipt,
  sample,
  extras,
}: ThemePickerProps) {
  const committedFamily = controller.family();
  const [appearance, setAppearance] = useState<ThemeAppearance>(() => controller.appearance());
  const [staged, setStaged] = useState<string>(committedFamily);
  const close = useCallback(() => onClose?.(), [onClose]);

  const ordered = useMemo(
    () =>
      leadFirst
        ? [...KBC_THEME_FAMILIES.filter((f) => f.lead), ...KBC_THEME_FAMILIES.filter((f) => !f.lead)]
        : KBC_THEME_FAMILIES,
    [leadFirst],
  );
  const families = useMemo(() => ordered.map((f) => f.family), [ordered]);

  const stage = useCallback(
    (family: string, nextAppearance: ThemeAppearance = appearance) => {
      setStaged(family);
      controller.preview(family, nextAppearance);
    },
    [appearance, controller],
  );

  const commit = useCallback(
    (family: string) => {
      controller.setFamily(family);
    },
    [controller],
  );

  useEffect(() => {
    if (inline) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        controller.revert();
        close();
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
        close();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [inline, families, staged, stage, commit, controller, close]);

  // Unmount safety. Every dismissal path (Escape, the close button, an outside
  // click, a route change) lands here, so a staged-but-uncommitted preview can
  // never outlive the picker. Re-applying the PERSISTED prefs is correct
  // whether or not anything was staged (it is idempotent).
  const controllerRef = useRef(controller);
  controllerRef.current = controller;
  useEffect(() => {
    return () => controllerRef.current.revert();
  }, []);

  const chooseAppearance = (a: ThemeAppearance) => {
    setAppearance(a);
    controller.setAppearance(a);
    // Keep any staged (uncommitted) family visible under the new appearance.
    if (staged !== controller.family()) controller.preview(staged, a);
  };

  const stagedId = resolveThemeId(staged, appearance, prefersLight(), host);
  const stagedEntry = stagedId ? KBC_THEMES.find((t) => t.id === stagedId) : undefined;

  return (
    <div
      className={`kbc-themepicker${asSheet ? " kbc-themepicker--sheet" : ""}${
        inline ? " kbc-themepicker--inline" : ""
      }`}
      data-kbc-themepicker
      role={inline ? "group" : "dialog"}
      aria-modal={asSheet ? true : undefined}
      aria-label="theme"
    >
      <div className="kbc-themepicker__head">
        <span className="kbc-themepicker__title">Theme</span>
        {!inline && (
          <button
            type="button"
            className="kbc-iconbtn kbc-themepicker__close"
            onClick={() => {
              controller.revert();
              close();
            }}
            aria-label="close theme picker"
          >
            <Icon.X width={12} height={12} />
          </button>
        )}
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

      <ul
        className="kbc-themepicker__list"
        role="listbox"
        aria-label="theme family"
        onMouseLeave={
          inline
            ? () => {
                // Inline has no Esc/close to end a hover preview: leaving the
                // list snaps back to what is persisted.
                setStaged(controller.family());
                controller.revert();
              }
            : undefined
        }
      >
        {ordered.map((f) => {
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
                  close();
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
          (builtinReceipt ?? null)
        )}
      </p>

      {sample}

      {extras?.({ staged, appearance })}

      {!inline && <p className="kbc-themepicker__hint">↑↓ preview · Enter apply · Esc revert</p>}
    </div>
  );
}
