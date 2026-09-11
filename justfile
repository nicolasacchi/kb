# List available recipes
default:
    @just --list

# Low-priority prefix for long builds (nice + ionice; Linux/WSL2).
prio := "nice -n 20 ionice -c 3"

# Run the full CI suite (fmt + clippy + test) across the workspace and the
# ORT-linking embedder surface, then assert the generated artifacts (TS wire
# bindings + the API route table) are in sync with the Rust source — a derive
# or router change without `just types` / `just api-docs` fails here, not in
# review. (The bootstrap-spike recipes lived here until all 6 spikes retired;
# git history on this file preserves them.)
ci: ci-workspace ci-embedder types-check api-docs-check

# Supply-chain gate — local mirror of the CI `supply-chain` job. Needs
# cargo-deny on PATH: `cargo install --locked cargo-deny`. Split so a license
# regression and an advisory hit are distinguishable.
deny:
    cargo deny check advisories
    cargo deny check licenses bans sources

# Regenerate THIRD-PARTY-LICENSES.md (the distributed-dependency NOTICE) from
# the dep graph via cargo-about. Needs cargo-about on PATH (the binary is behind
# the `cli` feature): `cargo install --locked cargo-about --features cli`.
# Commit the result; `licenses-check` guards drift in CI-style sweeps.
licenses:
    cargo about generate about.hbs > THIRD-PARTY-LICENSES.md

# Drift guard for the committed NOTICE (same shape as types-check / api-docs-check).
licenses-check: licenses
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "$(git status --porcelain -- THIRD-PARTY-LICENSES.md)" ]; then
      git status --short -- THIRD-PARTY-LICENSES.md
      echo "ERROR: THIRD-PARTY-LICENSES.md is out of sync — run 'just licenses' and commit." >&2
      exit 1
    fi

# Re-derive the distinct SPDX id set after a big dependency bump, so
# deny.toml [licenses].allow + about.toml accepted can be updated BEFORE the
# gate fails on a newly-pulled license. Prints one SPDX id per line.
licenses-graph:
    #!/usr/bin/env python3
    import json, subprocess, re
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--all-features"]))
    ids = sorted({
        t.strip()
        for p in meta["packages"]
        for t in re.split(r"\bOR\b|\bAND\b|/",
                          (p.get("license") or "").replace("(", " ").replace(")", " "))
        if t.strip()
    })
    print("\n".join(ids))

# fmt + clippy + test for the workspace (crates/*), EXCLUDING kb-embedder AND
# the kb-code-* crates. kb-embedder is the only crate that links ONNX Runtime
# (via the kb-core `local-embedder` feature) — excluding it keeps that
# feature from being unified ON across the shared kb-core rlib, so the
# workspace build/test/clippy link zero onnxruntime (fast, portable, the real
# shipping shape for kb/kb-server). kb-code-server/kb-code-cli are a separate
# sibling daemon (kb-code Wave 1+) with their own dependency surface (gix,
# tree-sitter, grep-*, nucleo) and no ORT involvement at all — the exclusion
# here is pure build-time isolation (keep the main sweep's compile surface
# stable), not a feature-unification workaround. kb-lip (design-lip.md,
# Track L1) joins that SAME carve-out for the SAME reason, even though it
# links no ONNX/gix/tree-sitter of its own: it's a kb-code-adjacent sibling
# (an opt-in LSP intelligence provider kb-code-server talks to over HTTP,
# never links) whose own CI lane is `ci-code` below — excluding it here is
# pure isolation, not a dependency-surface concern, and avoids clippy/test
# running it twice (once implicitly here, once explicitly in `ci-code`).
ci-workspace:
    cargo fmt --all -- --check
    cargo clippy --workspace --exclude kb-embedder --exclude kb-code-server --exclude kb-code-cli --exclude kb-lip --all-targets -- -D warnings
    cargo test --workspace --exclude kb-embedder --exclude kb-code-server --exclude kb-code-cli --exclude kb-lip --no-fail-fast

