// D7-prep — one place for the daemon base URL.
//
// Today every fetch is same-origin (`base = ""`). D7 (multi-daemon
// SPA) lets the SPA talk to ≥1 daemon — co-located peers via
// federation (B1) and remote daemons via true multi-origin (B2). Both
// patterns need the same primitive: a typed, mutable base URL that
// fetchers read at call time, plus a setter for the boot path to wire
// from `/api/identity`, daemons.toml, or a `?daemon=` query param.
//
// This module ships the primitive without committing to a wiring
// strategy. `currentDaemonBase()` defaults to `""` — bit-identical to
// the pre-D7-prep code. The D7 implementation round adds the boot
// path that calls `setDaemonBase(...)` once at startup (and ideally
// reactively, so daemon-switch from the UI updates every subsequent
// fetch).
//
// See `~/project/kb/docs/research/multi-daemon-architecture.html`
// (forthcoming) for the architecture decisions this primitive
// supports.

let _daemonBase = "";

/// The current daemon's HTTP origin (`https://kb.example.com`, `http://laptop.local:4000`,
/// etc.) WITHOUT a trailing slash, or `""` for same-origin. Read by
/// every fetcher in `api/`.
export function currentDaemonBase(): string {
  return _daemonBase;
}

/// Set the daemon base URL. Strips a trailing slash so callers can
/// concat `${base}/api/...` without doubling. Idempotent.
///
/// Pass `""` to revert to same-origin (e.g. for a "this daemon"
/// switch back). The fetcher modules don't memoise the base across
/// calls — they read on each request — so a switch takes effect
/// immediately.
export function setDaemonBase(next: string): void {
  _daemonBase = next.replace(/\/+$/, "");
}
