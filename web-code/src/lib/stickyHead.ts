// v0.47 SH — pinned file headers in the full-page review diff.
//
// The review toolbar (`.kbc-rdiff__toolbar`) is `position: sticky; top: 0`
// inside the `.kbc-approute` scroller, and every per-file header
// (`.kbc-rdiff__section-head`) is sticky inside its OWN section. A file header
// must pin directly UNDER the toolbar, so its `top` is the toolbar's measured
// height — not a constant, because the toolbar's height depends on the type
// ramp, density and (on narrow screens) its own wrapping. The review root
// publishes the measurement as `--rdiff-stick-top`; this module is the pure
// part of that.

/// The CSS length for the pinned-header offset. A non-finite or negative
/// measurement (a detached/hidden toolbar reads 0 or NaN) yields `null`, so the
/// caller removes the property and the stylesheet's `var()` fallback applies.
export function stickTopCss(toolbarHeightPx: number | null | undefined): string | null {
  if (toolbarHeightPx == null || !Number.isFinite(toolbarHeightPx) || toolbarHeightPx <= 0) return null;
  return `${Math.ceil(toolbarHeightPx)}px`;
}

/// How far the scroller must move so a section whose top edge sits at
/// `sectionTop` (viewport px) lands flush under the pinned chrome at
/// `stickTop`. Only a section that has scrolled UP past the pin line needs
/// help (positive-or-zero means its header is still in normal flow) — this is
/// the "viewed collapsed a long file while I was mid-way through it" case:
/// without it the shrunken page leaves the next file's header far from where
/// the collapsed one was pinned.
export function stuckScrollDelta(sectionTop: number, stickTop: number): number {
  return sectionTop < stickTop ? sectionTop - stickTop : 0;
}
