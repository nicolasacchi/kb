# Live sessions cockpit — who holds the ball, right now

Design: [`docs/research/kb-live-sessions-cockpit-2026-08.html`](research/kb-live-sessions-cockpit-2026-08.html)
(the "LSC" milestone). Semantics + posture are the LSC amendment in
[`architecture-invariants.md`](architecture-invariants.md) §11; this doc is
the operator-facing how-to: what's wired by default, every env switch that
exists, how to read the output honestly, and how to wire a push
notification without writing daemon code.

This is a **read-only** feature. It never answers a prompt, approves a
permission, or resumes a session for you — it tells you which of your
agent sessions needs you, and hands you the command to act yourself.

## The model, in one paragraph

Every session is scored on two **independent** axes: *who holds the
ball* (`agent` / `human` / `none`) and *how long since it moved*.
Silence never flips the holder axis — a 20-minute build appends nothing
to a transcript, but the agent still owns the turn. Silence only
**escalates** a lane: `working` → `stalled` (45 min silent) →
`presumed_ended` (8h silent, an inference, not a fact); `waiting` →
`cold` (8h silent). An explicit end signal is `finished` — a fact,
never inferred. See the six-state table below.

## Reading the output honestly

The six wire/`--json`/`--state`-filter values are plain enum strings —
`working`, `stalled`, `waiting`, `cold`, `finished`, `presumed_ended`. The
human (non-`--json`) table renders `stalled` and `presumed_ended` with a
trailing `?` (`stalled?`, `presumed_ended?`) as an explicit doubt marker
(`state_prefix` in `sessions_status.rs`) — that `?` is display-only, never
part of the wire value.

| State | Holder | Meaning |
|---|---|---|
| `working` | agent | Recently active; the agent owns the turn. |
| `stalled` (shown `stalled?`) | agent | Silent > 45 min but still agent-held — could be a long tool call, could be dead. Never reclassified as `waiting`: the transcript says the agent has the turn, and silence isn't evidence to the contrary. |
| `waiting` | human | The agent handed control back; you are the bottleneck. |
| `cold` | human | Waiting > 8h — still resumable, sorted below `waiting`, out of the notification path. |
| `finished` | none | An explicit end signal fired (`SessionEnd` or equivalent). A **fact**. |
| `presumed_ended` (shown `presumed_ended?`) | none | Agent-held, silent > 8h, and no end signal ever landed — a dead process that never wrote a close. An **inference**, never rendered as the same claim as `finished`. |

Every row also carries `source` and `confidence` — read these before
trusting a row:

| `source` | `confidence` | What it means |
|---|---|---|
| `hook` | `observed` | A push beat landed for this session. The freshest, most trustworthy signal. |
| `transcript` | `inferred` | `--local` mode: read straight from the harness's own on-disk transcript/event file. No daemon involved. |
| `capture` | `presumed` | Daemon mode's Tier-0 fallback: no beat has ever landed for this session, so the row is aged off the last **landed capture** instead (`kb sessions capture` / the Stop hook). Coarser — only as fresh as the last capture, not truly live. |

**Push wiring is uneven across harnesses on purpose, and it changes which
column above you're reading:**

| Harness | Push (`hook`/`observed`) wiring today | `--local` (pull) |
|---|---|---|
| Claude Code | Full lifecycle: `SessionStart`/`UserPromptSubmit`/`Stop`/`SessionEnd`/`Notification` + a throttled `PostToolUse` heartbeat. | Reads `~/.claude/projects` directly. |
| Codex | `UserPromptSubmit`/`Stop` only — no verified `SessionEnd`/`Notification`-equivalent hook exists yet, so "finished" stays ambiguous for codex (it exits after `exec`, lingers interactively). | Reads `~/.codex/sessions`. |
| Kimi Code | `UserPromptSubmit`/`Stop`/`SessionEnd`. | Reads `~/.kimi-code` (or wherever `KIMI_CODE_HOME` points). |
| opencode | **Not wired until you apply it** — see below; the plugin file lives outside this repo. | Reads `~/.local/share/opencode/opencode.db` (sqlite; opencode has no durable busy/idle state at all, so this lane is `Presumed`-only by construction — "who spoke last", never a real live signal). |
| Grok | **None.** `kb-beat.sh`'s own dispatch calls this out: "best-effort — no live-verified push-hook payload shape exists yet; dormant". | Reads `~/.grok/sessions/<url-encoded-cwd>/<session_id>/events.jsonl` directly — this is the ONLY way to see a live grok session; deliberately does not read `active_sessions.json` (verified empty for headless use) or the grokclaude job tier (proven stale `"running"` status). |