# clippy + test for the ORT-linking surface: kb-embedder itself, plus kb-core
# WITH `local-embedder` (the in-process backend + the name→fastembed-enum maps).
# This is the slow path (static ONNX Runtime link), kept separate so the rest of
# CI stays fast. Needs network on a cold build (ORT binaries fetched from CDN;
# for offline/air-gapped builds set ORT_STRATEGY=system + ORT_LIB_LOCATION=<dir>).
ci-embedder:
    cargo clippy -p kb-embedder --all-targets -- -D warnings
    cargo clippy -p kb-core --features local-embedder --all-targets -- -D warnings
    cargo test -p kb-embedder --no-fail-fast
    cargo test -p kb-core --features local-embedder --no-fail-fast

# Build the kb-embedder release binary (lands in the shared target/release/
# next to `kb`, satisfying locate_embedder_bin's sibling contract).
build-embedder:
    cargo build --release -p kb-embedder

# clippy + test for the kb-code sibling daemon (kb-code Wave 1, W1.1
# scaffold: crates/kb-code-server + crates/kb-code-cli) PLUS kb-lip
# (design-lip.md Track L1 — the opt-in LSP intelligence adapter kb-code
# talks to over lip/1 HTTP; L2 wires the client side into kb-code-server
# itself). Deliberately OUT of the `ci` aggregate (two-command discipline —
# `just ci` proves kb proper is untouched; `just ci-code` proves the
# kb-code scaffold + its adjacent providers compile + their own tests
# pass). No ORT surface here (none of these three link ONNX Runtime) — the
# split from `ci-workspace` is pure build-time isolation, mirroring the
# kb-embedder split's shape without its feature-unification reason.
#
# V70-A5 — this recipe is ALSO the kbc-cmd/1 registry gate: `kb-code commands
# doctor` runs as a `#[test]` inside kb-code-cli (`command_registry_passes_
# doctor`), so `cargo test -p kb-code-cli` below fails the build on registry
# drift — a stale CLI twin, a claimed browser chord, a global key redeclared
# narrower, an Esc row with no dismiss order, or an unratified conflict. No
# separate step to remember to add, and the twin check runs against the clap
# tree of the binary actually being tested.
ci-code:
    cargo clippy -p kb-code-server -p kb-code-cli -p kb-lip --all-targets -- -D warnings
    cargo test -p kb-code-server -p kb-code-cli -p kb-lip --no-fail-fast

# W4.1 — the kb-code reader SPA (web-code/): npm ci (a committed lockfile,
# same discipline as tests/e2e's — see the /dist .gitignore comment above)
# + tsc -b (typecheck) + vite build (→ web-code/dist/, served by
# kb-code-server's own spa::serve fallback) + the vitest unit suite. Mirrors
# `ci-spa` (web/'s own recipe) in shape; deliberately its OWN recipe, OUT of
# the `ci` aggregate — same two-command discipline as `ci-code` above (`just
# ci` proves kb proper untouched; `just ci-code`/`ci-code-spa` prove the
# kb-code SPA scaffold builds + its own tests pass).
ci-code-spa:
    # V70-A7 — `lint:themes` is the kbc-theme/1 contrast oracle (WCAG AA +
    # APCA + Oklab state separation incl. CVD simulation). It runs BEFORE the
    # unit suite so a palette regression fails on its own named step with a
    # table of offending pairs, rather than inside a vitest assertion.
    cd web-code && npm ci && npm run build && npm run lint:themes && npm test

