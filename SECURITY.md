# Security policy

kb is a self-hosted daemon. There is no multi-tenant kb.dev — every
deployment is a single process an operator runs themselves, so this policy
covers the code, not a hosted service.

## Supported versions

Security fixes target the **latest tagged release** and **`main`**. There is
no long-term-support branch; upgrade to the latest tag to pick up a fix.

## Reporting a vulnerability

**Please do not open a public issue for a security report.** Use GitHub's
private vulnerability reporting on this repository: **Security tab → Report a
vulnerability**. That opens a private advisory thread with the maintainers
before anything is public. There is no security email address — the GitHub
flow is the only reporting channel.

## Security posture

kb's default posture is **loopback-first and fail-closed**. The points below
are the load-bearing ones (full detail, with rationale, in
[docs/architecture-invariants.md](docs/architecture-invariants.md) §3–§4):

- **Loopback bypasses auth and outbound scrubbing** — but only
  for the *genuine* TCP peer. `auth_bearer` (and the other security
  middleware) treat `127.0.0.1`/`::1`, or an address listed in
  `[server] trusted_proxies`, as the trusted hop; every other request is
  admitted only with a valid bearer token.
- **`trusted_proxies` is empty by default.** A same-host reverse proxy already
  connects from loopback and needs no configuration; a proxy reaching the
  daemon from elsewhere (e.g. a container bridge) must be added explicitly or
  the loopback bypass never engages for it.
- **`X-Forwarded-For` is walked right-to-left**, skipping trusted hops, so a
  client can't forge a leading `X-Forwarded-For: 127.0.0.1` to impersonate a
  loopback origin — a real proxy appends the true client IP on the right.
- **A non-loopback bind with no token refuses to start.** If no bearer token
  is configured, the loopback bypass is the only thing standing between the
  network and destructive routes, so the daemon refuses to bind a non-loopback
  address (including `0.0.0.0`) unless `KB_ALLOW_NO_AUTH=1` is explicitly set
  — an intentional opt-out for deployments where an upstream proxy is the
  actual gate.
- **A token-less non-loopback request gets `401`**, never a silent bypass —
  enforced by the same rule at request time, not just at startup.
- **Identity is attribution, not authorization.** kb resolves *who* made a
  request (for comment ownership, per-user read state, etc.) but never
  authenticates and has no roles, ACLs, or visibility tiers — every admitted
  identity is a full co-operator with the same authority as the operator.
  Restricting *what* a deployment can see is done by controlling what a
  daemon's `kb.toml` mounts (the corpus mount is the access boundary), not by
  an in-daemon permission model.
- **kb-code's working-tree mutations are loopback-only.** The sibling
  code-browsing daemon (`kb-code-server`) reuses the same `auth_bearer` gate
  for its read routes, but anything that mutates the working tree or serves
  raw transcript text (checkout, suggestion apply, branch/ref writes, live
  transcript reads) is loopback-only regardless of token, with no
  `KB_ALLOW_NO_AUTH`-style override. Only the review-scoped mutation routes
  (comments, verdicts, dispositions on a review session) can be opened to a
  bearer caller, and only by explicitly setting `[review] remote_mutations =
  true` (default `false`).
- **Artifacts are served under a separate host suffix**
  (`[server] artifact_host_suffix`, e.g. `*.artifacts.<domain>`), isolating
  untrusted rendered HTML/JS from the daemon's own API origin so an artifact
  can't script the parent page or ride the operator's session/token.

If you find a way to defeat any of the above — bypass the loopback gate,
forge an identity, escape the artifact host isolation, or trigger a mutation
without a token — that's exactly what the private reporting channel above is
for.
