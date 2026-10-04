# Public mirror recipe

kb has no public mode, no ACLs and no read-only token (see
[README Non-goals](../README.md#non-goals)): every federated read fans out over
every mounted corpus, and every authenticated caller is a full co-operator. A
public, read-only mirror of some documents is therefore a **deployment
recipe**: a *dedicated* daemon that mounts only public corpora, behind an edge
(reverse proxy) that allowlists a handful of read routes. This page is that
recipe. The daemon gains nothing; the edge is the only gate.

The allowlist lives in [`public-mirror/allowlist.txt`](public-mirror/allowlist.txt).
A test (`crates/kb-server/tests/public_mirror_allowlist.rs`) pins every rule to
a real route in [`api-routes.md`](api-routes.md), rejects private route
families, and checks that the Traefik and Caddy snippets below are exactly the
ones derived from the allowlist. Regenerating the route table or editing a
snippet by hand fails CI.

## Architecture

- **One daemon, own `kb.toml`, own state dir.** Never reuse the operator's
  daemon: its `/api/events`, `/api/config`, memories and sessions would sit one
  misconfigured rule away from the internet.
- **Mount only public corpora**, read-only (`:ro` bind mount is recommended;
  verify the daemon boots on your setup). Do not mount capture, comments,
  notes, memory or session corpora, and no corpus with `memory-*` categories.
- **Bind loopback or a private network** and publish only through the edge.
  A non-loopback bind with no token is refused unless `KB_ALLOW_NO_AUTH=1`
  (invariant #4). Setting it is acceptable here only because the edge is the
  gate and the daemon is unreachable except through it; never set it on a
  daemon that is directly exposed.
- **Host allowlist.** Set `[server] hostnames` to the public names so a
  DNS-rebinding page cannot reach the daemon with a forged `Host`
  ([configuration.md](configuration.md#server)). Set `parent_origin` to the
  public SPA origin and `artifact_host_suffix` to `.artifacts.example.com`
  (wildcard DNS + certificate for `*.artifacts.example.com`).
- **Scrub the kb-prompt.** Every mirrored kb needs
  `[kb.<name>.outbound] strip_kb_prompt = true`. The scrub is applied on the
  artifact serve path only when that flag is set; a mirror kb without it
  serves the kb-prompt template verbatim. The scrub also only engages for a
  request that looks non-loopback: a loopback peer with no `X-Forwarded-For`
  is treated as local and is NOT scrubbed. The edge must therefore forward
  `X-Forwarded-For` (Traefik and Caddy both add it by default), and
  `strip_kb_prompt` does nothing for a direct loopback fetch of the daemon
  (run the prompt check below through the edge, never against `127.0.0.1`).
- **Neutral mount path.** `GET /api/kbs` returns each kb's `path`, and the
  docs list, `docs/{id}` and `by-path` responses carry the canonical absolute
  `path` of each document, so the allowlist exposes the daemon's filesystem
  layout. Mount the corpus at a neutral path such as `/srv/public-docs`
  (inside the container, if you use one), never under a home directory or a
  path that names people, clients or hosts.

```toml
[daemon]
name = "public-mirror"

[server]
addr = "127.0.0.1:4000"
parent_origin = "https://docs.example.com"
artifact_host_suffix = ".artifacts.example.com"
hostnames = ["docs.example.com"]

[kb.public-docs]
path = "/srv/public-docs"

[kb.public-docs.outbound]
strip_kb_prompt = true
```

## Check the config

`kb --config mirror.toml doctor --public-mirror` lints the config file against
the posture above. It reads the file only (no daemon, no network), prints one
line per finding and exits 1 when there is any, 0 when clean (`--json` emits
`{ok, findings}`). It cannot see the edge rules, the `KB_ALLOW_NO_AUTH`
environment or what the mounted directories contain, so the checklist below
still applies. The sample config above passes its own check (pinned by a
test that extracts it from this page). The logic is the pure function
`kb_core::public_mirror::public_mirror_findings`.

| Rule | Fires when |
|---|---|
| `no-corpus` | no `[kb.*]` is configured |
| `strip-kb-prompt` | a kb lacks `[kb.<name>.outbound] strip_kb_prompt = true` |
| `hostnames-unset` | `[server] hostnames` is empty |
| `artifact-origin-default` | `parent_origin` or `artifact_host_suffix` is still the local-dev default |
| `bind-not-private` | `[server] addr` is not loopback or a private address |
| `private-corpus` | a kb is named `memory*`/`session*`, or sets `default_search_category` to such a value, `memory_scope` or `decay_policy` |
| `capture-lane` | a kb sets `capture_dir` |
| `sessions-live-dir` | `[sessions] live_transcripts_dir` is set |
| `webhook-egress` | `[webhooks]` has a URL |
| `home-path` | a kb path is under `/home`, `/Users` or `/root` |

## Profiles

- **minimal**: the edge forwards only `GET /healthz`, the artifact wildcard
  host and the SPA shell. Artifacts render in their sandboxed iframes through
  the daemon's host-dispatching fallback; no `/api` route is reachable.
- **browse** (the allowlist file): minimal plus search, the document list,
  facets, folders, tags and artifact bytes. Everything else under `/api` is
  denied.

The SPA calls routes the mirror denies (`/api/events`, `/api/identity`,
`/api/config`, comment panels, memory and session views). It degrades by
design: expect 403s in the browser console and missing panels. Use the SPA as
a browser of the public corpus, not as a full client.

## Edge rules

Both snippets are derived from `allowlist.txt` (path templates with `{x}`
becoming `[^/]+` and `{*x}` becoming `.+`). Everything under `/api`,
`/capture` and `/metrics` that does not match is denied; the SPA shell and the
artifact hosts pass through. Allow only `GET` and `HEAD`.

Traefik (dynamic configuration). The `noop@internal` service alone answers
418, so the deny router carries the `mirror-forbid` middleware: an
`ipAllowList` whose only range is a TEST-NET address no client can have,
which answers 403 for every real caller (Traefik v3 `ipAllowList`; on v2 use
`ipWhiteList`). Denied requests are 403 on both edges, except that Caddy
answers 405 for a non-GET/HEAD method outside `/api`; the exact status per
edge is listed in the checklist. Neither status is verified in CI (no edge
runs in the test suite), so confirm them with the checklist after deploy.

```traefik
http:
  routers:
    mirror-read:
      priority: 100
      service: kb-mirror
      rule: >-
        Host(`docs.example.com`) && Method(`GET`, `HEAD`) && (
        PathRegexp(`^/api/kb/[^/]+/artifact/[^/]+$`) ||
        PathRegexp(`^/api/kb/[^/]+/docs$`) ||
        PathRegexp(`^/api/kb/[^/]+/docs/by-path/.+$`) ||
        PathRegexp(`^/api/kb/[^/]+/docs/[^/]+$`) ||
        PathRegexp(`^/api/kb/[^/]+/facets$`) ||
        PathRegexp(`^/api/kb/[^/]+/folders$`) ||
        PathRegexp(`^/api/kb/[^/]+/tags$`) ||
        Path(`/api/kbs`) ||
        Path(`/api/search`) ||
        Path(`/healthz`))
    mirror-deny:
      priority: 90
      service: noop@internal
      middlewares:
        - mirror-forbid
      rule: Host(`docs.example.com`) && (PathPrefix(`/api`) || PathPrefix(`/capture`) || PathPrefix(`/metrics`) || !Method(`GET`, `HEAD`))
    mirror-shell:
      priority: 10
      service: kb-mirror
      rule: Host(`docs.example.com`) || HostRegexp(`^[a-z0-9-]+\.artifacts\.example\.com$`)
  middlewares:
    mirror-forbid:
      ipAllowList:
        sourceRange:
          - 192.0.2.1/32
  services:
    kb-mirror:
      loadBalancer:
        servers:
          - url: http://127.0.0.1:4000
```

Caddy:

```caddy
docs.example.com, *.artifacts.example.com {
    @mirror_read {
        method GET HEAD
        path_regexp mirror_read ^(?:/api/kb/[^/]+/artifact/[^/]+|/api/kb/[^/]+/docs|/api/kb/[^/]+/docs/by-path/.+|/api/kb/[^/]+/docs/[^/]+|/api/kb/[^/]+/facets|/api/kb/[^/]+/folders|/api/kb/[^/]+/tags|/api/kbs|/api/search|/healthz)$
    }
    handle @mirror_read {
        reverse_proxy 127.0.0.1:4000
    }
    @mirror_deny {
        path /api /api/* /capture /capture/* /metrics /metrics/*
    }
    handle @mirror_deny {
        respond 403
    }
    @mirror_write {
        not method GET HEAD
    }
    handle @mirror_write {
        respond 405
    }
    handle {
        reverse_proxy 127.0.0.1:4000
    }
}
```

## Threats

| Threat | Why | Mitigation |
|---|---|---|
| `GET /api/events` | SSE stream of every event (comments, sessions, indexing) | Not in the allowlist; test rejects `/events` |
| `GET /api/artifacts/.../prompt`, `/review*`, `/notes*`, `/memor*`, `/sessions*` | Private state and the kb-prompt template | Not in the allowlist; test rejects these families |
| `GET /api/config`, `/identity`, `/metrics` | Daemon configuration and operator identity | Denied; `/metrics` and `/api` also refused on artifact hosts by the daemon |
| Mutating routes | Daemon trusts any caller that reaches it | Edge allows GET/HEAD only; allowlist is GET-only |
| DNS rebinding | A page rebinds its name to the loopback daemon and gets operator authority | `[server] hostnames` set; edge matches `Host` |
| kb-prompt leak via artifact bytes | Scrub only runs with `strip_kb_prompt = true` | Set it on every mirrored kb |
| `GET /api/kbs`, docs list, `docs/{id}`, `by-path` | Return absolute server paths (`path`), exposing the daemon's filesystem layout | Mount at a neutral path such as `/srv/public-docs`; mount public corpora exclusively |
| Direct loopback fetch of the daemon | No `X-Forwarded-For`, so the request counts as local and `strip_kb_prompt` does not scrub | Publish only through the edge; keep the edge forwarding `X-Forwarded-For` |

## Verification checklist

Run against the public name after every deploy (expected status in brackets; 403 is what both reference edges are written to return, 405 is Caddy's answer to a non-GET/HEAD method):

```sh
curl -si https://docs.example.com/healthz | head -1                 # 200
curl -si 'https://docs.example.com/api/search?q=test' | head -1     # 200
curl -si https://docs.example.com/api/events | head -1              # 403
curl -si https://docs.example.com/api/config | head -1              # 403
curl -si https://docs.example.com/api/identity | head -1            # 403
curl -si https://docs.example.com/api/sessions | head -1            # 403
curl -si -X POST https://docs.example.com/api/kb/public-docs/capture | head -1  # 403 (Traefik), 403 or 405 (Caddy)
curl -si -H 'Host: evil.example' http://127.0.0.1:4000/api/kbs | head -1        # 403 (Host guard)
# a served artifact must not contain the prompt template:
curl -s "https://docs.example.com/api/kb/public-docs/artifact/<id>" | grep -c 'kb-prompt'   # 0
```

## What the mirror does not give you

No per-user anything, no ACL, no rate-limit tiers beyond the daemon's own, no
comment or review UI (those routes are denied by design), and no protection if
a private document is placed in a mounted corpus. The corpus mount is the ACL.
