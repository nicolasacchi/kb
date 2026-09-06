import { Component, type ErrorInfo, type ReactNode } from "react";
import { Link } from "react-router-dom";

// F3a — route-level error boundary, ported from kb's own
// `web/src/components/ErrorBoundary.tsx` (root CLAUDE.md invariant #32). A
// render/throw in any lazy route used to blank the entire SPA (no boundary
// existed anywhere in kb-code). This catches it, keeps the app chrome
// (TopBar/Omnibox) alive, and offers a way back.
//
// `resetKey` (app.tsx passes the route path) clears a caught error when the
// route changes — but via componentDidUpdate, NOT a React `key`. Keying on
// the path would remount the whole route subtree on every in-session
// navigation, which is fine for kb-code today (no cross-nav state to
// preserve yet) but matching kb's exact mechanism keeps the two apps'
// error-recovery behavior identical if that ever changes.
interface Props {
  resetKey?: string;
  children: ReactNode;
}
interface State {
  error: Error | null;
}

export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // Recoverable in the UI; log for debugging.
    console.error("route error boundary:", error, info.componentStack);
  }

  componentDidUpdate(prev: Props) {
    if (this.state.error && prev.resetKey !== this.props.resetKey) {
      this.setState({ error: null });
    }
  }

  render() {
    if (this.state.error) {
      return (
        <div className="kbc-reader__hint kbc-reader__hint--error" data-kbc-error-boundary>
          <p>Something went wrong: {this.state.error.message || "an unexpected error crashed this view."}</p>
          <p>
            <Link to="/">← back to home</Link>
          </p>
        </div>
      );
    }
    return this.props.children;
  }
}
