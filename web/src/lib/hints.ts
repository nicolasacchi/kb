// W2.6b — hint mode (`f`/`F`) target enumeration + label assignment.
//
// `labelFor`/`labelsFor` are pure (golden-pinned by `hints.test.ts`); the
// enumeration side necessarily touches the DOM (there's no other way to
// find "every clickable thing currently on screen"), so it isn't unit
// tested here — vitest runs `environment: "node"` (no DOM); the resolution
// behavior (typing narrows, Esc/scroll/resize closes) is exercised by
// `HintOverlay.tsx`, a Playwright concern per the harness's own split.
//
// Recon verdict (w2-recon-keyboard2.md §2): a composite selector is honest
// and sufficient — no per-surface `data-kb-hint` registration attribute is
// needed for coverage. `data-kb-act` alone only covers chrome VERBS (~50
// buttons); the navigational mass (gallery cards, search results, tag
// pills, inspector rows, TocSpy rows, backlinks, wikilinks, nav rows) is
// all plain `<a href>`/`<button>` — hence the union below.

/// Every element hint mode can target. `[data-kb-act]` first so a chrome
/// verb keeps its natural DOM order relative to nearby links/buttons; the
/// other two clauses cover the navigational mass no single attribute
/// tags today (recon §2's "significant clickable atoms without
/// data-kb-act").
export const HINT_SELECTOR =
  '[data-kb-act], a[href]:not([aria-hidden="true"]), button:not([disabled])';

/// Home-row-first ordering (Vimium-style): the 9 home-row keys, then the
/// remaining 17 letters in QWERTY row order. Fixed and deterministic —
/// `labelFor`'s golden test pins this exact sequence.
const ALPHABET = "asdfghjklqwertyuiopzxcvbnm";

/// The nth (0-indexed) hint label: single-key for the first
/// `ALPHABET.length` (26) targets, two-key combinations drawn from the same
/// alphabet beyond that (up to 26*26 additional targets — far more than any
/// real page needs). Deterministic and pure so it's golden-pinned.
///
/// Known limitation (documented, not fixed): once 2-key labels are in use,
/// every 2-key label necessarily shares its first character with some
/// already-assigned 1-key label (all 26 single letters are exhausted
/// before 2-key labels start) — `HintOverlay`'s narrowing resolves an
/// exact 1-key match immediately, so a 2-key label sharing that first
/// character becomes unreachable. This only bites pages with >26
/// simultaneously visible hintable targets, which is rare in practice.
export function labelFor(n: number): string {
  if (n < 0 || !Number.isInteger(n)) {
    throw new RangeError(`labelFor: n must be a non-negative integer, got ${n}`);
  }
  const base = ALPHABET.length;
  if (n < base) return ALPHABET[n];
  const rest = n - base;
  const first = Math.floor(rest / base) % base;
  const second = rest % base;
  return ALPHABET[first] + ALPHABET[second];
}

/// The first `count` labels in order — the exact assignment `HintOverlay`
/// hands out to its enumerated targets (index `i` in DOM order gets
/// `labelFor(i)`).
export function labelsFor(count: number): string[] {
  return Array.from({ length: count }, (_, i) => labelFor(i));
}

export type HintTarget = {
  element: HTMLElement;
  label: string;
  /// Present only for `<a href>` targets — lets the `F` (new-tab) variant
  /// `window.open` instead of `.click()`-ing (which would navigate the
  /// current tab via react-router regardless of intent).
  href: string | null;
};

/// The topmost open dialog, if any (last in DOM order — overlays mount
/// later in the tree as they open, mirroring the stacking convention
/// elsewhere in this codebase). Hint enumeration scopes to it when
/// present (recon: "scope to the topmost open dialog when one exists") so
/// hints never target chrome hidden behind a modal. Intentionally
/// duplicates `useRovingCursor.ts`'s private `isOverlayOpen` query shape —
/// that helper isn't exported and that file isn't owned by this phase.
function topmostDialog(): HTMLElement | null {
  const dialogs = document.querySelectorAll<HTMLElement>(
    'dialog[open], [role="dialog"][aria-modal="true"]',
  );
  return dialogs.length > 0 ? dialogs[dialogs.length - 1] : null;
}

function isHintable(el: HTMLElement): boolean {
  // Opt-out for noisy micro-buttons (e.g. a tag-remove `×` repeated per
  // pill) — an attribute rather than a registry, so any future surface can
  // opt out without a code change here.
  if (el.closest('[data-kb-hint="skip"]')) return false;
  const rect = el.getBoundingClientRect();
  if (rect.width === 0 && rect.height === 0) return false;
  if (rect.bottom <= 0 || rect.top >= window.innerHeight) return false;
  if (rect.right <= 0 || rect.left >= window.innerWidth) return false;
  return true;
}

/// Enumerate hintable targets: scoped to the topmost open dialog when one
/// exists (else the whole document), filtered to viewport-visible and not
/// opted out via `data-kb-hint="skip"`. DOM order (source order) is the
/// order labels get assigned in — deterministic given a stable page.
export function enumerateHintables(): HTMLElement[] {
  const root: ParentNode = topmostDialog() ?? document;
  return Array.from(root.querySelectorAll<HTMLElement>(HINT_SELECTOR)).filter(isHintable);
}

/// `enumerateHintables()` + label assignment in one call — what
/// `HintOverlay` mounts with.
export function assignHints(): HintTarget[] {
  const elements = enumerateHintables();
  return elements.map((element, i) => ({
    element,
    label: labelFor(i),
    href: element instanceof HTMLAnchorElement ? element.href : null,
  }));
}
