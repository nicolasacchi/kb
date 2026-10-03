# kb in 5 minutes

Go from a downloaded release (tarball or Docker image) to a searchable,
browsable knowledge base — a local daemon indexing your HTML/Markdown
artifacts, a hybrid (keyword + vector) search API, and a web UI — in
about five minutes.

The daemon binds **loopback only (`127.0.0.1:4000`) by default**, so
nothing in this guide exposes anything to your network. If you later
bind a public address, jump to [Going beyond loopback](#going-beyond-loopback)
*first* — it's one `kb token generate` plus a reverse proxy.

## 0. Install

Pick one of three channels — this guide uses the **tarball** for steps
1–5 and calls out the one place the **Docker** image differs (it also
bakes in an embedding model).

**Prebuilt tarball** (needs glibc ≥ 2.39 — Debian 13+, Ubuntu 24.04+,
Fedora 40+; see the alternatives below if you're on an older distro):

```bash
curl -fsSL https://raw.githubusercontent.com/nicolasacchi/kb/main/scripts/install.sh | sh
```

This detects your OS/arch, downloads the matching `kb-<version>-<target>.tar.gz`
release asset, verifies its checksum (a missing checksum aborts the install;
`KB_INSECURE_SKIP_VERIFY=1` overrides), and installs **both** binaries —
`kb` (the CLI + daemon) and `kb-embedder` (the embedding sidecar) — into
`~/.local/bin` (override with `PREFIX=`), plus the web reader and a sample
corpus into `~/.local/share/kb`. The two binaries must stay
**side by side**: `kb` finds `kb-embedder` as a sibling of itself. Add
`~/.local/bin` to your `PATH` if the installer says to.

> The tarball carries the web reader (`share/kb/web/dist`), which the daemon
> finds next to its own binary. Platforms: Linux x86_64/aarch64 and WSL2;
> macOS and native Windows are unsupported.

**Option B: Docker** (binaries + web UI + a baked-in embedding model, all
in one image — no separate SPA build step):

```bash
docker pull ghcr.io/nicolasacchi/kb
mkdir -p ~/kb-config ~/kb-state
docker run -d --name kb --network host \
  -v ~/kb-config:/var/lib/kb/config \
  -v ~/kb-state:/var/lib/kb/state \
  -v ~/notes:/corpus:ro \
  ghcr.io/nicolasacchi/kb
```

(`--network host` is Linux-only and deliberate. The daemon's default bind is
`127.0.0.1:4000` *inside* the container, which `-p 4000:4000` cannot reach;
host networking puts it on the host's loopback, so it stays loopback-first,
needs no token, and passes the `Host` guard. Do not reach for
`KB_ALLOW_NO_AUTH` to make a port publish work. On Docker Desktop, or to
publish a port, bind a non-loopback address with a token instead: see
[Going beyond loopback](#going-beyond-loopback).)

(The config volume is writable here so `kb add` — next step — can create
`kb.toml` inside it; [self-host.md](self-host.md)'s production recipe
mounts a hand-written config `:ro` instead.) With Docker, steps 2–4 below
become `docker exec kb kb add /corpus --kb notes` and
`docker exec kb kb search "…" --kb notes`, and the daemon is already
running — jump straight to step 5.

**Option C: build from source** (for contributors, or platforms without
a prebuilt binary — older glibc, musl/Alpine, non-x86_64/aarch64):

```bash
git clone https://github.com/nicolasacchi/kb && cd kb

cargo build --release -p kb-cli        # → target/release/kb           (no ORT)
cargo build --release -p kb-embedder   # → target/release/kb-embedder  (static ORT)
install target/release/kb target/release/kb-embedder ~/.local/bin/
```

Build them in **two separate cargo invocations** — building them
together links ONNX Runtime into `kb` too; see
[architecture invariant §26](architecture-invariants.md). Needs
**rustc** (auto-pinned by `rust-toolchain.toml` — just have `rustup`) and
**protoc** (`sudo pacman -S protobuf` · `apt-get install protobuf-compiler`).
Prefer `cargo install`? `cargo install --path crates/kb-cli` and
`cargo install --path crates/kb-embedder` drop both into `~/.cargo/bin/`.
Just want to kick the tyres without installing? Substitute
`cargo run -p kb-cli --` for `kb` in every command below.

## 1. Write a minimal kb.toml

`kb add` (step 2) writes this for you, but here's the entire file a
working single-corpus config needs — everything else has a default:

```toml
# ~/.config/kb/kb.toml
[kb.notes]
path = "/home/you/notes"
embedding_model = "bge-small-en-v1.5"   # optional — omit for keyword-only search
```

One `[kb.<name>]` table with a `path` is a complete config. The full key
reference (every `[section]`, default, and one-line meaning) is
[configuration.md](configuration.md).

## 2. Register a corpus (`kb add`)

`kb add <dir>` writes the source folder into your config
(`~/.config/kb/kb.toml`, creating the file on first use) and names the
kb — this *is* the config-scaffolding step (there's no separate `kb init`).
Point it at the bundled sample corpus to see results immediately:

```bash
kb add ./corpus/canon --kb canon
```

That writes the `[kb.canon]` section shown above to
`~/.config/kb/kb.toml`. Use your own folder of `.html` / `.md` files
anytime: `kb add ~/notes --kb notes`.

**Want semantic + hybrid search (recommended)?** Add an embedding model
— the first run downloads ~130 MB to your cache once:

```bash
kb add ./corpus/canon --kb canon --embedding-model bge-small-en-v1.5
```

Without a model, keyword (BM25) search still works fully; `hybrid` and
`semantic` modes need an embedder configured.

## 3. Run the daemon (`kb daemon`)

```bash
kb daemon
```

This binds `http://127.0.0.1:4000` (loopback only) in the foreground and
starts watching your corpus — edits reindex automatically. Leave it
running; open a second terminal for the next steps. (First run with an
embedding model spends a moment downloading + embedding; watch the log.)

Confirm it's healthy:

```bash
kb status
curl -s http://127.0.0.1:4000/healthz      # unauthenticated liveness probe
```

## 4. Search

```bash
kb search borrow --kb canon
```

Hybrid (BM25 + vector, the default) ranks across your corpus. Other
modes + machine-readable output:

```bash
kb search borrow --kb canon --mode keyword     # BM25 only (no model needed)
kb search borrow --kb canon --mode semantic    # vector only (needs a model)
kb search borrow --kb canon --json             # parseable stdout
```

Or hit the API directly:

```bash
curl -s 'http://127.0.0.1:4000/api/search?q=borrow&kb=canon&mode=hybrid'
```

Loopback requests skip auth entirely, so the local CLI / API / web UI
all just work with no token.

## 5. Open the web UI

Open **<http://127.0.0.1:4000/>** in your browser. You get the gallery,
faceted search, the per-artifact reader (sandboxed iframe), the atlas
view, reading lists, and inline comments.

**If you installed via the Docker image or the tarball, it's already there**
— the image bakes in the built SPA and the tarball ships it under
`share/kb/web/dist`, which the daemon finds relative to its own binary
(`<prefix>/bin/kb` beside `<prefix>/share/kb/web/dist`). **If you built from
source**, build the bundle once (`just ci-spa`) and run from the checkout, or
point the daemon at it with `KB_SPA_DIST=/path/to/web/dist`. The CLI and HTTP
API work identically either way — nothing above this step needed the web UI.

That's the loop. From here: register more corpora with `kb add`, scaffold
new artifacts with `kb new`, watch the fleet with `kb fleet status` +
`kb events --follow`, and read
[authoring-artifacts.md](authoring-artifacts.md) to write HTML that
indexes cleanly.

---

## Going beyond loopback

The default `127.0.0.1` bind is the safe one: loopback clients bypass
auth because nothing else can reach the socket. **The moment you bind a
reachable address (`0.0.0.0:PORT` or a LAN IP), do all four of these:**

1. **Generate a bearer token.** Every non-loopback `/api/*` request must
   then send `Authorization: Bearer <token>` or get a `401` — and the
   daemon now **refuses to start** on a non-loopback bind with no token
   (override with `KB_ALLOW_NO_AUTH=1` only when an upstream proxy is your
   auth gate):

   ```bash
   kb token generate     # writes ~/.config/kb/token, mode 0600
   kb token path         # show where it lives
   ```

   The daemon reads the token once at startup — restart after generating
   or rotating (`kb token rotate`).

2. **Put a reverse proxy (TLS) in front.** Don't expose the plain-HTTP
   daemon directly. Traefik/Caddy/nginx terminates TLS and sets
   `X-Forwarded-For` so the daemon sees the real client IP. See
   [self-host.md](self-host.md) for full Traefik configs (wildcard cert,
   artifact subdomains, bearer injection at the edge).

3. **Set the production `[server]` fields** in `kb.toml`:

   ```toml
   [server]
   addr                 = "127.0.0.1:4000"            # keep the daemon on loopback; let the proxy own the public IP
   artifact_host_suffix = ".artifacts.example.com"    # your wildcard
   parent_origin        = "https://kb.example.com"    # your SPA origin (locks down artifact framing)
   trusted_proxies      = ["172.17.0.1"]              # only if the proxy reaches the daemon off-loopback (e.g. dockerised)
   hostnames            = ["kb.example.com"]          # the names the proxy publishes: THIS is what turns the DNS-rebinding Host check on for proxied peers
   ```

   `hostnames` matters even though `parent_origin`'s host is admitted
   automatically: a peer that is neither loopback nor in `trusted_proxies` is
   `Host`-checked **only once this list is non-empty**, so an unset list leaves
   a reverse-proxied deployment unchecked. Set it to the names you publish.

   The proxy must pass the browser's `Host` header through unchanged
   (Caddy does by default; Traefik `passHostHeader: true`; nginx
   `proxy_set_header Host $host`). Requests from loopback and from
   `trusted_proxies` peers are `Host`-checked, and `parent_origin`'s host
   passes automatically; any other name needs a `hostnames` entry.

4. **Gate the artifact hosts at the edge.** `*.artifacts.example.com` serves
   sandboxed artifacts **without any daemon auth**, by design, and an artifact
   id is an unkeyed hash of its path (`index.html`'s id is computable by
   anyone). Put the same identity-aware proxy or network allowlist in front of
   the artifact hosts as in front of `kb.example.com`. See
   [SECURITY.md](../SECURITY.md).

> **Why this matters.** A loopback bind needs no token because the kernel
> won't route remote traffic to it. A *public* bind with no token is wide
> open — anyone who can reach the port can read and mutate every kb, which
> is why the daemon refuses that configuration outright. The safe pattern
> is always: **keep the daemon on `127.0.0.1` and let a TLS-terminating
> proxy own the public address**, with a bearer token on the daemon.

Full deployment guide — Docker, systemd, mDNS, rate limiting, outbound
scrubbing — is in [self-host.md](self-host.md).
