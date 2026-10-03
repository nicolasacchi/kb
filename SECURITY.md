# Security policy

kb is a self-hosted daemon. There is no multi-tenant kb.dev — every
deployment is a single process an operator runs themselves, so this policy
covers the code, not a hosted service.

## Supported versions

Security fixes target the **latest tagged release** and **`main`**. There is
no long-term-support branch; upgrade to the latest tag to pick up a fix.

A fix is released as a new `v*` tag within 72 hours of landing on `main`
(the cadence is recorded in [packaging/README.md](packaging/README.md)).
Release tarballs and container images carry build-provenance attestations;
see [Verifying a release](docs/packaging.md#verifying-a-release).

## Reporting a vulnerability

**Please do not open a public issue for a security report.** Use GitHub's
private vulnerability reporting on this repository: **Security tab → Report a
vulnerability**. That opens a private advisory thread with the maintainers
before anything is public. There is no security email address — the GitHub
flow is the only reporting channel.

## The boundary, in one table

kb has **one trust tier**: identity is attribution, not authorization. Every
request kb admits is a full co-operator. There are no roles, no read-only
tokens, no per-corpus ACLs and no public mode, and none are planned: *the
corpus mount is the access boundary* (what a daemon's `kb.toml` mounts is what
it can show), and anything that must cross a trust boundary goes through
`kb share`, which scrubs.

"Loopback peer" means the **raw TCP peer** is `127.0.0.1`/`::1` (or a peer
listed in `[server] trusted_proxies`); `X-Forwarded-For` is never believed
about the peer itself.

| Surface | Who can reach it | Auth | Notes |
|---|---|---|---|
| `/api/*` (kb daemon) | Loopback peer; any other peer that presents a token | `auth_bearer`: loopback peer is the operator; otherwise a registry token or the shared token; a token-less non-loopback request is `401` | Behind the `Host` guard (see below). A non-loopback bind with no token refuses to start unless `KB_ALLOW_NO_AUTH=1`. |
| `/capture` (web share target) | Same as `/api` | `auth_bearer` + `Host` guard | A corpus write that lives outside the `/api` nest because browsers post to a fixed path. |
| `/healthz` | Anyone who can connect, on every host | None | Liveness only; carries no corpus data. |
| `/metrics` (Prometheus) and `/api/metrics` | As `/api` | `auth_bearer` + `Host` guard | Coarse counters by default; detailed tables need `[server] metrics = true`. |
| Artifact hosts `<id>.artifacts.<suffix>` | **Whoever can resolve and reach the host** | **None.** They sit outside `auth_bearer` by design | Serve sandboxed HTML for the SPA's iframes. Non-loopback requests get the outbound scrub. See "Artifact hosts" below. |
| `?cm=on` annotator payload | Same as artifact hosts | None | Inlines the comment sidecar for the annotator. Private notes are stripped structurally and the ETag is computed over the filtered body. |
| Live transcript routes (`/api/sessions/presence`, `/api/sessions/{id}/live`) | Loopback peer only | Loopback, checked in the handler; no override | Serve unscrubbed mid-flight transcript bytes. `live-status` and the slate routes ride `auth_bearer` with a forced scrub floor. |
| kb-code working-tree and ref mutations (checkout, suggestion apply, branch/ref writes), raw transcript text | Loopback peer only | Loopback; no `KB_ALLOW_NO_AUTH`-style override | Review-scoped mutations (comments, verdicts, dispositions) open to a bearer caller only with `[review] remote_mutations = true` (default `false`). |
| kb-code `/api/*` (reads) | As the kb daemon | `auth_bearer` + its own `Host` allowlist | Loopback peers are always `Host`-checked; other peers once `hostnames` is non-empty. |
| Webhooks (`[webhooks]`) | Outbound only | n/a | Adds no inbound surface. Destinations are policy-filtered and the dialled IPs pinned per POST; redirects are disabled. |

### The `Host` guard (DNS rebinding)

A browser always sends `Host`, and that is the one request header a
DNS-rebinding page controls. Without a check, a page at
`http://evil.example:4000` whose DNS flips to `127.0.0.1` arrives from a
loopback peer and would inherit the loopback bypass. kb refuses a request
whose `Host` is not a loopback label, the host of `parent_origin`, an entry in
`[server] hostnames`, or the host literal of `addr`. Enforcement is decided on
the raw TCP peer:

- a **loopback peer or a `trusted_proxies` peer is always checked**, with no
  configuration;
- any **other peer is checked only once `[server] hostnames` is non-empty**.
  This is deliberate (an upgrade must not 403 a deploy that never listed its
  proxy), and it means **a reverse-proxied or LAN deployment is not protected
  by this check until you set `hostnames`**. Set it.

This guard first shipped after v0.43. v0.43 and earlier are affected.

### Artifact hosts

Artifact hosts are unauthenticated and an artifact id is
`sha256(source-relative path)[:12]` with no secret key, so the id of any
common path (`index.html`, `README.md`) is computable by anyone. Treat
`*.artifacts.<domain>` as readable by whoever can reach it:
**gate it at your edge** (the same identity-aware proxy or network allowlist
as the rest of the deployment). Do not rely on the id being unguessable.

### Posture details

The points below are the load-bearing ones (full detail, with rationale, in
[docs/architecture-invariants.md](docs/architecture-invariants.md) §3-§4):

- **Loopback bypasses auth and outbound scrubbing**, but only for the
  *genuine* TCP peer.
- **`trusted_proxies` is empty by default.** A same-host reverse proxy already
  connects from loopback and needs no configuration; a proxy reaching the
  daemon from elsewhere (e.g. a container bridge) must be listed explicitly.
  Entries may be IPs or DNS names (re-resolved every 30 s; a name stops being
  trusted after about 5 minutes of failed resolution).
- **`X-Forwarded-For` is walked right-to-left**, skipping trusted hops, so a
  client cannot forge a leading `127.0.0.1`.
- **A non-loopback bind with no token refuses to start**, and a token-less
  non-loopback request gets `401` at request time too.
- **Identity is attribution, not authorization.** Restricting what a
  deployment can see is done by what its `kb.toml` mounts, not by an
  in-daemon permission model.
- **Artifacts are served under a separate host suffix**
  (`[server] artifact_host_suffix`) so untrusted rendered HTML/JS cannot
  script the daemon's own API origin. Artifact-iframe `Host`s are refused on
  `/api`, `/capture` and `/metrics`.

## Known open edges

These are public, tracked, and not yet closed. The adversarial review that
found most of them is public:
[docs/research/kb-adversarial-review-2026-09-30.md](docs/research/kb-adversarial-review-2026-09-30.md)
(see also [kb-week-review-2026-09-28.md](docs/research/kb-week-review-2026-09-28.md)).

- **`hostnames` is fail-open for non-loopback peers until it is set** (see
  above). A boot warning fires when it is empty and the bind is non-loopback,
  `trusted_proxies` is set, or `KB_ALLOW_NO_AUTH=1`.
- **HTTP/2 `:authority`.** The guard reads `Host`, or the request-URI
  authority when `Host` is absent. Whether every HTTP/2 front-end presents a
  consistent value is not proven for all proxies; terminate with one that
  rewrites `Host` to the name you publish.
- **Artifact-id predictability** and the unauthenticated artifact hosts (above).
  Edge gating is a requirement, not an option.
- **Transcripts are scrubbed for secrets only** (see "Data at rest"): personal
  data in a transcript is stored as written.

## Data at rest

What kb keeps on the box that runs it. Nothing here leaves the box unless you
configure `[webhooks]`, a backup `remote_cmd`, or run `kb share`.

| What | Where | Scrubbed? | How to remove it |
|---|---|---|---|
| Your corpus files (HTML/Markdown) | The source directories you mount | n/a (they are your files) | Delete the file; `kb exclude` hides one from the index |
| The search index (text, chunks, embeddings) | `<state>/` per kb (LanceDB + SQLite) | No: it indexes your files as they are | `kb reset --kb NAME` (daemon stopped, or `--force`) |
| Captured agent transcripts (sessions corpus) | The sessions corpus directory, as artifacts | **Secrets only**, at capture time (`kb sessions scrub`); not PII | `kb sessions rescrub --apply` re-scrubs old captures; delete the artifact file to remove one |
| Subagent sidecar text and resolved commit subjects | Inside the same capture artifact | Secrets scrubbed at capture; commit subjects too | As above |
| Reading history and progress | SQLite in `<state>/` | No | `kb reset` (wipes index state, including history) |
| Zero-hit search queries | In the daemon's memory (a bounded ring) | No | Restart the daemon |
| Memory recall ledger (`memory_recalls`) | SQLite in `<state>/` | No | `kb reset`; `kb forget --purge` removes a memory itself |
| Comments and notes | `.review/` sidecars next to the corpus | No | Delete the comment; `kb reset --all` removes sidecars too |
| Backups | `<state>/exports/` and any `remote_dest` | Not separately scrubbed: a backup holds the data listed above | Delete the tarball; retention is `[backup] keep_exports` |
| Daemon logs | `<state>/log/` | Not a scrub target: the daemon's own diagnostics | Deleted after `log_retention_days` (default 14) |

Outbound scrubbing (`[outbound]`) applies when an artifact **leaves** the
daemon to a non-loopback caller, and `kb share` always strips the
`kb-prompt` template. It is not an at-rest control.

If you find a way to defeat any of the above (bypass the loopback gate, forge
an identity, escape the artifact-host isolation, or trigger a mutation without
a token), that is exactly what the private reporting channel above is for.
