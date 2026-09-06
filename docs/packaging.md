# Packaging + distribution

How kb is built into shippable artifacts: the Docker images, the
per-target release tarballs, and the `curl | sh` installer. All three
channels are built and wired into CI (`.github/workflows/release.yml`,
triggered on a `v*` tag push) — see
[packaging/README.md](../packaging/README.md) for the full asset list and
the release checklist.

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

## Docker

The [`Dockerfile`](../Dockerfile) is the canonical reproducible build:

```bash
KB_GIT_SHA=$(git rev-parse --short=12 HEAD) \
  docker build --build-arg KB_GIT_SHA="$KB_GIT_SHA" -t kb:latest .
```

It builds both binaries (separate invocations), bakes in the SPA bundle
(`KB_SPA_DIST`) and the default embedding model, runs as a non-root user
(uid 1000), carries OCI image labels, and declares a `HEALTHCHECK` against
[`/healthz`](self-host.md). See [self-host.md](self-host.md) for running it.

The same Dockerfile also builds the **kb-code** image — the sibling
read-first code-browsing daemon (`kb-code-server`) plus its `web-code` SPA
(`KB_CODE_SPA_DIST`), with no embedder, no ONNX Runtime and no baked model —
from its own `kb-code-runtime` target:

```bash
docker build --target kb-code-runtime \
  --build-arg KB_GIT_SHA="$KB_GIT_SHA" -t kb-code:latest .
```

kb's `runtime` is deliberately the **last** stage, so a bare `docker build`
keeps producing the kb image and kb-code is only ever reachable by an
explicit `--target`. A `v*` tag publishes both as
`ghcr.io/nicolasacchi/kb:{version,latest}` and
`ghcr.io/nicolasacchi/kb-code:{version,latest}`, alongside per-target
`kb-<ver>-<triple>.tar.gz` and `kb-code-<ver>-<triple>.tar.gz` archives — the
full asset list is in [`packaging/README.md`](../packaging/README.md).

## Prebuilt releases — hand-rolled, not cargo-dist

A `v*` tag push runs [`.github/workflows/release.yml`](../.github/workflows/release.yml),
which builds and publishes **both halves** from one workflow:

| Half | Release assets (per target) | Image |
|---|---|---|
| **kb** | `kb-<ver>-<triple>.tar.gz` + `.sha256` — `kb`, `kb-embedder`, `INSTALL` | `ghcr.io/nicolasacchi/kb:<ver>` + `:latest` |
| **kb-code** | `kb-code-<ver>-<triple>.tar.gz` + `.sha256` — `kb-code-server`, `kb-code`, `kb-lip`, `INSTALL` | `ghcr.io/nicolasacchi/kb-code:<ver>` + `:latest` |

`<triple>` is `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` —
the aarch64 leg runs on a **native** `ubuntu-24.04-arm` GitHub-hosted
runner, not a cross-compile, which sidesteps the usual cross-linking risk
for `kb-embedder`'s statically-bundled ONNX Runtime. Neither tarball
bundles its SPA (both daemons resolve `KB_SPA_DIST`/`KB_CODE_SPA_DIST`
from disk at boot); the container images bake both SPAs in — that's the
difference between the two channels. Versioning stays git-tag-derived
(the workspace pins `version = "0.0.0"`; `VERSION="${GITHUB_REF_NAME#v}"`
in the workflow, matching the `KB_GIT_DESCRIBE` build stamp — see
`routes/identity.rs`), so there is no crate-version bump to keep in sync
per release.

[cargo-dist](https://opensource.axo.dev/cargo-dist/) was evaluated and
**rejected** in favor of this hand-rolled workflow, for two reasons
(full rationale in `release.yml`'s header comment):

1. **The two-binary split.** `kb-embedder` must be built in a *separate*
   `cargo build` invocation from `kb` (invariant §26) or a single build
   unifies `kb-core`'s `local-embedder` feature and links ONNX Runtime
   into `kb` too. cargo-dist models a workspace and builds its members
   together — steering it away from that is exactly the shape it makes
   awkward.
2. **SHA-pinned actions.** `ci.yml` pins every third-party action to a
   40-char commit SHA; cargo-dist generates and owns its own release
   workflow with tag/floating action refs, which would drift on every
   `dist` upgrade. The hand-rolled file mirrors `ci.yml`'s pins and
   otherwise uses the preinstalled `gh`/`docker` CLIs, so there are no
   extra actions to pin at all.

See [packaging/README.md](../packaging/README.md) for the full asset
list, the release checklist, and the outstanding
`TODO(verify-after-public)` markers the workflow carries until the first
real tag exercises it end-to-end.

## From source

The canonical from-source path is in the [5-minute quickstart](quickstart.md):
two `cargo build --release` invocations + `install … ~/.local/bin/`, or
`cargo install --path crates/kb-cli` + `cargo install --path crates/kb-embedder`.
