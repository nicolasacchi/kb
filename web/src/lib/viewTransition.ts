// W1.gallery — progressive gallery→reader view transition.
//
// react-router's own `viewTransition` prop (<Link viewTransition> /
// useNavigate(..., { viewTransition: true })) only works under a data
// router (createBrowserRouter + <RouterProvider>) — confirmed against the
// react-router 6.30 docs. This app mounts a plain <BrowserRouter>
// (main.tsx), so that prop is unavailable here; this module hand-rolls the
// same effect with `document.startViewTransition()` instead.
//
// The reader side already gives its title a STATIC
// `view-transition-name: kb-doc-title` (chrome.css, `.kb-ctxbar__crumb b`,
// shipped by W1.reader on main — not yet in this worktree, see the
// W1.gallery brief's WIRE CAVEAT). The View Transitions API requires every
// name active at once to be unique document-wide, so the gallery can't set
// it statically on every card title (many cards render at once) — only the
// ONE title the user actually clicked gets the name, right before the
// transition starts, cleared again once it finishes (or immediately, on any
// browser/user-preference that skips the transition).
import { flushSync } from "react-dom";

export const CARD_TITLE_TRANSITION_NAME = "kb-doc-title";

type TransitionCapableDocument = Document & {
  startViewTransition?: (callback: () => void) => { finished: Promise<void> };
};

export function reducedMotionRequested(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/// Navigate via `go()`, morphing `titleEl` into the reader's title when the
/// View Transitions API is available and the user hasn't asked for reduced
/// motion; otherwise `go()` runs plain. `go` is flushed synchronously inside
/// the transition callback (`flushSync`) so the DOM mutation the browser
/// needs to snapshot actually lands before the callback returns — without
/// it React 18 may batch the update past the transition's capture point.
export function navigateWithTitleTransition(
  titleEl: HTMLElement | null,
  go: () => void,
): void {
  const doc = document as TransitionCapableDocument;
  if (!titleEl || !doc.startViewTransition || reducedMotionRequested()) {
    go();
    return;
  }
  titleEl.style.viewTransitionName = CARD_TITLE_TRANSITION_NAME;
  const transition = doc.startViewTransition(() => {
    flushSync(go);
  });
  void transition.finished
    .catch(() => {
      /* transition can be skipped/interrupted — navigation already happened */
    })
    .finally(() => {
      titleEl.style.viewTransitionName = "";
    });
}
