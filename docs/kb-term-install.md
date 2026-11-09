# kb-term install (host side)

kb-term has four host pieces that `kb` itself cannot ship inside the kb
container: the runner `kb-termd`, the operator-local client `kb-term`
(`attach|new|ls|doctor`), the compiled tap `kb-tap`, and a pinned private
tmux 3.7c. The kb container image carries no tmux and no host `HOME`, so
this install puts the pieces on the host under `~/.local` and adds one
bind mount so the container can reach the runner socket.

## Requirements

- Linux x86_64. kb-term is published for x86_64 Linux only; there is no
  aarch64 archive.
- glibc >= 2.39 (Debian 13+, Ubuntu 24.04+, Fedora 40+). The prebuilt
  binaries (and the bundled tmux) need it; on an older glibc the installer
  says so instead of placing a binary that cannot run.
- A systemd user manager (`systemctl --user`). The units and tmpfiles file
  below are rendered for it. If the bundled tmux is dynamically linked it
  also needs the usual `libevent`, `libncurses` and `libutf8proc` runtime
  libraries — the archive's `INSTALL` note lists them (`ldd` output) and
  `docs/packaging.md` describes the same floor. On Arch, any of these
  under a different shared-object soname shows up as "the bundled tmux does
  not run" and the installer names the likely causes.

## Install from a published release

```bash
curl -fsSL https://raw.githubusercontent.com/nicolasacchi/kb/main/scripts/install-kb-term.sh | sh
```

The installer downloads `kb-term-<ver>-x86_64-unknown-linux-gnu.tar.gz` from
the latest kb tag, then fails closed: it verifies the `.sha256` checksum
sidecar and, when `gh` is present and signed in, the SLSA build provenance
(`gh attestation verify`). A missing sidecar, a missing sha256 tool, or a
failed provenance check aborts before anything is installed;
`KB_INSECURE_SKIP_VERIFY=1` is the explicit override.

Pin a version instead (for example when the latest kb tag predates kb-term):

```bash
KB_VERSION=0.44 sh install-kb-term.sh
```

`install-kb-term.sh` only ships on tags built after kb-term shipped; for a
tag without the archive the installer says so and points at the main-build
path below. `PREFIX` (default `~/.local`) overrides the install root.
Known limitation: the shipped systemd units, the tmpfiles file and the
`kb-termd` config lookup assume the default `~/.local` layout, so with a
custom `PREFIX` the operator must adjust the installed units (and the
conf the runner reads) to match — the binaries alone landing in a custom
`PREFIX` is not a working install.

## Install from a main-build artifact

A push to `main` builds the same archive via the `term-int` job in
`.github/workflows/ci.yml` (packaging steps at the end of that job) and
uploads it as an artifact. Main artifacts are
**not** attested (only tag pushes are), so they carry a checksum but no
provenance. The documented path uses `KB_INSECURE_SKIP_VERIFY=1` — keep
that variable to the CI path; without it, a present signed-in `gh` that
cannot verify provenance fails closed, while absent or unsigned `gh` is
checksum-only (the same as `install.sh`).
```bash
gh run download <run-id> -n kb-term-main-<sha12> -D /tmp/kb-term-main
KB_BASE_URL=file:///tmp/kb-term-main KB_VERSION=main-<sha12> \
  KB_INSECURE_SKIP_VERIFY=1 sh scripts/install-kb-term.sh
```

## Why tmux is off PATH

A long-lived tmux server must never be paired with a client from an upgraded
package, so the archive carries a pinned tmux 3.7c and the installer puts it
at `~/.local/libexec/kb-term/tmux` — never in `~/.local/bin`, never
symlinked onto the PATH. The holder units and `kb-termd` reference that
exact path, so a distro tmux upgrade cannot kill or mismatch any session.

## The three operator steps, in order

1. tmpfiles (one-time `sudo`), so `/run/kb-term` exists before anything
   else needs it and keeps a stable inode for the bind mount:

   ```bash
   sudo install -m 0644 ~/.local/share/kb-term/tmpfiles.d/kb-term.conf /etc/tmpfiles.d/kb-term.conf
   sudo systemd-tmpfiles --create /etc/tmpfiles.d/kb-term.conf
   ```

2. The runner user unit:

   ```bash
   systemctl --user enable --now kb-termd.service
   ```

   Anything that did not change the machine (a tmpfiles re-run, a schema
   note) needs no restart. Do **not** restart `kb-term@*` holder units to
   pick up an upgrade — that ends every live session. Restart `kb-termd`
   only and let new sessions pick up the new tmux.

3. Point the kb container at the runner socket (only `/run/kb-term/runner`
   is bind-mounted — never the per-session holder sockets), then enable
   the surface in the host copy of `kb.toml`:

   ```yaml
   # compose (kb service volumes the runner dir only; <user> is the runtime account)
   - /run/kb-term/runner:/run/kb-term/runner
   ```

   ```toml
   [terminal]
   enabled = true
   ```

   The source directory must exist **before** `docker compose up` or Docker
   creates it as root. Order is tmpfiles → `kb-termd` → the kb container.

   It also helps to keep the user manager alive across logout:

   ```bash
   loginctl enable-linger "$(id -un)"
   ```

Verify with `kb-term doctor`, which checks the host: tmux version and sha,
the tap binary, the omp overlay, and the `/run/kb-term` layout.

## Rollback

Re-run the installer with the previous archive (pin `KB_VERSION`, or the
`file://` main artifact) and restart `kb-termd` only; no session or data
state lives under `~/.local/share/kb-term`, so nothing else needs cleaning.

## Compose snippet and service notes

kb keeps running under Docker (the one-machine default); kb as its own
systemd service is a later option and changes nothing about the host steps
above. The compose line is generic — `- /run/kb-term/runner:/run/kb-term/runner` —
exposing the runner directory (and only that) to the container; the
per-session holder sockets under `/run/kb-term/s` never reach it.
