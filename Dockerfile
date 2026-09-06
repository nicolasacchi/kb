# syntax=docker/dockerfile:1.7
# Multi-stage build: rust:1.96 builder → debian:bookworm-slim runtime.
#
# Outputs a single image with the `kb` binary at /usr/local/bin/kb and the
# `kb-embedder` sidecar beside it, plus the bge-large-en-v1.5 model so
# semantic and hybrid search work out of the box. The React SPA is built in a
# Node stage and baked in at /usr/local/share/kb/web/dist (KB_SPA_DIST), so the
# daemon serves the full web UI out of the box — no dist mount needed. The CLI
# runs `kb daemon` by default; other subcommands (search, add, tui) still work
# via `docker exec`. Track-D bake-off recommendation: bge-large for
# technical-English corpora; image size ~1.7 GB (was ~400 MB on
# bge-small) — see docs/research/foundation/14-embedding-bakeoff-2026-05-19.html.
#
# ONNX Runtime is isolated to `kb-embedder` (invariant #26) and STATICALLY
# bundled via fastembed's `ort-download-binaries` feature — `ort-sys` fetches a
# known-good ONNX Runtime from a CDN at BUILD time and links it, so the binary
# is self-contained (no `libonnxruntime.so` at runtime, no `ORT_DYLIB_PATH`).
# `kb`/`kb-server` link no ORT at all. For an offline/air-gapped build,
# set `ORT_STRATEGY=system` + `ORT_LIB_LOCATION=<dir>` to link a local copy.
#
# Both stages are on Debian TRIXIE (glibc 2.41), not bookworm (2.36): the
# prebuilt ORT 1.24.2 archive ort-sys links (ort 2.0.0-rc.x) references the C23
# `__isoc23_strtol` family, versioned `GLIBC_2.38` — absent on bookworm, so the
# kb-embedder link fails there with misleading "undefined reference"s pinned to
# the archive's own .o members (onnx::OnnxParser / NodeStatsRecorder / absl::…).
# trixie's glibc 2.41 supplies them; the runtime stage MUST match the builder's
# glibc floor (a trixie-built binary needs GLIBC_2.38, which bookworm-slim lacks).
#
# Every FROM below pins the tag to its resolved multi-arch index digest
# (`docker buildx imagetools inspect <ref>`'s top-level `Digest:` line), so a
# workflow re-run of the SAME commit pushes the SAME bits under the SAME
# `main-<sha12>` tag — a floating tag (rust:1.96-trixie, node:22-bookworm-slim,
# debian:trixie-slim) can move under us between two CI runs of one commit
# otherwise. The tag stays alongside the digest for human readability; it is
# not what Docker resolves. To refresh a pin (e.g. an upstream security
# patch), re-run `docker buildx imagetools inspect <image>:<tag>` and swap in
# the new digest — node:22-bookworm-slim and debian:trixie-slim each appear
# twice below (kb + kb-code stages); both occurrences of one tag share ONE
# digest, update them together.

# --- Shared workspace stage --------------------------------------------
#
# Everything both branches below need: toolchain + apt deps + the fetched,
# dep-cached workspace source tree. `builder` (kb-cli + kb-embedder + the
# model download) and `kb-code-builder` (kb-code-server) each `FROM
# workspace`, so a kb-code image build never drags kb's unrelated
# embedder+model tail through on a cache miss, and vice versa — see the
# split-point comment at `builder` below.
FROM rust:1.96-trixie@sha256:1f0dbad1df66647807e6952d1db85d0b2bda7606cb2139d82517e4f009967376 AS workspace

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        cmake \
        libssl-dev \
        protobuf-compiler \
        libprotobuf-dev \
        curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Bound cargo's crate-level parallelism inside the image build. The dind