# W4.3 — the Search-Everywhere box's Playwright smoke: a real
# kb-code-server fast-profile binary against a fresh git fixture repo
# (web-code/e2e/fixture-repo.ts), driving the omnibox overlay + the
# /search page. Self-contained under web-code/e2e/ (its own package.json +
# playwright.config.ts), mirroring `ci-e2e` (root tests/e2e's own recipe)
# in shape; deliberately its OWN recipe, OUT of the `ci` aggregate — same
# two-command discipline as `ci-code`/`ci-code-spa` above. `ci-code-spa`
# runs first so the daemon's SPA fallback has a dist/ to serve.
# `--profile fast` (not `--release`): e2e is a functional smoke; shipping
# stays `just redeploy` / `build-embedder` / the image.
ci-code-e2e: ci-code-spa
    cargo build --profile fast -p kb-code-server
    cd web-code/e2e && npm ci
    # --with-deps shells out to the distro package manager (apt); a host
    # without apt-get (a local Arch dev box) falls back to the browser-only
    # install, trusting system deps are already present. NOT the CI runner:
    # that used to be claimed here and in ci.yml, and it was wrong — CI jobs
    # run on GitHub-hosted ubuntu runners with apt and passwordless
    # sudo, so the fallback branch never fires there. The bad assumption
    # cost a red `e2e` job (browsers installed, then failed to launch on a
    # missing libglib-2.0.so.0). Parenthesized so
    # `cd` scopes over both branches of the fallback (bare `a && b || c`
    # would run `c` from the original directory on failure).
    cd web-code/e2e && (npx playwright install chromium --with-deps || npx playwright install chromium)
    cd web-code/e2e && npm test

# Regenerate web/src/api/generated/ from the Rust wire types (ts-rs,
# feature ts-export). TS_RS_LARGE_INT=number — this API serialises
# i64/u64 as JSON numbers. web/src/api/drift.ts typechecks the
# hand-written client.ts types against the output.
types:
    rm -rf web/src/api/generated
    TS_RS_EXPORT_DIR={{justfile_directory()}}/web/src/api/generated TS_RS_LARGE_INT=number cargo test -p kb-core --features ts-export export_bindings
    TS_RS_EXPORT_DIR={{justfile_directory()}}/web/src/api/generated TS_RS_LARGE_INT=number cargo test -p kb-server --features ts-export export_bindings

# Regenerate docs/api-routes.md — the complete METHOD/PATH/handler
# table extracted from crates/kb-server/src/router.rs.
api-docs:
    cargo test -p kb-server --test api_docs -- --ignored

# Drift guard for the route table (same shape as types-check).
api-docs-check: api-docs
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "$(git status --porcelain -- docs/api-routes.md)" ]; then
      git status --short -- docs/api-routes.md
      echo "ERROR: docs/api-routes.md is out of sync — run 'just api-docs' and commit." >&2
      exit 1
    fi

# Drift guard: regenerating must be a no-op against the committed
# bindings. status --porcelain (not bare diff) so newly-exported types
# that haven't been committed yet also fail the check.
types-check: types
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "$(git status --porcelain -- web/src/api/generated)" ]; then
      git status --short -- web/src/api/generated
      echo "ERROR: web/src/api/generated is out of sync — run 'just types' and commit." >&2
      exit 1
    fi

# V76-R4a — regenerate web-code/src/api/generated/ from kb-code-server
# wire types (ts-rs, feature ts-export). Mirrors `types` (the kb SPA
# generator; there is no `gen-ts` recipe). TS_RS_LARGE_INT=number — this
# API serialises i64/u64 as JSON numbers. Committed, so CI can diff it.
gen-ts-code:
    rm -rf web-code/src/api/generated
    TS_RS_EXPORT_DIR={{justfile_directory()}}/web-code/src/api/generated TS_RS_LARGE_INT=number cargo test -p kb-code-server --features ts-export --lib -- export_bindings

# Drift guard: regenerating kb-code bindings must be a no-op against the
# committed files. Own recipe — never folded into `types-check` (the kb
# SPA drift job must stay a separate compile flavor).
gen-ts-code-check: gen-ts-code
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "$(git status --porcelain -- web-code/src/api/generated)" ]; then
      git status --short -- web-code/src/api/generated
      echo "ERROR: web-code/src/api/generated is out of sync — run 'just gen-ts-code' and commit." >&2
      exit 1
    fi

# Build the SPA bundle (web/dist) — required before ci-e2e
ci-spa:
    cd web && npm ci && npm run build

# SPA unit tests + typecheck (vitest, node-only — no Rust, no daemon).
# Covers the pure logic: config deep-merge, lib/ derivations, sort/group.
test-spa:
    cd web && npm ci && npm run typecheck && npm test

