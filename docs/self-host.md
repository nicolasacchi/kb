# Self-host kb (v0.4+)

Production deployment guide. v0.4 added bearer-token auth + rate
limiting on `/api/*` (with a loopback bypass so local dev is
unaffected) and mDNS daemon advertisement. v0.6 made the artifact
host suffix + parent origin configurable so the daemon can be
fronted on any domain. The kb-server itself stays plain HTTP — a
reverse proxy in front (Traefik in this guide) terminates TLS for
`*.artifacts.<domain>` plus the parent SPA origin.

## Platforms (Linux · WSL2)

kb runs on Linux and Windows via WSL2 (a real Linux kernel). Native Windows
is not supported — use WSL2.

- **ONNX Runtime is statically bundled** into `kb-embedder` (no system
  onnxruntime needed at runtime — the binary is self-contained, no
  `ORT_DYLIB_PATH`). Binaries are fetched from a CDN at build time; for
  offline/air-gapped builds set **both** `ORT_STRATEGY=system` and
  `ORT_LIB_LOCATION=<dir>` to link a local ONNX Runtime. Only `kb-embedder`
  links it; `kb` and `kb-server` carry no ORT.
- **Paths** default to platform-native dirs. Override on any OS with `KB_HOME`
  (→ `<home>/{state,config,cache}`) or the per-dir `KB_STATE_DIR`/`KB_CONFIG_DIR`/
  `KB_CACHE_DIR` (handy for containers; `XDG_*` only applies on Linux).
- **WSL2 file watching:** native inotify does **not** fire for corpora on Windows
  drives (`/mnt/c/...`, DrvFs/9p). Either keep the corpus in the Linux filesystem
  (e.g. `~/corpus`), or set `[indexer] watch_mode = "poll"`. The periodic
  `reconcile` pass is the correctness backstop regardless — it re-walks on
  `reconcile_secs` (default 60s); lower it for faster pickup on `/mnt`.