# builder shares the two-slot runner box with whatever the other slot is
# compiling; an unbounded release build of the datafusion/lance graph peaks
# past the box's free RAM and the OOM killer SIGKILLs rustc mid-crate
# (observed 2026-08-13, run 31724787163: three datafusion crates killed at
# once). The cache mounts make warm rebuilds cheap regardless of how low
# this goes. Set here (on the shared `workspace` stage) so BOTH `builder`
# and `kb-code-builder` inherit it — neither redeclares it.
#
# SECOND reduction (4 → 2): four jobs was still not enough — run
# 31733040591's build-image job SIGKILLed kb-core + lance simultaneously at
# t+1178s. The release profile's lto="thin" + codegen-units=1 makes each
# parallel rustc job memory-heavy, so concurrency is the lever here (the
# profile itself is a deliberate shipping-binary-quality setting).
#
# THIRD reduction (2 → 1): two jobs still OOM'd — run 31752473030 SIGKILLed
# lancedb + lance simultaneously at t+198s (exactly two concurrent crates,
# so -j2 was in effect). The lance/lancedb/datafusion/kb-core libs are each
# heavy enough under codegen-units=1 that ANY two of them overlapping can
# exceed the dind builder's share of the two-slot box. One job fully
# serializes crate compilation: slowest, but no concurrent-rustc peak at
# all. The target/ cache mount carries partial progress between runs, so a
# serialized build still finishes well inside the 120-minute job timeout.
#
# BACK UP to 2 (2026-08-16): the whole -j4→-j2→-j1 hunt was aimed at the
# wrong cgroup. Kernel logs on the runner host show every rustc kill
# landed in the DIND SIDECAR's 4g scope (the build executes inside the
# nested daemon; the 10g runner cgroup never appears in the kill log) —
# and that sidecar's limit is now 8g hard + 12g memswap, sized for
# exactly two concurrent heavy crates (see kb-ci-runner/compose.yaml's
# own comment on the runner host). If -j2 OOMs again, the evidence says
# raise the dind limit further or drop back here — the kill log names
# the cgroup either way.
ENV CARGO_BUILD_JOBS=2

# Copy the workspace metadata first so dependency compilation can be
# cached separately from source changes. The `crates/*/Cargo.toml`
# files come along but the source under `crates/*/src/` does not yet.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/kb-core/Cargo.toml crates/kb-core/Cargo.toml
COPY crates/kb-server/Cargo.toml crates/kb-server/Cargo.toml
COPY crates/kb-cli/Cargo.toml crates/kb-cli/Cargo.toml
COPY crates/kb-embedder/Cargo.toml crates/kb-embedder/Cargo.toml
COPY crates/kb-code-server/Cargo.toml crates/kb-code-server/Cargo.toml
COPY crates/kb-code-cli/Cargo.toml crates/kb-code-cli/Cargo.toml
COPY crates/kb-lip/Cargo.toml crates/kb-lip/Cargo.toml
COPY crates/kb-buildstamp/Cargo.toml crates/kb-buildstamp/Cargo.toml

# Stub-out every crate's src so `cargo fetch` + a no-op pre-build
# warm the dep graph cache. The real sources land in the next layer.
# kb-code-server/kb-code-cli (the sibling daemon, never built into this
# image) still need stubs — they're workspace members, so `cargo fetch
# --locked` refuses to resolve the workspace without their manifests+targets.
RUN mkdir -p crates/kb-core/src crates/kb-server/src crates/kb-cli/src crates/kb-embedder/src \
        crates/kb-code-server/src crates/kb-code-cli/src crates/kb-lip/src/bin \
        crates/kb-buildstamp/src \
    && echo '' > crates/kb-buildstamp/src/lib.rs \
    && echo 'fn main() {}' > crates/kb-cli/src/main.rs \
    && echo 'fn main() {}' > crates/kb-server/src/main.rs \
    && echo 'fn main() {}' > crates/kb-embedder/src/main.rs \
    && echo 'fn main() {}' > crates/kb-code-server/src/main.rs \
    && echo 'fn main() {}' > crates/kb-code-cli/src/main.rs \
    && echo 'fn main() {}' > crates/kb-lip/src/main.rs \
    && echo 'fn main() {}' > crates/kb-lip/src/bin/fake_lsp.rs \
    && echo '' > crates/kb-lip/src/lib.rs \
    && echo '' > crates/kb-core/src/lib.rs \
    && echo '' > crates/kb-server/src/lib.rs \
    && echo '' > crates/kb-code-server/src/lib.rs \
    && mkdir -p crates/kb-core/benches \
    && echo 'fn main() {}' > crates/kb-core/benches/perf.rs \
    && cargo fetch --locked

