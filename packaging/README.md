# Packaging & release

How kb ships: prebuilt binaries per platform, a `curl | sh` installer, and
ghcr container images, all produced from one tag push. This repo is already
public and the pipeline below is live; the "Cutting a release" section
further down is the runbook for pushing the next tag.

## What's already wired up

| Artifact | Path | What it does |
|---|---|---|
| Release workflow | [`.github/workflows/release.yml`](../.github/workflows/release.yml) | On a `v*` tag, ships **both halves** (see [What a `v*` tag ships](#what-a-v-tag-ships) below). |
| Installer | [`scripts/install.sh`](../scripts/install.sh) | POSIX `curl \| sh`-safe. Detects OS/arch, downloads the latest release tarball, installs both binaries into `~/.local/bin` (honors `$PREFIX`), verifies `kb --version`. |
| Bootstrap command | [`../plugins/kb-memory/commands/kb-setup.md`](../plugins/kb-memory/commands/kb-setup.md) | `/kb-setup` inside Claude Code — guided first-run: install → daemon → `kb.toml` (memory + sessions) → start → verify. |

The Dockerfile ([`../Dockerfile`](../Dockerfile)) and `ci.yml` predate this
release pipeline and are unchanged by it.

## What a `v*` tag ships

One tag, two shipping surfaces, one workflow. `release.yml` publishes:

| Half | Release assets (per target) | Image |
|---|---|---|
| **kb** | `kb-<ver>-<triple>.tar.gz` + `.sha256` — `kb`, `kb-embedder`, `INSTALL` | `ghcr.io/nicolasacchi/kb:<ver>` + `:latest` |
| **kb-code** | `kb-code-<ver>-<triple>.tar.gz` + `.sha256` — `kb-code-server`, `kb-code`, `kb-lip`, `INSTALL` | `ghcr.io/nicolasacchi/kb-code:<ver>` + `:latest` |

`<triple>` is `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` —
four tarballs per release, each with a checksum sidecar. **kb-code** is the
sibling read-first code-browsing daemon (`crates/kb-code-server` +
`crates/kb-code-cli`) and `kb-lip` its standalone opt-in LSP→HTTP adapter
(`crates/kb-lip`): a separate daemon on its own port (4747), shipped from the
same repo and the same tag, never a `kb` subcommand.

Both images come from the one repo-root `Dockerfile`; kb-code's is its
`kb-code-runtime` target, kb's is the (deliberately last) `runtime` stage.

**Neither tarball bundles its SPA.** Both daemons resolve their web UI from
disk at boot (`KB_SPA_DIST` / `KB_CODE_SPA_DIST`), so the archives ship
binaries only and the `INSTALL` note in each points at `just ci-spa` /
`just ci-code-spa`. The container images bake both SPAs in — that is the
difference between the two channels.

The kb-code assets are **additive**: the kb tarball's name, contents and
`INSTALL` text are unchanged by them, and
[`scripts/install.sh`](../scripts/install.sh) — which resolves
`kb-<version>-<triple>.tar.gz` by name — is untouched and installs kb only.

## Why hand-rolled, not cargo-dist

kb ships **two** binaries. `kb-embedder` is a workspace member but must be
built/tested in a **separate** `cargo build` invocation from workspace
sweeps (`--exclude kb-embedder`): otherwise kb-core's `local-embedder`
feature unifies and links ONNX Runtime into `kb` (architecture invariant
#26 / `docs/self-host.md`). cargo-dist models a workspace and builds
members together, and it generates+owns its own release workflow with
un-pinned action refs — fighting both that split and ci.yml's 40-char-SHA
action-pinning discipline. The hand-rolled `release.yml` keeps the two
builds explicit and mirrors ci.yml's pins (using the preinstalled `gh` /
`docker` CLIs so there are no extra actions to pin). Full rationale is in
the header comment of `release.yml`.

kb-code's three binaries were added to the **same** per-target `binaries`
job rather than a parallel one, on measured evidence: the last three green
release runs took 48–58 min (x86_64) and 42–44 min (aarch64) for kb +
kb-embedder, and `kb-code-server` links `kb-core`, so the dominant cost — the
lance/datafusion/arrow release graph — is already compiled in that job's
target dir. A separate job would recompile it from scratch. Projected worst
case is ~110 min against a 180-minute timeout and GitHub's hard 360-minute
cap. They are still separate `cargo build` invocations, but for a different
reason than the ORT split above: none of the three enables `local-embedder`,
so what matters is only that they stay OUT of the `kb-embedder` invocation.

## Cutting a release

Run top-to-bottom for any new `vX.Y.Z` tag.

1. **Pre-flight.**
   - `just ci` green on `main`; `bash -n scripts/install.sh` clean.
   - Optional dry run: push a throwaway tag (e.g. `v0.0.0-rc1`, deletable
     afterward) to smoke `release.yml` end-to-end **before** the real tag.
     Watch both `binaries` legs + the `ghcr` job.

2. **Tag the release.**
   ```bash
   git push origin main
   git tag -a vX.Y.Z -m "kb vX.Y.Z"
   git push origin vX.Y.Z
   ```

3. **Watch `release.yml`.** Actions tab → the `release` run. Verify:
   - `create-release` made the GitHub Release.
   - both `binaries` legs uploaded `kb-<ver>-<triple>.tar.gz` + `.sha256`.
     If `aarch64-unknown-linux-gnu` fails on the embedder link, it's the ORT
     aarch64 prebuilt (see the matrix TODO in `release.yml`) — either wire
     `ORT_STRATEGY=system` + a system libonnxruntime, or drop that matrix row.
   - both legs also uploaded `kb-code-<ver>-<triple>.tar.gz` + `.sha256`
     (four tarballs + four checksums on the release in total). Spot-check one:
     `tar tzf` should list `kb-code-server`, `kb-code`, `kb-lip`, `INSTALL` and
     no `web-code/dist` — the SPA is a separate `just ci-code-spa` build by
     design.
   - `ghcr` pushed `ghcr.io/nicolasacchi/kb:<ver>` + `:latest` **and**
     `ghcr.io/nicolasacchi/kb-code:<ver>` + `:latest`. kb-code builds and
     pushes first (it's the small image); if the job dies between the two
     pushes, the kb-code tags are already live and only kb needs a re-run —
     the job is idempotent (`--clobber` uploads, overwriting image tags).

4. **Test `install.sh` on a clean machine** (a fresh container / VM, no cargo):
   ```bash
   curl -fsSL https://raw.githubusercontent.com/nicolasacchi/kb/main/scripts/install.sh | sh
   kb --version
   ```

   `install.sh` has been verified end-to-end in clean containers (Debian
   trixie, Ubuntu 24.04, Fedora 41, Debian 12, Alpine) against a
   faithfully-built release tarball. Its detect/download/checksum/extract/
   place logic works on every distro; the **prebuilt bundle requires glibc
   ≥ 2.39** (kb-embedder's static ORT forces it — it needs GCC 12+/
   GLIBCXX_3.4.30 to build and glibc 2.39 to run, so it will neither build on
   ubuntu-22.04 nor run on Debian 12 / Ubuntu 22.04 / RHEL 9 / Alpine);
   `install.sh` detects that and points at the Docker image instead. To
   reproduce this kind of smoke test without a real release:
   `KB_BASE_URL=http://host:PORT KB_VERSION=X.Y.Z sh install.sh` against a
   locally-served tarball.

5. **Announce (optional, for a notable release).** e.g.
   [selfh.st](https://selfh.st/) (self-hosted software directory),
   [awesome-selfhosted](https://github.com/awesome-selfhosted/awesome-selfhosted).

## Outstanding `TODO(verify-after-public)` markers

Grep for them: `git grep -n "TODO(verify-after-public)"`. They flag every
assumption that could not be exercised without a public repo + a real tag —
runner availability, the ORT aarch64-linux prebuilt, ghcr package visibility,
and the `sha256` fills. Clear each as you verify it in the
steps above.
