// Derive the artifact-subdomain suffix the daemon dispatches on. The
// daemon exposes the authoritative value via `/api/identity`
// (`artifact_host_suffix`); callers should pass that in. The
// `deriveArtifactHostSuffix` heuristic below is the fallback used until
// identity resolves — it works for DNS-name parents:
//
//   parent SPA host              → artifact suffix
//   ────────────────────────────   ────────────────────────────
//   localhost[:port]              → .artifacts.localhost[:port]
//   kb.example.com                → .artifacts.example.com
//   kb-research.example.com       → .artifacts.example.com
//
// The rule: drop the first label of the hostname; if the result is empty
// (a bare TLD-less hostname like `localhost`) or the hostname is an IP
// literal (no labels to strip), use the whole hostname. The port from the
// parent's URL is preserved so dev (non-443 port) keeps working.
//
// Why the heuristic isn't enough on its own: when the SPA is reached via
// a bare IP (`127.0.0.1`), there is no DNS suffix to recover from
// `window.location` — the daemon is still configured with a real suffix
// like `.artifacts.localhost`. So the IP case MUST use the authoritative
// value from identity; the heuristic only keeps the origin well-formed.

// IPv4 literal, or bracketed/colon'd IPv6 — these have no DNS labels to
// strip, so they behave like the bare-hostname (`localhost`) case.
function isIpLiteral(hostname: string): boolean {
  return /^\d{1,3}(\.\d{1,3}){3}$/.test(hostname) || hostname.includes(":");
}

export function deriveArtifactHostSuffix(loc: Location = window.location): {
  /// e.g. `.artifacts.example.com` or `.artifacts.localhost`
  suffix: string;
  /// `:4000`, or empty if the parent is on the default 80/443 port
  portSuffix: string;
} {
  const portSuffix = loc.port ? `:${loc.port}` : "";
  const hostname = loc.hostname;
  // Strip the first label for DNS names. `kb.example.com` → `example.com`.
  // `localhost` (no dot) and IP literals keep the whole hostname.
  const firstDot = hostname.indexOf(".");
  const root =
    firstDot >= 0 && !isIpLiteral(hostname)
      ? hostname.slice(firstDot + 1)
      : hostname;
  return { suffix: `.artifacts.${root}`, portSuffix };
}

/// ARTIFACT HOST GRAMMAR v2 — encode a kb name into the host-label form:
/// every `_` becomes `-` (DNS labels can't carry underscores reliably
/// across resolvers/proxies, and the grammar needs a byte both a kb name
/// and an artifact id can never itself contain: `--`). Shared by
/// `artifactOrigin` below and `lib/artifactLinks.ts`'s parser, so the two
/// directions of the grammar can never drift apart.
export function encodeKbForHost(kb: string): string {
  return kb.replace(/_/g, "-");
}

/// Build the full artifact-origin URL for the given kb + content-hash id.
/// Produces `<protocol>//<kb_enc>--<id>.artifacts.<root>[:port]` — the
/// QUALIFIED label from ARTIFACT HOST GRAMMAR v2, matching the daemon's
/// `parse_artifact_id` dispatch shape (rightmost `--` splits `kb_enc` from
/// the 12-hex `id`). `kb` is REQUIRED: every artifact lives in exactly one
/// corpus, and a bare (unqualified) origin can no longer be told apart from
/// another kb's same-id artifact — see `isOriginOfArtifact` below, the
/// reason this changed from optional to required. Pass `suffix` (the
/// daemon's authoritative `artifact_host_suffix`) when available; otherwise
/// the `window.location` heuristic is used.
export function artifactOrigin(
  id: string,
  kb: string,
  suffix?: string,
  loc: Location = window.location,
): string {
  const derived = deriveArtifactHostSuffix(loc);
  const effectiveSuffix = suffix ?? derived.suffix;
  const label = `${encodeKbForHost(kb)}--${id}`;
  return `${loc.protocol}//${label}${effectiveSuffix}${derived.portSuffix}`;
}

/// True iff `origin` matches the artifact-subdomain shape for the
/// current parent — the corpus-wide TRUST BOUNDARY ("some artifact iframe
/// of this daemon sent this"), not an attribution. Pass `suffix` (the
/// authoritative `artifact_host_suffix`) when available.
///
/// W3.P-a — every SPA postMessage handler now uses `isOriginOfArtifact`
/// below instead, because each of them belongs to ONE artifact and this
/// check cannot tell two artifact iframes apart. Kept (and unit-pinned) as
/// the boundary primitive + the contrast case the exact check is defined
/// against; reach for it only for something genuinely corpus-wide.
export function isArtifactOrigin(
  origin: string,
  suffix?: string,
  loc: Location = window.location,
): boolean {
  try {
    const url = new URL(origin);
    if (url.protocol !== loc.protocol) return false;
    if (url.port !== loc.port) return false;
    const effectiveSuffix = suffix ?? deriveArtifactHostSuffix(loc).suffix;
    return url.hostname.endsWith(effectiveSuffix);
  } catch {
    return false;
  }
}