# Bring in the real sources. Both downstream stages build straight off
# this tree (no further COPY of it), so the dep-fetch cache above and this
# source layer are shared, byte-identical, by `builder` and
# `kb-code-builder` below.
COPY crates ./crates

# --- kb builder stage ---------------------------------------------------
#
# Continues from `workspace` into the kb-cli + kb-embedder release build
# and the 1.34 GB model download below — the heavy, kb-only tail that
# `kb-code-builder` (further down) branches away from AT `workspace`
# instead of here, so a kb-code image build never forces this unrelated
# compile+download in front of it on a cache miss (kb-code-runtime ships
# neither the embedder nor the model — see that stage's own comment).
# `kb-embedder` is the I1 subprocess the daemon spawns for ONNX inference
# at nice 20 (CPU isolation from the request path) — needs to ship next
# to `kb`.
FROM workspace AS builder

# Inject the git sha for the build-stamp: `.git` is excluded from the build
# context, so kb-server's build.rs can't probe it here. Passed via
# `--build-arg KB_GIT_SHA=...`; defaults to "unknown" for a bare build.
ARG KB_GIT_SHA=unknown
ENV KB_BUILD_SHA=${KB_GIT_SHA}
# Cache mounts: target/ is NOT in the image layer, so each cargo RUN
# copies the stripped binary to /tmp/<bin> (which is). Downstream
# COPY --from must read /tmp/<bin>, never target/release/. Two separate
# cargo invocations (invariant #26 — never unify `local-embedder` into
# kb). Before trusting a cache-mounted image as main-latest, run the
# deploy verification protocol: build twice from one commit, docker
# create + docker cp both /usr/local/bin/kb, compare --version + sizes,
# byte-compare SPA dist and model tree, smoke `daemon --help`.
RUN --mount=type=cache,id=kb-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=kb-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=kb-target,target=/build/target,sharing=locked \
    cargo build --release -p kb-cli --bin kb \
    && strip target/release/kb \
    && cp target/release/kb /tmp/kb
RUN --mount=type=cache,id=kb-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=kb-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=kb-target,target=/build/target,sharing=locked \
    cargo build --release -p kb-embedder --bin kb-embedder \
    && strip target/release/kb-embedder \
    && cp target/release/kb-embedder /tmp/kb-embedder

# Pre-fetch the default embedding model into the image's cache so the
# daemon serves semantic + hybrid search without a first-run download.
# XDG_CACHE_HOME matches the runtime stage, so the cache tree lands at
# the path the daemon resolves (`<XDG_CACHE_HOME>/kb/models/`).
# Binary lives at /tmp/kb (cache-mounted target/ is gone after the RUN).
ENV XDG_CACHE_HOME=/var/lib/kb/cache
RUN /tmp/kb model download bge-large-en-v1.5

# --- SPA build stage --------------------------------------------------
#
# Builds the React SPA (web/) into web/dist as static assets, copied into the
# runtime image below. Node's glibc is irrelevant here — only static files
# cross into runtime, never a binary, so a slim Node base is fine.
FROM node:22-bookworm-slim@sha256:d649c27dae7ba0137b3cef5dd75baa422c08dc3d9e3fc0c23dfb172dc3cc6436 AS spa
WORKDIR /web
# Lockfile-first so `npm ci`'s layer is cached until deps actually change.
COPY web/package.json web/package-lock.json ./
RUN npm ci
# Now the sources. `.dockerignore` excludes web/node_modules + web/dist, so
# this neither clobbers the installed deps nor ships a stale prior build.
COPY web/ ./
# Stamp the same commit the daemon binary carries (KB_BUILD_SHA) so the SPA's
# build-drift banner compares like-for-like. `.git` is out of the build
# context, so vite's git probe reads this env var instead (vite.config.ts).
ARG KB_GIT_SHA=unknown
ENV KB_GIT_SHA=${KB_GIT_SHA}
RUN npm run build

