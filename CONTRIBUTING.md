# Contributing to kb

kb is a self-hosted daemon that indexes, searches, and serves a personal
collection of LLM-generated HTML/Markdown artifacts — see
[README.md](README.md) for the user-facing surface and
[docs/research/index.html](docs/research/index.html) for the design
rationale behind it. This is a small, opinionated project; the bar for a
patch is green CI and a clear commit message.

## Developer Certificate of Origin (DCO) — sign off your commits

kb uses the [Developer Certificate of Origin](https://developercertificate.org/)
(the same lightweight mechanism the Linux kernel, GitLab, and many CNCF
projects use) instead of a heavyweight CLA. Every commit must carry a
`Signed-off-by` line certifying you wrote the patch (or have the right to
submit it under the project's MIT license):

```
Signed-off-by: Jane Doe <jane@example.com>
```

Add it automatically with `-s`:

```bash
git commit -s -m "fix(kb-core): handle empty corpus on first index"
```

The full text you certify is in [CLA.md](CLA.md). A CI check
(`.github/workflows/dco.yml`) verifies every commit in a PR is signed off —
**this is enforced**, not a formality; a PR with an unsigned commit is red.

## Building + testing

`justfile` is the canonical command list. Essentials:

```bash
just ci          # workspace fmt + clippy + tests
just ci-spa      # web/ npm ci + npm run build → web/dist/
just ci-code     # the kb-code sibling daemon's own fmt + clippy + tests
```

For the inner loop, `cargo build --profile fast -p kb-cli` (no LTO,
`codegen-units=16`, ~3x faster than `--release`) is enough to try a change;
reserve `--release` for the binary you'll actually run. Prefix any build
likely to run past 30s with `nice -n 20 ionice -c 3` (a full release link +
the lance/datafusion compile runs 20+ minutes cold) so it doesn't starve
everything else on the box.

**Always `--exclude kb-embedder`** on a workspace sweep — `cargo test
--workspace --exclude kb-embedder --no-fail-fast` /
`cargo clippy --workspace --exclude kb-embedder --all-targets -- -D warnings`.
kb-embedder is the only crate that links ONNX Runtime (via kb-core's
`local-embedder` feature); a bare `cargo test --workspace` unifies that
feature ON across the shared kb-core rlib and pulls a statically-linked ONNX
Runtime into every downstream crate — slow to link and not the real shipping
shape. The embedder has its own recipe: `just ci-embedder`.

New to the codebase? Start with [README.md](README.md), then
[docs/architecture-invariants.md](docs/architecture-invariants.md) — read the
matching entry before changing a load-bearing subsystem.

## CI

`.github/workflows/ci.yml` runs 7 jobs on every push/PR to `main`: `workspace`
(fmt + clippy + test), `e2e` (Playwright iframe/SPA smoke), `embedder` (the
ORT-linking surface), `code` + `code-e2e` (the kb-code sibling daemon + its
SPA), `web-unit` (SPA typecheck + vitest), and `supply-chain` (`cargo-deny`).
A separate workflow (`dco.yml`) gates the DCO sign-off described above. All of
it must be green before merge.

## Commit + PR conventions

- **Conventional-commit subject:** `feat(crate): summary`, else `fix:` /
  `docs:` / `chore:` / `feat(spa):`. If your change is one phase of a larger,
  multi-commit piece of work, name the phase: `feat(crate): summary (PhaseID)`.
- **Sign off every commit** (`git commit -s`) — the DCO check gates it.
- **Co-Authored-By trailer when AI drove the change.** If an AI coding agent
  wrote some or all of a commit, add a trailer naming the model that drove it
  (e.g. `Co-Authored-By: Claude <noreply@anthropic.com>`) — don't attribute a
  different model than the one that actually did the work.
- Keep PRs focused, phased, each green on its own; kb ships as a sequence of
  small green-CI commits on `main`, not long-lived feature branches.
- If you change the HTTP API, run `just api-docs` so
  [docs/api-routes.md](docs/api-routes.md) stays current
  (`just api-docs-check` gates it).
- **PRs from forks run on GitHub-hosted runners** (standard `ubuntu-latest`
  minutes, free on a public repo) — nothing in `ci.yml`'s pull-request path
  touches self-hosted infrastructure. Only push-to-`main`/schedule/dispatch
  jobs (image builds, nightly benches) run on a maintainer-controlled runner,
  and those are gated to this repository, never a fork's PR.

## Where to start

kb has no generic plugin SDK, but it does have named extension surfaces —
full picture in [docs/extending.md](docs/extending.md). Three of them need
**no coordination with core maintainers** at all, because they never touch
the storage actor, the atlas, or the security stack:

- **Agent-layer plugins** (`plugins/<name>/`) — Claude Code hooks/skills/commands
  that just drive the `kb` CLI against a running daemon. See the existing
  `kb-memory`/`kb-research`/`kb-comments`/`kb-reflect`/`kb-code` plugins for
  the shape.
- **Subprocess sidecars** — an out-of-process plugin speaking newline-delimited
  JSON over stdio, the same pattern the embedder itself uses. Any heavy,
  optional, or foreign-language extension (an alternate embedder, a new file
  extractor) fits here.
- **Sibling daemons** (`kb-sibling/1`) — a separate deployed binary with its
  own storage that calls kb over HTTP, the pattern kb-code follows. See
  [docs/extending.md § 5](docs/extending.md) for the identity-handshake
  contract two independently-deployed binaries must honor.

A change to core kb itself — a new route, a storage schema change, anything
touching the indexer or the security middleware — is a normal PR against
`crates/`, but read [docs/architecture-invariants.md](docs/architecture-invariants.md)
first: it documents the load-bearing constraints (single-writer-per-kb
storage, deterministic atlas, fail-closed security) that break silently at
runtime rather than at compile time.

Note: the `CLAUDE.md` files throughout this repo (root, `crates/kb-core/`)
are instructions for AI coding agents working in this codebase, not
contributor documentation — useful background reading, but not written for a
human contributor's first PR.

## License

kb is MIT-licensed ([LICENSE](LICENSE)). Contributions are accepted under
the same license, as certified by your DCO sign-off.