A harness with no push wiring still shows up in the **default,
daemon-backed** `kb sessions status` once it has landed at least one
capture — just at `confidence: presumed`, aged off that capture, never
truly live. `kb sessions status --local` bypasses all of this: it reads
each harness's own files directly, regardless of whether any beat hook
is wired.

## `kb sessions status`

```
kb sessions status                 # daemon-backed (GET /api/sessions/live-status)
kb sessions status --local         # direct-disk, zero daemon, every harness
kb sessions status --json          # machine surface — same shape either way
kb sessions status --state waiting --project kb
kb sessions status --harness codex
kb sessions status --limit 5       # caps EACH lane independently
```

Flags that actually exist (`kb sessions status --help`):

| Flag | Meaning |
|---|---|
| `--local` | Direct-disk snapshot, zero daemon, fans out across every harness (`kb_core::sessions::live_adapters::scan_all`). Omit for the daemon-backed default. |
| `--root DIR` | Overrides **only** the Claude Code transcripts root (`[sessions] live_transcripts_dir` from `kb.toml`, else `~/.claude/projects`). The other four harnesses always use their real default locations — there are no per-harness root flags yet. |
| `--state S,…` | csv over `working,stalled,waiting,cold,finished,presumed_ended`. |
| `--project P` | Restrict to one project (derived `project` field). |
| `--harness H` | One value from `claude,codex,opencode,grok,kimi` (rejected otherwise). |
| `--limit N` | Caps rows **per lane**, so a small limit still shows a balanced snapshot rather than letting a busy IN PROGRESS lane crowd WAITING out. |
| `--json` | Machine surface — the human table and `--json` are built from the exact same filtered/sorted rows. |
| `--no-color` | Plain output. |
| `--daemon URL` | Daemon URL override (daemon-backed mode only). |

There is **no `--watch` flag** — it was deliberately not built. For a
live stream, pipe the event bus instead:

```
kb events --follow --types session.state --json
```

## Wiring the beat hooks

### What's wired by default

Claude Code's own `hooks.json` (`plugins/kb-memory/hooks/hooks.json`) is
already wired for `SessionStart`/`UserPromptSubmit`/`Stop`/`SessionEnd`/
`Notification`, plus `kb-beat-throttle.sh` on `PostToolUse` — the same
plugin install that wires memory recall/capture wires this too, no
extra step. The gate is the same one every capture hook already uses:
if `KB_SESSIONS_DIR` isn't set for a project, every beat script no-ops
silently (`kb` isn't configured here, so there's nothing to push to).

Every beat script is fail-open by construction: no `curl`/`jq`, no
daemon, a 404 route, a malformed payload — every path is a silent
`exit 0`. A beat can never fail a hook chain or slow down typing (the
POST itself is fire-and-forget: a detached background subshell with
`curl --max-time 2 --connect-timeout 1`).

### Env vars (verified against `plugins/kb-memory/hooks/kb-beat.sh` and
`kb-beat-throttle.sh`)