# --- kb-code stages (W4.8 — hosted kbc.example.com) ------------------------
#
# The SIBLING daemon (crates/kb-code-server + web-code/) gets its own
# image, built from the SAME Dockerfile via an explicit compose
# `target: kb-code-runtime`. These stages sit BEFORE kb's own `runtime`
# stage below ON PURPOSE: a bare `docker compose build kb` (no target)
# builds the LAST stage, which must remain kb's runtime — reordering
# would silently ship the wrong daemon to kb.example.com.
#
# `FROM workspace`, NOT `FROM builder`: it reuses the shared stage's
# fetched dep graph + COPY'd sources (kb-code-server links kb-core →
# lance → the same protoc need) but branches BEFORE builder's
# kb-cli/kb-embedder release build + 1.34 GB model download tail (see
# `builder`'s split-point comment above) — so this adds exactly one
# cargo invocation, not a second toolchain setup AND not that unrelated
# tail. No embedder, no ONNX Runtime, no model download ships in this
# image — kb-code's semantic lane stays off in the hosted config
# (governance: the W5.5 bench decides that default, not a deploy).
FROM workspace AS kb-code-builder

# Inject the git sha for the build-stamp: `.git` is excluded from the build
# context, so kb-code-server's own `option_env!("KB_BUILD_SHA")` read can't
# probe it here either. Same `--build-arg KB_GIT_SHA=...` the `builder` stage
# takes above; ARGs are per-stage, so this stage needs its own declaration.
ARG KB_GIT_SHA=unknown
ENV KB_BUILD_SHA=${KB_GIT_SHA}
RUN --mount=type=cache,id=kb-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=kb-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=kb-target,target=/build/target,sharing=locked \
    cargo build --release -p kb-code-server \
    && strip target/release/kb-code-server \
    && cp target/release/kb-code-server /tmp/kb-code-server

FROM node:22-bookworm-slim@sha256:d649c27dae7ba0137b3cef5dd75baa422c08dc3d9e3fc0c23dfb172dc3cc6436 AS kb-code-spa
WORKDIR /web-code
COPY web-code/package.json web-code/package-lock.json ./
RUN npm ci
COPY web-code/ ./
RUN npm run build

FROM debian:trixie-slim@sha256:3a39a0592364683e6bab97937b72cad5a8fa6dcbbee90edb3bb48c7f8e94f258 AS kb-code-runtime
ARG KB_GIT_SHA=unknown
LABEL org.opencontainers.image.title="kb-code" \
      org.opencontainers.image.description="Read-first code browsing daemon (session-aware blame, search-everywhere, agent verbs)." \
      org.opencontainers.image.source="https://github.com/nicolasacchi/kb" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${KB_GIT_SHA}"

# git: the blame/diff/checkout services shell out to a real git binary
# (ADR-4); the repos arrive as read-only bind-mounts owned by uid 1000,
# which matches the container user, so safe.directory is satisfied.
# curl: the /healthz HEALTHCHECK probe.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3 \
        curl \
        git \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 1000 -d /var/lib/kb -s /sbin/nologin kb \
    && mkdir -p /var/lib/kb/state /var/lib/kb/config /var/lib/kb/cache \
    && chown -R kb:kb /var/lib/kb

COPY --from=kb-code-builder /tmp/kb-code-server /usr/local/bin/kb-code-server
COPY --from=kb-code-spa --chown=kb:kb /web-code/dist /usr/local/share/kb-code/web-code/dist

