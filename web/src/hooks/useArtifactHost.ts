import { useEffect, useState } from "react";
import { fetchIdentity, type Identity } from "../api/client";
import { deriveArtifactHostSuffix } from "../lib/artifactHost";

// Single source of truth for the daemon's `/api/identity` payload.
// Two consumers share one once-per-session fetch + cache:
//   - `useArtifactHostSuffix` — the authoritative `artifact_host_suffix`
//     the detail view uses to build iframe `src` URLs.
//   - `useIdentity` — the full payload, used by the app shell to
//     cross-check the window.location-derived values and warn on a
//     mis-wired reverse proxy.
//
// Why this matters: the `deriveArtifactHostSuffix` heuristic guesses the
// suffix from `window.location`, which is correct for DNS-name parents
// but wrong when the SPA is reached via a bare IP (`127.0.0.1` would
// yield `.artifacts.0.0.1`). The daemon always knows the real suffix.

let cached: Identity | null = null;
let inflight: Promise<Identity | null> | null = null;

function loadIdentity(): Promise<Identity | null> {
  if (cached) return Promise.resolve(cached);
  if (!inflight) {
    inflight = fetchIdentity()
      .then((id) => {
        cached = id;
        inflight = null;
        return cached;
      })
      .catch(() => {
        // v0.7.1 H11 — do NOT cache the failure. Clearing `inflight`
        // lets the next caller re-attempt. Previously `.catch(() =>
        // null)` left `inflight` a settled-to-null promise forever, so
        // a single transient first-fetch failure (the daemon mid-
        // restart, a 502 from the proxy) poisoned identity for the
        // whole session — every artifact iframe then 404'd on a bare-IP
        // parent with no recovery path.
        inflight = null;
        return null;
      });
  }
  return inflight;
}

// Bounded retries for the startup warm so a daemon that's slow to boot —
// or briefly down — still gets picked up without waiting for a consumer
// to re-mount. Each `loadIdentity()` is a fresh attempt now that the
// failure path clears `inflight` (H11). ~31 s of coverage at 1/2/4/8/16 s.
const IDENTITY_WARM_MAX_RETRIES = 5;
const IDENTITY_WARM_BACKOFF_MS = 1000;

// Warm the cache as soon as this module loads (it's pulled in by
// detail.tsx + app.tsx, imported statically — so this fires at app
// startup). By the time anyone opens a detail view the authoritative
// suffix is usually already resolved, avoiding a heuristic-then-correct
// iframe remount.
function warmIdentity(attempt = 0): void {
  void loadIdentity().then((id) => {
    if (id || attempt >= IDENTITY_WARM_MAX_RETRIES) return;
    setTimeout(
      () => warmIdentity(attempt + 1),
      IDENTITY_WARM_BACKOFF_MS * 2 ** attempt,
    );
  });
}
warmIdentity();

/// Returns the daemon's `artifact_host_suffix`. Until identity resolves
/// it returns the `window.location` heuristic — correct for DNS-name
/// hosts, well-formed (if non-functional) for IP hosts. When identity
/// resolves the consumer re-renders with the authoritative value.
export function useArtifactHostSuffix(): string {
  const [suffix, setSuffix] = useState<string>(
    () => cached?.artifact_host_suffix ?? deriveArtifactHostSuffix().suffix,
  );
  useEffect(() => {
    if (cached?.artifact_host_suffix) {
      setSuffix(cached.artifact_host_suffix);
      return;
    }
    let alive = true;
    loadIdentity().then((id) => {
      if (alive && id?.artifact_host_suffix) setSuffix(id.artifact_host_suffix);
    });
    return () => {
      alive = false;
    };
  }, []);
  return suffix;
}

/// Returns the full `/api/identity` payload, or `null` until the first
/// fetch resolves. Shares the once-per-session cache with
/// `useArtifactHostSuffix` — no second request.
export function useIdentity(): Identity | null {
  const [identity, setIdentity] = useState<Identity | null>(cached);
  useEffect(() => {
    if (cached) {
      setIdentity(cached);
      return;
    }
    let alive = true;
    loadIdentity().then((id) => {
      if (alive && id) setIdentity(id);
    });
    return () => {
      alive = false;
    };
  }, []);
  return identity;
}
