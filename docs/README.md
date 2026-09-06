# kb documentation

Start with the top-level [`README.md`](../README.md) for the
project overview, install, CLI surface, and HTTP API. The guides here go
deeper on individual surfaces.

Every page below is plain Markdown or self-contained HTML, so this folder
indexes cleanly as a kb corpus — point a `[kb.<name>]` at `docs/` and the
whole manual becomes searchable next to whatever else the daemon serves.

## Guides

| Doc | What it covers |
|---|---|
| [`quickstart.md`](quickstart.md) | Clone → run daemon → add a corpus → search → open the web UI in five minutes, plus the safe path (loopback bind + token) to a public deployment. |
| [`packaging.md`](packaging.md) | How kb is built into shippable artifacts — the two-binary rule, the Docker build, and the hand-rolled release.yml + Docker path with its open decisions. |
| [`configuration.md`](configuration.md) | Complete `kb.toml` reference — `[daemon]`, `[server]` (incl. rate limits), `[indexer]`, `[ui]`, and per-kb `[kb.<name>]` (path, embedding model, atlas, outbound scrub, templates). |
| [`self-host.md`](self-host.md) | Deployment: TLS, reverse proxies, bearer-token auth, rate limiting, comment isolation, mDNS, hardening checklist. |
| [`multi-machine.md`](multi-machine.md) | ADR (accepted 2026-07-10) — the supported topology for reaching one corpus, index, and memory from several machines, and which alternatives are explicitly rejected. |
| [`comment-workflow.md`](comment-workflow.md) | The review/comments system end to end — storage, the `kb comments` verbs, and the realtime `watch → reply → resolve` loop with Claude Code. |
| [`web-ui.md`](web-ui.md) | The single-page web app — gallery filters, search modes, the detail/annotate view, atlas, history timeline, and the stale-anchors dashboard. |
| [`reading-lists.md`](reading-lists.md) | Reading lists (RL-track) — ordered, section-aware lists with derived read state; the `kb list` verbs, the kb-list/1 Markdown/JSON import-export format, section permalinks + the trail queue bar. |
| [`authoring-artifacts.md`](authoring-artifacts.md) | How to write HTML artifacts that index cleanly and keep comment anchors stable across regen. |
| [`live-sessions.md`](live-sessions.md) | The live-sessions cockpit — the two-axis working/waiting/finished model, `kb sessions status`, wiring the `kb-beat.sh` push hooks (every env var), and the ntfy notification recipes (in-daemon `[webhooks]` bridge vs. the shell one-liner). |

## Developing

| Doc | What it covers |
|---|---|
| [`architecture-invariants.md`](architecture-invariants.md) | The deep developing reference — full text of every numbered invariant (the root [`CLAUDE.md`](../CLAUDE.md) carries only the one-line index), the file-by-file code map, and the Rust/build pitfalls. Read the matching entry before changing a load-bearing subsystem. |
| [`invariant-test-map.md`](invariant-test-map.md) | Traceability from each numbered invariant slot to the tests that pin it; every pinning test carries an `// invariant:N` comment, so `git grep 'invariant:N'` finds them all. |
| [`api-routes.md`](api-routes.md) | The complete route table, generated from `router.rs` by `just api-docs` (CI fails when stale). Also documents the three surfaces outside the `/api` tree: the artifact-host fallback, `GET /healthz`, and `POST /capture`. |
| [`extending.md`](extending.md) | Why kb has no generic plugin SDK, and the three extension mechanisms it does have. |
| [`web-internals.md`](web-internals.md) | Developer reference for the SPA — components, hooks, the API/SSE layer, and `web/src/` conventions. |

## Reference

- [`research/`](research/index.html) — 145 pages of frozen design
  rationale (the decisions behind lance, fastembed, the iframe sandbox,
  comments, the gallery, multi-daemon, the embedding bake-off), grouped
  into `foundation/`, `surfaces/`, `memory/`, `publishing/`,
  `commercialization/`, and the competitive/handoff studies.
- [`review/`](review/) — the full-project review: per-dimension findings,
  the A–F scorecard, and the level-up roadmap.
- [`spike-findings.md`](spike-findings.md) — phase-by-phase spike notes;
  the research artifacts are never edited, so this file is the delta.
- [`bench/`](bench/) — embedding-model bake-off reports.
- [`deploy-sessions.md`](deploy-sessions.md) — Track-S operator runbook
  for turning on `/api/sessions/*` on an existing deployment.
- The daemon serves its own authoritative event registry at
  `/api/events.schema.json`.