# Run the Playwright iframe + SPA smoke against a fast-profile build of
# kb-server. e2e is a functional smoke (keyword + SPA/iframe), not a
# shipping-perf test — `--release` stays for `redeploy` / the image.
# `ci-spa` runs first so the daemon's ServeDir has something to serve.
ci-e2e: ci-spa
    cargo build --profile fast -p kb-server
    # spa-lists-import-cli.spec.ts shells out to the fast-profile `kb`
    # binary (the reading-list import/export round-trip).
    cargo build --profile fast -p kb-cli
    cd tests/e2e && npm ci
    # Same apt-less-host fallback as `ci-code-e2e` above (see its comment):
    # --with-deps shells out to apt-get, which a local Arch box doesn't have;
    # fall back to the browser-only install there. CI's ubuntu runner
    # container always takes the first branch.
    cd tests/e2e && (npx playwright install chromium --with-deps || npx playwright install chromium)
    cd tests/e2e && npm test

# Rebuild the release binary + SPA bundle from the CURRENT tree and restart
# the daemon for CONFIG, so the binary and bundle can never drift apart. A
# stale daemon serving a freshly-rebuilt SPA white-screens when the HTTP
# contract changed under it (the SPA reads a field the old daemon doesn't
# emit) — this rebuilds both halves together and swaps the process.
#   just redeploy /path/to/kb.toml
redeploy CONFIG:
    #!/usr/bin/env bash
    set -euo pipefail
    echo ">>> building release binary (kb-cli)"
    {{prio}} cargo build --release -p kb-cli
    echo ">>> building SPA bundle (web/dist)"
    # Always `npm ci` (see `redeploy-code`'s sibling comment) — the old
    # `[ -d node_modules ] || npm ci` shortcut silently builds against
    # stale deps when a pull only bumps package-lock.json without
    # touching node_modules' mtime.
    ( cd web && npm ci && npm run build )
    KB="$(pwd)/target/release/kb"
    echo ">>> stopping daemon for {{CONFIG}}"
    "$KB" --config "{{CONFIG}}" daemon stop || true
    # Graceful drain can wedge on inflight requests (SIGTERM ignored); force
    # any survivor on this exact config, then WAIT for it to actually leave
    # the process table — starting before it's gone makes the new daemon see
    # a live pid in the pidfile and refuse to boot. (The daemon's own
    # stale-pid detection then clears the leftover pidfile on start.)
    pkill -9 -f -- "--config {{CONFIG}} daemon\$" 2>/dev/null || true
    for _ in $(seq 1 20); do
      pgrep -f -- "--config {{CONFIG}} daemon\$" >/dev/null 2>&1 || break
      sleep 0.3
    done
    echo ">>> starting fresh daemon for {{CONFIG}}"
    log="/tmp/kb-daemon-$(basename "{{CONFIG}}").log"
    nohup "$KB" --config "{{CONFIG}}" daemon >"$log" 2>&1 &
    echo "    started (pid $!); tail -f $log"

