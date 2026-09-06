import { Component, type ErrorInfo, type ReactNode } from "react";
import EmptyState from "./EmptyState";

// F2 — route-level error boundary. A render/throw in any lazy route used to
// blank the entire SPA (no boundary existed anywhere). This catches it, keeps
// the app chrome (header / rails / SSE worker) alive, and offers a reload.
//
// `resetKey` (App passes the route path) clears a caught error when the route
// changes — but via componentDidUpdate, NOT a React `key`. Keying on the path
// would remount the whole route subtree on every in-session artifact nav,
// resetting Detail's panel/scroll state (the panels are designed to persist).
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
        <main className="body">
          <EmptyState
            title="Something went wrong"
            hint={
              this.state.error.message ||
              "An unexpected error crashed this view."
            }
            action={{
              label: "Reload",
              onClick: () => window.location.reload(),
            }}
          />
        </main>
      );
    }
    return this.props.children;
  }
}
