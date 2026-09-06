# ADR: kb across your machines

**Status:** accepted. **Date:** 2026-07-10.

## Context

The same person routinely works from more than one machine (laptop, desktop,
a remote box) and wants the same corpus, search index, comments, sessions,
and agent memory visible from all of them. This ADR rules on the supported
topology for that — and what is explicitly rejected.

## Decision

**One remote daemon, reached by every client machine over the network with
bearer-token auth, is the canonical multi-machine topology.** This is not a
future design: it already runs in production. `docs/self-host.md` documents
the full recipe — bearer-token auth (`## Bearer-token auth (v0.4)`), rate
limiting per token, the loopback-bypass-vs-remote-auth threat model
(`## Threat model`), and a public deployment behind Traefik with a DNS-01
wildcard cert (`## Public deployment with DNS-01 wildcard`) — and the
`kb.example.com` reference deploy binds `0.0.0.0` with a mounted token per
that guide (`docs/self-host.md`, "Fail-closed on a token-less public bind").

The client side already supports this: `kb-cli` is not hardwired to
`127.0.0.1:4000` — `http::detect_daemon` (`crates/kb-cli/src/http.rs`) probes
`GET /api/identity` against either the default loopback URL or an explicit
one, and search (and other read verbs) accept `--daemon <URL>` to force a
specific HTTP endpoint (`crates/kb-cli/src/commands/search.rs`); every
request carries the bearer token via `client_with_timeout_and_bearer`. So
"several machines, one daemon" is: run `kb daemon` on exactly one host, point
every other machine's `kb` (and browser, for the SPA) at that host's
`https://kb.<you>.example.com` with the shared token.

## Why file-sync (Syncthing / NFS / similar) is rejected for live state

The temptation is to sync the corpus + kb's state directory between machines
and run a daemon on each. This is rejected for anything that's live state,
for three concrete reasons, each grounded in code:

1. **Per-kb storage is single-writer, not sync-safe.** The storage actor
   (`crates/kb-core/src/storage/actor.rs`) is documented as
   "Single-writer-per-kb storage actor" — it owns one lance `Storage` + one
   sqlite `Db` and every mutation crosses one mpsc channel *by design*, "so
   maintenance never head-of-line-blocks search." That single-writer
   assumption is a single **process**, not "single writer across a
   filesystem" — two daemons on two machines each independently believe
   they're the sole writer to the same `lance/` + `index.db` files. Nothing
   in that design coordinates a second process, let alone a second machine
   with sync latency in between.
2. **sqlite + lance corruption risk under concurrent/racing writers.**
   `index.db` is a sqlite file and `lance/` is an Arrow-based columnar
   dataset; both assume exclusive local access from the one process that
   opened them. A file-sync tool moving bytes around *underneath* a live
   daemon (or two daemons converging on the same directory at slightly
   different times) is exactly the failure mode sqlite's locking and lance's
   dataset versioning are not designed to arbitrate across hosts — the
   realistic outcomes are a corrupt index requiring `kb reindex`, or two
   daemons silently diverging on `.review`/`history` state with no merge.
3. **Side-car state lives in the daemon's state dir, and file-sync never
   carries it.** Comments (`.review/<id>.json`, kb-comments/1), reading
   history, lists, and agent memory are NOT part of the source corpus — they
   live under `KbPaths::state` (`~/.local/state/kb/<daemon>/<kb>/`, see
   `crates/kb-core/src/paths.rs`): `kb_review_dir` for comments (guarded by
   the per-kb `review_lock`, see `crates/kb-core/src/review.rs` and
   `crates/kb-core/src/cascade.rs`), plus the `index.db` tables for history,
   lists, sessions, and recall. A tool that syncs the *source corpus dir*
   (what you point `kb add` at) never touches any of this — it would sync
   the artifacts and silently leave every comment thread, every reading
   position, every list, and every memory desynced or absent on the second
   machine. There is no supported way to merge two divergent `.review`
   trees or two divergent sqlite `history` tables — kb has no CRDT layer
   ([README → Non-goals](../README.md#non-goals): "No CRDT / multiplayer
   editing") and never will.

## What IS safe to sync

The **read-only source corpus directory** — the plain HTML/Markdown
artifact files a `[kb.<name>]` block's `path` points at — is safe to mirror
into a single daemon's mount by any means (Syncthing, rsync, NFS, git). The
daemon's indexer treats that directory as an input to reconcile against, not
as its own state; as long as exactly one daemon process owns the index built
from it, syncing files *into* that one daemon's corpus is unremarkable. What
is unsafe is running a second live daemon (or a second *writer* of any kind)
against a *second* copy of that directory and expecting the two daemons'
derived state (index, comments, history, memory) to reconcile — they won't,
per the three reasons above.

## This is the same operator, not a second one

To be explicit: multi-machine access by the same person is squarely inside
kb's design, not a violation of the **one trust tier** non-goal. README's
non-goals section rules out *authorization* tiers, not additional named
identities: kb "never *authenticates*: no passwords, sessions, roles,
read-only tokens, ACLs or visibility tiers. Every identity holds the
operator's full authority, so a teammate is a co-operator, not a guest"
([README → Non-goals](../README.md#non-goals)). That ruling is about
**what** an identity can do once admitted, not **how many machines** a
legitimate holder reaches the daemon from. One operator (or one named
identity via [`[identity]`](configuration.md#identity-v034)), one credential,
N client machines (laptop + desktop + phone browser, all pointed at the
same daemon) is exactly the supported shape — it's the same credential
model the SPA and CLI already use from a single machine, just reached over
the network instead of loopback. Nothing about auth, rate limiting, or
`.review` ownership changes when the second machine is yours instead of a
different identity's — and a second *identity* (a real teammate) is a
separate, supported case (`[identity]`), not the multi-machine case this
ADR is about.

## Open ends

These are open, not resolved — recorded so a future ruling can pick them up
deliberately rather than by accident:

- **HTTP write path for kb-memory capture from a remote client — partially
  closed already, but inconsistently.** `kb remember` is *not* a gap: it
  already POSTs to `POST /api/kb/{kb}/artifacts` (`crates/kb-server/src/
  routes/artifacts.rs`), which renders the memory HTML server-side and
  writes it into the daemon's own corpus dir — `crates/kb-cli/src/commands/
  memory.rs` even documents remember/recall/forget as "thin daemon
  wrappers... no offline mode." That route works fine pointed at a remote
  daemon over `--daemon <URL>` with a bearer token, same as search. The gap
  is the **Stop-hook session-transcript capture**
  (`plugins/kb-memory/hooks/kb-capture.sh`): it writes the transcript HTML
  directly into `$KB_SESSIONS_DIR` on the local filesystem and relies on the
  daemon's own file watcher to pick it up — there is no HTTP ingest endpoint
  it POSTs to. On a client machine that isn't the daemon host (no local
  corpus dir to write into), that hook is currently a dead end, so session
  capture from a remote client doesn't work even though memory capture
  does. Closing this needs either an HTTP ingest endpoint mirroring
  `POST /api/kb/{kb}/artifacts` for session transcripts, or an explicit
  decision that remote session capture stays out of scope while remote
  memory capture stays in.
- **Offline read.** The remote-daemon topology means a client machine with
  no network path to the daemon host (offline laptop, flaky connection) has
  no local fallback — search, recall, and the SPA all go dark. Whether kb
  ever grows a read-only local cache/mirror for offline use, or whether that
  stays explicitly out of scope (analogous to the archived desktop app), is
  undecided.
