// W1.mobile — pure gesture math for the inbox row's swipe-to-triage
// enhancement. Kept DOM-free (no touch/pointer types) so vitest
// (`environment: "node"`) can pin the arithmetic directly, mirroring the
// cmdkRows.ts split (geometry lives in a plain .ts sibling of the component
// that drives it, tested in a colocated *.test.ts).
//
// The gesture is an ENHANCEMENT ONLY (see InboxTriage.tsx header comment) —
// the per-row buttons are the e2e contract; swipe is never required to
// drive an action in a test.

export const SWIPE_THRESHOLD_PX = 96;
// A wild drag can't reveal the hint past "fully revealed" — clamp so the
// row never trails the finger indefinitely.
const MAX_DRAG_PX = 140;

export type SwipeDirection = "left" | "right" | null;
export type SwipeAction = "resolve" | "reply" | null;

/** Clamp the raw touch delta into `[-max, max]`. */
export function clampSwipeDelta(deltaX: number, max: number = MAX_DRAG_PX): number {
  if (deltaX > max) return max;
  if (deltaX < -max) return -max;
  return deltaX;
}

/**
 * Direction of a delta: right-swipe reveals "resolve", left-swipe reveals
 * "reply". Zero is neither (row at rest).
 */
export function swipeDirection(deltaX: number): SwipeDirection {
  if (deltaX > 0) return "right";
  if (deltaX < 0) return "left";
  return null;
}

/**
 * 0..1 "how revealed" the colored action hint under the row is, for the
 * hint layer's opacity. Saturates at 1 exactly when the drag crosses the
 * trigger threshold, so the hint reaches full strength right as a release
 * would fire the action.
 */
export function swipeHintStrength(
  deltaX: number,
  threshold: number = SWIPE_THRESHOLD_PX,
): number {
  return Math.min(1, Math.abs(deltaX) / threshold);
}

/**
 * The action a RELEASE fires, or `null` (spring back — the drag never
 * crossed the threshold in either direction). Past-threshold right =
 * resolve, past-threshold left (negative) = reply.
 */
export function resolveSwipeAction(
  deltaX: number,
  threshold: number = SWIPE_THRESHOLD_PX,
): SwipeAction {
  if (deltaX >= threshold) return "resolve";
  if (deltaX <= -threshold) return "reply";
  return null;
}
