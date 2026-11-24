# kb-term install (host side)

kb-term has four host pieces that `kb` itself cannot ship inside the kb
container: the runner `kb-termd`, the operator-local client `kb-term`
(`attach|new|ls|kill|doctor`), the compiled tap `kb-tap`, and a pinned private
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

   The unit carries an `[Install]` section, so `enable` makes it start on
   boot; `kb-term doctor` WARNs (`termd_enabled`) when it is not enabled.

   **Give the runner its environment.** Every pane gets an allowlisted copy
   of the environment of `kb-termd`, and under systemd that is the user
   manager's: a minimal `PATH` (usually `/usr/local/bin:/usr/bin`) and no
   `KB_SESSIONS_DIR`. Without this step a harness installed in a per-user
   directory (for example under `~/.bun/bin`) exits 127 in the pane, and
   the capture and beat hooks exit immediately because `KB_SESSIONS_DIR`
   is unset, so SPA-launched sessions get no beats, no terminal-to-session
   binding and no capture. The installer prints this block pre-filled from
   your current shell; it never writes it for you:

   ```bash
   mkdir -p ~/.config/systemd/user/kb-termd.service.d
   cat > ~/.config/systemd/user/kb-termd.service.d/env.conf <<'EOF'
   [Service]
   Environment="PATH=/path/to/kb/bin:/path/to/harness/bin:/usr/local/bin:/usr/bin"
   Environment="KB_SESSIONS_DIR=/path/to/the/sessions/corpus"
   EOF
   systemctl --user daemon-reload
   systemctl --user restart kb-termd.service
   ```

   The `PATH` always includes the directory `kb` resolves from and the
   directory of every harness found, in the order your shell's own `PATH`
   lists them, then `/usr/local/bin:/usr/bin`. That order matters: an earlier
   directory shadows a later one, so a stale copy of a harness in an earlier
   directory is what every pane launches. After moving or reinstalling a
   harness, re-run the installer (it only prints the step) or reorder the
   `PATH` by hand so it matches your shell. Each
   assignment is one double-quoted word, as systemd requires for a value with
   a space; inside the quotes a backslash and a double quote are
   backslash-escaped and `%` is written `%%` (systemd expands `%x` in unit
   files). The installer does this for you; if you edit the file by hand,
   keep the quotes.

   `kb-term doctor` reads the running runner's environment (the process the
   unit's `MainPID` names) and FAILs `runner_env_sessions_dir` when
   `KB_SESSIONS_DIR` is missing there, and WARNs `runner_env_kb` or
   `runner_env_harness_*` when `kb` or a harness your shell finds is not on
   the runner's `PATH`. It also WARNs `runner_env_harness_*` when the
   runner's `PATH` and your shell resolve a harness to two different files
   (a symlink to the same file is fine), and the `harness_*` version probe
   runs the binary the runner resolves when its environment is readable,
   saying which one in the check detail.

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

To end a session from the host, use `kb-term kill <id>`; it goes through the
runner, so the holder, tap and session files are cleaned up. If you stopped
the `kb-term@<id>` and `kb-tap@<id>` units by hand and `kb-term ls` shows the
id as `stale`, `kb-term kill <id>` removes that session's leftover socket and
directory.

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
