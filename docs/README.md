# kb documentation

The project overview, install channels and the recorded non-goals live in the
top-level [`README.md`](../README.md). This folder holds everything longer than
a front door: guides for each surface, three reference files, and the frozen
design record.

Every page here is plain Markdown or self-contained HTML, so the folder indexes
cleanly as a kb corpus — point a `[kb.<name>]` at `docs/` and the whole manual
becomes searchable next to whatever else the daemon serves.

## Reading order

**New user** — [`../README.md`](../README.md) → [`quickstart.md`](quickstart.md)
→ the [user guide](guide/index.html) (`start` → `concepts` → `search` → `web`)
→ [`configuration.md`](configuration.md) → [`self-host.md`](self-host.md) when
you are ready to expose it. Then whichever surface you actually use:
[`authoring-artifacts.md`](authoring-artifacts.md) if you write artifacts,
[`comment-workflow.md`](comment-workflow.md) if you review them,
[`live-sessions.md`](live-sessions.md) if you run agents against it.

**Contributor** — [`../README.md`](../README.md) →
[`../CONTRIBUTING.md`](../CONTRIBUTING.md) →
[`architecture-invariants.md`](architecture-invariants.md) (the constraints that
break at runtime, not at compile time) → the reference file for the surface you
are touching ([`cli.md`](cli.md), [`http-api.md`](http-api.md),
[`kb-code.md`](kb-code.md)) → [`web-internals.md`](web-internals.md) for SPA
work → [`invariant-test-map.md`](invariant-test-map.md) to find the tests that
pin what you changed.

## Start here

| Doc | What it covers |
|---|---|
| [`quickstart.md`](quickstart.md) | Build or install → run the daemon → add a corpus → search → open the web UI, in five minutes, plus the safe path to a public deployment. |
| [`guide/`](guide/index.html) | The illustrated user guide as HTML pages: [start](guide/start.html), [concepts](guide/concepts.html), [search](guide/search.html), [web reader](guide/web.html), [lists & notes](guide/organize.html), [comments](guide/comments.html), [memory & sessions](guide/agent.html), [operate](guide/operate.html). |

## Operate

| Doc | What it covers |
|---|---|
| [`configuration.md`](configuration.md) | The complete `kb.toml` reference — `[daemon]`, `[server]` (incl. rate limits), `[indexer]`, `[ui]`, `[webhooks]`, and per-kb `[kb.<name>]` (path, embedding model, atlas, outbound scrub, templates). |
| [`self-host.md`](self-host.md) | Deployment: TLS and reverse proxies, bearer-token auth, the threat model, rate limiting, team identities, comment isolation, backup/restore, mDNS, and a hardening checklist. |
| [`packaging.md`](packaging.md) | How kb becomes shippable artifacts — the two-binary rule, the Docker build, the release workflow, and building from source. |
| [`multi-machine.md`](multi-machine.md) | ADR (accepted 2026-07-10): the supported topology for reaching one corpus, index and memory from several machines, and which alternatives are explicitly rejected. |
| [`deploy-sessions.md`](deploy-sessions.md) | Operator runbook for turning on the `/api/sessions/*` surface and its SPA route on an existing reverse-proxied deployment. |
| [`deploy-perf.md`](deploy-perf.md) | Operator runbook for the performance wave: what the migration does, what to watch, and the rollback shape. |

## Use

| Doc | What it covers |
|---|---|
| [`web-ui.md`](web-ui.md) | The single-page reader — gallery filters, search modes, the detail/annotate view, atlas, history timeline, the stale-anchors dashboard. |
| [`authoring-artifacts.md`](authoring-artifacts.md) | How to write HTML artifacts that index cleanly and keep comment anchors stable across a regeneration. |
| [`comment-workflow.md`](comment-workflow.md) | The review/comments system end to end — storage, the `kb comments` verbs, and the realtime watch → reply → resolve loop with an agent. |
| [`reading-lists.md`](reading-lists.md) | Ordered, section-aware reading lists with derived read state: the `kb list` verbs, the `kb-list/1` Markdown/JSON import-export format, section permalinks, the trail queue bar. |
| [`slate.md`](slate.md) | The per-project blackboard — the twelve post kinds, advisory takes, the digest, exit codes, and how a session reads it at start and after a compaction. |
| [`live-sessions.md`](live-sessions.md) | The live-sessions cockpit — the working/waiting/finished model, `kb sessions status`, wiring the beat hooks for each harness, and the notification recipes. |
| [`extending.md`](extending.md) | Why kb has no generic plugin SDK, the three extension mechanisms it does have, and the sibling-daemon contract. |

## Reference

| Doc | What it covers |
|---|---|
| [`cli.md`](cli.md) | Every `kb` verb, flag and behaviour, one block per family. `kb tools` prints the same manifest from your installed binary. |
| [`http-api.md`](http-api.md) | The narrative HTTP API canon — endpoints, parameters, response shapes, SSE event kinds, and the behaviour behind each. |
| [`api-routes.md`](api-routes.md) | The complete method/path/handler table, generated from `router.rs` by `just api-docs` (CI fails when stale). Also documents the three surfaces outside the `/api` tree: the artifact-host fallback, `GET /healthz`, and `POST /capture`. |
| [`kb-code.md`](kb-code.md) | The sibling code-reading daemon: what it indexes, its review/PR surface, its search grammar, its auth split, and how it talks to kb. |

The daemon also serves its own authoritative event registry at
`/api/events.schema.json`, and the generated wire types for SPA-facing
responses live in [`../web/src/api/generated/`](../web/src/api/generated/).

## Internals and design

| Doc | What it covers |
|---|---|
| [`architecture-invariants.md`](architecture-invariants.md) | The deep developer reference: full text of every numbered invariant (the root [`CLAUDE.md`](../CLAUDE.md) carries only the one-line index), the file-by-file code map, and the Rust/build pitfalls. Read the matching entry before changing a load-bearing subsystem. |
| [`invariant-test-map.md`](invariant-test-map.md) | Traceability from each numbered invariant slot to the tests that pin it; every pinning test carries an `// invariant:N` comment. |
| [`web-internals.md`](web-internals.md) | Developer reference for the reader SPA — components, hooks, the API/SSE layer, and `web/src/` conventions. |
| [`spike-findings.md`](spike-findings.md) | What the six de-risking spikes confirmed, surprised or refuted. The research artifacts are never edited, so this file is the delta. |
| [`research/`](research/index.html) | The frozen design record — the decisions behind lance, fastembed, the iframe sandbox, comments, memory, the atlas, plus prior-art and handoff studies, grouped into `foundation/`, `surfaces/`, `memory/`, `publishing/` and more. |
| [`bench/`](bench/) | Embedding-model bake-off and search-quality reports, JSON plus Markdown. |

> This index lists what exists at the time of writing. `docs/` grows with the
> project; if a file here is missing or a new one is not listed, the folder
> listing is the truth.