> **⚠️ Never point two daemon processes at the same state dir.** The
> storage actor's single-writer guarantee (kb-core invariant #2) is
> **per-process only** — sqlite is opened in WAL mode with a 5s
> `busy_timeout` and lance has no advisory/cross-process lock at all.
> There is **no flock or cross-process guard** on `KB_HOME` (or the
> per-kb `<state>/<kb>/{index.db,lance/}`). Two daemons (or two
> machines) writing the same corpus state — including via Syncthing,
> NFS, SMB, or an iCloud/Dropbox-synced folder — **will** corrupt
> `index.db` and/or the `lance/` dataset; sqlite WAL semantics assume a
> single local writer, and lance's optimistic-commit protocol is not
> designed to arbitrate across independent OS processes on different
> hosts. **One physical state dir, one daemon, always.** If you need
> access from a second machine, run **one daemon** and reach it over
> the network as a remote bearer-token client (see [Bearer-token
> auth](#bearer-token-auth-v04) below) — do **not** sync or
> network-mount the state dir itself. See
> [docs/multi-machine.md](multi-machine.md) for the supported
> multi-machine topology.

## Threat model

- **Personal/local (v0.3 default):** daemon binds `127.0.0.1:4000`,
  no token written, no proxy. Loopback bypass means the existing
  CLI/SPA flow is unchanged.
- **Self-host/team (v0.4+):** daemon binds an internal address the
  proxy can reach (the docker bridge `172.17.0.1:4000` for a
  Dockerised Traefik on the same host, or `127.0.0.1` for a
  host-installed proxy). Traefik terminates TLS on the public IP and
  every `/api/*` request needs `Authorization: Bearer <token>` (one
  shared token in v0.4; per-kb ACLs ship later). Comments, atlas
  recompute, and search are rate-limited per token.

The artifact subdomains (`<id>.artifacts.<domain>`) intentionally stay
auth-free — they serve sandboxed HTML that is meant to be embedded in
iframes by the SPA. The `outbound` scrubbing layer (v0.3 G3) runs on
non-loopback requests to those subdomains, so any prompt or PII can be
stripped before bytes leave the box.

### Fail-closed on a token-less public bind

The daemon **refuses to start** if it is told to bind a **non-loopback**
address (anything other than `127.0.0.1` / `::1` — including the
catch-all `0.0.0.0`) while **no bearer token** is configured. A token-less
public bind would otherwise expose every `/api/*` verb — including the
destructive `DELETE /api/kb` — to the whole network with no
authentication, which is the single worst misconfiguration for this
daemon. The boot fails with an explicit message naming the token file.

To run a public bind you therefore do **one** of:

- **Configure a token** (recommended for a direct bind): `kb token generate`
  writes `~/.config/kb/token` (mode 0600); restart the daemon. Off-loopback
  requests then need `Authorization: Bearer <token>`.
- **Set `KB_ALLOW_NO_AUTH=1`** when an **upstream proxy is the
  authentication gate** (Authelia, oauth2-proxy, …) and you deliberately
  run kb token-less behind it. This is a loud, explicit opt-out — without
  it the daemon will not bind a public address unauthenticated.

The same rule is enforced a second time at request time: even on a
loopback bind (the reverse-proxy-on-the-same-host case), a request whose
genuine client resolves to a **non-loopback** address — via the
`trusted_proxies` + `X-Forwarded-For` walk below — is rejected `401` when
no token is configured and `KB_ALLOW_NO_AUTH` is unset, rather than
silently bypassing. (The `kb.example.com` reference deploy binds `0.0.0.0`
*and* mounts a token, so neither gate fires there.)

### Artifact bytes are non-executable on the API origin

`GET /api/kb/{kb}/artifact/{id}` returns artifact source bytes inline on
the **parent (trusted SPA) origin** — for `kb get --format html`, the SPA
download control (`?download=1`), and `kb pull`. Interactive artifacts are
meant to render only on the isolated `<id>.artifacts.<suffix>` **subdomain**,
so this main-origin endpoint hardens the bytes against ever executing in
the trusted origin: it always sends `X-Content-Type-Options: nosniff` and
`Content-Security-Policy: sandbox` (a unique opaque origin with scripts,
forms, and same-origin access disabled). Header-blind byte consumers (the
CLI) and the save-to-disk download path are unaffected.

## v0.6 [server] block — host suffix + parent origin

Production daemons need two extra fields in `kb.toml`:

```toml
[server]
addr                 = "172.17.0.1:4000"           # reachable from the proxy
artifact_host_suffix = ".artifacts.example.com"    # production wildcard
parent_origin        = "https://kb.example.com"    # parent SPA origin
trusted_proxies      = ["172.17.0.1"]              # see below — v0.7.1
```

Defaults preserve the old behavior (`.artifacts.localhost` /
`http://localhost:4000`, empty `trusted_proxies`), so local dev + tests
are unchanged. The suffix flows two places:

- **dispatcher** — `Host: <id>.artifacts.example.com` routes to the
  artifact serve handler, anything else falls through to the SPA.
- **origin allowlist** — POSTs from `https://*.artifacts.example.com`
  are accepted as same-iframe origin. The same-origin (Origin == Host)
  path keeps working on its own; this gate handles cross-origin
  POSTs from an iframe back to the parent.

Reads carry **loopback-only CORS** (SW3): GETs on `/api/*` answer
`Access-Control-Allow-Origin` only when the requesting page's origin is
`http(s)://localhost`, `127.0.0.1`, or `[::1]` (any port). That's what
lets the SPA served by one local daemon stream `/api/events` from the
other daemons in a multi-daemon fleet (DaemonsManager URLs on other
ports). It is intentionally NOT configurable and never matches a
non-loopback origin — a drive-by website cannot read your local API; on
a public deployment behind TLS the SPA and daemon share one origin and
CORS never engages. Writes are untouched (same-origin + the allowlist
above).

The `parent_origin` field replaces the hardcoded
`http://localhost:4000` whitelist entry — set it to the URL the
browser sees the SPA at.

Setting it to a non-default value **also enables artifact-subdomain
lockdown**: the probe + cross-artifact trampoline postMessage to that
specific origin (not `'*'`), and every artifact response carries
`Content-Security-Policy: frame-ancestors <parent_origin>;` so only
the configured SPA can iframe artifacts. The default
`http://localhost:4000` is treated as "not configured for production"
and keeps the permissive `'*'` so the common dev flows (SPA at
`127.0.0.1`, vite proxying on `:4738`) work without silent drops.

### trusted_proxies (v0.7.1)

`trusted_proxies` lists the source IP(s) of the reverse proxy in front
of the daemon. It controls when `X-Forwarded-For` is believed for the
auth / rate-limit / outbound-scrub loopback bypass:

- **Empty (default).** No proxy is assumed. The daemon trusts the TCP
  peer IP directly: loopback peers bypass (the local CLI/SPA), all
  others are enforced. A **same-host** proxy still works without
  configuration — it connects from loopback, which is always treated as
  a trusted hop, and a real proxy appends the genuine client IP to
  `X-Forwarded-For`, which the daemon reads.
- **Populated.** List the proxy's IP when it is **not** on loopback —
  e.g. a Dockerised Traefik reaching a host daemon over the bridge
  network (`172.17.0.1`). Only then is that peer treated as a trusted
  hop whose `X-Forwarded-For` is consulted.

Either way the header is walked **right-to-left**, skipping loopback +
listed-proxy entries, so the genuine client is whatever the trusted
proxy appended — a client cannot forge a leftmost
`X-Forwarded-For: 127.0.0.1` to claim a loopback origin and skip auth.
Entries that don't parse as an IP are dropped with a warn log at boot.

**Verification (v0.7).** The SPA derives the artifact host suffix from
`window.location` and cross-checks it against the daemon's
authoritative `artifact_host_suffix` + `parent_origin` from
`/api/identity`. A mismatch — the classic symptom of a reverse proxy
forwarding a `Host` the daemon's config doesn't expect — surfaces as a
console warning plus a banner across the top of the SPA. The
derivation still drives the runtime; the banner is a loud "your proxy
is mis-wired" signal, not a hard failure. If you see it, the two
`[server]` fields above don't match what the browser actually loads.

## Editing config from the web (CE)

The daemon loads `kb.toml` from an explicit `--config <file>` or the
home-local default `~/.config/kb/kb.toml`. Either way it remembers the
path, and the SPA's **Settings → Config** tab edits that file in place:

- `GET /api/config` returns the running config; `PUT /api/config`
  validates it, writes it back to the **same file** the daemon loaded
  (via a byte-preserving splice — your comments + key order survive, and
  defaults you omitted aren't materialised), then **restarts the daemon
  in-process** so every field takes effect. No supervisor needed; the pid
  file + state dir are untouched (no re-fork).
- **Last-good rollback.** If the edited config fails to boot — most often
  an unbindable `[server] addr` — the daemon reverts to the previously-
  serving config and stays up, logging the failure. A changed addr is
  also test-bound at save time and rejected (400) before it can reach
  disk, so a typo can't strand the daemon.
- **Two fields can't change this way:** `[daemon] name` (it pins the
  state dir + pid file for the process's lifetime — rejected with a hint
  to `kb daemon stop` + restart), and secret *values* (the `[share]`
  editor edits the env-var **names** like `KB_CF_API_TOKEN`; the daemon
  reads the value from its environment — see "Embedding model" and the
  share docs). Changing `[server] addr` moves the daemon to a new socket;
  the web UI warns and links you to the new origin.
- The endpoint inherits the same auth posture as the rest of `/api/*`
  (loopback bypass; bearer token off-loopback), so it's as privileged as
  `POST /api/shutdown`. Behind a proxy, only expose it to trusted
  operators.

## Docker

The published image (`Dockerfile` at the repo root) is a multi-stage
build: a `rust:1.96-trixie` builder produces the `kb` binary + the
`kb-embedder` sidecar, a `node:22` stage builds the React SPA, then a
`debian:trixie-slim` runtime ships the binaries at `/usr/local/bin/` and
the SPA bundle at `/usr/local/share/kb/web/dist`. `KB_SPA_DIST` points
the daemon at that bundle, so the full web UI serves out of the box — no
`web/dist` mount or in-container rebuild needed. (Pass
`--build-arg KB_GIT_SHA=$(git rev-parse --short HEAD)` so the binary and
SPA carry the same build stamp and the drift banner stays quiet.)

As of v0.14 (track D) the image **bakes in the bge-large-en-v1.5
model**, so semantic and hybrid search work without a first-run
download and benefit from the bake-off's Recall@1 win. ONNX Runtime is
**statically linked into `kb-embedder` at build time** (fastembed's
`ort-download-binaries`; `ort-sys` fetches a known-good ORT from a CDN
during `cargo build`), so the runtime stage carries no `libonnxruntime`
and sets no `ORT_DYLIB_PATH` — it ships only `libgomp1` (ONNX Runtime's
CPU provider links OpenMP). The builder pre-fetches the model into the
image cache. For an offline/air-gapped Docker build, pass
`ORT_STRATEGY=system` + `ORT_LIB_LOCATION=<dir>` into the builder so
`ort-sys` links a local ONNX Runtime instead of reaching the CDN. Cost:
the image is ~1.7 GB (mostly the ~1.34 GB bge-large model). To bake a smaller model instead —
e.g. ship a bge-small image for a memory-constrained deployment —
edit the `kb model download bge-large-en-v1.5` line in `Dockerfile`
to name the model you want, and adjust the kb.toml `[defaults]
embedding_model` accordingly.

```bash
docker build -t kb:latest .
docker run -d --name kb \
  -p 4000:4000 \
  -v /srv/kb/config:/var/lib/kb/config:ro \
  -v /srv/kb/state:/var/lib/kb/state \
  -v /srv/artifacts:/corpus:ro \
  kb:latest
```

Mount a `kb.toml` under `/var/lib/kb/config/kb/` whose `[kb.<name>]`
sections point at corpus paths visible inside the container (e.g.
`/corpus`). Either set `[defaults] embedding_model = "bge-large-en-v1.5"`
once to apply the baked-in model to every kb, or pin per-kb via
`[kb.<name>].embedding_model = "bge-large-en-v1.5"`. `kb model list`
inside the container reports the resolved daemon default + flags
`bge-large-en-v1.5` as `downloaded`. A dockerised Traefik reaching the daemon over the
docker bridge connects from a non-loopback peer, so bearer auth + rate
limiting apply unconditionally — list that bridge IP in
`trusted_proxies` (above) if you want the daemon to honour the proxy's
`X-Forwarded-For` for the loopback bypass.

### Embedding model — picking and switching (D)

The bake-off at
[`docs/research/foundation/14-embedding-bakeoff-2026-05-19.html`](../docs/research/foundation/14-embedding-bakeoff-2026-05-19.html)
benchmarks bge-small vs bge-base vs bge-large on kb's own design
corpus. On technical-English prose, bge-large lifts hybrid Recall@1 by
+15.6pp over bge-small; bge-base barely moves the needle. The Docker
image bakes in bge-large (the bake-off-recommended pick, ~1.34 GB);
ship a bge-small image instead for memory-constrained deployments by
editing the model name in `Dockerfile` and `kb.toml`.

Three layers compose to pick the model at daemon startup (highest
precedence first):

1. `[kb.<name>].embedding_model` in `kb.toml` — per-kb override.
2. `[defaults].embedding_model` — daemon-wide default.
3. Registry default (`bge-small-en-v1.5`) — fires when both above are
   absent.

`kb add --embedding-model <name>` writes layer 1 at creation time. To
flip every existing `embedding_model`-less kb at once, set layer 2:

```toml
[defaults]
embedding_model = "bge-large-en-v1.5"

# Optional — turn off the registry-default fallback. Useful for
# lexical-only daemons; tests in CI use this to avoid spawning the
# embedder subprocess. Default false.
# disable_embedder_fallback = true
```

`kb model list` prints the resolved daemon default above the table so
the operator can confirm which model fires for unconfigured kbs.

**Switching the model for an existing kb.** Daemon startup compares
the on-disk lance dim against the resolved model's dim (architecture
invariant #12); a mismatch fails with `Error::Config` naming both
dims, and the failing kb is skipped while siblings come up. Recovery
for a dim change on an existing kb:

```bash
# 1. Stop the daemon.
systemctl --user stop kb        # or docker stop kb / kill <pid>

# 2. Wipe the lance state for the kb whose model is being upgraded.
#    Other kbs in the same daemon are untouched.
rm -rf /var/lib/kb/state/<daemon-name>/<kb-name>/lance/

# 3. Restart. The indexer rebuilds the lance dataset at the new dim
#    and re-embeds every doc — proportional to N_docs × bge-<size>
#    throughput (~5.6 emb/s on i7-7700 Kaby Lake; faster on newer
#    cores with AVX-VNNI).
systemctl --user start kb
```

For **same-dim** model swaps (e.g. bge-base ↔ jina-v2-base-code, both
768-dim), use `kb model set <name> --kb <kb> --in-place` to clear the
embedding column in place — no lance wipe needed, indexer just
re-embeds.

## Local-dev with mkcert

For testing the production path on your laptop without ACME:

```bash
# 1. Install mkcert + the local CA.
sudo pacman -S mkcert      # Arch
mkcert -install

# 2. Issue a wildcard cert for *.artifacts.localhost.
cd /tmp
mkcert "*.artifacts.localhost" "artifacts.localhost" localhost 127.0.0.1

# 3. Drop a Traefik static config (traefik.yml) that watches a
#    dynamic file directory for service definitions.
cat > traefik.yml <<'EOF'
entryPoints:
  websecure:
    address: ":443"
providers:
  file:
    directory: ./dynamic
    watch: true
EOF

# 4. Dynamic config for the kb upstream + TLS.
mkdir -p dynamic
cat > dynamic/kb.yml <<'EOF'
tls:
  certificates:
    - certFile: /tmp/_wildcard.artifacts.localhost+3.pem
      keyFile:  /tmp/_wildcard.artifacts.localhost+3-key.pem

http:
  routers:
    kb-parent:
      rule: "Host(`localhost`)"
      entryPoints: [websecure]
      tls: {}
      service: kb
    kb-artifacts:
      rule: "HostRegexp(`{sub:[a-z0-9._-]+}.artifacts.localhost`)"
      entryPoints: [websecure]
      tls: {}
      service: kb
  services:
    kb:
      loadBalancer:
        servers:
          - url: "http://127.0.0.1:4000"
        passHostHeader: true
EOF

# 5. Run kb daemon + Traefik in two terminals.
kb daemon &
traefik --configFile traefik.yml
```

Now `https://kitchen-sink.artifacts.localhost/` works in the browser
without a self-signed warning. Traefik sets `X-Forwarded-For` on its
own — it connects from loopback (a trusted hop) and appends the real
client IP — so the daemon resolves a non-loopback client and enforces
auth + outbound scrubbing as if from a real client. No
`trusted_proxies` entry is needed for this same-host setup.

## Public deployment with DNS-01 wildcard

Traefik can pull a wildcard cert via Let's Encrypt's DNS-01 challenge
using its built-in DNS providers — no plugin build required (unlike
Caddy's `xcaddy` flow). Static config selects the resolver:

```yaml
# traefik.yml (static)
entryPoints:
  web:
    address: ":80"
    http:
      redirections:
        entryPoint:
          to: websecure
          scheme: https
  websecure:
    address: ":443"

certificatesResolvers:
  myresolver:
    acme:
      email: you@example.com
      storage: /letsencrypt/acme.json
      dnsChallenge:
        provider: namedotcom    # or cloudflare, route53, etc. —
                                # see https://doc.traefik.io/traefik/https/acme/
                                # for the full list

providers:
  file:
    directory: /etc/traefik/dynamic
    watch: true
```

Provider credentials come from environment variables (Traefik reads
them at boot — e.g. `NAMECOM_USERNAME`/`NAMECOM_API_TOKEN`,
`CF_DNS_API_TOKEN`, `AWS_ACCESS_KEY_ID`, etc.).

```yaml
# dynamic/kb.yml — substitute example.com
http:
  routers:
    kb-parent:
      rule: "Host(`kb.example.com`)"
      entryPoints: [websecure]
      service: kb
      tls:
        certResolver: myresolver
        domains:
          - main: example.com
            sans: ["*.example.com", "*.artifacts.example.com"]
    kb-artifacts:
      rule: "HostRegexp(`{sub:[a-z0-9._-]+}.artifacts.example.com`)"
      entryPoints: [websecure]
      service: kb
      tls:
        certResolver: myresolver

  services:
    kb:
      loadBalancer:
        servers:
          # Daemon on the host, Traefik in a container: use the
          # docker-bridge IP and set `extra_hosts:
          # host.docker.internal:host-gateway` on the Traefik service.
          - url: "http://host.docker.internal:4000"
        passHostHeader: true
```

The artifact subdomain regex uses Traefik v3's named-group syntax
(`{name:regex}`). `passHostHeader: true` is the default but listed
for clarity — the daemon's dispatcher reads the unmodified Host
header. Traefik auto-renews ACME certs ~30 days before expiry.

### Authenticating SPA traffic from the browser

The kb-daemon's loopback bypass fires only when the *genuine* client —
resolved by walking the `X-Forwarded-For` chain right-to-left past any
loopback + `trusted_proxies` hops — is itself a loopback address.
Behind a proxy a remote browser never resolves to loopback, so every
`/api/*` request the SPA makes from a remote browser needs a Bearer
token. The cleanest single-operator workflow:

1. Run `kb token generate` and capture the token (Phase B below).
2. Put it in your Traefik env file as `KB_BEARER_TOKEN=...`.
3. Add a Traefik middleware that overwrites the `Authorization`
   header on `/api/*` so the browser never has to learn the token:

   ```yaml
   http:
     middlewares:
       kb-inject-bearer:
         headers:
           customRequestHeaders:
             Authorization: "Bearer ${KB_BEARER_TOKEN}"
   ```

   `customRequestHeaders` **replaces** any header the browser sent —
   chain it after your edge auth (e.g. `basic-auth-global`) so the
   browser challenge runs first and the bearer injection happens
   second. Attach both to the kb-parent + kb-artifacts routers:

   ```yaml
   routers:
     kb-parent:
       middlewares: [basic-auth-global, kb-inject-bearer]
       # ...
   ```

Per-user bearer tokens are a future redesign (the v0.4 model is one
shared token); for now this `basic-auth at the edge + single bearer
injected to the daemon` setup is the pragmatic option.

## Daemon as a systemd unit

`/etc/systemd/system/kb-daemon.service`:

```ini
[Unit]
Description=kb daemon
After=network.target

[Service]
User=kb
Group=kb
Environment=XDG_STATE_HOME=/var/lib/kb/state
Environment=XDG_CONFIG_HOME=/var/lib/kb/config
Environment=XDG_CACHE_HOME=/var/lib/kb/cache
Environment=RUST_LOG=warn,kb=info
ExecStart=/usr/local/bin/kb daemon --config /var/lib/kb/config/kb/kb.toml
Restart=on-failure
RestartSec=5

# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/kb
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```bash
useradd -r -d /var/lib/kb -s /sbin/nologin kb
mkdir -p /var/lib/kb/{state,config,cache}
chown -R kb:kb /var/lib/kb
sudo -u kb kb token generate    # writes /var/lib/kb/config/kb/token
systemctl enable --now kb-daemon
```

## Sharing artifacts (`kb share`)

`kb share` publishes an artifact/folder to Cloudflare Pages + Access
(gated) or GitHub Pages (public). The engine runs **inside the daemon**
— the CLI and the SPA "Share…" button both POST `/api/kb/{kb}/share` —
so the host API tokens must live in the **daemon's environment**, not
the CLI's. kb has no `pass://` resolver by design; inject the secrets at
daemon launch.

- **systemd:** keep them in an `EnvironmentFile` (mode 0600, owned by
  `kb`) and reference it from the unit:

  ```ini
  # /var/lib/kb/config/kb/share.env   (chmod 600, populated out-of-band
  # from your secret store — e.g. a deploy script that reads Proton Pass)
  KB_CF_API_TOKEN=cf_xxx
  KB_GH_TOKEN=ghp_xxx
  ```
  ```ini
  [Service]
  EnvironmentFile=/var/lib/kb/config/kb/share.env
  ```
  then `systemctl restart kb-daemon`.

- **Docker Compose:** pass the vars into the `kb` service from the
  compose `.env` (itself populated from your secret store), then
  `docker compose up -d` to recreate the container:

  ```yaml
  services:
    kb:
      environment:
        - KB_CF_API_TOKEN=${KB_CF_API_TOKEN}
        - KB_GH_TOKEN=${KB_GH_TOKEN}
  ```

`kb.toml` names the account + IdPs (never the token itself):

```toml
[share]
live_origin = "https://kb.example.com"     # for --links absolute

[share.cloudflare]
account_id  = "<cf-account-id>"
team_domain = "<team>.cloudflareaccess.com"
# api_token_env defaults to KB_CF_API_TOKEN
# google_idp / github_idp = "<pre-registered IdP UUID>"  # for --gate google|github

[share.github]
owner = "<github-user-or-org>"
# token_env defaults to KB_GH_TOKEN
```

The Cloudflare token is one custom token with **Pages:Edit**, **Access:
Apps and Policies:Edit**, and **Access: Organizations, Identity
Providers, and Groups:Edit**; the GitHub token needs repo create/delete
+ Pages write. A `--gate google|github` needs the matching IdP
pre-registered once (callback
`https://<team>.cloudflareaccess.com/cdn-cgi/access/callback`); email
gates ride Cloudflare's built-in One-Time PIN with no IdP.

**Reverse-proxy timeout.** A deploy uploads files and waits on the
host's build — tens of seconds for a large share. The daemon has no
request timeout, but a reverse proxy in front (Traefik/nginx) may cut a
long `POST /share`; raise the proxy's read/response timeout for the kb
backend if you share big folders.

**State-dir note (v0.7).** Each kb keeps a small
`<XDG_STATE_HOME>/<daemon>/<kb>/.anchors-stale.json` sidecar tracking
which comment anchors went stale on the last reindex. It lets
`comment.anchor_resolved` still fire for a comment that went stale in a
*previous* daemon process. It's safe to lose (the next reindex rebuilds
it), but don't single it out for deletion — leave the kb's state dir
intact across restarts and upgrades.

**Upgrading to v0.7.1 — review files.** v0.7 changed the artifact id
from a content hash to a hash of the source-relative path, which left
pre-v0.7 `.review/<id>.json` comment files keyed on the old id. v0.7.1
migrates them automatically: the first index of each artifact renames
its review file from the old content-hash id to the path-based id when
the file's bytes still match (an artifact edited before the upgrade
already had its review orphaned under the old scheme). No `kb reset` or
manual step is needed — just start the v0.7.1 daemon.

## Backup & restore (B1)

`kb backup <kb>` writes a **consistent** `tar.gz` of a kb's persistent
state — `index.db`, the `lance/` dataset, and `.review/` comments — to
`<state>/exports/<kb>-<timestamp>.tar.gz` (override with `--out PATH`):

```bash
kb backup research                       # → <state>/exports/research-20260522-…​.tar.gz
kb backup research --out /backups/kb.tgz
```

- **sqlite is snapshotted with `VACUUM INTO`**, so the copy is
  transactionally consistent even while the daemon writes — no torn
  `index.db`, no `-wal`/`-shm` to reconcile.
- **lance is copied then validated** by re-opening the staged dataset. If
  the daemon commits mid-copy the backup fails loudly and asks you to
  retry; for a guaranteed-consistent lance snapshot under heavy indexing,
  **stop the daemon first** (the sqlite half is consistent regardless).
- `RequestMetrics` (process-RAM counters) are deliberately not captured.
- `<state>/query-embed-cache.json` — the daemon-wide query-embedding cache,
  persisted on shutdown so hot hybrid/semantic searches stay warm across a
  restart (incl. the in-process config restart) — is a **regenerable perf
  cache**: not part of a kb backup, and safe to delete (the next searches
  just re-embed and re-warm it).

**Backups must leave the box.** `kb backup` writes its tarball under the
same host's `<state>/exports/`, so a local-only backup shares every
single-point-of-failure with the live state it's protecting (disk
failure, host loss, an accidental `rm -rf` that takes both the live dir
and its sibling `exports/` with it). Copy the tarball off-host as part of
any real backup routine.

GC-B4 — an optional `[backup]` section in `kb.toml` runs an off-host copy
step right after a successful local backup:

```toml
[backup]
remote_cmd  = ["rclone", "copyto", "{src}", "{dest}"]
remote_dest = "remote:bucket/kb-backups/"
```

- `remote_cmd` is an **explicit argv** (never a shell string — no shell is
  invoked, so there's no quoting/injection surface); `{src}` is replaced
  with the local tarball's path, `{dest}` with `remote_dest`. Any argv
  works — `["scp", "{src}", "user@host:/path/"]`, `["rsync", "{src}",
  "{dest}"]`, a wrapper script, etc. The program is resolved via `PATH`
  like any other command.
- Both `remote_cmd` and `remote_dest` must be set for the copy to run;
  either alone is a no-op (`kb config validate` warns).
- The copy is **best-effort**: a failed or unreachable remote (bad
  credentials, network down, `rclone` not installed) is reported loudly —
  a stderr warning plus an annotated summary line in `kb backup`'s own
  stdout — but never fails the backup itself. The local tarball is
  already a complete backup on its own; treat the remote copy as
  best-effort insurance, and watch for the warning if you rely on it.
- Absent `[backup]` (the default), nothing changes — `kb backup` behaves
  exactly as before, local-only.

Restore with `kb restore <tarball> --kb <name>`:

```bash
systemctl --user stop kb-daemon          # the daemon holds index.db open
kb restore /backups/kb.tgz --kb research # refuses a non-empty state…
kb restore /backups/kb.tgz --kb research --force   # …unless --force (wipes first)
```

Restore extracts into the kb's state dir and verifies the archive
produced an `index.db` (catching a wrong tarball or a `--kb` that doesn't
match the archive). **Stop the daemon for that kb before restoring** — it
holds `index.db` open. `--force` is required to replace a non-empty state
and wipes it first, so the restore is clean (no stale leftovers).

## Bearer-token auth (v0.4)

The daemon reads a single shared token from `<XDG_CONFIG_HOME>/kb/token`
at boot. Requests whose genuine client is loopback skip the auth check
entirely — the local SPA and CLI work unchanged. "Genuine
client" means: the TCP peer must be a trusted hop (loopback, or a
`trusted_proxies` entry — see the `[server]` block above) before
`X-Forwarded-For` is consulted, and the header is then walked
right-to-left so a forged leftmost entry can't fake a loopback origin.
Every other request must send `Authorization: Bearer <token>` or get a
401 problem+json.

```bash
# Create the token (refuses to overwrite an existing one).
kb token generate

# Show the file path (for `chmod`, ACL probes, etc.).
kb token path

# Print the token to stdout (script-friendly).
TOKEN=$(kb token show --print)

# Rotate (overwrites the file with a fresh 32-byte hex value).
kb token rotate
```

The token file is created mode 0600, owned by the user that ran
`kb token generate`. Rotation is a single-file overwrite — the
daemon picks up the new value on the next restart (`systemctl
restart kb-daemon`).

## Rate limiting (v0.4 + v0.5)

Hot endpoints are bucketed per-token. Default for each: **60 req/min/token**.
Loopback requests are not bucketed. Over-limit requests get 429
`urn:kb:errors:rate-limited` problem+json with `Retry-After`.

| Endpoint | kb.toml key | Default |
|---|---|---|
| `GET /api/search` | `search` | 60 |
| `POST /api/kb/{kb}/atlas/recompute` | `atlas_recompute` | 60 |
| review comment mutations (`POST .../comments`, `/resolve`, `PATCH`/`DELETE`, …) + `/export` | `review_post` | 240 |

v0.5 Q1 makes each independently configurable:

```toml
[server.rate_limit]
search          = 120  # bump for heavy search use
atlas_recompute = 6    # one recompute every ~10s is plenty
review_post     = 240  # default; fine-grained comment mutations + export
```

Three separate per-token bucket maps so search bursts don't starve
review writes. Each field optional — omit to keep the default.

## Team identities (v0.34)

kb can attribute reads, comments, and API calls to named users while staying
**one trust tier**: it never authenticates anyone, and every identity keeps the
operator's full authority (admin verbs, config, delete, shared memory and
sessions). Adding a teammate means accepting that — see
[README → Non-goals](../README.md#non-goals). Config reference:
[configuration.md → `[identity]`](configuration.md#identity-v034).

**How identity is resolved** (first match wins, per request):

| # | Source | Wins when |
|---|---|---|
| 1 | Per-user token — `Authorization: Bearer <t>` **or** `X-Kb-Token: <t>` matching a `<config>/tokens` entry | any peer (possession is the credential) |
| 2 | Identity header (default `Remote-User`) | the immediate peer is loopback or a listed `[server] trusted_proxies` IP |
| 3 | Legacy shared `<config>/token` | → `[identity].operator` |
| 4 | Loopback, no credential | → `[identity].operator` |

Tokens beat the header deliberately: an agent or script behind the proxy must be
able to attribute as itself, and Authelia's `client_credentials` flow sets **no**
`Remote-*` headers at all (machine clients have no username to forward), so
tokens are the only machine attribution lane.

### Wiring it behind Authelia + Traefik

The reference deployment needs no new middleware — Authelia already returns the
`Remote-*` set and Traefik already forwards it:

```yaml
- "traefik.http.middlewares.authelia.forwardauth.address=http://authelia:9091/api/authz/forward-auth"
- "traefik.http.middlewares.authelia.forwardauth.trustForwardHeader=true"
- "traefik.http.middlewares.authelia.forwardauth.authResponseHeaders=Remote-User,Remote-Groups,Remote-Name,Remote-Email"
```

**Required, not optional:** the header named by `[identity].header` MUST appear
in `authResponseHeaders`. Traefik *deletes* the client-supplied value of every
listed header before copying the auth server's, so listing it is what makes a
forged `Remote-User` from a browser impossible; an **unlisted** header passes
through untouched and would be trusted forgery. kb's own trusted-hop gate
(`trusted_proxies` must name the proxy's exact IP) is the second lock — a
request arriving from anywhere else has its identity header ignored entirely.

Adding a teammate:

1. Add them to Authelia (`users_database.yml`) and to the `kb.example.com`
   access rule — that is the whole authentication story.
2. Optionally add `[[identity.users]] name = "…"` for a display name.
3. If they use the CLI or an agent: `kb token issue <user>`, hand them the
   printed plaintext over your own secure channel (it is shown once), restart
   the daemon. They set it as `Authorization: Bearer` locally, or `X-Kb-Token`
   when their traffic crosses an edge that rewrites `Authorization`.
4. Rotate = `kb token revoke <user>` → `kb token issue <user>` → restart.

Verify:

```bash
kb whoami                       # user + how it was resolved
kb users                        # configured ∪ observed roster
curl -s -H 'Remote-User: alice' localhost:4000/api/identity | jq '.user,.identity_source'
```

A request whose `Remote-User` arrives from an untrusted peer must come back as
the operator with source `loopback`/`legacy` — never `header`. If it does not,
`trusted_proxies` is wider than you think.

## Doc-code bridge CORS (DCB v1)

kb's reader (kb.example.com) calls kb-code's `/api/doc-lens` directly, browser-side,
cross-origin — kb never proxies this server-side (Scope→Out: no kb→kb-code
client). Two deployment shapes, both supported with zero CORS config:

- **Loopback-colocated** (the default, e.g. local dev): browser, kb, and
  kb-code all on `127.0.0.1`/`localhost`. kb-code's doc-lens sub-router
  carries the SAME loopback-origin GET-only `CorsLayer` predicate kb-server
  already ships (`is_loopback_web_origin`, `kb-server/src/middleware.rs:135`)
  — works with an empty `[doclens] origins`, no config needed.
- **Public, same-site** (kb.example.com calling kbc.example.com — a
  same-site kb + kb-code deployment): set `[doclens] origins =
  ["https://kb.example.com"]` in kb-code's hosted `kb-code.toml`. The
  browser's fetch carries `credentials:'include'` (the Authelia SSO session
  cookie, domain-scoped to `*.example.com`, rides same-site) so Authelia's
  forward-auth on the `kbc.example.com` router passes; after that, the
  reverse proxy's bearer-injection middleware overwrites `Authorization`
  with kb-code's own daemon token regardless (same pattern as the
  equivalent middleware on kb.example.com) — the CORS allowlist governs
  only whether the BROWSER is permitted to read the cross-origin response,
  not authentication. The response needs an exact-origin
  `Access-Control-Allow-Origin: https://kb.example.com` +
  `Access-Control-Allow-Credentials: true` (never a wildcard with
  credentials — the browser rejects that combination outright). PNA
  (Private-Network-Access preflight) does not apply here: both origins are
  public HTTPS, not a public page reaching into a private address.

**This origin allowlist is layered ONLY on the doc-lens sub-router, never
`/api`-wide** — kb-code's `/api` nest is NOT uniform (a separate
`loopback_only` sub-router covers raw-transcripts/session-diff/checkout,
`router.rs:244/463`); an `/api`-wide CORS layer would additionally expose
`GET /api/file` (full repo contents) and `GET /api/search/transcripts` to any
allowlisted browser origin, not just doc-lens. Verify with:

```bash
curl -si -H 'Origin: https://kb.example.com' https://kbc.example.com/api/doc-lens?kb=platform\&doc=x | grep -i access-control
curl -si -H 'Origin: https://kb.example.com' https://kbc.example.com/api/file?repo=kb\&path=README.md | grep -i access-control   # MUST be empty
curl -si -H 'Origin: https://kb.example.com' https://kbc.example.com/api/search/transcripts?q=x | grep -i access-control       # MUST be empty
```

**Example redeploy flow: enabling DCB on an existing deployment.** If your
kb-code container currently mounts doc corpora that don't need the bridge
yet, and you want to light up DCB for a corpus with a linked code checkout:

1. Add a `[[repos]]` entry to `kb-code.toml` for the checkout(s) you want
   browsable, and a bind-mount volume in your compose file's `kb-code`
   service for each (read-only, same pattern as your existing repo mounts).
2. Add `[doclens]` to the same `kb-code.toml`:
   `kbs = ["<kb-name>"]` (opt-in allowlist — empty = feature off entirely) and
   `origins = ["https://kb.example.com"]`.
3. Rebuild and restart the kb-code service (e.g. `docker compose build
   kb-code && docker compose up -d kb-code`, or your platform's equivalent
   rebuild-and-swap step).
4. If `kbc.example.com`'s access rule is narrower than `kb.example.com`'s
   (e.g. admins-only vs. a wider readers group), a kb.example.com reader
   without kbc access sees the honest "code bridge unavailable — you don't
   have kb-code access" degrade, never a lying "kb-code down".
5. Add `code_url = "https://kbc.example.com"` to the relevant
   `[kb.<name>]` section in kb's own `kb.toml` (the `[kb.<name>]`
   `code_url` row in `configuration.md`) and restart the kb container
   (config-put restart per invariant #13, or a compose restart).

## Multi-user comment isolation

The artifact subdomain handler injects a user's comment payload when
`?cm=on`. To prevent reverse proxies (Caddy's cache, Cloudflare, etc.)
from leaking one user's comments to another, the response carries:

```
Cache-Control: private, no-store
Vary: Authorization, Accept, Cookie
```

Plain artifact bytes (no `?cm=on`) stay cacheable.

## mDNS daemon discovery (opt-in)

When the daemon runs on a LAN with multiple kbs, opt in to mDNS so
peers can discover each other automatically:

```toml
# kb.toml
[server]
addr = "0.0.0.0:4737"
mdns = true
```

This is a **non-loopback (`0.0.0.0`) bind**, so it is subject to the
[fail-closed rule](#fail-closed-on-a-token-less-public-bind) above: the daemon
**refuses to start** here unless you either run `kb token generate` first (then
non-loopback peers send `Authorization: Bearer <token>`) or set
`KB_ALLOW_NO_AUTH=1` to acknowledge an upstream auth gate. Do **not** run this
recipe token-less and unauthenticated on a shared LAN — it would expose every
`/api/*` verb (including `DELETE /api/kb`) to the network.

The daemon advertises `_kb._tcp.local.` with TXT record `v=<version>`.
The fleet verbs (`kb fleet status`) read `~/.config/kb/daemons.toml` —
auto-population from mDNS isn't wired in the consumer side yet
(deep-review D4). Browse advertised daemons from the shell:

```bash
avahi-browse -r _kb._tcp -t              # Linux (avahi-utils)
```

## Capture from the Android share sheet (v0.25)

The SPA ships a Web App Manifest `share_target` (`manifest.webmanifest`), so
once it's installed as a PWA, kb shows up as a share destination for any
`.md`/`.html`/`.txt` file (or a shared URL/selection) from another Android
app. **Android/Chrome only** — Safari on iOS has no `share_target` API, so
this recipe doesn't reach iPhones; use `kb capture` from a shell or the SPA's
in-app capture sheet there instead.

1. **Install the PWA.** Open the daemon in Chrome on the phone (e.g.
   `https://kb.<your-domain>`), then "Add to Home screen" / the install
   prompt. Chrome only offers a `share_target` app once it's installed —
   visiting the site in a regular tab isn't enough.
2. **Share into kb.** From any app's share sheet (a browser's "Share page…",
   a file manager, another PWA), pick kb. Chrome POSTs the file(s) + any
   shared `title`/`text`/`url` straight to `POST /capture` on the daemon —
   no intermediate upload UI, no service worker in the loop (kb deliberately
   ships none; the daemon handles the POST directly). A shared **URL or
   plain-text selection** (no file) lands as a small `.md` stub — the daemon
   never fetches the URL itself (SSRF ruling; the `[webhooks]` outbound POST
   is the one place kb dials a URL, and even that pins/filters the resolved
   IP — see [`configuration.md` § webhooks](configuration.md#webhooks)).
3. **Pick the destination kb.** The share-target route carries no `{kb}` in
   its URL (the OS decides where to POST, not the user), so set
   `[server.capture].default_kb` in `kb.toml` to the kb you want share-sheet
   captures to land in; unset, it falls back to the first configured kb. See
   [`configuration.md` § `[server.capture]`](configuration.md#servercapture).
4. **Sanitize-on-by-default for shared HTML.** Unlike the CLI/SPA capture
   paths (sanitize is opt-in there), an `.html` file arriving via the
   share-target route is sanitized (`ammonia`) by default — a page saved
   from a random app is untrusted content, and the stored source *is* the
   sanitized output.
5. **On success**, Chrome navigates to `/?captured=<kb>:<source_relative>`
   and the SPA shows a toast; there's no direct jump to the artifact detail
   because indexing is async (the watcher's next debounce), so the artifact
   may not exist yet at redirect time.
6. **Authelia (or any upstream auth gate) can swallow the share.** If the
   daemon sits behind an authentication proxy and your browser session has
   gone stale, Chrome's share-target POST hits the login redirect instead of
   `/capture` — the share silently fails (no toast, no error surfaced to the
   share sheet) rather than prompting you to log in inline. Open the PWA
   directly first to refresh the session if a share seems to have vanished;
   this is a known gap in how `share_target` interacts with redirect-based
   auth and out of scope for kb itself to fix.

## Hardening checklist

- Bind kb-server to `127.0.0.1` only; let Caddy own the public IP.
- Set `KB_DEV_ORIGIN_ANY=0` (the default) — only the production
  Caddy host(s) should pass the Origin allowlist.
- `kb token generate` and verify `stat -c %a $(kb token path)` is
  `600`.
- Enable systemd `ProtectSystem=strict` + `PrivateTmp=true`.
- For comments + reviews, enable `[kb.<name>.outbound]` scrubbing
  (v0.3 G3) so any kb-prompt or PII is stripped on egress.
- If you enable `[webhooks]`, prefer a **loopback** receiver. LAN
  receivers need `allow_private = true` (RFC1918/ULA only). Link-local
  and cloud-instance metadata are always refused; dial IPs are pinned
  after resolve. See [`configuration.md` § webhooks](configuration.md#webhooks).
- Rotate the token any time the proxy config or operator set
  changes (`kb token rotate && systemctl restart kb-daemon`).
- Monitor `/api/identity` from your uptime checker; the daemon
  exposes no secret info there.