/// True iff `origin` is the artifact-subdomain origin of EXACTLY the
/// artifact `id` in kb `kb` — string equality against
/// `artifactOrigin(id, kb, suffix)`, which is the same value we use as the
/// `targetOrigin` when posting INTO the frame. The QUALIFIED artifact id
/// lives in the HOSTNAME (`<kb_enc>--<id>.artifacts.<root>`), so this is
/// the only guard that can tell two artifact iframes apart — including two
/// DIFFERENT kbs that happen to share the same 12-hex id (a real
/// possibility now ids are minted per-kb).
///
/// `isArtifactOrigin` above is suffix-only: EVERY artifact iframe passes
/// it. That is fine for a "is this the trust boundary" gate (the
/// navigation trampoline), but it is NOT an attribution: a per-artifact
/// message handler (`kb:scroll` / `kb:reading` → the append-only history
/// table + reading progress, invariants #8 / #19) must use THIS function,
/// or a second pane's beacons get POSTed against the first pane's visit.
///
/// Invariant #7 — `suffix` is runtime config (`artifact_host_suffix`,
/// `.artifacts.<domain>` in prod, `.artifacts.localhost` in dev). Callers
/// pass the authoritative `/api/identity` value via
/// `useArtifactHostSuffix()`; nothing here is hardcoded. Origins are
/// case-insensitive in scheme + host, so both sides are lowercased before
/// comparison.
export function isOriginOfArtifact(
  origin: string,
  id: string | null | undefined,
  kb: string | null | undefined,
  suffix?: string,
  loc: Location = window.location,
): boolean {
  if (!origin || !id || !kb) return false;
  return (
    origin.toLowerCase() === artifactOrigin(id, kb, suffix, loc).toLowerCase()
  );
}

/// Cross-check the heuristic-derived artifact host suffix + parent
/// origin against the daemon's authoritative `/api/identity` values.
/// Returns human-readable mismatch warnings (empty array = all good).
///
/// This is an observability signal that a reverse proxy is mis-wired
/// (the most common self-host footgun: the proxy forwards a Host the
/// daemon's `artifact_host_suffix` config doesn't expect, so artifact
/// iframes 404). Fields the daemon didn't report (older daemon) are
/// skipped.
///
/// v0.7.1 P2/P3 — the check is SKIPPED for an IP-literal parent
/// (`127.0.0.1`, a raw IPv6). There the `window.location` heuristic
/// cannot recover a DNS suffix — `.artifacts.127.0.0.1` is meaningless —
/// so the SPA correctly adopts the daemon's authoritative value and a
/// heuristic ≠ daemon mismatch is *expected*, not a misconfiguration.
/// Warning on it gave every `127.0.0.1` user a permanent red banner even
/// though the iframe (served from `<id>.artifacts.localhost`, which
/// resolves to loopback) worked fine.
///
/// A bare hostname like `localhost` is NOT skipped: `.artifacts.localhost`
/// IS a meaningful, resolvable suffix, so a mismatch there is a real
/// signal — same as a DNS-name parent, the mis-wired-production-proxy
/// case the check exists for. (The earlier P2 fix also listed `localhost`
/// as skip-worthy; that was too broad — IP-literal is the precise gate.)
export function verifyAgainstIdentity(
  identity: {
    artifact_host_suffix?: string;
    parent_origin?: string;
    build_sha?: string;
  },
  loc: Location = window.location,
): string[] {
  const warnings: string[] = [];

  // Build-stamp drift: the daemon binary and this SPA bundle were built
  // from different commits. The most damaging case is a stale daemon
  // serving a fresh bundle — when the HTTP contract changed under it the
  // SPA can white-screen on a now-missing field. This check is independent
  // of host/proxy wiring, so it runs BEFORE the IP-literal gate below
  // (which skips the suffix/origin checks for raw-IP parents).
  //
  // We compare the BASE commit only — `build.rs` / vite append a `-dirty`
  // suffix when the tree had uncommitted changes at build time, and the
  // binary and bundle are built moments apart, so the same commit can land
  // as `<sha>` on one side and `<sha>-dirty` on the other during normal
  // local iteration (editing web/ with the tree uncommitted). That's not a
  // contract drift — only a different base commit is — so stripping the
  // suffix stops the banner from crying wolf on every dirty working tree.
  const baseSha = (s: string) => s.replace(/-dirty$/, "");
  const bundleSha =
    typeof __KB_BUILD_SHA__ === "string" ? __KB_BUILD_SHA__ : "unknown";
  if (
    identity.build_sha &&
    identity.build_sha !== "unknown" &&
    bundleSha !== "unknown" &&
    baseSha(identity.build_sha) !== baseSha(bundleSha)
  ) {
    warnings.push(
      `build mismatch: this page was built from "${bundleSha}" but the ` +
        `daemon is running "${identity.build_sha}". The API contract may ` +
        `have drifted — rebuild the binary and SPA together and restart ` +
        `the daemon (\`just redeploy <config>\`).`,
    );
  }

  if (isIpLiteral(loc.hostname)) {
    return warnings;
  }
  const { suffix } = deriveArtifactHostSuffix(loc);
  if (
    identity.artifact_host_suffix &&
    identity.artifact_host_suffix !== suffix
  ) {
    warnings.push(
      `artifact host suffix mismatch: derived "${suffix}" from ` +
        `window.location, but the daemon reports ` +
        `"${identity.artifact_host_suffix}". Artifact iframes may fail ` +
        `to load — check the reverse-proxy host configuration.`,
    );
  }
  if (identity.parent_origin && identity.parent_origin !== loc.origin) {
    warnings.push(
      `parent origin mismatch: window.location.origin is "${loc.origin}" ` +
        `but the daemon's [server] parent_origin is ` +
        `"${identity.parent_origin}".`,
    );
  }
  return warnings;
}
