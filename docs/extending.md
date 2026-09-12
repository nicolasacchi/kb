# Extending kb

kb deliberately has **no generic plugin SDK**. It is a single-author /
small-fleet daemon whose value rests on three correctness invariants —
single-writer-per-kb storage, a deterministic atlas, and a fail-closed
security stack — that a generic framework would fight. (The full reasoning
is in the [plugin/extensibility RFC](research/kb-plugins-extensibility-rfc.html).)

What kb *does* have is **three working extension mechanisms it simply never
named**, plus one in-tree seam for first-party features and one named
contract for sibling daemons. This page is the supported extension surface.

| # | Mechanism | Isolation | Who writes it | Status |
|---|---|---|---|---|
| 1 | Subprocess IPC | process | anyone (any language) | shipping — the embedder |
| 2 | Event-bus / webhook subscriber | process / network | anyone | shipping — `/api/events`, `[webhooks]` |
| 3 | Declarative `<meta>` / config | none | artifact authors | shipping — zero code |
| 4 | In-process enrichment hook | none (in-task) | kb maintainers | in-tree, first-party |
| 5 | Sibling daemon (`kb-sibling/1`) | process / network | kb maintainers | shipping — kb-code |

The bar for a *new* seam is high, by design: the same edit must repeat
across many features, **or** the extension must genuinely belong
out-of-process. Adding a share host or an atlas layout is a new enum
variant in one obvious place — not a plugin. See
[§What is *not* a seam](#what-is-not-a-seam).

---

## 1. Subprocess IPC — the out-of-process plugin lane

kb already runs an out-of-process plugin: the **embedder**. The daemon
speaks newline-delimited JSON over the child's stdio, correlates requests
by `req_id`, learns the child's capabilities from a `Ready` handshake, and
kills the child on `Drop`. The child runs ONNX at `nice 20` so it can't
starve the daemon's HTTP/SSE handlers, and a crashed child can't take the
daemon down (dead-flag + respawn).

```
// crates/kb-core/src/embed_ipc.rs — one JSON value per line
Request  = Embed { req_id, texts } | Shutdown
Response = Ready { model, dim } | EmbedOk { req_id, vectors } | Error { req_id, msg }
```

This is the template for **any** heavy, optional, or foreign-language
extension: an alternate embedder, an extractor for a new file type
(PDF / notebook → `Fields`-shaped JSON), or a subprocess share host. A
plugin is any executable that speaks the protocol; the binary is discovered
via `$KB_EMBEDDER_BIN → sibling-of-exe → PATH`.

**Invariant for embedder plugins:** the `Ready` handshake MUST declare a
`dim` matching the kb's vector-column width (fixed at `Storage::open`). A
wrong-width row returns `Err`, never a panic — one bad plugin can't kill the
storage actor.

> Generalising the `embed_ipc` envelope into a reusable `plugin_ipc` module
> is a future step (RFC Tier 2 / phase X4). Build it when a concrete second
> subprocess plugin demands it — not before.

## 2. Event-bus & webhooks — the reactive lane

`/api/events` is a durable, resumable, filtered pub/sub firehose: a
1024-entry replay ring + a 1024-deep broadcast channel, monotonic ids, and
`Last-Event-ID` resume with synthetic `lag` / `gap` frames. Anything that
reads the SSE stream — the SPA, `kb events --follow`, `kb push`, the
internal history rings — is *already* a plugin reacting to kb.

For consumers that can't hold a long-lived connection, the daemon ships a
**`[webhooks]` bridge** (phase X1): one background subscriber that POSTs
selected event envelopes to a URL.

```toml
# kb.toml
[webhooks]
url   = "http://127.0.0.1:9000/kb-hook"
types = ["artifact.indexed", "comment.added", "session.captured"]
# timeout_ms = 5000   # optional; per-POST timeout
```

Each POST body is the raw envelope: `{v, id, type, ts, payload}`. Use it for
auto-share, notifications, external backups — any *reactive side-effect*.
Full key reference: [`configuration.md` § `[webhooks]`](configuration.md).

**Contract — read this before you wire one up:**

- **Read-only + post-emit.** The bridge only reads the bus and makes an
  outbound request. It adds no inbound surface and never writes storage. A
  webhook can *react* to kb; it can never make a synchronous in-request
  decision for kb.
- **SSRF posture.** By default only loopback + public unicast. Set
  `allow_private = true` only for intentional LAN (RFC1918/ULA) hooks.
  Link-local and cloud-metadata addresses are always refused. Each POST
  resolves, filters, then **pins** dial IPs (`resolve_to_addrs`) so a
  mid-flight DNS rebind cannot retarget the connect; redirects are off.
  The operator still trusts the daemon process and whoever can write
  `kb.toml` / `PUT /api/config`.
- **Eventually-consistent, lossy under load.** A slow or unreachable endpoint
  makes the subscriber lag and **drop** events (logged at warn) rather than
  back-pressure the daemon — the bounded-bus guarantee. Tolerate gaps; if you
  need completeness, reconcile against `/api/events` with `Last-Event-ID`.
- **Explicit allowlist.** `types` is matched exactly; there is no "all events"
  form, so a misconfiguration can't fan the entire firehose at a URL.

Implementation: `spawn_webhook_bridge` in `crates/kb-server/src/lib.rs` — a
direct clone of the per-kb history-ring task (`bus.subscribe()` → filter →
act), watched by the shutdown signal and joined on teardown like every other
background task (so an in-process config reload can't leak it).

## 3. Declarative `<meta>` / config — the no-code lane

Artifact authors extend kb's behaviour with **zero code** by writing
`<meta name="kb-*">` tags. The parser already recognises ten:
`kb-category`, `kb-tags`, `kb-status`, `kb-severity`, `kb-salience`,
`kb-decay`, `kb-supersedes`, `kb-session`, `kb-global`, `kb-linked-kbs`.
Each drives indexing, recall, sessions, or memory behaviour without the
author touching Rust. Authoring rules: [`authoring-artifacts.md`](authoring-artifacts.md).

> Today every *new* facet pays a parser tax (a `Fields` field + a compiled
> selector). RFC Tier 0 / phase X4 proposes a generic registry that collects
> unrecognised `kb-*` metas into one JSON column, so new meta-driven facets
> land with no parser/schema churn. Not yet built — typed promotion stays the
> path for any facet that needs BM25 ranking.

## 4. In-process enrichment hooks — the first-party seam

After a document is committed to lance, the indexer runs a sequence of
best-effort **enrichers** — session-transcript parsing, memory-link seeding,
and cross-artifact edge recording. These are first-party, in-tree, and run
in the indexer task; an error is logged and skipped, never failing the index.

This is the seam where the *next* internal feature shaped like "sessions"
should land — as one new hook impl rather than edits scattered across the
pipeline. See `crates/kb-core/src/indexer.rs` (the post-`upsert_doc` phase)
and the [kb-core CLAUDE.md](../crates/kb-core/CLAUDE.md) for the registry's
ordering + best-effort contract.

**Invariant:** an enrichment hook writes *through* the `StorageHandle`
(single-writer-per-kb), never to lance/sqlite directly. A second writer
corrupts the storage guarantee.

## 5. Sibling daemons — the `kb-sibling/1` contract

kb-code (`kb-code-server`) is not a kb route and not a plugin: it is a
**sibling process** with its own binary, its own sqlite volume, and its own
migration set, which calls kb over HTTP (`join::kb_client::KbClient`). One
live call direction only — kb-code → kb; kb knows kb-code as an inert
`code_url`. `kb-sibling/1` is the contract that keeps two independently
deployed binaries honest about each other.

**The Hello.** Both daemons carry it on `GET /api/identity`, additively
beside everything already there:

| Field | Meaning |
|---|---|
| `sibling_protocol` | `"kb-sibling/1"` — compared **exactly**; a different string is a different contract, not a newer one |
| `sibling_major` | `1` — a different major means fail closed, never guess |
| `schema_epoch` | this binary's highest **embedded** migration version (one binary, one epoch) |
| `build_sha` | the commit it was built from (`KB_BUILD_SHA`, else the git probe / `"dev"`) |

`/healthz` stays **pure liveness** on both daemons and learns none of this,
deliberately: an orchestrator's restart loop must never be driven by a
contract mismatch, and a liveness probe that reports schema state is a
liveness probe you can't trust to mean "the process is up".

**The boot guard is hard.** Every sqlite volume carries refinery's own
`refinery_schema_history`; its `MAX(version)` is the *volume* epoch. If it
exceeds the binary's epoch, the daemon **refuses to boot** — naming both
epochs, the db path, and the remediation — before running a single
migration (`kb_core::sibling::refuse_if_volume_ahead`, called from
`Db::open` for every kb as it opens and from kb-code's `Store::open`).
Refinery only ever migrates *forward*, so without the guard an older binary
pointed at a forward-migrated volume boots clean, goes green on `/healthz`,
and fails per-request on columns it doesn't know about. That is exactly the
13.5 h kbc outage of 2026-08: a deploy rollback paired an old binary with a
volume a newer one had already migrated. A refused boot is loud, immediate,
and fixed by deploying forward or restoring the matching state backup.

**The client handshake fails closed.** Before its first real call per
process, `KbClient` GETs kb's identity and classifies the answer once
(cached for the process' lifetime — the answer is a property of the two
binaries, so a redeploy is what refreshes it):

- **Match** → proceed.
- **Mismatch** → every subsequent call returns the `kb_sibling_mismatch`
  degrade (`SiblingMismatch`, HTTP 502 on doc-lens surfaces), logged once at
  error level. Deliberately *distinct* from "peer unreachable": the peer is
  up and answering — talking to it is what's unsafe.
- **Hello absent** (a kb binary older than `kb-sibling/1`) → **legacy-peer
  grandfather rule**: warn once, proceed exactly as before the handshake
  existed. Rolling deploys are the reason — during one, kb-code may reach a
  not-yet-upgraded kb, and refusing there would turn an ordering detail into
  an outage.
- **Unreachable / non-2xx** → unchanged degrade, and *nothing is cached*: a
  network blip must never latch federation off until a restart.

Advisory checks were considered and rejected: rust-analyzer's advisory-only
client/server handshake is the counter-example — advisory *is* the
documented failure mode. Both halves of this contract are hard, in the shape
of nushell's plugin Hello.

---

## What is *not* a seam

Some boundaries look pluggable but must stay closed — breaking them surfaces
as a subtle runtime failure, not a compile error:

- **The storage actor** — single-writer-per-kb is a correctness boundary. No
  plugin gets a second writer; in-process hooks write through the
  `StorageHandle`, out-of-process plugins go through the HTTP API (which
  funnels to the actor) or stay read-only.
- **The security middleware** — auth / rate-limit / origin / loopback bypass
  must never sit behind a plugin (a missing `ConnectInfo` fails *closed*).
  This is why **route contribution is not a seam**: expose new capability via
  the event bus or a read-only client over the HTTP API, never by letting a
  plugin inject middleware or routes.
- **The atlas layout** — bit-identical determinism across libc is
  load-bearing (CI verifies it). Layout stays a config enum (`umap` / `pca`),
  not a pluggable engine.
- **Share hosts** — an `enum`, not a trait: Rust async-fn-in-trait doesn't
  object-dispatch cleanly, and the hosts sequence differently (Cloudflare
  deploys *and* gates; GitHub only deploys). Add a host as a variant + a match
  arm; if a third host ever needs out-of-process logic, add a single
  `ShareBackend::Subprocess` variant rather than a trait.

## Dynamic loading: deferred / rejected

- **WASM (wasmtime / extism)** — *deferred*. It buys sandboxed third-party
  code without a separate process, but kb has no untrusted-code threat model,
  and a WASM runtime is a heavy dependency on top of the already-pinned,
  slow-to-link arrow/lance graph. Subprocess IPC gives ~90% of the isolation
  for ~5% of the complexity, and kb already runs it.
- **Dynamic native `.so` (libloading)** — *rejected*. Rust has no stable ABI,
  and the exact arrow pin (`=58.4.0` as of V76-R4c) means a plugin built
  against a different
  arrow is undefined behaviour, not a load error — while keeping the
  in-process crash blast radius. Both reasons are disqualifying.

## Operational notes

- **No hot-reload.** Config, corpora, and the webhook bridge load at daemon
  (re)start. A `PUT /api/config` triggers an in-process restart that re-reads
  everything; there is no hot-add.
- **Tolerate gaps.** Every event consumer — SSE, webhook, or in-process — can
  miss events under load (bounded bus). Never assume exactly-once; resume via
  `Last-Event-ID` where completeness matters.

## Claude Code plugins

kb ships as a Claude Code plugin **marketplace** — `.claude-plugin/marketplace.json`
at the repo root lists five self-contained plugins under `plugins/<name>/`.
All are LLM-free: every hook and command just drives the `kb` CLI against a
running daemon. (A repo can be a marketplace **or** a single root-level plugin,
not both — so kb-memory lives at `plugins/kb-memory/`, not the repo root.)

| Plugin | Surface | What it does |
|---|---|---|
| `kb-memory` | 3 hooks | Agent memory: recall each turn (`UserPromptSubmit`), wake protocol (`SessionStart`), transcript capture (`Stop`). |
| `kb-research` | 2 skills + command | The `kb-artifact` authoring skill (the authoring contract) + the `kb-audit` skill (reconcile research docs' status claims against repo truth; `--fix` flips stale markers with a dated note) + `/kb-tools` (prints the live `kb` CLI manifest). Author + index in one flow; audit keeps status honest after shipping. |
| `kb-comments` | 2 commands | `/kb-comments <artifact>` (list → edit source → reply/resolve) + `/kb-comments-watch` (the live SSE triage loop). |
| `kb-reflect` | 1 command | `/kb-reflect [<session-id>]` — the memory "dream": distil past sessions into durable facts (read digests via `kb sessions show` → dedup via `kb recall --no-floor` → approved dry-run → write via `kb remember`). The one plugin whose reasoning is the live agent's, not mechanical; the daemon stays model-free (#26). |
| `kb-code` | 1 hook | Provenance at the point of edit: a `PreToolUse` hook (`kb-code-why`) that injects trailer/exact-confidence "who wrote this and why" right before an Edit/Write touches an attributable line — LLM-free, fail-open, never fuzzy. Drives the kb-code sibling daemon (§5) via `kb-code hook install|uninstall|status`, which prints the `settings.json` wiring rather than editing it silently. |

Install from a local checkout:

```
/plugin marketplace add /path/to/kb
/plugin install kb-memory@kb-plugins
/plugin install kb-research@kb-plugins
/plugin install kb-comments@kb-plugins
/plugin install kb-reflect@kb-plugins
/plugin install kb-code@kb-plugins
```

`kb-research`, `kb-comments`, and `kb-reflect` are **packaging, not new daemon
code** — they wrap CLI surface that already works (`kb tools`, `kb add`,
`kb find`, `kb comments {list,reply,resolve,reanchor,export,watch}`,
`kb sessions {list,show}`, `kb recall`, `kb remember`). `kb-reflect` is the one
whose *reasoning* (which facts are durable) is the agent's, not a fixed script —
but it still only drives the same CLI; the daemon never gains a model.

**No MCP server.** Recall / remember stay hooks (an unconditional
`UserPromptSubmit` injection beats a tool the model must elect to call), and
exploration (`search` / `related` / open-comments) is already reachable
through the `kb` CLI via Bash at zero per-session context cost. An
exploration-only MCP server is a real but *optional* future option (it trades
a per-session schema cost for typed, model-elected discovery); it is
deliberately not built here. See the RFC §7 for the honest trade.

## See also

- [Plugin / extensibility RFC](research/kb-plugins-extensibility-rfc.html) — the full design rationale, seam map, and phased roadmap.
- [`configuration.md`](configuration.md) — `[webhooks]` and every other key.
- [`authoring-artifacts.md`](authoring-artifacts.md) — the `<meta name="kb-*">` vocabulary.
- [`http-api.md`](http-api.md) — `/api/events` and the event-type registry.
