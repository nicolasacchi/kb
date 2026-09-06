# kb

**kb is a self-hosted system of record for everything your AI writes.** One
Rust daemon watches folders of HTML and Markdown, indexes them with hybrid
BM25 + vector search, and serves them over a CLI, an HTTP API, and a
single-page web reader.

It is built for two users at once — a person and their coding agent — and keeps
three ledgers for both: the **deliverables** an agent writes (served, searched,
sandbox-isolated one origin per artifact); **episodic sessions**, transcripts
captured and digest-indexed, pulled on demand with `kb why <file>` / `kb
recollect <q>` and never auto-injected into a prompt; and **curated memory**,
facts an agent chose to keep, recalled by rank × salience × decay at zero
per-call LLM cost.

Every surface — reader, CLI, capture hooks, inline comments — is a view of that
one record. Nothing is summarised by a model inside the daemon: ranking, recall
and digests are reproducible from their inputs. The daemon binds loopback by
default and refuses a token-less public bind, so it is safe behind a TLS reverse
proxy on day one.

**Who it is for:** someone who runs their own machines, generates a lot of
AI-written material, and wants it in plain files they can `grep`, `git` and
outlive the tool with. Not a hosted memory API and not a capture plugin — the
store is your filesystem, recall costs no tokens, and it is all MIT.

### Concepts

| noun | what it is |
|---|---|
| **artifact** | a rendered HTML or Markdown file — the atom kb indexes, serves, versions and comments on |
| **corpus** (a *kb*) | a watched source folder named in `kb.toml`; one daemon serves many, and every federated read fans out across all of them |
| **daemon** | the single `kb daemon` process: watcher, indexer, search, HTTP API, SSE event bus |
| **reader** | the built-in web SPA — gallery, search, atlas, the artifact reader with inline comments |
| **CLI** | `kb`, the only supported write path. Plugins and agents shell out to it; there is no in-daemon LLM to talk to |
| **memory** | a curated agent-explicit fact in a memory corpus, written by `kb remember`, ranked by `kb recall` |
| **session** | one captured agent transcript, indexed as a digest and pulled on demand — episodic, never ambient |
| **slate** | a project's shared *working* state (who is on what, open questions, dead ends), append-only and daemon-wide |
| **kb-code** | the sibling daemon that reads a *git checkout* the way kb reads a corpus — separate binary, separate port |

## Install

> Status: the first public tag has not been cut yet. Until it is, no release
> tarball or image is published and building from source (option C below) is
> the working path.

```bash
curl -fsSL https://raw.githubusercontent.com/nicolasacchi/kb/main/scripts/install.sh | sh
```

Detects your OS/arch, downloads the matching release tarball, verifies its
checksum, puts both binaries in `~/.local/bin`. `PREFIX=` overrides the root,
`KB_VERSION=` pins a release, `KB_BASE_URL=` points at a mirror. Other channels:

