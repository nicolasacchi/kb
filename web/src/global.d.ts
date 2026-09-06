/// <reference types="vite/client" />

// Git commit this SPA bundle was built from, injected at build time by
// `vite.config.ts` (`define`). Compared against the daemon's
// `/api/identity` `build_sha` to detect a stale-daemon/fresh-bundle drift.
// `"unknown"` when the build ran outside a git checkout.
declare const __KB_BUILD_SHA__: string;
