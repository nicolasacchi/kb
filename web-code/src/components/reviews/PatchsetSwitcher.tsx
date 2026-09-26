import type { ReviewPatchset } from "../../api/types";
import type { DiffPsSelection } from "../../lib/codeUrl";

export interface PatchsetSwitcherProps {
  patchsets: ReviewPatchset[];
  /// `null` = "latest" (the URL omits `?ps=` at its default).
  value: DiffPsSelection | null;
  /// Which patchset "latest" currently resolves to. V80-R4: no longer
  /// printed inline in the option text (it was this control's single
  /// widest string, part of the toolbar's one-line-at-1280px width
  /// budget) — it's the select's own `title` tooltip now, and the SAME
  /// number is already visible one glance to the left on the toolbar's
  /// ps chip whenever this select shows "latest".
  latestPs: number | null;
  onChange: (next: DiffPsSelection | null) => void;
}

const LATEST = "latest";

/// V73-K2a — the full-page diff's patchset switcher. Until this unit the
/// diff route READ `?ps=` and nothing on the page ever wrote it, so the
/// full-page diff was permanently pinned to `latest` (the recon's own
/// finding).
///
/// TWO MODES, one control pair:
///
///   * base = "base" → `GET /reviews/{id}/files?ps=N`, the review's base
///     vs. patchset N. This is what the page has always shown.
///   * base = a patchset number → `GET /reviews/{id}/interdiff?from=&to=`,
///     the INTERDIFF between two patchsets ("any-vs-any patchset
///     interdiff", design §D9). The interdiff wire's file rows carry NO
///     viewed/annotation fields (`ReviewInterdiffFile`), so the page says
///     so rather than rendering an empty column: absence is captioned, not
///     zeroed.
///
/// The pair is always written as ONE `?ps=` value (`N` or `A..B`) through
/// `codeUrl.ts`'s `formatDiffPs`, so there is exactly one param to read
/// back and a reload reproduces the view.
export default function PatchsetSwitcher({
  patchsets,
  value,
  latestPs,
  onChange,
}: PatchsetSwitcherProps) {
  const numbers = patchsets.map((p) => p.ps_number).sort((a, b) => a - b);
  const head = value === null ? null : typeof value === "number" ? value : value.to;
  const base = value !== null && typeof value !== "number" ? value.from : null;

  function setHead(raw: string) {
    if (raw === LATEST) {
      onChange(null);
      return;
    }
    const n = Number(raw);
    if (!Number.isInteger(n) || n < 1) return;
    if (base !== null && base < n) onChange({ from: base, to: n });
    else onChange(n);
  }

  function setBase(raw: string) {
    const effectiveHead = head ?? latestPs;
    if (raw === "base" || effectiveHead === null) {
      onChange(effectiveHead === null ? null : effectiveHead);
      return;
    }
    const n = Number(raw);
    if (!Number.isInteger(n) || n < 1 || n >= effectiveHead) return;
    onChange({ from: n, to: effectiveHead });
  }

  return (
    <div className="kbc-rdiff__psw" data-kbc-rdiff-psw>
      <label className="kbc-rdiff__psw-label">
        <span className="kbc-sr-only">Diff base</span>
        <select
          value={base === null ? "base" : String(base)}
          onChange={(e) => setBase(e.target.value)}
          aria-label="diff base"
          data-kbc-rdiff-psw-base
        >
          {/* V80-R4 — "review base" shortened to "base": this select's
              own `aria-label` ("diff base") already carries the fuller
              phrase for a screen reader, and the toolbar's width budget
              (`web-code/CLAUDE.md` § Review diff v2, one line at 1280px)
              is tight enough that the native `<select>`'s WIDEST option
              sets its rendered width regardless of which one is chosen. */}
          <option value="base">base</option>
          {numbers
            .filter((n) => (head ?? latestPs) === null || n < (head ?? latestPs ?? 0))
            .map((n) => (
              <option key={n} value={n}>
                ps{n}
              </option>
            ))}
        </select>
      </label>
      <span className="kbc-rdiff__psw-arrow" aria-hidden="true">
        →
      </span>
      <label className="kbc-rdiff__psw-label">
        <span className="kbc-sr-only">Diff head patchset</span>
        <select
          value={head === null ? LATEST : String(head)}
          onChange={(e) => setHead(e.target.value)}
          aria-label="diff head patchset"
          /* V80-R4 — "latest (psN)" shortened to bare "latest": a native
             `<select>` sizes its CLOSED box to its widest option, so this
             was the single biggest item in the toolbar's Navigate cluster
             (part of the one-line-at-1280px width budget,
             `web-code/CLAUDE.md` § Review diff v2). Not a silent loss —
             when this select shows "latest" (`psSel === null`), the
             ADJACENT ps chip in the SAME cluster always resolves to the
             SAME number (`ReviewDiffToolbar.tsx`'s own `ps${activePsNum}`
             branch), so "which patchset does latest mean" is still never
             a mystery, one glance to the left; the hover `title` below
             keeps it a zero-click answer too. */
          title={latestPs !== null ? `latest resolves to ps${latestPs}` : undefined}
          data-kbc-rdiff-psw-head
        >
          <option value={LATEST}>latest</option>
          {numbers.map((n) => (
            <option key={n} value={n}>
              ps{n}
            </option>
          ))}
        </select>
      </label>
    </div>
  );
}
