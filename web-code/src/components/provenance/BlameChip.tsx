export interface BlameChipProps {
  rect: DOMRect;
  label: string;
  solid: boolean;
}

/// The disclosure ladder's step 2 — a small floating chip positioned next
/// to the hovered gutter dot (`editor/lineGutter.ts`'s `onHover` reports the
/// marker's own `getBoundingClientRect()`), showing the session display
/// name/commit subject + confidence label the dot itself only hints at via
/// solid/outline styling. `position: fixed` (viewport coordinates, matching
/// `getBoundingClientRect`) — deliberately outside `CodeView`'s scrolling
/// container so it never gets clipped by `overflow: auto`.
export default function BlameChip({ rect, label, solid }: BlameChipProps) {
  return (
    <div
      className={"kbc-blame-chip" + (solid ? " kbc-blame-chip--solid" : " kbc-blame-chip--outline")}
      style={{ position: "fixed", left: rect.right + 6, top: Math.max(4, rect.top - 4) }}
      data-kbc-blame-chip
    >
      {label}
    </div>
  );
}