- **Release tarball** — `kb-<version>-<target>.tar.gz` (+ `.sha256`) from
  [GitHub Releases](https://github.com/nicolasacchi/kb/releases), for
  `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, containing `kb`
  and `kb-embedder`; the sibling daemon ships beside it as
  `kb-code-<version>-<target>.tar.gz`. Neither bundles the web UI — that is a
  `just ci-spa` build.
- **Docker** — `docker pull ghcr.io/nicolasacchi/kb` (and
  `ghcr.io/nicolasacchi/kb-code`): binaries, SPA and an embedding model baked
  in, the self-contained channel — [`docs/self-host.md`](docs/self-host.md).
- **From source** — needs Rust (pinned in `rust-toolchain.toml`) and **protoc**:

  ```bash
  git clone https://github.com/nicolasacchi/kb && cd kb
  cargo build --release -p kb-cli
  cargo build --release -p kb-embedder      # a SEPARATE invocation, on purpose
  install target/release/kb target/release/kb-embedder ~/.local/bin/
  ```

  ONNX Runtime is isolated to `kb-embedder`; building it in the same invocation
  as `kb` would link ORT into the daemon
  ([`docs/packaging.md`](docs/packaging.md)).

> **Prebuilt binaries need glibc ≥ 2.39** (Debian 13+, Ubuntu 24.04+, Fedora
> 40+) — `kb-embedder` statically bundles ONNX Runtime, which sets that floor.
> On older glibc (Debian 12, Ubuntu 22.04, RHEL 9) or musl (Alpine) use the
> Docker image or build from source; `install.sh` detects an incompatible libc
> and says so rather than failing silently.

## Try it in 60 seconds

```bash
kb daemon                              # binds 127.0.0.1:4000 — no token needed locally
kb add ~/notes --kb notes              # watch a folder as a corpus
kb search "the thing I wrote" --kb notes
```

Open <http://127.0.0.1:4000/> for the reader, or call
`/api/search?q=…&kb=notes` directly. Before binding anything reachable, generate
a bearer token (`kb token generate`) and front the daemon with a TLS reverse
proxy — it refuses a token-less public bind. Longer walkthrough:
[`docs/quickstart.md`](docs/quickstart.md); deployment:
[`docs/self-host.md`](docs/self-host.md).

## Use it from Claude Code

This repo is a Claude Code plugin marketplace — `claude plugin marketplace add
nicolasacchi/kb` from a shell, or `/plugin marketplace add nicolasacchi/kb`
inside a Claude Code session. Every plugin is LLM-free: hooks and commands drive the `kb`
CLI against a running daemon. The CLI *is* the protocol; there is no MCP
server, a [recorded refusal](#non-goals) with re-open triggers.

| plugin | what it adds |
|---|---|
| **kb-memory** | agent memory: recall on every prompt, a memory protocol at session start, verbatim transcript capture when the agent stops. Ships `/kb-setup`, a guided first run that writes a `kb.toml` with memory + sessions corpora and starts the daemon |
| **kb-research** | the `kb-artifact` authoring skill (the kb authoring contract), `kb-audit` to reconcile a research corpus' status claims against repo truth, and `/kb-tools` to print the live CLI manifest |
| **kb-comments** | triage reader comments from the terminal — list/export, edit the source, reply/resolve/reanchor, plus a live `kb comments watch` loop |
| **kb-reflect** | `/kb-reflect` consolidates past sessions into durable curated facts: dedup against existing memories, an approved dry run, then `kb remember` |
| **kb-code** | a pre-edit hook that injects "who wrote this line and why" from the kb-code daemon before an Edit/Write lands |

Other harnesses (codex, opencode and friends) drive the same CLI from their own
instructions file; their capture adapters live in `plugins/kb-memory/hooks/`.
Wiring, beats and notifications:
[`docs/live-sessions.md`](docs/live-sessions.md).

## What it does

**Search and read.** Hybrid BM25 + vector search (RRF), with `mode=keyword` and
`mode=semantic` as pure arms, an optional reranker, and `kb bench` to measure
quality *and* latency on your own corpus. The reader adds a faceted gallery,
deep-linkable metadata, a similarity atlas, reading progress and two-pane
compare ([`docs/web-ui.md`](docs/web-ui.md)); notes carry wikilinks and
checklists, and reading lists derive their read state per request
([`docs/reading-lists.md`](docs/reading-lists.md)). What keeps an artifact
indexable and its anchors stable:
[`docs/authoring-artifacts.md`](docs/authoring-artifacts.md).

**Comment in the browser, answer in the terminal.** Click any block to anchor a
comment; anchors re-resolve after every reindex and go stale honestly rather than
drifting onto the wrong element. The agent answers through `kb comments reply` /
`resolve` / `reanchor` and the reader repaints over SSE
([`docs/comment-workflow.md`](docs/comment-workflow.md)).

**Remember and recollect.** `kb remember` writes a curated fact; `kb recall`
ranks by rank × salience × decay with an explainable score. Transcripts are
captured as digests and stay out of ordinary search until `kb why <file>` or
`kb recollect <q>` pulls them. The live cockpit — who is working, waiting or
finished — is [`docs/live-sessions.md`](docs/live-sessions.md).

**Coordinate several agents.** A *slate* is a project's blackboard: twelve post
kinds, advisory takes, an append-only ledger read at session start and after a
compaction ([`docs/slate.md`](docs/slate.md)). The *desk* is its short-lived
counterpart — `kb desk offer` pushes a draft into a corpus so a human reads it
with the full reader, comments flow back through the usual loop, and `kb desk
promote` graduates the keepers.

**Publish a slice.** `kb share` exports a self-contained, relativized bundle
(the embedded prompt template is always stripped), and per-corpus outbound
scrubbing applies regex redactions on non-loopback requests only
([`docs/self-host.md`](docs/self-host.md)).

**Read code the same way.** `kb-code` is a sibling daemon over a git checkout —
blame-backed provenance, session↔commit joins, symbol search, local review
sessions bound to GitHub PRs, agent verbs — on its own binary and port
([`docs/kb-code.md`](docs/kb-code.md)).

**Extend it.** No generic plugin SDK, but three real seams: subprocess sidecars
over stdio JSON (what the embedder itself is), the `/api/events` stream plus a
`[webhooks]` bridge, and declarative `<meta name="kb-*">` / config. What is
deliberately *not* a seam — storage actor, security middleware, atlas — and why:
[`docs/extending.md`](docs/extending.md).

## Documentation

Full index with a suggested reading order: [`docs/README.md`](docs/README.md).

**Reference** — [`docs/cli.md`](docs/cli.md) (every `kb` verb, flag and
behaviour) · [`docs/http-api.md`](docs/http-api.md) (the API canon: endpoints,
parameters, response shapes, SSE events) ·
[`docs/api-routes.md`](docs/api-routes.md) (the generated method/path/handler
table, `just api-docs`) · [`docs/kb-code.md`](docs/kb-code.md) (the sibling
daemon) · [`docs/configuration.md`](docs/configuration.md) (`kb.toml`).

**Guides** — [quickstart](docs/quickstart.md) · [self-host](docs/self-host.md) ·
[web UI](docs/web-ui.md) · [authoring artifacts](docs/authoring-artifacts.md) ·
[comment workflow](docs/comment-workflow.md) ·
[reading lists](docs/reading-lists.md) · [slate](docs/slate.md) ·
[live sessions](docs/live-sessions.md) · [multi-machine](docs/multi-machine.md) ·
[extending](docs/extending.md).

**Design** — [`docs/research/`](docs/research/index.html) holds the frozen
rationale; the constraints it produced are in
[`docs/architecture-invariants.md`](docs/architecture-invariants.md).

## Operating a daemon

`kb daemon doctor` gives a green/yellow/red health report; `kb status` and `kb
metrics` print the same numbers the reader's Settings pages show — request rate,
storage-channel depth, embedder health and respawn count, all emitted at 1 Hz as
[`metrics.tick`](docs/http-api.md#instrumentation-events). `kb events --follow`
is a server-filtered SSE tail with
`Last-Event-ID` resume, one JSON line per event. Several daemons are a *fleet*:
`kb fleet status` sweeps the endpoints in `~/.config/kb/daemons.toml` and the
reader aggregates them from its own list. There is no terminal dashboard — the
TUI was retired once every panel it drew had a reader or CLI home. Reaching one
corpus from several machines has a supported topology and a set of rejected
alternatives ([`docs/multi-machine.md`](docs/multi-machine.md)); the findings
from the bootstrap spikes live on in
[`docs/spike-findings.md`](docs/spike-findings.md).

## Non-goals

kb records its refusals as decisions — the scope it will *not* grow into, and
why. These are rulings, not backlog.

- **No in-daemon LLM, ever.** All ranking, recall and digest logic is
  deterministic and inspectable — a score is reproducible from its inputs.
  LLM-driven steps live in the agent layer, never in the daemon.
- **No CRDT / multiplayer editing.** kb is a system of record, not a
  collaborative editor; comments and reviews are the collaboration surface.
- **One trust tier — identity is attribution, not authorization.** kb resolves
  *who* acted (a proxy's `Remote-User` over a trusted hop, a per-user token, or
  the loopback operator) and keys read-state and comment authorship per user,
  but never *authenticates*: no passwords, sessions, roles, read-only tokens,
  ACLs or visibility tiers. Every identity holds the operator's full authority,
  so a teammate is a co-operator, not a guest (one hygiene exception: editing
  or deleting a comment *body* is owner-only). Outside that tier the answer is
  `kb share`, or a daemon of their own.
- **No in-daemon visibility, ACLs or public mode — the corpus mount is the
  ACL.** Every federated read fans out over every mounted corpus by design; a
  public mirror is a *dedicated daemon* mounting only public corpora behind an
  allowlisting edge, a deployment recipe rather than a daemon feature.
- **The TUI is retired.** Removed after a per-tab parity check showed every
  panel already had a reader or CLI home; the accepted losses, recorded, are
  the watcher heatmap and the `$EDITOR` shortcut.
- **Mobile is a reader with a capture slot, not an editor.** The share sheet
  gets files *into* a corpus as a one-way, provenance-stamped staging drop;
  editing them stays a desktop/CLI job. The desktop app is likewise archived
  (`archive/kb-desktop`), never primary distribution.
- **No MCP server *yet*.** The CLI is the protocol — every harness in real use
  drives `kb` via shell plus an instructions file, and ambient memory (recall
  injection, capture) is hook work MCP cannot carry. Recorded build triggers: a
  harness with no shell tool or no config escape; recurring call-approval
  fatigue config cannot fix; a harness regression removing the transcript
  hooks. If triggered: a read-only stdio facade in the CLI over the existing
  API, never in-daemon, writes still CLI-only.
- **No memory-benchmark arms race.** kb competes on inspectability and
  provenance, not leaderboard scores.
- **The two transcript tailers stay separate.** kb's digest pipeline and
  kb-code's loopback-only raw-text lane read the same files deliberately —
  different projections for different masters; a shared cursor would buy disk
  reads at the price of a cross-daemon contract and a new wedge mode. *Re-open
  trigger: transcript volume grows ~10×.*
- **No stored memory↔artifact edge table.** Two tiers suffice: exact highlight
  provenance parsed back from the source, and a related-memories panel
  recomputed per render and labelled as such. A third store would rot under
  relocate/supersede/forget; the panel, storing nothing, can never dangle.
- **No wikilinks inside memories.** Declined three times: the Markdown parser
  sees no inline links inside `<p>`-wrapped HTML, every memory body's shape, so
  widening the hook would silently do nothing. Memories link outward through
  recall, session ids, highlight provenance and code refs.
- **The duplicate scan never compares memories against ordinary artifacts.** A
  memory that "duplicates" a research doc is the system working: the distilled,
  recall-injectable pointer to the fuller write-up.
- **No push events for citation drift, and no daemon-side memory↔code synthesis
  in either direction.** A drift *event* would persist a trust-class transition,
  contradicting the doc↔code bridge's rule that classes are computed per request
  and never cached; and a deterministic daemon must not author curated prose,
  because memory quality *is* its curation gate. Both are agent-layer
  compositions of verbs that already exist.

## Development

```
crates/kb-core       storage, indexer, embed, watcher, ids, types, parser, atlas
crates/kb-server     the axum daemon (API + reader + artifact subdomains)
crates/kb-cli        the `kb` binary
crates/kb-embedder   embed + rerank sidecar — the ONLY crate linking ONNX Runtime
crates/kb-code-*     the sibling code-reading daemon, its CLI, and kb-lip
web/ · web-code/     the two SPAs (React + Vite + TypeScript)
plugins/             Claude Code plugins (hooks, skills, commands)
docs/                guides, reference, and the frozen design research
tests/e2e/           Playwright suite
```

```bash
just ci        # workspace fmt + clippy + tests
just ci-spa    # build the reader bundle → web/dist/
just ci-e2e    # build + bundle + Playwright (chromium)
just ci-code   # the kb-code sibling daemon's own lane
```

Prereqs: the Rust toolchain pinned in `rust-toolchain.toml`, `just`, node 22+
for the SPA and e2e, **protoc** (lancedb needs it at build time) and chromium for
Playwright. No system ONNX Runtime is needed — it is bundled into `kb-embedder`
alone, and workspace sweeps must pass `--exclude kb-embedder`.

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) before your first PR: commits need a
DCO sign-off (`git commit -s`) and CI enforces it, and
[`docs/architecture-invariants.md`](docs/architecture-invariants.md) records the
constraints to read before changing a subsystem. Security reports go through
GitHub's private vulnerability reporting, never a public issue
([`SECURITY.md`](SECURITY.md)). kb is MIT-licensed ([`LICENSE`](LICENSE));
third-party notices are in [`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md).

## History

This public repository starts at the current version — earlier development
history (every commit before the public cut) is kept in a private archive.
Commit conventions and provenance carry over unchanged: `feat(crate): summary
(PhaseID)`-style messages, DCO sign-off, and `Co-Authored-By` trailers naming the
model that drove a change when AI wrote it — kb's own session↔commit join works
the same way going forward.
