// DCB W2.B — the repo-less, PATH-addressed lens entry ramp (R2). Exists for
// a caller that genuinely only holds a path — a hand-typed/bookmarked
// human-readable URL, or any future surface that hasn't got the id handy.
// Mirrors kb's OWN `by-path` route naming (`GET
// /api/kb/{kb}/docs/by-path/{*path}`) so the grammar reads the same way on
// both sides of the bridge.
//
// This resolves SERVER-SIDE (`GET /api/doc-lens/resolve-path`, R20) through
// kb-code-server's own token-bearing `KbClient` — kb's by-path lookup lives
// on kb's daemon behind a loopback-origin-only CORS layer, so a direct
// browser fetch from web-code's own origin would work in local dev (both
// loopback) but silently fail in prod. The route this calls is same-origin
// to web-code either way (it's this daemon's own route), so a plain `fetch`
// via `resolveDocByPath` has no CORS concern.

import { useEffect, useState } from "react";
import { Navigate, useLocation, useParams } from "react-router-dom";
import { ApiError, resolveDocByPath } from "../api/client";
import { lensEntryUrl } from "../lib/docLensUrl";

type RampState =
  | { status: "loading" }
  | { status: "ok"; docId: string }
  | { status: "error"; message: string };

export default function LensEntryByPath() {
  // React Router v6 splat: the trailing path segment lives under the `"*"`
  // key. `kb`'s the single named segment ahead of `by-path/`.
  const { kb = "", "*": path = "" } = useParams<{ kb: string; "*": string }>();
  const location = useLocation();
  const [state, setState] = useState<RampState>({ status: "loading" });

  useEffect(() => {
    let cancelled = false;
    setState({ status: "loading" });
    resolveDocByPath(kb, path)
      .then((out) => {
        if (!cancelled) setState({ status: "ok", docId: out.doc_id });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        // A 404 renders the same "not linked to a code repo" / "doc
        // unknown" degrade prose the rest of the lens page already uses
        // for a vanished doc, rather than a raw fetch error.
        const message =
          err instanceof ApiError && err.status === 404
            ? "Not linked to a code repo — this path is unknown to kb-code."
            : err instanceof Error
              ? err.message
              : "Failed to resolve this path.";
        setState({ status: "error", message });
      });
    return () => {
      cancelled = true;
    };
  }, [kb, path]);

  if (state.status === "loading") {
    return <div className="kbc-reader__hint">Resolving…</div>;
  }
  if (state.status === "error") {
    return <div className="kbc-reader__hint kbc-reader__hint--error">{state.message}</div>;
  }
  // One extra hop through the id-addressed ramp above — never a second
  // copy of the repo-selection logic.
  return <Navigate replace to={lensEntryUrl(kb, state.docId) + location.search} />;
}
