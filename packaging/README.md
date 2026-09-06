# Packaging & release (Release Zero)

Everything the operator needs to turn a private repo into a public one that
ships prebuilt binaries, a `curl | sh` installer, and a ghcr image. All of
this was prepared in **Z6-prep**; the steps below are the manual
**Z6-final** flip.

## What Z6-prep already built

| Artifact | Path | What it does |
|---|---|---|
| Release workflow | [`.github/workflows/release.yml`](../.github/workflows/release.yml) | On a `v*` tag: builds `kb` + `kb-embedder` for linux-x64 and linux-arm64; uploads per-target `.tar.gz` + `.sha256` to the GitHub Release; builds the Dockerfile and pushes `ghcr.io/nicolasacchi/kb:{version,latest}`. |
| Installer | [`scripts/install.sh`](../scripts/install.sh) | POSIX `curl \| sh`-safe. Detects OS/arch, downloads the latest release tarball, installs both binaries into `~/.local/bin` (honors `$PREFIX`), verifies `kb --version`. |
| Bootstrap command | [`../plugins/kb-memory/commands/kb-setup.md`](../plugins/kb-memory/commands/kb-setup.md) | `/kb-setup` inside Claude Code — guided first-run: install → daemon → `kb.toml` (memory + sessions) → start → verify. |

The Dockerfile ([`../Dockerfile`](../Dockerfile)) and `ci.yml` predate Z6.

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

## Z6-final checklist (operator, on the flip day)

Run top-to-bottom. Nothing below was runnable during Z6-prep — treat each as a
first-time verification.

1. **Pre-flight (still private).**
   - `just ci` green on `main`; `bash -n scripts/install.sh` clean.
   - Confirm the GitHub slug is `nicolasacchi/kb` and `git remote -v` matches.
   - Optional dry run: push a throwaway tag `v0.0.0-rc1` (you can delete the
     release + tag after) to smoke `release.yml` end-to-end **before** the real
     tag. Watch both `binaries` legs + the `ghcr` job.

2. **Flip the repo public.** GitHub → Settings → Danger Zone → Change visibility
   → Public. (`ubuntu-24.04-arm` hosted runners and unlimited Actions minutes are
   free only once public.)

3. **Enable GitHub Packages / ghcr.** Ensure the repo can publish packages; after
   the first push, set the `kb` package visibility to **Public** (Packages →
   `kb` → Package settings) so `docker pull ghcr.io/nicolasacchi/kb` is open.

4. **Push `main` + tags.**
   ```bash
   git push origin main
   git push origin --tags
   ```

5. **Tag the release.**
   ```bash
   git tag -a v0.24 -m "kb v0.24 — Release Zero"
   git push origin v0.24
   ```

6. **Watch `release.yml`.** Actions tab → the `release` run. Verify:
   - `create-release` made the GitHub Release.
   - both `binaries` legs uploaded `kb-0.24-<triple>.tar.gz` + `.sha256`.
     If `aarch64-unknown-linux-gnu` fails on the embedder link, it's the ORT
     aarch64 prebuilt (see the matrix TODO in `release.yml`) — either wire
     `ORT_STRATEGY=system` + a system libonnxruntime, or drop that matrix row.
   - `ghcr` pushed `:0.24` and `:latest`.

7. **Test `install.sh` on a clean machine** (a fresh container / VM, no cargo):
   ```bash
   curl -fsSL https://raw.githubusercontent.com/nicolasacchi/kb/main/scripts/install.sh | sh
   kb --version
   ```

   > **Pre-verified in Z6-prep (2026-07-03).** `install.sh` was run end-to-end in
   > clean containers (Debian trixie, Ubuntu 24.04, Fedora 41, Debian 12, Alpine)
   > against a locally-served, faithfully-built (ubuntu-24.04) release tarball via
   > `KB_BASE_URL`. Findings, all folded back into this repo: the script's
   > detect/download/checksum/extract/place logic works on every distro; the
   > **prebuilt bundle requires glibc ≥ 2.39** (kb-embedder's static ORT forces
   > it — it needs GCC 12+/GLIBCXX_3.4.30 to build and glibc 2.39 to run, so it
   > will neither build on ubuntu-22.04 nor run on Debian 12 / Ubuntu 22.04 /
   > RHEL 9 / Alpine); `install.sh` now detects that and points at the Docker
   > image. To reproduce a pre-public smoke without a release:
   > `KB_BASE_URL=http://host:PORT KB_VERSION=0.24 sh install.sh`.

8. **Announce / list.** Submit to:
   - [selfh.st](https://selfh.st/) (self-hosted software directory)
   - [awesome-selfhosted](https://github.com/awesome-selfhosted/awesome-selfhosted)
   - [anthropics/claude-plugins-community](https://github.com/anthropics/claude-plugins-community)
     (the `kb-plugins` marketplace — `kb-memory` now ships `/kb-setup`).

## Outstanding `TODO(verify-after-public)` markers

Grep for them: `git grep -n "TODO(verify-after-public)"`. They flag every
assumption that could not be exercised without a public repo + a real tag —
runner availability, the ORT aarch64-linux prebuilt, ghcr package visibility,
and the `sha256` fills. Clear each as you verify it in the
steps above.
