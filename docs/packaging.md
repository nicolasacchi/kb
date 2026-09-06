# Packaging + distribution

How kb is built into shippable artifacts, and the plan for prebuilt
multi-arch releases. **Docker is ready today**; the cargo-dist
path is a ready-to-apply template gated on two project
decisions (called out under [Blockers](#blockers-decide-before-cutting-a-release)).

## The two-binary distribution rule

kb ships **two** binaries that must live in the **same directory**:

- `kb` — the CLI + daemon (no ONNX Runtime linked).
- `kb-embedder` — the embedding/rerank sidecar (statically-linked ONNX
  Runtime).

`kb` spawns the sidecar by path: `kb-core`'s `embed_ipc::locate_embedder_bin()`
resolves `$KB_EMBEDDER_BIN`, else `current_exe().parent()/kb-embedder`, else
`kb-embedder` on `$PATH`. **Any installer or archive that ships `kb` without
`kb-embedder` beside it degrades the install to keyword-only search.** Every
distribution channel below ships both.

They must also be built in **separate cargo invocations** — a single
`cargo build` over both feature-unifies `kb-core`'s `local-embedder`
feature and pulls ONNX Runtime into `kb` too (architecture invariant §26).

## Docker (ready)

The [`Dockerfile`](../Dockerfile) is the canonical reproducible build:

```bash
KB_GIT_SHA=$(git rev-parse --short=12 HEAD) \
  docker build --build-arg KB_GIT_SHA="$KB_GIT_SHA" -t kb:latest .
```

It builds both binaries (separate invocations), bakes in the SPA bundle
(`KB_SPA_DIST`) and the default embedding model, runs as a non-root user
(uid 1000), carries OCI image labels, and declares a `HEALTHCHECK` against
[`/healthz`](self-host.md). See [self-host.md](self-host.md) for running it.

## Prebuilt releases via cargo-dist (template — see blockers)

[cargo-dist](https://opensource.axo.dev/cargo-dist/) (the `dist` tool) can
build per-platform archives + a `curl | sh` installer
from a tagged release. The intended config (`dist-workspace.toml` at the
repo root):

```toml
[workspace]
members = ["cargo:."]

[dist]
cargo-dist-version = "0.28.0"
ci = ["github"]
# Linux is -gnu, NOT -musl: kb-embedder statically links a prebuilt ONNX
# Runtime that needs GLIBC_2.38, which musl can't satisfy.
targets = [
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
]
installers = ["shell"]
# LOAD-BEARING (invariant §26): build each package on its own cargo
# invocation so the workspace build never unions `local-embedder` into
# `kb`. Without it, `kb` would link ONNX Runtime.
precise-builds = true
```

Both `kb-cli` and `kb-embedder` are distable, so each platform archive
packs **both** binaries side-by-side (satisfying the discovery rule above).
Apply with `dist init` (scaffolds + generates `.github/workflows/release.yml`,
which is owned by `dist generate` — never hand-edit it).

### Blockers (decide before cutting a release)

1. **Version scheme.** The workspace pins `version = "0.0.0"` *deliberately*
   and versions via git tags (the `KB_GIT_DESCRIBE` build stamp is the real
   version; see `routes/identity.rs`). cargo-dist instead derives the release
   version from the **crate version** and matches it to the tag, so it needs a
   real semver (`0.19.0`) in `[workspace.package]`. Adopting cargo-dist means
   **either** bumping the crate version per release (abandoning 0.0.0-forever)
   **or** configuring dist to take the version from the tag. This is a project
   decision, not made here.
2. **Tag format.** Existing tags are bare (`v0.13`, `v0.18`); dist's default
   wants full semver (`v0.19.0`). Pick one going forward.
3. **aarch64-linux ONNX.** The static ONNX Runtime link on a cross-compiled
   `aarch64-unknown-linux-gnu` target is the riskiest matrix entry — validate
   it on an arm runner first, and be ready to drop to native-arm-only if the
   cross link fails.
4. **Build deps in CI.** dist's build job needs `protoc` (lancedb) and, on
   Linux, `libssl-dev` — wire them via `[dist.dependencies]` when running
   `dist init`.

## From source

The canonical from-source path is in the [5-minute quickstart](quickstart.md):
two `cargo build --release` invocations + `install … ~/.local/bin/`, or
`cargo install --path crates/kb-cli` + `cargo install --path crates/kb-embedder`.
