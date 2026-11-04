#!/usr/bin/env bash
# Self-test for scripts/ci/ort-isolated.sh (run by `just ci-selfcheck`).
# Proves, on offline path-only fixture workspaces (cargo resolves, nothing
# builds): a member that enables the ORT feature (directly, through a
# dev-dependency, or through an off-by-default forwarding feature) fails the
# reachability check; a clean workspace passes; a broken positive control
# exits 2. And on synthetic cargo command lines: a combined kb-embedder
# invocation, --workspace without --exclude, a bare root command, and
# local-embedder / --all-features outside -p kb-core or -p kb-embedder fail,
# while the legal shapes pass and the tripwire exits 2 on a near-empty scan.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
fail=0

# check <label> <want-rc> <must-contain-or-empty> <cmd...>
check() {
  local label="$1" want="$2" needle="$3"; shift 3
  "$@" >"$work/out" 2>&1; local rc=$?
  if [ "$rc" -ne "$want" ]; then
    echo "SELFTEST FAIL: $label: want rc $want got $rc" >&2; cat "$work/out" >&2; fail=1; return
  fi
  if [ -n "$needle" ]; then
    local n
    for n in $needle; do
      if ! grep -F -- "$n" "$work/out" >/dev/null; then
        echo "SELFTEST FAIL: $label: output lacks '$n'" >&2; cat "$work/out" >&2; fail=1; return
      fi
    done
  fi
  echo "ok: $label"
}

# ---- reachability fixtures -------------------------------------------------
mkdir -p "$work/stub/ort-sys/src"
printf '[package]\nname = "ort-sys"\nversion = "0.1.0"\nedition = "2021"\n' > "$work/stub/ort-sys/Cargo.toml"
: > "$work/stub/ort-sys/src/lib.rs"