| Var | Default | Effect |
|---|---|---|
| `KB_SESSIONS_DIR` | unset | **Required gate** for every beat (both scripts) — kb not configured for this project ⇒ silent no-op. Same variable the capture hooks already use. |
| `KB_BEAT` | unset | `0`/`false`/`FALSE`/`no`/`NO` kills **every** beat (lifecycle AND heartbeat), both scripts. |
| `KB_BEAT_HEARTBEAT` | unset | `0`/`false`/`FALSE`/`no`/`NO` disables **only** the `PostToolUse` mid-turn heartbeat (`kb-beat-throttle.sh`); lifecycle beats (`start`/`prompt`/`turn_end`/`end`/`blocked`) keep firing. |
| `KB_BEAT_HEARTBEAT_MIN_INTERVAL_SECS` | `180` | Min seconds between heartbeat beats for one session (`kb-beat-throttle.sh`; keyed on a per-session marker file's mtime, since there's no capture artifact to stat here). |
| `KB_DAEMON_URL` | `http://127.0.0.1:4000` | Daemon base URL the beat POSTs to. |
| `KB_BEAT_PATH` | `/api/sessions/beat` | The beat route path. Left overridable so a future repoint doesn't require touching the script. |
| `KB_BEAT_CONTENT` | unset | `0` omits `last_line` (Claude's `Stop`-carried `last_assistant_message`, capped 240 chars) even when available. |
| `KB_BEAT_LEASE_SECS` | `900` | `lease_secs` sent with every beat. |
| `KB_BEAT_DRYRUN` | unset | `1` prints the JSON body to stdout instead of POSTing — no backgrounding, no curl. Used by `tests/test-beat.sh`; also a handy manual sanity check: `echo '{"session_id":"x","cwd":"/tmp"}' | KB_SESSIONS_DIR=/tmp KB_BEAT_DRYRUN=1 plugins/kb-memory/hooks/kb-beat.sh claude start`. |

The beat auth token (when present) is read from
`${XDG_CONFIG_HOME:-$HOME/.config}/kb/token` — the same file `kb token
generate`/`kb token rotate` writes (`kb token path` prints the resolved
path), and the same one every other `kb` CLI daemon call reads.

### Registering the beat for harnesses whose config lives outside this repo

Codex and Kimi Code registration are one-time edits to files outside this
repo (`~/.codex/hooks.json`, `${KIMI_CODE_HOME:-~/.kimi-code}/config.toml`
— Kimi's installer, `install-kimi-hooks.sh`, wires it automatically). The
exact snippets — including the caveat that codex has no verified
`SessionEnd` yet, so `start`/`blocked`/`end` stay unwired for it — live
in [`plugins/kb-memory/hooks/README.md`](../plugins/kb-memory/hooks/README.md#codex-cli-registration)
and its ["Kimi Code registration"](../plugins/kb-memory/hooks/README.md#kimi-code-registration)
section; don't duplicate them here, they'll drift.

opencode is the one that needs **manual** action even though the plugin
file already exists: `~/.config/opencode/plugin/kb-memory.ts` is an
operator-managed file outside this repo, and its `event` handler is
currently filtered to `session.idle` (capture) only. The exact TS diff
to widen it — mapping `session.status{busy|idle}` /
`permission.asked` / `permission.replied` to beat events — is documented
in [`plugins/kb-memory/hooks/README.md` → "opencode registration
(LSC-3 beats)"](../plugins/kb-memory/hooks/README.md#opencode-registration-lsc-3-beats).
Until you apply it, opencode sessions are visible only through
`kb sessions status --local`'s direct sqlite read (`Presumed` confidence,
"who spoke last" only — opencode keeps no durable busy/idle state
anywhere, so this ceiling is structural, not a missing feature).

Grok has no push registration to apply in this phase at all — there is
no hook payload shape for it yet. Grok sessions are visible via
`--local` (which reads `events.jsonl` directly) or, once a capture has
landed, as a Tier-0 `presumed` row in the daemon-backed default.

## Notification, without writing daemon code

kb has one sanctioned outbound path — the `[webhooks]` event→URL bridge
(see [`configuration.md` → `[webhooks]`](configuration.md#webhooks)) —
and one machine-readable event stream (`kb events --follow`). The
`session.state` SSE kind (fires **only on a derived-state change**,
never per beat) is what either path subscribes to. No new outbound code
was written for this: both routes below are configuration, or a small
example script, never a daemon change — kb's own recorded posture is
"no push mechanics in the daemon" (see README → Non-goals), and building
an ntfy client into kb would cross it.

The payload on `session.state` is deliberately small:
`{session_id, harness, state, holder, project, since_unix}` — **no**
`title`/`last_line` (those only ever ride the request/response bodies,
never the broadcast).

This example assumes you already run delivery infrastructure such as ntfy
(`https://ntfy.example.com`, default access `deny-all` — publishing needs a
token or an explicit per-topic grant) fronted by a reverse proxy, possibly
alongside Alertmanager webhooking into its own topic for infra alerts.
Session notifications should use their **own** ntfy topic (e.g.
`kb-sessions`), separate from any topic Alertmanager owns.

### Path A — kb's own `[webhooks]` bridge

```toml
[webhooks]
url = "https://ntfy.example.com/kb-sessions"
types = ["session.state"]
timeout_ms = 3000
# allow_private = true   # only if this URL resolves to an RFC1918/LAN address
#                         # from the daemon's own network namespace — public
#                         # unicast (the normal case for a Traefik-fronted
#                         # public domain) needs nothing extra.
```

Fields verified against `WebhooksSection`
(`crates/kb-core/src/config.rs`): `url` (required, `http`/`https` only),
`types` (exact-match allowlist on the event envelope's `type` — there is
no payload-level filter, so this forwards **every** `session.state`
transition matching the type, not just `waiting`), `timeout_ms`
(default 5000), `allow_private` (default `false`; loopback + public
unicast always pass, link-local/metadata/multicast are refused even with
it on — see `crates/kb-core/src/webhook_url.rs`).

**Two real limits to know before you rely on this path:**

1. **`[webhooks]` has no header/auth field** (`url`/`types`/`timeout_ms`/
   `allow_private` is the whole struct) — it can only reach an endpoint
   that accepts an unauthenticated POST. Against a `deny-all` ntfy
   instance, that means granting anonymous **write-only** access
   to the one dedicated topic (never loosening the server default),
   mirroring the pattern ntfy itself documents for UnifiedPush:
   ```
   docker exec <ntfy-container> ntfy access '*' kb-sessions write-only
   ```
   This does not affect any other topic (`alerts` stays deny-all); a
   read stays refused (`write-only`, not `rw`).
2. **The notification body is the raw event envelope**, not a rendered
   message. kb POSTs `{v, id, type, ts, payload}` as
   `Content-Type: application/json` to the exact configured URL — since
   that URL is topic-suffixed (not ntfy's root), ntfy treats the whole
   request body as literal message text (ntfy's "publish as JSON"
   shortcut only triggers on a POST to the **root** URL with a `topic`
   key in the body, which this isn't). The phone buzzes with the raw
   JSON blob as the message and the topic's short URL as the title
   (ntfy's own default when no `Title`/`X-Title` header is sent — kb's
   bridge sends none). Functional, honest, and zero daemon code — but
   not pretty, and not filtered to "only tell me when I'm the
   bottleneck". For that, use Path B.

Per [`configuration.md`](configuration.md#webhooks)'s existing caveat:
the bridge is **fire-and-forget and eventually-consistent** — a slow or
unreachable receiver makes the subscriber lag and drop events, never
back-pressures the daemon. Never build a synchronous decision on it.

### Path B — the shell one-liner (filtered, readable, your own auth)

```bash
kb events --follow --types session.state --json \
  | jq --unbuffered -r '
      select(.payload.state == "waiting") |
      "\(.payload.project // "?") · \(.payload.harness) session \(.payload.session_id[0:8]) is waiting on you"
    ' \
  | while IFS= read -r line; do
      curl -fsS \
        -H "Authorization: Bearer $NTFY_TOKEN" \
        -H "Title: kb session waiting" \
        -H "Priority: default" \
        -d "$line" \
        "https://ntfy.example.com/kb-sessions"
    done
```

`kb events --help`-verified flags used: `--follow` (required — the only
mode today), `--types` (comma-separated globs, server-side filtered),
`--json` (NDJSON envelopes `{id, type, ts, v, payload}` instead of the
human one-line form). `$NTFY_TOKEN` is a per-user ntfy access token
(`docker exec <ntfy-container> ntfy token add <user>`, per your ntfy
deployment's own token-rotation instructions) — never hardcode it in the script; export
it in the shell/systemd-unit environment that runs this loop. Because
this path shapes the message itself (via `jq`) and authenticates with a
real token, it doesn't need ntfy's deny-all loosened at all, and it can
filter to exactly the transitions worth a buzz (`waiting`, or add
`or .payload.state == "presumed_ended"` for "an agent session silently
died").

This inherits `kb events --follow`'s own reconnect discipline
(`Last-Event-ID` resume + exponential backoff) — it survives a daemon
restart without missing a beat past the one in flight, unlike Path A's
best-effort broadcast.

### Which path to use

Path A is the zero-script option, sanctioned entirely by configuration —
reach for it first if "every state transition buzzes as raw JSON" is
acceptable (e.g. as a coarse liveness feed, or while testing that the
wiring works at all). Path B is a few extra lines but gives you a
filtered, human-readable message over your own authenticated channel —
reach for it for the actual "ping me only when I'm the bottleneck" use
case the design set out to satisfy.

## See also

- [`architecture-invariants.md` §11](architecture-invariants.md) — the
  full LSC amendment: the two-axis model, the abandon horizon, the
  in-memory registry + Tier-0 degraded view, and the posture rationale
  for why `/beat`/`/live-status` are `auth_bearer` while `/presence`/
  `/{id}/live` stay loopback-only.
- [`http-api.md`](http-api.md) — `POST /api/sessions/beat`,
  `GET /api/sessions/live-status`.
- [`docs/research/kb-live-sessions-cockpit-2026-08.html`](research/kb-live-sessions-cockpit-2026-08.html) —
  the design round: harness-by-harness evidence, the wire contract, and
  the refusals (no control surface, no persisted state, no per-user
  scoping).
- [`configuration.md` → `[webhooks]`](configuration.md#webhooks) — the
  full field reference and SSRF policy behind Path A.
- [`plugins/kb-memory/hooks/README.md`](../plugins/kb-memory/hooks/README.md) —
  every hook's full behavior, including `kb-beat.sh`/`kb-beat-throttle.sh`'s
  own table rows and the codex/Kimi/opencode registration snippets.
