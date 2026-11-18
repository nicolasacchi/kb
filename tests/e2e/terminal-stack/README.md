# terminal-stack: the real-stack terminal end-to-end lane (T25)

`tests/e2e/spa-terminal-stack.spec.ts` drives the whole terminal chain with
synthetic fake harnesses: a pinned tmux 3.7c holder, the compiled tap,
`kb-termd` (direct supervisor), three `kb-server` processes and the real SPA in
Chromium. It is CI-only (`term-e2e` job) and opt-in locally.

## Running

Needs `KB_TMUX_BIN` (a tmux 3.7c, see `scripts/ci/build-tmux.sh`), `KB_TAP_BIN`
(`bun build --compile` of `crates/kb-term/tap/src/main.ts`), fast-profile
`kb-server` and `kb-termd` binaries, and `web/dist`:

    KB_E2E_TERMINAL=1 npx playwright test --project=terminal-stack --project=terminal-stack-dpr2

Without `KB_E2E_TERMINAL=1` the spec is ignored by every default project and the
default `e2e` lane is unchanged.

## What the setup does (`global-setup-terminal.ts`)

* Everything lives under one unique temp root; `HOME` and every `XDG_*` of every
  child point inside it. Nothing reads a real home or a real runtime dir.
* A fake `omp` is first on the pane `PATH`; it runs `./scenario.sh` in the
  pane's working directory. Each launch (`POST /api/terms`) gets its own
  directory and its own script, so panes never share a file; the per-test
  cleanup reports a kill that failed or a pane that is still alive.
* A bring-up that fails part-way tears down what it started and rethrows
  (the setup self-test injects such a failure and looks for survivors).
  `setupTerminalStack({ injectFailure, ports, keepLogsIn })` takes three
  self-test options:
  * `injectFailure` throws at one named point of the bring-up:
    `"after-root"` (the temp root and its directories exist, nothing is
    running), `"after-termd"` (kb-termd is up) or `"after-servers"` (every
    kb-server answered). The thrown error carries the unique root so the
    caller can look for survivors.
  * `ports` replaces the three server ports. The self-test runs while the real
    stack is up, so it uses 4747 (main), 4748 (ttl) and 4749 (off); the lane's
    own ports are 4737-4739.
  * `keepLogsIn` is where teardown copies the child logs (default: the CI
    artifact directory); the self-test checks the `kb-server-<port>.log` files.
* Ports: 4737 terminal enabled (main), 4738 enabled with a 2 s ticket TTL,
  4739 with no `[terminal]` section (disabled). Never 4000.
* Teardown kills tmux holders by their own socket, then `pkill -f <root>`, and
  fails if anything naming the root survives.

## Files

* `oracle.ts`: `checkNumbered` (fails when `highest != total`), `scanForQueries`
  (independent VT parser, from the spike branch's strip oracle), `frameTracker`.
* `stack.ts`: addresses, REST helpers, scenario launching, beats, socket
  recording, scrollback read.
* `fixtures/numbered_harness.py`: numbered lines, ED2+ED3 rebuild frames (each
  stamps `soon-N`, `frame-start-N`, `frame-end-N` with epoch-ms so the join
  tests can prove where their join landed), query injection, flood + tail. `fake_harness.py` and `synth_omp_kitty_stream.py` are
  the merged generators (`crates/kb-term/fixtures`) copied unchanged.

All fixtures are generated at run time; no recording or binary is committed.