# Same idea as `redeploy`, for the sibling kb-code-server daemon: rebuild
# the release binary + web-code SPA bundle from the CURRENT tree, then
# swap the running process, so the two can't drift apart. Unlike `redeploy`
# this takes no CONFIG arg — kb-code-server runs as a single instance off
# its default config (<config-dir>/kb-code.toml; `--config` exists but
# nothing here uses a second instance), so there's exactly one process to
# find and replace. kb-code-cli has no `daemon stop`-equivalent verb (no
# process-lifecycle commands at all — see `kb-code --help`), so the running
# instance is found and killed by matching its exact binary path instead of
# a pidfile.
#   just redeploy-code
redeploy-code:
    #!/usr/bin/env bash
    set -euo pipefail
    echo ">>> building release binary (kb-code-server)"
    {{prio}} cargo build --release -p kb-code-server
    echo ">>> building web-code SPA bundle"
    # Always `npm ci` here (not `redeploy`'s `[ -d node_modules ] || npm ci`
    # shortcut) — a pull that only bumps package-lock.json without touching
    # node_modules' mtime builds against stale deps under that shortcut and
    # fails silently-until-runtime. `npm ci` is cheap/fast when the lockfile
    # is already satisfied (still a full node_modules teardown+reinstall,
    # just a quick one on a warm cache — not literally free).
    ( cd web-code && npm ci && npm run build )
    KBC="$(pwd)/target/release/kb-code-server"
    SPA_DIST="$(pwd)/web-code/dist"
    echo ">>> stopping kb-code-server"
    # `-x` (exact cmdline match, not `redeploy`'s trailing-\$-anchor-only
    # substring match) so a future second instance started with `--config`
    # for isolation — the recipe comment above admits nothing today uses
    # this, but the flag exists — is never caught by this pattern too.
    pkill -TERM -f -x -- "$KBC" 2>/dev/null || true
    # Give the graceful TERM a window, ESCALATE to -9 only if it's still
    # alive after that, then wait AGAIN and confirm departure before
    # starting a replacement. kb-code-server has no pidfile/stale-pid
    # fallback (unlike `redeploy`'s daemon), so this is the only thing
    # standing between a clean handoff and two processes fighting over
    # :4747 — unlike a naive "kill -9 unconditionally right after TERM",
    # this still gives an in-flight request a chance to drain first.
    for _ in $(seq 1 20); do
      pgrep -f -x -- "$KBC" >/dev/null 2>&1 || break
      sleep 0.3
    done
    if pgrep -f -x -- "$KBC" >/dev/null 2>&1; then
      pkill -9 -f -x -- "$KBC" 2>/dev/null || true
      for _ in $(seq 1 20); do
        pgrep -f -x -- "$KBC" >/dev/null 2>&1 || break
        sleep 0.3
      done
    fi
    echo ">>> starting fresh kb-code-server"
    log="/tmp/kb-code-server.log"
    KB_CODE_SPA_DIST="$SPA_DIST" nohup "$KBC" >"$log" 2>&1 &
    pid=$!
    disown
    # Confirm it's actually still alive before declaring success — there's
    # no pidfile/stale-pid net here, so a dead-on-arrival bind failure
    # (e.g. the kill/wait above didn't actually free :4747 in time) would
    # otherwise print "started" for an already-exited pid.
    sleep 0.5
    if kill -0 "$pid" 2>/dev/null; then
      echo "    started (pid $pid); tail -f $log"
    else
      echo "!!! kb-code-server exited immediately after start — check $log" >&2
      exit 1
    fi

# Sync the canon corpus from ~/project/research/kb-research (idempotent)
corpus-canon-sync:
    cp -r ~/project/research/kb-research/sample-artifacts/* corpus/canon/

# Verify *.localhost wildcard DNS resolves (artifact-subdomain prereq).
# Prefer getent; if getent is missing (minimal images) fall back to python3.
check-localhost-dns:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v getent >/dev/null 2>&1; then
      getent hosts s01.artifacts.localhost && exit 0
    elif python3 -c 'import socket; socket.getaddrinfo("s01.artifacts.localhost", 80)' 2>/dev/null; then
      echo "s01.artifacts.localhost resolves"; exit 0
    fi
    echo "FAIL: *.localhost does not resolve — Linux: dnsmasq address=/.localhost/127.0.0.1 or /etc/hosts" >&2
    exit 1

# Verify onnxruntime is loadable (prereq for spike-fastembed)
check-onnxruntime:
    @echo "ORT_DYLIB_PATH=${ORT_DYLIB_PATH:-(unset, fastembed will try system loader)}"
    @if [ -n "${ORT_DYLIB_PATH:-}" ] && [ ! -f "$ORT_DYLIB_PATH" ]; then echo "FAIL: ORT_DYLIB_PATH points to missing file"; exit 1; fi
    @ldconfig -p 2>/dev/null | grep -i onnxruntime || echo "(no system onnxruntime — fallback to ORT_DYLIB_PATH or download upstream)"