crate() { # crate <ws> <name> <extra-toml...>
  local ws="$1" name="$2"; shift 2
  mkdir -p "$ws/$name/src"; : > "$ws/$name/src/lib.rs"
  { printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\n' "$name"; printf '%s\n' "$@"; } > "$ws/$name/Cargo.toml"
}
FXCORE=('[dependencies]' "ortstub = { package = \"ort-sys\", path = \"$work/stub/ort-sys\", optional = true }" '[features]' 'local = ["dep:ortstub"]')
OWNER=('[dependencies]' 'fx-core = { path = "../fx-core", features = ["local"] }')
mkws() { # mkws <dir> <members...>   (crates must exist already); resolves the lock
  local ws="$1"; shift
  local ml; ml="$(printf '"%s", ' "$@")"
  printf '[workspace]\nresolver = "2"\nmembers = [%s]\n' "${ml%, }" > "$ws/Cargo.toml"
  if ! cargo generate-lockfile --offline --manifest-path "$ws/Cargo.toml" >"$work/gen.out" 2>&1; then
    echo "SELFTEST FAIL: fixture setup for $ws: cargo generate-lockfile failed" >&2; cat "$work/gen.out" >&2; fail=1
  fi
}
reach() { # reach <label> <want-rc> <needle> <ws>
  ORT_ISOLATED_OWNER=fx-owner ORT_ISOLATED_BANNED=ort-sys ORT_ISOLATED_FEATURE_HOME=fx-core \
    check "$1" "$2" "$3" bash "$here/ort-isolated.sh" --only reachability --manifest-path "$4/Cargo.toml"
}
base() { # base <ws>: owner + core + clean
  crate "$1" fx-core "${FXCORE[@]}"
  crate "$1" fx-owner "${OWNER[@]}"
  crate "$1" fx-clean '[dependencies]' 'fx-core = { path = "../fx-core" }'
}

ws="$work/r1"; base "$ws"; mkws "$ws" fx-owner fx-core fx-clean
reach "reach: clean workspace passes" 0 "" "$ws"

ws="$work/r2"; base "$ws"
crate "$ws" fx-leaky '[dependencies]' 'fx-core = { path = "../fx-core", features = ["local"] }'
mkws "$ws" fx-owner fx-core fx-clean fx-leaky
# invariant:26 a member that re-enables the ORT feature fails the supply-chain gate
reach "reach: a member that enables the feature fails" 1 "fx-leaky ort-sys" "$ws"

ws="$work/r3"; base "$ws"
crate "$ws" fx-devleak '[dev-dependencies]' 'fx-core = { path = "../fx-core", features = ["local"] }'
mkws "$ws" fx-owner fx-core fx-clean fx-devleak
reach "reach: a dev-dependency that enables the feature fails" 1 "fx-devleak" "$ws"

ws="$work/r4"; base "$ws"
crate "$ws" fx-fwd '[dependencies]' 'fx-core = { path = "../fx-core" }' '[features]' 'local = ["fx-core/local"]'
mkws "$ws" fx-owner fx-core fx-clean fx-fwd
reach "reach: an off-by-default forwarding feature fails under all-features" 1 "fx-fwd all-features" "$ws"

ws="$work/r5"
crate "$ws" fx-core "${FXCORE[@]}"
crate "$ws" fx-owner '[dependencies]' 'fx-core = { path = "../fx-core" }'
crate "$ws" fx-clean '[dependencies]' 'fx-core = { path = "../fx-core" }'
mkws "$ws" fx-owner fx-core fx-clean
reach "reach: control, owner not reaching the banned crate exits 2" 2 "control failed" "$ws"

# ---- topology fixtures -----------------------------------------------------
n=0
topo() { # topo <label> <want-rc> <needle> <relfile> <content>
  n=$((n + 1)); local d="$work/topo-$n"
  mkdir -p "$d/$(dirname "$4")"; printf '%s\n' "$5" > "$d/$4"
  ORT_ISOLATED_MIN_CARGO_LINES=0 check "$1" "$2" "$3" bash "$here/ort-isolated.sh" --only topology --root "$d"
}
BS=$'\\'
topo "topo: combined -p in one Dockerfile RUN fails" 1 "Dockerfile:2: (a)" Dockerfile \
"FROM scratch
RUN cargo build --release -p kb-cli --bin kb -p kb-embedder --bin kb-embedder $BS
    && cp target/release/kb /tmp/kb"
topo "topo: the real split shape passes" 0 "" Dockerfile \
"FROM scratch
RUN --mount=type=cache,target=/usr/local/cargo/registry $BS
    cargo build --release -p kb-cli --bin kb $BS
    && strip target/release/kb
RUN --mount=type=cache,target=/usr/local/cargo/registry $BS
    cargo build --release $BS
    -p kb-embedder --bin kb-embedder $BS
    && strip target/release/kb-embedder"
topo "topo: an && chain of two invocations passes" 0 "" Dockerfile \
"RUN cargo build -p kb-cli && cargo build -p kb-embedder"
topo "topo: --workspace without --exclude kb-embedder fails" 1 "justfile:" justfile \
"ci:
    cargo test --workspace --no-fail-fast"
topo "topo: --workspace with --exclude kb-embedder passes" 0 "" justfile \
"ci:
    cargo test --workspace --exclude kb-embedder --no-fail-fast"
topo "topo: a bare cargo build at the root fails" 1 "(b)" Dockerfile \
"RUN cargo build --release"
topo "topo: a selection from a shell variable is not judged" 0 "" scripts/x.sh \
'cargo nextest run --profile ci "${ARGS[@]}"'
topo "topo: a workflow run block is scanned" 1 ".github/workflows/x.yml:" .github/workflows/x.yml \
"jobs:
  a:
    steps:
      - name: build
        run: |
          cargo build -p kb-cli -p kb-embedder"
topo "topo: step names and comments are not commands" 0 "" .github/workflows/x.yml \
"jobs:
  a:
    steps:
      # Building kb-cli and kb-embedder together unifies the feature
      - name: cargo clippy (embedder + kb-core/local-embedder)
        run: cargo clippy -p kb-core --features local-embedder --all-targets -- -D warnings"
topo "topo: local-embedder with -p kb-server fails" 1 "(c)" justfile \
"x:
    cargo clippy -p kb-server --features kb-core/local-embedder"
topo "topo: local-embedder with --workspace fails" 1 "" justfile \
"x:
    cargo test --workspace --exclude kb-embedder --features kb-core/local-embedder"
topo "topo: --all-features over the workspace fails" 1 "" justfile \
"x:
    cargo test --workspace --exclude kb-embedder --all-features"
topo "topo: --all-targets is not --all and fmt is not a build" 0 "" justfile \
"x:
    cargo clippy -p kb-embedder --all-targets -- -D warnings
    cargo fmt --all -- --check"
topo "topo: cargo install of an external tool passes" 0 "" justfile \
"x:
    cargo install --locked cargo-about --features cli"
topo "topo: kb-embedder alone with kb-core/local-embedder passes" 0 "" justfile \
"x:
    cargo build -p kb-embedder --features kb-core/local-embedder"

n=$((n + 1)); d="$work/topo-$n"; mkdir -p "$d"; printf 'x:\n    cargo build -p kb-cli\n' > "$d/justfile"
check "tripwire: fewer than MIN_CARGO_LINES commands exits 2" 2 "" bash "$here/ort-isolated.sh" --only topology --root "$d"
exit "$fail"
