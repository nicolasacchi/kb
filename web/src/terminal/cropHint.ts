// FUTERMPOLISH — the one wording of "part of the grid is out of view", shared by
// the desktop bar and the phone sheet.

/** The chip. */
export const CROPPED_LABEL = "Cropped";
/** Desktop: the panel scrolls. */
export const CROPPED_DESKTOP_HINT = "The grid is larger than the panel at the smallest font: scroll the panel";
/** Phone Watch: the grid is wider than the screen until Fit to phone. */
export const CROPPED_PHONE_HINT = "Part of the grid is off screen: pan it, or Fit to phone";

/** Whether a scrolling box shows less than its content on the x axis (half a pixel of slack). */
export function isCroppedX(box: { scrollWidth: number; clientWidth: number }): boolean {
  return box.clientWidth > 0 && box.scrollWidth > box.clientWidth + 1;
}
