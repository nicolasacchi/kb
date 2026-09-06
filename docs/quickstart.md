# kb in 5 minutes

Go from a clone to a searchable, browsable knowledge base — a local
daemon indexing your HTML/Markdown artifacts, a hybrid (keyword +
vector) search API, and a web UI — in about five minutes.

The daemon binds **loopback only (`127.0.0.1:4000`) by default**, so
nothing in this guide exposes anything to your network. If you later
bind a public address, jump to [Going beyond loopback](#going-beyond-loopback)
*first* — it's one `kb token generate` plus a reverse proxy.

## 0. Prerequisites

You need the build toolchain:

- **rustc 1.96** (auto-pinned by `rust-toolchain.toml` — just have `rustup`)
- **protoc** — `sudo pacman -S protobuf` · `apt-get install protobuf-compiler`
- **node 22+** — only for the web UI bundle (step 2)

No ONNX Runtime install is needed: it's statically bundled into the
`kb-embedder` sidecar at build time.

## 1. Build + install the binaries (the first build's link step is the slow part)

kb ships **two** binaries — the `kb` CLI/daemon and the `kb-embedder`
sidecar that does embeddings. Build them in **two separate cargo
invocations** (building them together links ONNX Runtime into `kb` too;
see [architecture invariant §26](architecture-invariants.md)):

```bash
git clone https://github.com/nicolasacchi/kb && cd kb

cargo build --release -p kb-cli        # → target/release/kb           (no ORT)
cargo build --release -p kb-embedder   # → target/release/kb-embedder  (static ORT)
```

Install them **side by side** — the daemon finds `kb-embedder` as a
sibling of the running `kb`:

```bash
install target/release/kb target/release/kb-embedder ~/.local/bin/
```

> Prefer `cargo install`? `cargo install --path crates/kb-cli` and
> `cargo install --path crates/kb-embedder` drop both into
> `~/.cargo/bin/`. Either way, keep the two binaries in the same dir.
>
> Just want to kick the tyres without installing? Substitute
> `cargo run -p kb-cli --` for `kb` in every command below.

## 2. Build the web UI bundle (~1 min)

The daemon serves the SPA from `web/dist`, which is gitignored — build
it once:

```bash
just ci-spa        # = cd web && npm ci && npm run build → web/dist/
```

The daemon resolves `web/dist` relative to the repo, or via
`KB_SPA_DIST=/path/to/web/dist` if you run from elsewhere. (Skip this
step if you only want the CLI + HTTP API — search works without it.)

## 3. Register a corpus (`kb add`)

`kb add <dir>` writes the source folder into your config
(`~/.config/kb/kb.toml`) and names the kb. Point it at the bundled
sample corpus to see results immediately:

```bash
kb add ./corpus/canon --kb canon
```

That writes a `[kb.canon]` section to `~/.config/kb/kb.toml`. Use your
own folder of `.html` / `.md` files anytime: `kb add ~/notes --kb notes`.

**Want semantic + hybrid search (recommended)?** Add an embedding model
— the first run downloads ~130 MB to your cache once:

```bash
kb add ./corpus/canon --kb canon --embedding-model bge-small-en-v1.5
```

Without a model, keyword (BM25) search still works fully; `hybrid` and
`semantic` modes need an embedder configured.

## 4. Run the daemon (`kb daemon`)

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

## 5. Search

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

## 6. Open the web UI

Open **<http://127.0.0.1:4000/>** in your browser. You get the gallery,
faceted search, the per-artifact reader (sandboxed iframe), the atlas
view, reading lists, and inline comments.

That's the loop. From here: register more corpora with `kb add`, scaffold
new artifacts with `kb new`, watch the fleet with `kb fleet status` +
`kb events --follow`, and read
[authoring-artifacts.md](authoring-artifacts.md) to write HTML that
indexes cleanly.

---

## Going beyond loopback

The default `127.0.0.1` bind is the safe one: loopback clients bypass
auth because nothing else can reach the socket. **The moment you bind a
reachable address (`0.0.0.0:PORT` or a LAN IP), do all three of these:**

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
   ```

> **Why this matters.** A loopback bind needs no token because the kernel
> won't route remote traffic to it. A *public* bind with no token is wide
> open — anyone who can reach the port can read and mutate every kb, which
> is why the daemon refuses that configuration outright. The safe pattern
> is always: **keep the daemon on `127.0.0.1` and let a TLS-terminating
> proxy own the public address**, with a bearer token on the daemon.

Full deployment guide — Docker, systemd, mDNS, rate limiting, outbound
scrubbing — is in [self-host.md](self-host.md).
