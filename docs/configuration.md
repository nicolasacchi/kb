# kb.toml configuration reference

A daemon reads one `kb.toml` (default `~/.config/kb/kb.toml`, override
with `--config`). It declares the daemon's identity, the HTTP server,
indexer tuning, UI defaults, and one `[kb.<name>]` table per knowledge
base. This page is the complete key reference; for deployment-shaped
guidance (TLS, reverse proxies, auth) see
[`self-host.md`](self-host.md), and for the schema source of truth see
`crates/kb-core/src/config.rs`.

Multi-daemon discovery for the fleet verbs (`kb fleet status`) is a
*separate* file, `~/.config/kb/daemons.toml` — see
[daemons.toml](#daemonstoml-fleet-discovery) at the bottom.

## Minimal example

```toml
[kb.notes]
path = "/home/me/notes"
embedding_model = "bge-small-en-v1.5"
```

Everything else has defaults; a single `[kb.<name>]` with a `path` is a
working config.

## `[daemon]`

| key | type | default | meaning |
|---|---|---|---|
| `name` | string | short hostname | Daemon name. Used in `kb fleet status` output, the pid-file path (`<state>/kb/<name>/`), and `/api/identity`. |

## `[server]`

| key | type | default | meaning |
|---|---|---|---|
| `addr` | string | `127.0.0.1:4000` | Listen address. Bind to `0.0.0.0:PORT` only behind a reverse proxy with auth. |
| `mdns` | bool | `false` | Opt-in: advertise `_kb._tcp.local.` so other daemons / fleet tooling on the LAN can auto-discover. |
| `artifact_host_suffix` | string | `.artifacts.localhost` | Subdomain root for sandboxed artifact iframes. In production set `.artifacts.<your-domain>` (needs wildcard DNS + cert). |
| `parent_origin` | string | `http://localhost:4000` | Origin the SPA is served from; used for the CSP + postMessage target. Set to `https://kb.<your-domain>` in production. |
| `trusted_proxies` | [string] | `[]` | Reverse-proxy IPs whose `X-Forwarded-For` the daemon trusts. Loopback always counts; a dockerised proxy reaching the host over a bridge must be listed here or the auth/rate-limit bypass never engages for it. Non-IP entries are dropped with a warn at boot. |
| `metrics` | bool | `false` | Opt-in detailed metrics. The coarse per-route latency histograms + `metrics.tick` SSE are always on (cheap atomics); this adds the richer layer to `GET /api/metrics`: per-search-stage (embed/bm25/vector/hybrid) + per-kb request timing, and ingest-pipeline timing (indexer throughput, index-side embed latency, storage-actor queue-wait + handler-time). Read once at boot; a config edit restarts in-process to apply. View with `kb metrics` or the SPA Settings → Traffic tab. |
| `log_retention_days` | u32 | `14` | v0.24 (L2) — retention window for the daemon's ndjson log files under `<state>/log/` (see [Daemon logging](#daemon-logging-v024) below). Files older than the window are deleted at boot + daily. Always on (logs are the daemon's own diagnostics; an unbounded daily-rolled log dir is a disk-fill risk); must be ≥ 1 — `0` is a hard validation error. |
| `fanout_cap` | usize | `8` | PF-R1 — concurrency cap for federated (`scope=all`) read handlers' per-corpus fan-out (`search`, `memory` recall, `sessions`, `lists`, `notes`, `anchors`, `/inbox`, `/desk`, `/queries/zero-hit`, `/stats`, `/kbs`, …; invariant #28's `routes::buffered_join`). Bounds how many corpora are queried concurrently for one such request — caps `spawn_blocking` pool + embedder IPC pressure. Raising it lowers fan-out latency on many-kb fleets at the cost of more parallel storage-actor pressure per request; lowering it trades latency for a gentler per-request load. Read once at boot; a config edit restarts in-process to apply (invariant #13). Never changes fan-out order — corpora are still queried in the same deterministic (`BTreeMap`) submission order regardless of this value. |

### Daemon logging (v0.24)

Both daemon paths (`kb daemon`, `kb-server`) write **ndjson** (one JSON
object per line) to a daily-rolled file,
`<state>/log/kb.ndjson.YYYY-MM-DD`. The file layer defaults to `info`;
override at boot with the `KB_LOG_FILE_LEVEL` env var (an `EnvFilter`
string, e.g. `debug` or `kb_core=debug,info`), or flip it at runtime —
no restart — via `PUT /api/log-level` / `kb daemon log-level <filter>`
(`GET /api/log-level` / bare `kb daemon log-level` reads it back). The
runtime flip is process-local (not persisted); the sweep governed by
`log_retention_days` above only ever deletes aged `kb.ndjson.*` files.

### `[server.rate_limit]`

Per-token request caps (req/min). Loopback bypasses entirely. Omit the
table to keep defaults; set any subset of keys to override.

| key | default | applies to |
|---|---|---|
| `search` | 60 | `GET /api/search` |
| `atlas_recompute` | 60 | `POST /api/kb/{kb}/atlas/recompute` |
| `review_post` | 240 | fine-grained comment mutations (`POST .../comments`, `/resolve`, `PATCH`/`DELETE`, …) + `/export` |
| `history_post` | 600 | `POST /api/kb/{kb}/history/{open,scroll,search}` |

`history_post` is higher on purpose: scroll updates are debounced to
~1/s in the SPA, so active reading legitimately runs at ~60–180/min; the
limiter exists to bound a buggy/adversarial client, not throttle real
use. Over-cap requests get `429` + `Retry-After`.

```toml
[server.rate_limit]
search = 120
history_post = 1200
```

### `[server.attachments]`

Upload limits for **comment attachments** (`kb comments upload`/`attach`,
the SPA composer's file picker). Omit the table to keep defaults. Distinct
from `[server.capture]` below (quick-capture / Share Target) — a different
upload surface with its own cap.

| key | type | default | meaning |
|---|---|---|---|
| `max_file_bytes` | u64 | 10 MiB | Max size for a single staged attachment. Over-cap → `413`. `0` is a hard validation error. |
| `max_per_comment` | usize | 20 | Max attachments one comment/reply can carry. `0` is a hard validation error. |
| `gc_grace_hours` | i64 | 24 | Hours a staged-but-never-adopted attachment (uploaded, never referenced in a comment body) survives before the GC sweep reaps it. `0` is accepted but warned on (reaps a staged upload almost immediately). |

```toml
[server.attachments]
max_file_bytes = 20_000_000
max_per_comment = 10
```

### `[server.capture]`

v0.25 (U1/U2) — quick-capture upload limits + the Web Share Target
destination kb. Omit the table to keep defaults. Distinct from
`[server.attachments]` above (comment attachments) — a different upload surface
with its own cap.

Both routes (`POST /api/kb/{kb}/capture` and the share-target `POST
/capture`) accept a MULTI-FILE batch, so `max_file_bytes` and
`max_request_bytes` are two independent budgets — one file over
`max_file_bytes` still 413s even if the batch is under
`max_request_bytes`, and a batch whose COMBINED size is over
`max_request_bytes` 413s up front (before any file is parsed) even if
every individual file is under `max_file_bytes`.

| key | type | default | meaning |
|---|---|---|---|
| `max_file_bytes` | u64 | 10 MiB | Max size for a SINGLE captured file, streaming-enforced per file. Over-cap → `413`. |
| `max_request_bytes` | u64 | 64 MiB | Max COMBINED size for one capture request (every file in the batch together). Checked against `Content-Length` before multipart parsing starts. Over-cap → `413` with a `detail` naming the limit. |
| `default_kb` | string | none (first configured kb) | Destination kb for the Web Share Target route (`POST /capture`), which carries no `{kb}` path segment — the Android share sheet can't pick a kb. `None` falls back to the daemon's first configured kb (lexicographically-first `[kb.<name>]` table name). |

```toml
[server.capture]
max_file_bytes = 20_000_000
max_request_bytes = 80_000_000
default_kb = "notes"
```

## `[identity]` (v0.34)

Who gets credit for reads, comments, and API calls. **Attribution only** —
kb never authenticates (the edge does) and every identity carries the same
full authority; see [README → Non-goals](../README.md#non-goals) and
invariant #2. Absent section = single-operator behaviour, unchanged.

| key | type | default | meaning |
|---|---|---|---|
| `operator` | string | `"operator"` | Attribution for loopback callers and the legacy shared token. **Set this to your own Authelia username** so local and edge work attribute to one identity. Must be lowercase `[a-z0-9._@-]{1,64}`. |
| `header` | string | `"Remote-User"` | Identity header, honoured ONLY from a trusted hop (loopback or a listed `[server] trusted_proxies` IP). `X-Remote-User` / authentik / oauth2-proxy names go here. |
| `users` | array of tables | `[]` | Optional display metadata: `{ name = "jordan", display = "Jordan" }`. Purely cosmetic — an unconfigured user still attributes verbatim and shows up in `GET /api/users` as observed. |

```toml
[identity]
operator = "alex"
header   = "Remote-User"

[[identity.users]]
name    = "alex"
display = "Alex"

[[identity.users]]
name    = "jordan"
```

**Per-user API tokens** live OUTSIDE this file (never put secrets in
`kb.toml`): `<config>/tokens`, one entry per line, mode 0600 —

```
# <user>:sha256:<hex>   (preferred; `kb token issue <user>` writes this)
alex:sha256:9f2c…
jordan:sha256:41ab…
```

Mint with `kb token issue <user>` (prints the plaintext once), remove with
`kb token revoke <user>`; the daemon loads the file at startup, so restart
after changing it. A client presents the token as `Authorization: Bearer
<token>` or — behind a proxy that rewrites `Authorization`, as
`kb-inject-bearer` does in the reference deployment — as `X-Kb-Token`.
The legacy single `<config>/token` still works and attributes to `operator`.

Resolution order, verification, and the deployment wiring:
[self-host.md → Team identities](self-host.md#team-identities-v034).

## `[ui]`

Daemon-wide UI defaults (a per-kb `[kb.<name>.ui]` overrides these for
that kb). The SPA's Settings page can also change these at runtime.

| key | type | values |
|---|---|---|
| `theme` | string | `paper`, `ink` |
| `accent` | string | an accent id from the fixed palette |
| `density` | string | `compact`, `normal`, `spacious` |

## `[indexer]`

Daemon-wide watcher + reconciler tuning. Each key has an env-var
override that wins when set (handy for one-off runs without editing the
file).

| key | type | default | env override | meaning |
|---|---|---|---|---|
| `debounce_ms` | u64 | 400 | `KB_DEBOUNCE_MS` | notify debounce window — how long to coalesce a burst of filesystem events before reindexing. |
| `reconcile_secs` | u64 | 60 | `KB_RECONCILE_SECS` | Periodic full re-walk interval. `0` disables it (the explicit `POST /api/kb/{kb}/reindex` and `kb reindex` still work). Its job is to catch what notify missed (inotify overflow, NFS/FUSE mounts, dropped events). The walk skips emitting `watch.modify` for files whose on-disk mtime matches the last index (producer-side dedup), and the indexer's content-hash pre-gate drops any that slip through, so a no-op pass is near-free and puts ~zero events on the bus. PF-I1: a per-kb `reconcile_secs` overrides this for one kb — see `[kb.<name>]` below. The periodic auto-compact ticker follows the same resolved value (per-kb when overridden). |
| `indexer_nice` | i32 | 20 | `KB_INDEXER_NICE` | `nice` value for the `kb-embedder` subprocess (0–19; clamped). 20→lowest priority so embedding yields to interactive work; `0` disables de-prioritisation. Applied at fork so ONNX worker threads inherit it from birth. |
| `watch_mode` | string | `auto` | `KB_WATCH_MODE` | Filesystem-watch backend: `auto` (native inotify, with a WSL `/mnt/*` heads-up logged at startup), `native`, or `poll`. Use `poll` for filesystems where native events don't fire — WSL2 `/mnt/*` (DrvFs/9p), SMB/NFS. An unrecognised value logs a warning and falls back to `auto`. The `reconcile_secs` pass is the correctness backstop regardless; lowering it is an alternative to `poll`. |
| _(poll interval)_ | — | `2000` ms | `KB_POLL_INTERVAL_MS` | Re-stat interval for `watch_mode = "poll"` (env-only). Coarse by design — polling re-stats the whole tree; raise it on a large SMB/NFS mount. Ignored under native backends. |
| `indexable_extensions` | table | built-in set | — | v0.24 (X1) — daemon-wide indexable-extension map: bare extension → parse-pipeline name (`"html"` or `"markdown"`), e.g. `[indexer.indexable_extensions]` with `txt = "markdown"`. Absent → the built-in set (`html`/`htm` → HTML, `md`/`markdown` → Markdown). A per-kb `indexable_extensions` overrides this for that kb. A set table is the ENTIRE map (it replaces, not extends, the default). Unknown pipeline names and empty/dotted extension keys are hard validation errors. This configures WHICH extensions parse — the two pipelines are the whole set; it never adds a parser (D1). Shrinking the map does NOT delete already-indexed files (reconcile never cleans present-on-disk files) — remove them explicitly via `kb exclude` / the exclusions API, or delete the file on disk. |

### Path overrides

By default kb uses Linux XDG dirs (`directories` crate): `~/.local/state|config|cache/kb` (honouring `XDG_*`). `KB_HOME` and the per-dir overrides still exist so tests and containers can redirect paths deterministically (highest precedence first):

| env | overrides |
|---|---|
| `KB_STATE_DIR` / `KB_CONFIG_DIR` / `KB_CACHE_DIR` | one dir each (the daemon segment is still appended to the state dir) |
| `KB_HOME` | all three under one root: `<home>/{state,config,cache}` |

These pin paths deterministically — used by the test suite and handy for containers / air-gapped installs.

### ONNX Runtime (embedder)

`kb-embedder` statically bundles ONNX Runtime (`ort-download-binaries`, fetched from a CDN at build time); the resulting binary is self-contained (no `libonnxruntime` at load time, no `ORT_DYLIB_PATH`). For offline/air-gapped builds set **both** `ORT_STRATEGY=system` and `ORT_LIB_LOCATION=<dir containing libonnxruntime>` so the build links a local copy instead of fetching from the CDN. No system onnxruntime is required at runtime.

**`KB_EVENT_BUS_CAPACITY`** (env-only, default `1024`, floor `256`) — sizes the
daemon-wide SSE event bus (both the live broadcast channel and the
Last-Event-ID replay ring). The escape hatch for a corpus large enough that a
single burst of *genuinely changed* files (a bulk edit, a `git checkout` of the
whole tree, an operator `reindex`) exceeds 1024 envelopes and overflows the
channel. No-op reconcile passes don't stress it (they emit ~nothing since the
producer-side dedup above), so most deployments never need this.

```toml
[indexer]
debounce_ms = 250
reconcile_secs = 30
indexer_nice = 10

[indexer.indexable_extensions]
txt = "markdown"
md  = "markdown"
html = "html"
htm  = "html"
```

## `[storage]`

Daemon-wide lance storage tuning — per-connection heap-cache caps and a
throttle on search-triggered index retrains. Host-level resource guards,
not per-corpus policy. Every key is optional; an absent `[storage]` section
resolves to the capped shipped defaults below. Each key shares one opt-out
convention: an explicit `0` restores the pre-knob lance behavior.

| key | type | default | meaning |
|---|---|---|---|
| `lance_index_cache_mb` | u64 | 256 | Cap for lance's per-connection index cache (MiB). Lance's own default is **6 GiB per kb connection** (byte-weighted, held for the table's lifetime) — the cap is what keeps a multi-kb daemon's heap bounded. `0` → lance's default. |
| `lance_metadata_cache_mb` | u64 | 64 | Cap for lance's per-connection metadata cache (MiB; lance default 1 GiB). `0` → lance's default. |
| `index_rebuild_min_secs` | u64 | 300 | Minimum interval between search-triggered FTS rebuilds, and independently between vector (IVF-PQ) rebuilds. Every upsert/delete still dirties the indexes; this only rate-limits the expensive `replace=true` full retrains. The first build after open is always immediate, and search freshness is preserved while throttled — lance scans fragments newer than the index (flat BM25 for FTS, brute-force for vector), so no rows are missed. `0` → rebuild on every dirty flag (pre-throttle behavior). |

```toml
[storage]
lance_index_cache_mb = 256
lance_metadata_cache_mb = 64
index_rebuild_min_secs = 300
```

Related maintenance behavior (not knobs): after every successful index
retrain and inside `compact_all`, kb garbage-collects orphaned
`<table>.lance/_indices/<uuid>` dirs — superseded index builds lance's own
cleanup never reaps. Only uuid-shaped dirs not referenced by the current
manifest and older than a 60-minute grace window are removed; reclamations
are logged at INFO (`orphan index GC reclaimed …`).

## `[defaults]`

D1 — daemon-wide fallback for any `[kb.<name>]` that omits its own
`embedding_model`. Precedence (highest first): per-kb `embedding_model` →
`[defaults] embedding_model` → the registry default (`bge-small-en-v1.5`).
See [self-host.md → Embedding model](self-host.md#embedding-model--picking-and-switching-d).

| key | type | default | meaning |
|---|---|---|---|
| `embedding_model` | string | none | Daemon-wide embedding-model fallback, used when a `[kb.<name>]` omits `embedding_model`. Must match a `kb_core::embed::SUPPORTED_MODELS` entry; an unknown name here is treated the same as unset (warn at boot). |
| `disable_embedder_fallback` | bool | `false` | D2 — suppress the final registry-default fallback. When `true`, a kb with no per-kb AND no `[defaults]` model resolves to `None` (lexical-only search, no embedder subprocess spawned). Useful for tests/CI that skip downloading model weights, or a deliberately no-embedder daemon. |

```toml
[defaults]
embedding_model = "bge-large-en-v1.5"
# disable_embedder_fallback = true   # lexical-only daemons / CI
```

## `[retention]`

v0.24 (R3) — opt-in retention windows for user-data tables. Daemon-wide
(uniform across kbs). Every key defaults to unset = **keep forever**, so
an absent `[retention]` section changes nothing. A set window arms one
background prune task (daily tick + once at boot) that deletes rows
older than `now - window`. Pruning is the documented retention exception
to the history append-only invariant (#8) — never an edit path.

| key | type | default | meaning |
|---|---|---|---|
| `history_days` | u32 | unset (keep forever) | Deletes `history` rows whose `started_at` is older than the window; each pruned visit cascades its child `reading_sections` rows. `0` is a hard validation error (it would delete ALL history on the next tick). |
| `reading_sections_days` | u32 | unset (share parent's lifetime) | Independent, more-aggressive prune of `reading_sections` rows by `last_at`, without touching the parent visit. `0` is a hard validation error. |

Only these two tables are prunable by age. `edges` has no timestamp
column and cannot be time-pruned — dangling edges are swept by the
delete cascade + reconcile orphan pass instead.

```toml
[retention]
history_days = 365
```

## `[sessions]`

W7 (sessions-rethink R15/LF-1) — daemon-wide (not per-kb; live transcripts
aren't corpus-bound) opt-in config for **Tier-1 live-follow**: the
"transcript IS the presence signal" liveness the `GET /api/sessions/presence`
probe and `GET /api/sessions/{sid}/live` route need. Omit the table (the
default) and Tier-1 is entirely off — every deployment stays byte-unchanged
from before W7: `/presence` answers the stable `{"enabled":false,"live":[]}`
and `/live` 404s. **Host/dev daemon only** — operator decision D-LF1
recommends leaving this unset in prod; the host/dev daemon is where the
operator actually sits during a live session, and mounting
`~/.claude/projects` into a prod container is a deliberate, separate
follow-up decision (never a silent flip). Tier-0 (capture-derived
presence/staleness, `sessionPresence.ts`) needs no config and works
everywhere already.

| key | type | default | meaning |
|---|---|---|---|
| `live_transcripts_dir` | path | unset (Tier-1 off) | Directory holding `<claude-project-slug>/<session_id>.jsonl` live transcripts — point this at `~/.claude/projects` (Claude Code's own transcript dir). `~`/`~/rest` is tilde-expanded at read time; anything else is used verbatim (resolved against the daemon's cwd, like every other path in this file). |
| `live_window_secs` | u64 | `120` | Seconds since a live transcript's last write before the presence probe drops it from the `live` set. `0` is accepted but warned on — it makes every transcript instantly read not-live regardless of recency (unset `live_transcripts_dir` instead to disable Tier-1). |

```toml
[sessions]
live_transcripts_dir = "~/.claude/projects"
# live_window_secs = 120   # default; rarely needs changing
```

**Security posture (LF-5):** both routes are **loopback-only, HARD, v1** —
a non-loopback request 403s even with a valid bearer token (stricter than
every other sessions route; `/raw`/`/export`/`/view` force a redaction
floor instead of refusing, since a live transcript is unscrubbed mid-flight
content with no floor that makes it safe to serve remotely). Both set
`Cache-Control: no-store`. See [`docs/http-api.md`](http-api.md)
for the route shapes and `kb sessions read --live`/`--follow` for the CLI's
direct-disk twin (zero daemon required).

## `[webhooks]`

Daemon-wide reactive bridge: one background subscriber forwards selected
event-bus envelopes (see [`docs/http-api.md`](http-api.md)
for `GET /api/events`) to an external URL as JSON POSTs. Omit the table to
disable it (nothing is spawned). It is
**read-only + post-emit** — it only reads the firehose and makes an
outbound request, so it adds no inbound surface and never writes storage.
See [`extending.md`](extending.md) for how this fits kb's extension model.

| key | type | default | meaning |
|---|---|---|---|
| `url` | string | **required** | Destination for `POST <url>` with the event envelope (`{v, id, type, ts, payload}`) as the JSON body. Must be `http://` or `https://`. |
| `types` | [string] | `[]` | Allowlist of event `type`s to forward, matched exactly (e.g. `artifact.indexed`, `comment.added`, `session.captured`). Empty forwards nothing — there is deliberately no "all events" form. |
| `timeout_ms` | u64 | `5000` | Per-POST timeout. The bridge `await`s each POST (bounded by this) only to log the result. |
| `allow_private` | bool | `false` | When `false`, only **loopback** and **public unicast** are allowed. When `true`, also permit **RFC1918 + ULA** (LAN hooks). **Always refused** (even with `allow_private`): link-local (`169.254/16`, `fe80::/10`), cloud metadata (incl. IPv4-mapped IMDS, AWS `fd00:ec2::/32`, Alibaba `100.100.100.200`), multicast, unspecified, CGNAT. |

**Outbound policy:** config validate and bridge spawn check URL syntax +
any IP-literal policy (no DNS, so the async config path never blocks);
**each POST** runs `prepare_webhook_dial` (resolve → filter → **pin** dial
IPs via `reqwest` `resolve_to_addrs`), closing the DNS-rebinding TOCTOU
between policy check and connect. The URL is parsed with the same
`reqwest::Url` the client dials, so the pin can't be keyed on a host the
client won't use. Redirects are disabled so a `302` cannot retarget past
the pin. Public unicast destinations are always OK.

Delivery is fire-and-forget and **eventually-consistent**: a slow or
unreachable endpoint makes the subscriber lag and *drop* events (logged
at warn) rather than back-pressure the daemon — the bounded event-bus
guarantee. Consumers must tolerate gaps; never use a webhook for a
synchronous in-request decision.

```toml
[webhooks]
url = "http://127.0.0.1:9000/kb-hook"
types = ["artifact.indexed", "comment.added", "session.captured"]
timeout_ms = 3000
# allow_private = true   # only if the receiver is on your LAN
```

For a concrete `session.state` → ntfy stanza (plus the shell-based
alternative for a filtered, human-readable notification), see
[`live-sessions.md` → Notification](live-sessions.md#notification-without-writing-daemon-code).

## `[backup]`

GC-B4 — daemon-wide, optional. Omit the table (the default) and `kb
backup` behaves exactly as before: a local-only tarball under
`<state>/exports/`. Set both keys to run an off-host copy step right
after a successful local backup.

| key | type | default | meaning |
|---|---|---|---|
| `remote_cmd` | [string] | none | Explicit argv for the off-host copy command — **not** a shell string, so no shell is ever invoked (no quoting/injection surface). `{src}` is substituted with the local tarball's path, `{dest}` with `remote_dest`. The program is resolved via `PATH`. |
| `remote_dest` | string | none | Destination passed through verbatim as `{dest}`, e.g. `remote:bucket/path` (rclone) or `user@host:/path/` (scp/rsync). |

Both keys must be set for the copy to run; either alone is a no-op and
`kb config validate` warns. An empty `remote_cmd` array is a hard
validation error (nothing to execute).

The copy is **best-effort**: `kb backup` runs it after the local tarball
is already written, and a failed or unreachable remote (bad credentials,
network down, uploader not installed) is reported loudly — a stderr
warning plus an annotated summary line on `kb backup`'s own stdout — but
never fails the backup command itself (exit code stays `0`; the local
tarball is already a complete backup on its own).

```toml
[backup]
remote_cmd  = ["rclone", "copyto", "{src}", "{dest}"]
remote_dest = "remote:bucket/kb-backups/"
```

## `[memory]`

MI-W2.1/2.2, split MI-W5.R (2026-08-08 operator ruling) — daemon-wide,
optional. Same precedent as `[retention]`/`[backup]`: an operator-level
toggle, not per-corpus (a `RecallHit` already spans corpora inside one
`/api/memory/recall` fan-out).

The original single `scoring_v2` flag gated two factors the W5.1 live-corpus
bench measured UNEQUALLY: relevance is strongly evidenced (25 queries, 14
better / 11 wash / 0 worse, 23/25 returning a different id set, +3% mean
latency); stability (the FSRS term) was never exercised at all, since the
bench didn't load the sessions corpus its `memory_recalls` ledger lives in.
The flag was split so the measured factor could ship without also enabling
the unmeasured one.

| key | type | default | meaning |
|---|---|---|---|
| `scoring_v2_relevance` | bool | `true` | Gates invariant #10's v2 **relevance** factor: a per-corpus min-max normalized search-engine relevance term folded into `score` alongside `rel`. **Measured** on the live corpus (W5.1 bench) — ships ON. `false` reverts this factor to the pre-W2 formula exactly (skipped, not multiplied by a neutral `1.0`). |
| `scoring_v2_stability` | bool | `false` | Gates invariant #10's v2 **stability** factor: an FSRS-inspired multiplier derived from the memory's own `memory_recalls` ledger (W1) — a memory that keeps getting actually recalled decays more slowly, bounded so it can never become immortal. **Unmeasured** on the live corpus (the W5.1 bench never loaded the sessions corpus this ledger lives in) — stays OFF pending a bench that does. |
| `scoring_v2` | bool | *(absent)* | **Deprecated** back-compat alias for both flags above. If present, overrides BOTH `scoring_v2_relevance` and `scoring_v2_stability` to its own value (reproducing the pre-split "one flag gates both" behavior) and logs a one-line deprecation warning at boot. Migrate to the two flags above instead of relying on this. |

Each factor is always surfaced independently when its own flag is on
(`relevance_factor`/`stability` on the wire hit, never silently absorbed
into `score` alone), so `kb recall --explain` renders the full arithmetic of
whichever factors are active.

```toml
[memory]
scoring_v2_relevance = true
scoring_v2_stability = false
```

### `Kb-Memory` commit trailers (CT-F1) — **not** a kb.toml key

The memory↔commit exact-id join (`GET /api/kb/{kb}/memories/{id}/commits`,
`kb why-memory`'s "committed in (exact-id citations)" section, the SPA
dossier's "Cited in commits") is fed by a `Kb-Memory: <hex12>` commit
trailer that `plugins/kb-memory/hooks/git-dispatch/trailer-logic.sh`
stamps. Writing opaque memory ids into git history is fine in a private
repo and not a sane default (operator ruling, 2026-08-20), so it is
**opt-in per repo and OFF by default**.

The gate deliberately does NOT live in `kb.toml`: the decision is a
property of the REPO being committed to, not of the daemon or of a
corpus — one machine routinely commits to both private and public repos,
and a daemon-wide switch could only ever be wrong for one of them. It is
therefore a repo-local git config key, set in the repo you want stamped:

```sh
git config --local kb.memoryTrailers true       # opt this repo in
git config --local --unset kb.memoryTrailers    # opt back out
git config --local --get kb.memoryTrailers      # check
```

`--local` is enforced by the hook (`git config --local --get --bool`), so
a value inherited from `~/.gitconfig` or the system config is ignored:
opting one repo in can never leak ids out of another. An un-opted-in repo
pays one `git config` read per commit and nothing else — no daemon call,
byte-identical commit messages.

The READ side is unconditional (kb always parses a `Kb-Memory:` trailer
it finds), and an empty result is a non-signal — see
[plugins/kb-memory/hooks/README.md](../plugins/kb-memory/hooks/README.md)
(“`Kb-Memory` trailers”) for the full contract, the 20-per-commit cap,
and the fail-open behaviour.

## `[share]`

`kb share` publishing config (Cloudflare Pages + Access, GitHub Pages).
Daemon-wide, optional — omit the table entirely and `kb share` errors with
a setup hint naming which host you asked for. Secrets are never inlined
here (see [self-host.md → Sharing artifacts](self-host.md#sharing-artifacts-kb-share)
for env-var wiring on systemd/Docker).

| key | type | default | meaning |
|---|---|---|---|
| `live_origin` | string | none | Live kb origin used to build absolute links (`kb share --links absolute`); only required for that mode. |
| `cloudflare` | table | none | See `[share.cloudflare]` below. |
| `github` | table | none | See `[share.github]` below. |

### `[share.cloudflare]`

| key | type | default | meaning |
|---|---|---|---|
| `account_id` | string | **required** | Cloudflare account id. |
| `team_domain` | string | **required** | Zero-Trust team domain, `<team>.cloudflareaccess.com`. |
| `google_idp` / `github_idp` | string | none | Pre-registered IdP UUID, for `--gate google`/`--gate github`. |
| `api_token_env` | string | `"KB_CF_API_TOKEN"` | Name of the env var the daemon reads the Cloudflare API token from at share time (the token itself never lives in `kb.toml`). |

### `[share.github]`

| key | type | default | meaning |
|---|---|---|---|
| `owner` | string | **required** | GitHub user/org that owns the created share repos. |
| `token_env` | string | `"KB_GH_TOKEN"` | Name of the env var the daemon reads the GitHub token from. |

```toml
[share]
live_origin = "https://kb.example.com"

[share.cloudflare]
account_id  = "<cf-account-id>"
team_domain = "<team>.cloudflareaccess.com"

[share.github]
owner = "<github-user-or-org>"
```

## `[kb.<name>]`

One table per knowledge base. `<name>` is the kb's identifier used in
URLs (`/api/kb/<name>/…`) and `--kb`.

| key | type | default | meaning |
|---|---|---|---|
| `path` | path | **required** | Source folder watched + indexed for this kb. |
| `skip_patterns` | [string] | `[]` | Glob patterns to skip during indexing (e.g. `.git`, `*.tmp`, `drafts/**`). |
| `embedding_model` | string | none | Model name (must match a supported model). **Absent → no embedder:** the indexer skips embedding and `/api/search?mode=hybrid\|semantic` returns `400` (keyword search still works). The on-disk embedding-column width is fixed at first index; changing dims needs `kb model set --in-place` or a `kb reset`. |
| `reranker_model` | string | none | SQ4 — opt-in cross-encoder reranker name (from the reranker registry, ~1 GB model). A load failure is non-fatal (falls back to unranked hybrid/keyword results). |
| `chunked_embeddings` | bool | `false` | SQ5 — opt-in passage/chunk embeddings into a separate `artifact_chunks` table for finer-grained semantic hits. Flipping it on needs a `kb reindex` to backfill existing artifacts. |
| `graph_boost` | float | none | GS-track — additive post-fusion hybrid-search boost from the edge graph: `weight/60 × sqrt(in_degree)/sqrt(max_in_degree)`. Sane range `(0, 4]`; an out-of-range value warns at validate time and still applies verbatim. |
| `memory_scope` | string | none | v0.9 (M2) — marks this corpus as agent-memory: `"global"` or `"project"`. Unknown value warns and is treated as unset. |
| `decay_policy` | string | none | v0.13 — recall decay curve for a memory corpus: `"strict"`, `"balanced"`, or `"loose"`. Only meaningful when `memory_scope` is set; unknown value warns and falls back to the daemon-wide policy cell. |
| `default_search_category` | string | none | R0 — per-kb default for the `?category` filter on a `scope=one` search when the request omits it (e.g. `"memory-session"` on a sessions corpus). Never applies to `scope=all` (federated search builds its filters from the request alone). |
| `reading_progress` | bool | `true` | RP-track — per-kb reading-progress capture toggle. `false` makes `POST …/history/reading` a no-op (`204`) for this kb. |
| `versions` | string | `auto` | Source for the artifact **Versions/Diff** timeline (`kb versions` / `kb diff`, the SPA Versions panel): `auto` (git history when the file is tracked, else kb index snapshots — per-file hybrid), `git`, `index`, `both` (union), or `off`. Index snapshots are captured on each changed-content re-index and pruned to the newest 25 per artifact (memory-session transcripts excluded); a `git`/`off` kb stores none. In the deployed container (no `.git`/`git` binary) `git`/`auto` degrade cleanly to snapshots / an empty timeline. |
| `indexable_extensions` | table | inherit | v0.24 (X1) — per-kb override of `[indexer] indexable_extensions` (same shape + validation rules; see that entry). Absent → inherit the daemon-wide map, else the built-in set. |
| `reconcile_secs` | u64 | inherit | PF-I1 — per-kb override of `[indexer] reconcile_secs` (same semantics — `0` disables the background walk, just for this kb; the explicit `POST /api/kb/{kb}/reindex` still works). Absent → inherit the daemon-wide `[indexer] reconcile_secs`, else 60. Resolution order is `KB_RECONCILE_SECS` env (still trumps everything, including this) → this kb's `reconcile_secs` → daemon-wide `[indexer] reconcile_secs` → 60. The periodic auto-compact ticker, which is already spawned per kb, follows this same resolved value. |
| `capture_dir` | string | `"capture"` | v0.25 (U1) — subfolder (relative to `path`) quick-captured files land in (`kb capture`, the SPA capture sheet, the Web Share Target route). Provenance-stamped, then indexed like any other artifact — not a separate storage surface. |
| `code_url` | string | none | v1 (DCB) — kb-code SPA base URL this kb's Code section resolves references against (e.g. `https://kbc.example.com`, or `http://127.0.0.1:4747` for a colocated local dev daemon). Absent → the Links tab's Code section renders ONE honest "not linked to a code repo" note and nothing else — the raw extracted rows are deliberately NOT shown (an unresolved ref with no href and no repo context reads as noise). Follows `ProjectSection.code_url`'s field shape (`config.rs`) but lives on `[kb.<name>]` directly — `code_repo` (singular checkout pin) is deliberately NOT carried over: DCB's checkout selection is a per-read-time human pick (the scorecard), never a config-file mapping. |

### `[kb.<name>.ui]`

Same keys as the top-level `[ui]` (`theme`, `accent`, `density`),
overriding the daemon default for this kb only.

### `[kb.<name>.atlas]`

Per-kb 2-D layout overrides for the SPA atlas view.

| key | type | default | meaning |
|---|---|---|---|
| `k` | usize | √n | Cluster count (k-means). Set a fixed value to pin the legend; clamped to `[1, MAX_CLUSTERS]`. |
| `layout` | string | `umap` | `umap` (default) or `pca`. Use `pca` only when a corpus's UMAP layout collapses. Unknown values fall back to `umap` with a warn. |

### `[kb.<name>.search]`

Query-time keyword-search knobs.

| key | type | default | meaning |
|---|---|---|---|
| `typo_tolerance` | bool | `false` | GC-D1 spike: a zero-hit **fallback**, not a query rewrite. The exact query always runs first, unchanged; only when it returns **zero hits** does `mode=keyword` (and the BM25 half of `mode=hybrid`) retry through lance's native fuzzy full-text query (max edit distance 1 — a Levenshtein automaton over the FTS index's own term dictionary) and return that instead. Any query the exact pass answers is byte-identical whether the flag is on or off, so `true` is bench-confirmed quality-neutral (an always-fuzzy rewrite was rejected — it regressed Recall@1 0.150→0.000 on typo-free queries). Query-time only, no reindex on flip. `false` (the default) issues the exact same single query as before this knob existed — deterministic tie order is unaffected either way. Bench findings: `docs/research/typo-tolerance-spike-2026-07.html`. |

```toml
[kb.notes.search]
typo_tolerance = true
```

### `[kb.<name>.resurface]`

W2.9 — per-kb override of the resurfacing queue's scoring weights (`kb
resurface` / the gallery strip / the review overlay). All five fields are
independently optional; an absent field keeps the shipped default for
just that knob. These are **tuning knobs, not architecture** — like
`graph_boost`, a nonsensical value (non-finite, non-positive weight/
half-life, or a `0` saturation) warns at `PUT /api/config` time and boot
still applies the shipped default for that ONE field; the daemon never
refuses to start over a bad resurface value. Every renderer (the CLI's
`--explain`, the SPA score chips) reads the ACTUAL weights back off the
`GET .../resurface` response, so a tuned kb's arithmetic can never drift
from what's displayed — tune and observe via `kb resurface --explain`,
there's no auto-flip.

| key | type | default | meaning |
|---|---|---|---|
| `comment_weight` | f32 | `0.6` | Weight of the open-comments term. |
| `read_weight` | f32 | `0.4` | Weight of the unfinished-read term. |
| `comment_saturation` | u32 | `4` | Open-comment count where the comment term saturates at 1.0. |
| `read_halflife_days` | f32 | `45.0` | Half-life (days) of the unfinished-read term's decay. |
| `score_floor` | f32 | `0.05` | Items scoring below this are dropped entirely. |

```toml
[kb.notes.resurface]
comment_weight = 0.8
read_halflife_days = 20.0
```

### `[kb.<name>.slo]`

CT-F5 — per-kb **corpus-health SLO targets**. Four indicators computed from
tables kb already keeps, read via `GET /api/kb/<name>/slo`, `kb slo status`,
and the SPA's Settings → **SLOs** tab.

**Surfaced, never enforced.** Nothing changes behaviour on a missed target:
no alert, no gate, no retry, no auto-repair, and no ranking/recall consumer
anywhere. `kb slo status` exits `0` even on a warn — deliberately, so an SLO
never lands on a CI gate's critical path and becomes a number people tune to
stay green. If you want an alert, build it from `kb slo status --json`.

**Every key is optional and so is the whole table.** A kb with no `[slo]`
section still gets a full four-indicator report — every indicator *measured*,
every status `unknown` for want of something to judge it against. Configuring
a target adds a verdict; it never adds an indicator.

**Status vocabulary** is `ok | warn | unknown`, with no `fail` (a "fail"
invites something to act on it). `unknown` covers two honest cases, told apart
by whether `value` is present: *not measurable* (the inputs genuinely aren't
there) and *measured but untargeted*. A `null` value is never rendered as `0`
— an unmeasured indicator and a measured zero are different facts.

Out-of-range targets (negative, non-finite, or a percentage above 100) **warn
at validate time and still apply verbatim** — the `graph_boost` precedent. The
daemon never refuses to start over an SLO target, and a bad one only pins its
own indicator to a constant status.

| key | type | direction | meaning |
|---|---|---|---|
| `coderef_resolution_pct` | f64 | **minimum** | Percentage of this corpus's extracted `code_refs` hints whose kind carries a **local-tree path shape** (`path`, `path_line`, `path_range`, `path_list`). Symbol (`Foo::Bar`, `Foo#bar`), `issue` and `external` (gem/vendor) hints stay in the denominator but never the numerator — none of them names a file in the local tree. **This is a structural measure, not a resolution result:** kb has no checkout and never calls kb-code (invariant #2), so a counted hint may still point at a file that no longer exists. It answers "what share of what this corpus cites is even the *kind* of thing a code daemon could resolve" — a corpus drifting toward bare symbol names and vendored paths is drifting out of reach of the doc↔code bridge. `unknown` when the corpus has no `code_refs` rows at all (never 0% — a corpus that cites no code has not failed at citing code). |
| `orphan_kb_sessions` | f64 | **maximum** | Count of docs carrying a `kb_session` for which **no `sessions` row exists anywhere on this daemon**. The join is daemon-wide on purpose (invariant #11): a memory written during a session routinely lives in a different corpus than its transcript, so a per-kb-only check would call every memory an orphan. An orphan is a dangling provenance pointer — the capture never landed, the transcript was deleted, the transcripts corpus isn't mounted here (a legitimate deployment), or the `kb_session` hint itself is dirty. A nonzero count is an invitation to look, not a defect. Transcripts are included in this scan (unlike the `memory_count` reads, which exclude them). A corpus where nothing carries a `kb_session` measures an honest `0`. |
| `ledger_parse_failure_pct` | f64 | **maximum** | Percentage of injected memory-recall hits that parsed via **neither** the `kb-recall/1` machine marker **nor** the free-text fallback grammar, and therefore produced no `memory_recalls` row — `failed / (marker_parsed + fallback_parsed + failed)` over the CT-A3 census (V0039), scoped to the newest capture per session. A nonzero rate means the recall ledger is silently under-recording: memories were shown to an agent and no row says so, which is invisible from every other surface. `unknown` when no capture carries a census — captures written before V0039 are **not backfilled**, so this reads `unknown` until the next capture lands — or when the censused captures walked zero injected hits (an empty denominator is not 0%). |
| `capture_freshness_hours` | f64 | **maximum** | Hours between now and the newest `sessions.started_at` in this corpus. The staleness of the capture pipeline itself: a sessions corpus whose newest capture is four days old usually means the Stop hook stopped firing — a failure mode that is otherwise completely silent (the daemon is healthy, search works, the corpus just stops growing). Clock skew clamps to `0.0` rather than reporting a negative age. `unknown` when the corpus has no `sessions` rows (a non-sessions corpus is not stale, it simply has no capture pipeline). |

```toml
[kb.sessions.slo]
capture_freshness_hours  = 48      # warn if nothing captured in 2 days
ledger_parse_failure_pct = 1.0     # warn above a 1% silent-drop rate
orphan_kb_sessions       = 0       # warn on any dangling session pointer

[kb.research.slo]
coderef_resolution_pct = 70.0      # warn below 70% path-shaped citations
```

**The snapshot log.** `kb slo snapshot [--kb NAME]` appends one row per
indicator to a per-kb, **append-only** `slo_snapshots` table (`kb slo log`
reads it back, newest first; `GET /api/kb/<name>/slo/snapshots`). Every run
lands — there is deliberately no skip-if-unchanged, because a flat line is
itself the signal. The daemon **never snapshots on its own**: an operator (or
their cron) decides when a reading is worth keeping, so the log can't quietly
become a retention question. Each row stores the target it was judged by, so
an old reading stays interpretable after you change your targets. There is no
prune and no update path.

### `[kb.<name>.outbound]`

Optional privacy scrub applied **only when an artifact leaves the daemon
for a non-loopback origin** (e.g. through a reverse proxy to an outside
reviewer). Loopback requests are never scrubbed. See the README's
"Outbound scrubbing" section.

| key | type | default | meaning |
|---|---|---|---|
| `strip_kb_prompt` | bool | `false` | Drop any `<template id="kb-prompt">` element before sending. |
| `redactions` | [table] | `[]` | Ordered regex rules; each `{pattern, replacement}` is applied to the full body. An invalid pattern no-ops with a warn. |

```toml
[kb.work.outbound]
strip_kb_prompt = true
redactions = [
  { pattern = "INTERNAL-\\d{4}", replacement = "[redacted]" },
]
```

### `[kb.<name>.templates]`

Named HTML templates resolvable by `kb new --template <name>`. Keys are
short names; values are paths (prefer absolute — relative paths resolve
against the cwd at `kb new` time). Templates support `{{title}}`,
`{{date}}`, `{{slug}}`, and any `{{key}}` from `--var key=value`.

```toml
[kb.notes.templates]
idea = "/home/me/kb-templates/idea.html"
fix  = "/home/me/kb-templates/fix.html"
```

```bash
kb new --template idea --title "Ring buffer sizing" --kb notes --out notes/ring.html
```

## `[projects.<id>]`

W3.A — a declarative registry that relabels/merges the sessions "projects
home" grouping. `<id>` is a stable, operator-chosen key (e.g.
`[projects.kb]`); zero config still gives a useful projects home via
auto-projects (basenames of the derived project key), so this whole table
is optional.

| key | type | default | meaning |
|---|---|---|---|
| `label` | string | the registry id | Display label for this project. |
| `roots` | [string] | `[]` | Absolute path prefixes this project owns (worktrees, renamed subdirs). Empty matches nothing (warn); a non-absolute entry is a hard validation error. |
| `kb` | string | none | Primary `[kb.<name>]` corpus this project maps to, if any. |
| `code_url` | string | none | kb-code SPA base URL for this project, if any (P7/P8 integration). |
| `code_repo` | string | none | The `:repo` route segment kb-code uses for this project, if any. |

```toml
[projects.kb]
label = "kb"
roots = ["/home/me/project/kb"]
kb    = "research"
code_url  = "https://kbc.example.com"
code_repo = "kb"
```

## A fuller example

```toml
[daemon]
name = "workstation"

[server]
addr = "127.0.0.1:4000"
artifact_host_suffix = ".artifacts.localhost"

[server.rate_limit]
search = 120

[indexer]
reconcile_secs = 30

[ui]
theme = "ink"

[kb.notes]
path = "/home/me/notes"
embedding_model = "bge-small-en-v1.5"
skip_patterns = [".git", "*.tmp"]

[kb.notes.atlas]
k = 8

[kb.work]
path = "/home/me/work-artifacts"
embedding_model = "bge-base-en-v1.5"

[kb.work.outbound]
strip_kb_prompt = true
```

## kb-code.toml (kb-code daemon config)

`kb-code` — the separate, sibling read-oriented code-browsing daemon (see
[`docs/kb-code.md`](kb-code.md)) — reads its own
`kb-code.toml` (default `<KbPaths::new("kb-code").config>/kb-code.toml`,
override with `--config`; schema source of truth:
`crates/kb-code-server/src/config.rs`). It is a **completely separate file**
from `kb.toml` above — the two daemons share nothing but the config-dir
layout convention. This section covers only the sections v0.39 ("The PR
Room" / lip track) and S2-B ("Mobile mutations," kb-code v6.0) added; the
full section list (`[server]`, `[[repos]]`, `[watcher]`, `[semantic]`,
`[transcripts]`, `[doclens]`, `[github]`, `[review]`, `[behavioral]`, …) is
enumerated in that file's own module doc.

### `[kb_daemon]`

Where the Search-Everywhere box's sessions lane, the join ladder's
session↔commit federation, and the unified inbox's kb lane all federate to
`kb` — the OTHER daemon in this workspace, a separate process/binary. **V76-R4f:
DISABLED unless `url` is configured.** A minimal, `[[repos]]`-only
`kb-code.toml` must never federate against a live kb it was never pointed
at — a throwaway/local install used to inherit kb's own documented default
bind (`127.0.0.1:4000`) and quietly reach for whatever happened to be
listening there.

`enabled` therefore has no static default — it resolves from what the
operator actually wrote:

| `enabled` in toml | `url` in toml | resolved `enabled` |
|---|---|---|
| absent | absent | `false` |
| absent | set | `true` |
| `true` | absent | **boot error**, names `kb_daemon.url` |
| `true` | set | `true` |
| `false` | absent or set | `false` |

i.e. `enabled` defaults to `url.is_some()`; writing `enabled = true` with no
`url` refuses to boot rather than silently defaulting to `127.0.0.1:4000`.
Every kb-federated lane already degrades honestly when the section is off —
an additive `disabled` reason beside the existing `unreachable` one, never a
silent empty and never a `500` — so a daemon with no `[kb_daemon]` at all
still boots and answers every route, just without a sibling kb to ask.

| key | type | default | meaning |
|---|---|---|---|
| `enabled` | bool | `url.is_some()` | Master switch — see the table above. |
| `url` | string | none | kb's federation base URL (e.g. `http://127.0.0.1:4000` for a native side-by-side install, `http://kb:4000` for a container deployment). |
| `token_file` | string (path) | none | File holding kb's bearer token — needed whenever kb's `auth_bearer` doesn't see this daemon as loopback (e.g. the docker-published shape, where the container's peer is the bridge gateway IP). The FILE path lives in config, never the secret itself. |
| `public_url` | string | none | The browser-facing base URL for links kb-code's UI builds into kb's own SPA. Falls back to `url` when unset — correct for the native side-by-side install, wrong for a hosted/container deployment where `url` is a container hostname a browser can't resolve. |

```toml
[kb_daemon]
url = "http://127.0.0.1:4000"
```

An existing deployment that already sets `url` explicitly is unaffected by
this amendment — it only changes the behaviour of a `kb-code.toml` that
never configured the section at all.

### `[scip]`

PRR-N12 (N1)'s per-repo SCIP-indexer config — array-of-tables, one
`[[scip.repos]]` entry per repo `kb-code scip run` knows how to index. The
daemon only ever **parses** this table; it never spawns the configured
`command` itself — `kb-code scip run` (kb-code-cli) is the only thing that
executes it. A repo with no `[[scip.repos]]` entry simply reports
`ScipStatus::configured = false` on `GET /api/repos` — this section carries
no implicit per-repo default.

| key | type | default | meaning |
|---|---|---|---|
| `repos` | `[[scip.repos]]` | `[]` | See below. |

One `[[scip.repos]]` entry:

| key | type | default | meaning |
|---|---|---|---|
| `name` | string | — required | Must match a configured `[[repos]] name` exactly. |
| `command` | [string] | — required | Argv array (`command[0]` is the executable, the rest its args) — **never** a shell string; spawned via `Command::new(command[0]).args(&command[1..])`. |
| `output` | string | — required | Repo-root-relative path to the `.scip` file the indexer writes (e.g. `"index.scip"`); `kb-code scip run` chains straight into `scip ingest` against `<repo_path>/<output>` once the indexer exits 0. |
| `langs` | [string] | `[]` | Informational only — surfaced on `GET /api/repos`'s `ScipStatus` and used to compute `docs_total`; never gates or validates the ingest. |

```toml
[[scip.repos]]
name = "kb"
command = ["rust-analyzer", "scip", "."]
output = "index.scip"
langs = ["rust"]
```

### `[comments]`

V72-J1's `comments/1` annotation keyword grammar (`crate::comments::
keywords`). One key. Absent (or an all-blank list) ⇒ the shipped default
set.

**The override REPLACES the default set; it does not extend it.** That is
the point: the default is a *vocabulary*, and a team whose codebase says
`DEBT`/`PERF` wants those eight gone, not eight more. Matching is
case-sensitive, at ASCII word boundaries, anywhere in a comment line, with
the LEFTMOST hit winning; a colon is not required (RuboCop's
`RequireColon` is a rule about how to WRITE an annotation, not about what
exists in the tree).

The effective set is part of the per-row `comments_version` key, so
changing it re-extracts every file rather than leaving rows classified
under the old vocabulary. It is read ONCE at boot — no live reload, same
posture as `[occurrences]`/`[scopes]` — and `GET /api/comments/keywords`
(`kb-code comments keywords`) reports what is actually in force.

| key | type | default | meaning |
|---|---|---|---|
| `keywords` | [string] | `[]` | Uppercase annotation keywords. Empty ⇒ `TODO FIXME OPTIMIZE HACK REVIEW NOTE XXX BUG` — RuboCop `Style/CommentAnnotation`'s six, plus the two extra markers the pre-`comments/1` TODO index scanned. |

```toml
[comments]
keywords = ["TODO", "FIXME", "DEBT", "PERF"]
```

**This narrows `GET /api/todos` too.** That route is a filtered view over
the same index (`kind = annotation` and a keyword in the legacy family
`TODO FIXME HACK XXX BUG`), so a keyword the effective set no longer names
stops appearing there as well — the honest consequence of there being
exactly one scanner over these lines.

### `[rails_lens]`

PRR-N3's operator override for the Rails-lens auto-detection gate
(`frameworks::rails::detect_is_rails`, which greps a repo's working tree for
`config/routes.rb` + a `Gemfile` `gem "rails"`/`gem 'rails'` line). Unlike
`[semantic]`/`[occurrences]` (an explicit on/off default an operator opts
into), the base decision here is auto-detected per repo — this section is a
pair of override lists reached for only when auto-detection is wrong (a
vendored Rails-shaped fixture that isn't really a Rails app, or a Rails app
that keeps `routes.rb` somewhere non-conventional). `disabled_repos` is
checked first — an explicit "never" always wins over an explicit "always,"
which in turn wins over the auto-detected default.

| key | type | default | meaning |
|---|---|---|---|
| `repos` | [string] | `[]` | Repo NAMEs force-**enabled** regardless of auto-detection. |
| `disabled_repos` | [string] | `[]` | Repo NAMEs force-**disabled** regardless of auto-detection (checked before `repos`). |

### `[review]`

`remote_mutations` (S2-B, "Mobile mutations," kb-code v6.0) graduates FIVE
review-mutation route families — finding disposition `PUT`/`DELETE`,
verdict `PUT`/`DELETE`, finding/verdict publish-recording `POST`, and
manual finding create `POST` — off pure loopback-only so a non-loopback
caller carrying a valid bearer token can reach them too (e.g. from a phone
on the same tailnet). **Fail-closed default**: `false` — a non-loopback
caller keeps getting the same `404` a loopback-only route always returned
(never a `401`/`403` that would confirm the route's existence) until an
operator opts in. A loopback caller is unaffected either way. **The
working-tree mutation line never moves**: `checkout`, suggestion apply/
apply-batch, `scip/ingest`, `prs/fetch`, and every OTHER review mutation
(create/snapshot/patch/delete/viewed/gc, `/reviews/pr`, `/reviews/sweep`,
`/reviews/{id}/report`, `findings/import`) stay loopback-only HARD
regardless of this flag — see `crates/kb-code-server/src/review_gate.rs`'s
module doc for the full admission table. `GET /api/identity`'s
`remote_mutations` field mirrors this setting for capability discovery
(never required — the SPA renders a small "Remote review mutations: on/off"
chip on Home when present, but every route enforces the gate itself).

| key | type | default | meaning |
|---|---|---|---|
| `remote_mutations` | bool | `false` | Admit a non-loopback bearer caller on the five graduated route families. |

```toml
[review]
remote_mutations = true
```

### `[[intel.providers]]`

PRR-L2's lip/1 provider registry (`crate::lip`) — the live-LSP overlay
(`precision: "lsp-live"`) that sits ahead of the deterministic SCIP/
resolve-ladder tiers for a configured `(repo, lang)` pair. Array-of-tables,
mirroring `[scip]`'s per-repo shape; empty by default (no repo carries a
live-LSP overlay until explicitly configured). `[semantic]`-style allowlist
semantics: a provider only applies when **both** its `langs` and `repos`
lists name the pair — an empty `repos` opts in **zero** repos, never "every
repo."

| key | type | default | meaning |
|---|---|---|---|
| `providers` | `[[intel.providers]]` | `[]` | See below. |

One `[[intel.providers]]` entry:

| key | type | default | meaning |
|---|---|---|---|
| `name` | string | — required | This daemon's own label for the provider (need not match the adapter's own `server_name`); echoed back on `GET /api/repos`'s `intel.provider`. |
| `url` | string | — required | Base URL of the running `kb-lip` instance (e.g. `"http://127.0.0.1:4841"`). |
| `langs` | [string] | `[]` | Language ids this provider covers (e.g. `["ruby"]`). |
| `repos` | [string] | `[]` | Allowlist of `[[repos]] name`s this provider applies to — empty means none, not all. |

```toml
[[intel.providers]]
name  = "ruby"
url   = "http://127.0.0.1:4841"
langs = ["ruby"]
repos = ["my-rails-app"]
```

The provider itself — `kb-lip`, a new crate/binary (`crates/kb-lip`) that
adapts one LSP process to the closed `lip/1` HTTP protocol — is configured
and run separately from `kb-code-server`; see
[`providers/README.md`](../providers/README.md) for the full walkthrough
(installing a language server, the reference `ruby-lsp.toml`/
`solargraph.toml` configs, the blob-guard mechanism, and live-smoke evidence
against a real Rails app). S2-D adds four more reference configs alongside
those two — `rust-analyzer.toml`, `typescript-language-server.toml`,
`pyright.toml`, and `gopls.toml` (ports 4845/4847/4849/4851, one language
each) — plus matching `Dockerfile.rust`/`Dockerfile.typescript`/
`Dockerfile.python`/`Dockerfile.go` sidecar images, all reference-only and
never CI-built, same posture as `Dockerfile.ruby`.

A repo can legitimately carry **more than one** `[[intel.providers]]` entry
— a mixed-language repo like kb itself (Rust + TypeScript) is the motivating
case. `GET /api/repos`'s `intel` field stays single-valued (the FIRST
config-order match, back-compat); `intel_providers` (additive) is the full
array of every provider entry naming a given repo, in config order — same
`{provider, langs, alive, server_version}` shape as `intel`, just one row per
matching provider instead of one.

### `[server]` — V70-A2 hardening keys

`addr` is the pre-existing listen address. V70-A2 ("local-daemon
hardening," the v7 security critique's SEC-02/SEC-15) adds two keys.

| key | type | default | meaning |
|---|---|---|---|
| `hostnames` | [string] | `[]` | Non-loopback `Host:` values this daemon answers to, beside the always-allowed loopback forms (`localhost`, `127.0.0.1`, `[::1]`, `::1`, any port). |
| `git_fanout` | int | `4` | Permits on the daemon-wide git-subprocess semaphore (merge-check, the branches ahead/behind loop). `0` is coerced to `1`. |

**`hostnames` and the DNS-rebinding gate.** kb-code's `Host` allowlist is
enforced **unconditionally for a loopback peer** — that is the rebinding
attack (the victim's browser resolves `attacker.com` to `127.0.0.1`,
connects from the box, and inherits the daemon's tokenless loopback
bypass), and closing it needs no configuration. For a **non-loopback**
peer the check is enforced only once `hostnames` is non-empty, so an
already-deployed reverse-proxied daemon (e.g. `kbc.example.com`, where
`auth_bearer` is the real gate) does not go dark on upgrade; the daemon
logs one boot warning naming this key when it binds non-loopback with the
list empty. **Set it in any reverse-proxied deployment** to get strict
Host checking on every peer:

```toml
[server]
addr = "0.0.0.0:4747"
hostnames = ["kbc.example.com"]
```

Origins are checked against the same allowlist (plus the request's own
`Host`, plus every `[doclens] origins` entry) — an absent `Origin` passes,
matching kb's own `origin_allowlist` rule for curl/CLI/in-process callers.
Full admission table: `crates/kb-code-server/src/security/origin.rs`.

### `[security]`

V70-A2's denylist + strict-mode knobs (SEC-13, SEC-02).

| key | type | default | meaning |
|---|---|---|---|
| `secret_globs` | [string] | `[]` | Extra denylist globs, **additive** to the built-in floor — config can widen the policy, never narrow it. |
| `strict_request_header` | bool | `false` | Require `X-Kbc-Request: 1` on EVERY mutating `/api` request, not only browser-originated ones (those carrying an `Origin`). |

The built-in floor, always enforced, on `GET /api/file`, `GET /api/pack`
and every other content-returning read: `.env`, `.env.*`, `*.key`,
`*.pem`, `*.p12`, `*.pfx`, `id_rsa*`, `config/master.key`,
`config/credentials/*`, `*.sqlite3`, `*.keystore`. A match is a typed
`403` (`urn:kb:errors:redacted-by-policy`) naming the matched **pattern**,
never the bytes. A pattern containing `/` matches the repo-relative path;
every other pattern matches the basename; `*` never crosses a `/`.

Separately, served text is content-sniffed for private-key headers,
AWS-shaped access keys and line-leading `password=`/`secret=` assignments;
a match sets an additive `redaction_hint: true` on the file wire and
**withholds nothing** — the denylist refuses, the hint only informs.

`strict_request_header` is a tightening opt-in. By default the
`X-Kbc-Request: 1` requirement applies to requests carrying an `Origin`
(every browser-driven mutation), which is what stops a page on another
localhost port from driving this daemon; turning it on extends the
requirement to `curl`/CLI callers too (`kb-code` already sends the header
on every request).

```toml
[security]
secret_globs = ["*.enc", "deploy/*.yml"]
strict_request_header = true
```

### `[search]`

V71-D1's per-signal ranking flags for the Search-Everywhere box
(`crates/kb-code-server/src/search/Factors`, design D3). One boolean per
FACTOR — kb's own MI-W5.R amendment is the precedent: a single omnibus
"smart ranking" switch is what forced that milestone to split its flag after
a bench measured only one of its two factors.

| key | type | default | meaning |
|---|---|---|---|
| `frecency` | bool | `true` | Files lane: an additive open-history recency boost (3-day half-life, capped) blended into the fuzzy score. Shipped ON since W2.1 — this key only makes it switchable and explainable. |
| `demote_generated` | bool | `false` | Multiply a vendored/generated path's score by 0.5 (`node_modules/`, `vendor/`, `dist/`, `build/`, `target/`, `tmp/`, `*.min.js`, `db/schema.rb`, lockfiles…). A demotion, never an exclusion. **Off by default: plausible but unmeasured** — no bench has scored it, and the MI-W5.R rule is that an unmeasured factor ships off. |
| `lexical_rarity` | bool | `true` | Text lane: order files by the rarity of the identifier atoms they actually matched (dual whole-token + camel/snake sub-token tokenisation, document frequency counted across the result set) instead of alphabetically. On by default because the order it replaces (`ORDER BY path`) is not a relevance signal at all, and because it is derived purely from the query and the hits in hand — nothing learned, nothing personal. |

A factor whose flag is off is **skipped entirely**, not multiplied in as a
neutral `1.0` — so the arithmetic with it off is byte-identical to the
arithmetic before it existed — and it is surfaced on `explain:1`'s
per-hit decomposition **iff its own flag is on**. The hard `exact` →
`prefix` → `fuzzy` tier ladder is deliberately NOT a flag: it is structural,
sits above every factor, and no configuration can let a learned signal
displace an exact name match.

```toml
[search]
frecency = true
demote_generated = false
lexical_rarity = true
```

## daemons.toml (fleet discovery)

`~/.config/kb/daemons.toml` (same config dir as `kb.toml`; honours
`KB_CONFIG_DIR`/`KB_HOME`) lists the daemons the cross-daemon tooling
sweeps — `kb fleet status` fans out identity/stats/open-error probes to
every entry. The SPA keeps its *own* daemon list in
`localStorage["kb:daemons"]` (managed from the Settings page); the two
lists are independent.

```toml
# ~/.config/kb/daemons.toml
[daemon.laptop]
endpoint = "http://127.0.0.1:4000"

[daemon.server]
endpoint = "https://kb.example.com"
```

Each entry is just a name + `endpoint`. If the file is absent, the fleet
verbs default to a single `local` entry at `http://127.0.0.1:4000`. The
bearer token for an auth-on daemon comes from `~/.config/kb/token` (the
same file the rest of the CLI uses).