USER kb
# KbPaths' config root is the fixed app-name `kb` dir (kb-code.toml lives
# BESIDE kb.toml, distinguished by filename — see kb-code-server's main.rs),
# so the same XDG env kb's runtime uses resolves the default --config path.
ENV XDG_STATE_HOME=/var/lib/kb/state \
    XDG_CONFIG_HOME=/var/lib/kb/config \
    XDG_CACHE_HOME=/var/lib/kb/cache \
    KB_CODE_SPA_DIST=/usr/local/share/kb-code/web-code/dist \
    RUST_LOG=warn,kb_code_server=info

EXPOSE 4747

HEALTHCHECK --interval=30s --timeout=5s --start-period=40s --retries=3 \
    CMD curl -fsS http://127.0.0.1:4747/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/kb-code-server"]

# --- Runtime stage ----------------------------------------------------

FROM debian:trixie-slim@sha256:3a39a0592364683e6bab97937b72cad5a8fa6dcbbee90edb3bb48c7f8e94f258 AS runtime

# OCI image annotations (consumed by registries, `docker inspect`, and
# provenance tooling). KB_GIT_SHA is re-declared here because ARGs are
# per-stage; it carries the build commit into the image version label.
ARG KB_GIT_SHA=unknown
LABEL org.opencontainers.image.title="kb" \
      org.opencontainers.image.description="Personal search engine for LLM-generated HTML/Markdown artifacts (hybrid BM25 + vector)." \
      org.opencontainers.image.source="https://github.com/nicolasacchi/kb" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${KB_GIT_SHA}"

# libgomp1: ONNX Runtime's CPU execution provider links OpenMP.
# git: Track V's version timeline shells out to `git log`/`git show` for
# git-backed corpora (e.g. /srv/research). The container user is uid 1000,
# matching the host owner of the bind-mounted `.git` dirs, so git's
# safe.directory guard is satisfied; corpora without a `.git` fall back to
# index snapshots. Absent git, the daemon degrades to snapshots either way.
# curl: the HEALTHCHECK probe below hits the unauthenticated /healthz route.
# (The builder stage's curl doesn't carry into runtime; trixie-slim ships
# neither curl nor wget by default.)
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3 \
        libgomp1 \
        curl \
        git \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 1000 -d /var/lib/kb -s /sbin/nologin kb \
    && mkdir -p /var/lib/kb/state /var/lib/kb/config /var/lib/kb/cache \
    && chown -R kb:kb /var/lib/kb

COPY --from=builder /tmp/kb /usr/local/bin/kb
COPY --from=builder /tmp/kb-embedder /usr/local/bin/kb-embedder
COPY --from=builder --chown=kb:kb /var/lib/kb/cache /var/lib/kb/cache
# The prebuilt SPA bundle. KB_SPA_DIST (below) points the daemon's
# resolve_spa_dist() straight at it, so the web UI serves out of the box.
COPY --from=spa --chown=kb:kb /web/dist /usr/local/share/kb/web/dist

USER kb
ENV XDG_STATE_HOME=/var/lib/kb/state \
    XDG_CONFIG_HOME=/var/lib/kb/config \
    XDG_CACHE_HOME=/var/lib/kb/cache \
    KB_SPA_DIST=/usr/local/share/kb/web/dist \
    RUST_LOG=warn,kb=info

EXPOSE 4000

# Liveness probe — hit the unauthenticated /healthz on container-loopback.
# It's I/O-free (no storage/embedder) so it reports "process is accepting
# connections", not corpus readiness. The daemon only accepts connections
# after per-kb bring-up, so a generous start-period covers a cold-boot
# reconcile before the first probe counts against `retries`.
HEALTHCHECK --interval=30s --timeout=5s --start-period=40s --retries=3 \
    CMD curl -fsS http://127.0.0.1:4000/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/kb"]
CMD ["daemon", "--config", "/var/lib/kb/config/kb/kb.toml"]
