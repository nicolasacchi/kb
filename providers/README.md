# providers/ — reference lip/1 providers (Track L3)

`crates/kb-lip` is a generic LSP→HTTP adapter: it spawns ONE language server
as a stdio child process and speaks a small closed HTTP surface (`lip/1`) in
front of it. This directory holds REFERENCE configs + docs for wiring up a
real language server — not code, not CI-covered. Design of record:
`docs/research/design-lip.md` (search the corpus) + the addendum's §D
(`/lip/diagnostics`).

## Files

- **`ruby-lsp.toml`** — RECOMMENDED default for Ruby/Rails. Wraps
  [ruby-lsp](https://github.com/Shopify/ruby-lsp) (Shopify-maintained,
  Prism-based). This is the config the live smoke below was run against.
- **`solargraph.toml`** — alternative Ruby provider
  ([solargraph](https://github.com/castwide/solargraph)). Reach for it when
  ruby-lsp's composed-bundle bootstrap (see below) isn't practical for a
  given repo. NOT part of the smoke evidence below — validate it against
  your own target repo before relying on it.
- **`Dockerfile.ruby`** — reference multi-stage image (rust builder + a slim
  ruby runtime with ruby-lsp installed) proving the "can also live in a
  different docker image" deployment shape from the design doc. CI never
  builds this file — it's a reference, not a shipped artifact.
- **`rust-analyzer.toml`** — the only Rust provider config kb-lip ships
  (there is no ruby-lsp/solargraph-style choice for Rust). Wraps
  [rust-analyzer](https://rust-analyzer.github.io/), bundled with `rustup`.
  Port 4845.
- **`typescript-language-server.toml`** — the only TypeScript/JavaScript
  provider config kb-lip ships. Wraps
  [typescript-language-server](https://github.com/typescript-language-server/typescript-language-server).
  Port 4847.
- **`pyright.toml`** — the only Python provider config kb-lip ships. Wraps
  [pyright](https://github.com/microsoft/pyright) (an npm package, not a
  PyPI one — see its own file's prerequisites). Port 4849.
- **`gopls.toml`** — the only Go provider config kb-lip ships. Wraps
  [gopls](https://pkg.go.dev/golang.org/x/tools/gopls), the Go team's own
  language server. Port 4851.
- **`Dockerfile.rust`**, **`Dockerfile.typescript`**, **`Dockerfile.python`**,
  **`Dockerfile.go`** — one reference multi-stage image per language above,
  same shape and same "CI never builds this" posture as `Dockerfile.ruby`.

None of the four new `.toml` files above were part of the PRR-L3/L4/L5
live-smoke evidence below (ruby/Rails only) — each has its own "Live smoke
evidence" subsection further down. The S2-D smoke run (2026-08-31) has since
filled all four in: typescript, python, go AND rust (a same-day re-run once
the box's build lock cleared) each passed all six lip/1 endpoints live — see
each subsection for the honest detail, including rust's first-pass lock
blockage and go's decisive UTF-16 probe.

- **`ruby-lsp-target.toml`** — V71-E1: `ruby-lsp.toml` bound to the
  v7.1 target repo (port 4842, real `workspace_root`), so it can run beside
  the generic config. See "V71-E1 — the v7.1 target repo" below for why
  Ruby needs lsp-live for anything CROSS-FILE even after D4's STRICT rule
  landed. Config + docs only — no live smoke was run for it.

The six generic `.toml` files ship a `workspace_root` PLACEHOLDER
(`/CHANGE/ME/to/the/repo/root`) — edit it to your real repo's absolute path
before running kb-lip. Everything else is a sensible default; see each
file's own comments for what to tune and why.

## Design-stance supersede: code actions (S2-C, 2026-08-28)

design-lip.md's original closed-surface refusal — **"no code actions, no
rename, no formatting: kb-code is a reader"** — is PARTIALLY REVERSED,
operator-ratified 2026-08-28 (design-s2.md §S2-C). kb-lip now speaks a
SIXTH endpoint, `POST /lip/code-actions` (`crates/kb-lip/src/http.rs::
guarded_code_actions`), sitting behind the SAME `codeActionProvider`
capability gate as every other verb. The rest of the original refusal
STANDS unchanged: still no rename, still no formatting, and kb-lip still
NEVER calls `workspace/executeCommand` — an action whose edit can't be
materialized as plain `TextEdit`s (a bare `Command`, an unresolvable
`CodeAction`, or one naming a `Create`/`Rename`/`DeleteFile` resource
operation) is dropped and counted (`dropped_command_only`/
`dropped_unsupported` on the response), never executed. kb-lip does not
apply anything itself either way — `POST /lip/code-actions` only
TRANSLATES the server's answer into `TextEdit`s; turning one into an
actual working-tree change is kb-code-server's job (its own
bearer-create/loopback-apply suggestion split), entirely outside kb-lip.

## How to run a provider

1. Install the language server (see the chosen `.toml`'s own comments for
   exact install steps — they differ per provider).
2. Edit `workspace_root` in the `.toml` to your target repo's absolute path.
3. Run kb-lip:

   ```bash
   kb-lip --config providers/ruby-lsp.toml
   ```

   It binds loopback-only HTTP (`127.0.0.1:<port>`, `4841` by default for
   ruby-lsp.toml) and spawns the configured language server as a child
   process. `GET /lip/identity` reports whether the child is up:

   ```bash
   curl -s http://127.0.0.1:4841/lip/identity | jq
   ```

4. Wire it into kb-code-server — see "Enable ruby lsp-live on the local
   daemon" below.

kb-lip restarts the child on crash with capped exponential backoff
(`[restart_backoff]` in the config) and reports `healthy: false` on
`/lip/identity` while down — it never blocks the HTTP surface waiting for a
respawn.

## The blob guard

Every `POST /lip/*` request carries `{path, blob_sha, line, col}` — `path`
is workspace-relative, `blob_sha` is the caller's own git-blob-sha1 of the
file content it's asking about (`git hash-object <file>`, or the equivalent
kb-code already tracks in its live mirror). kb-lip hashes the file on disk
**before and after** talking to the LSP; if either hash doesn't match the
caller's `blob_sha`, it answers `{"refused": "blob_mismatch", "have":
"<actual>", "want": "<yours>"}` — HTTP 200, never a 5xx, and never a
best-effort answer against possibly-stale bytes.

This is the mechanism that lets kb-code label a lip/1 answer `trust:
"exact"` without violating its own "a wrong exact is a release blocker"
rule: an exact answer is only ever minted when the guard PASSED, i.e. the
LSP was queried against the exact bytes the caller already has open. A
`blob_mismatch` refusal means "the file changed under us mid-query, ask
again" — not "the language server is wrong."

The other two closed refusal reasons: `server_down` (the LSP child isn't
up, or the request to it failed) and `unsupported` (bad path, or the
configured server doesn't support that LSP method). All three are `refused`
fields in an otherwise-200 response — feature-detect the `capabilities`
array on `/lip/identity` if you need to know ahead of time whether a given
verb is worth trying.

## What lsp-live shows in the UI/CLI

Once wired into kb-code-server (below), a lip/1-backed answer is a NEW top
tier in the resolve/hover/usages ladder — `precision: "lsp-live"`, `trust:
"exact"` — consulted FIRST for any `(repo, lang)` pair with a configured
provider; on refusal/timeout/absence the existing ladder (SCIP → other
tiers) runs unchanged underneath it, so a provider that's down just quietly
falls back rather than breaking anything.

- **CLI** (`kb-code resolve`/`usages`): each candidate line is
  `<class> <precision> <path:line> <kind> <container> — <signature>` — a
  live answer prints `exact lsp-live app/models/...:42 ...`.
- **CLI** (`kb-code hover`): `hover · path:line:col [<trust> <precision>]`
  — `hover · app/models/order.rb:42:8 [exact lsp-live]`.
- **Repo status** (`GET /api/repos`): each repo carries an `intel` block —
  `{provider, langs, alive, server_version}` — showing which lip/1 provider
  (if any) is wired to that repo and whether kb-code-server's last handshake
  with it succeeded. `intel` is single-valued (the FIRST configured provider
  naming that repo, config order); a mixed-language repo (Rust + TypeScript,
  say) can have more than one `[[intel.providers]]` entry naming it, so
  S2-D adds a sibling `intel_providers` array — every matching provider, in
  config order, same per-entry shape — for callers that want the full set
  rather than just the first.

lip answers are computed fresh per request and never persisted — no new
table, no cache. The deterministic content-addressed index (SCIP etc.)
stays the reproducible tier; lsp-live is a live overlay that vanishes the
instant the provider does.

## Enable ruby lsp-live on the local daemon — operator steps

1. Get ruby-lsp running against your target repo (see "How to run a
   provider" above) and confirm it's healthy:

   ```bash
   curl -s http://127.0.0.1:4841/lip/identity | jq '.healthy, .capabilities'
   ```

2. In your `kb-code.toml` (`<KbPaths::new("kb-code").config>/kb-code.toml`
   by default, or wherever `kb-code-server --config` points), add:

   ```toml
   [[repos]]
   name = "my-rails-app"
   path = "/absolute/path/to/my-rails-app"

   [[intel.providers]]
   name  = "ruby"
   url   = "http://127.0.0.1:4841"
   langs = ["ruby"]
   repos = ["my-rails-app"]      # must match the [[repos]] `name` EXACTLY —
                                  # this is an allowlist, mirroring
                                  # [semantic]'s own polarity: an empty
                                  # `repos` opts in ZERO repos, never "every
                                  # repo".
   ```

3. Restart kb-code-server. The handshake against `/lip/identity` happens
   lazily on first real use (never at boot — a provider that's down at
   daemon start must not delay or fail kb-code's own boot), and is cached
   for the process's lifetime once it succeeds. If it fails (wrong
   `lip_major`, connection refused, malformed response), kb-code-server
   fails CLOSED for that provider and falls back to the existing ladder —
   it retries the probe on the next request rather than caching a failure.
4. Confirm: `GET /api/repos` should show `intel.alive: true` for the repo,
   and a resolve/hover/usages call against a ruby file in that repo should
   surface a `lsp-live`/`exact` candidate as the top hit.

## V71-E1 — binding a provider to a target repo (`acme-shop`)

`ruby-lsp-target.toml` is `ruby-lsp.toml` with the placeholder
resolved to the operator's own checkout and the port moved to **4842**, so
it runs beside the generic reference config. Everything else — the
prerequisites, the composed-bundle warning, the tuning notes — is in
`ruby-lsp.toml` and is deliberately not duplicated.

**Why Ruby specifically wants this after v7.1.** `usages/2` (D4, V71-E1)
gives Ruby its first `exact` tier: `intel::ruby_strict`'s STRICT rule mints
`exact` for a same-file local when the binding is unambiguous in its own
lexical scope, the name is not a method on the enclosing hierarchy
(`include`/`prepend`/`extend` walked), and the enclosing method contains
none of `eval`/`instance_eval`/`binding`/`send`/`define_method`/
`method_missing`. That rule stops at the file boundary by construction —
it is a *lexical* proof. **Every cross-file Ruby `exact` still comes from
exactly one place: a blob-verified `lsp-live` answer through this
provider.** Without it, a usages query on this repo tops out at `likely`
the moment it leaves the open file: honest, and much less useful.

Operator steps are the ones in "Enable ruby lsp-live on the local daemon"
above, with `url = "http://127.0.0.1:4842"`, `repos = ["acme-shop"]`
and:

```bash
kb-lip --config providers/ruby-lsp-target.toml
curl -s http://127.0.0.1:4842/lip/identity | jq '.healthy, .capabilities'
kb-code usages app/services/apply_coupon_service.rb:47:4 \
  --repo acme-shop --v2
```

The last line is the check that matters: rows whose `precision` is
`ruby-locals-strict` came from the static STRICT rule (same file); rows
whose `precision` is `lsp-live` came from this provider. If you see no
`lsp-live` rows at all, the provider is not reaching the daemon — the
`[[intel.providers]] repos` allowlist is the usual cause (it opts in ZERO
repos when empty, never "every repo").

**Config and docs only.** This unit ships no running server: nothing in
kb-code-server spawns `kb-lip` (the daemon spawns nothing but git), and the
smoke evidence below is unchanged — a live smoke against this repo has NOT
been run for V71-E1.

## Live smoke evidence

### PRR-L3 (2026-08-28, first pass) — two bugs found, one discrepancy left open

kb-lip (`cargo build --profile fast -p kb-lip`) was run against the real
acme-shop Rails checkout on this box (430-gem Gemfile.lock, ruby-lsp
0.26.11 + the ruby-lsp-rails addon). Summary, honest positives and
negatives:

**What worked:** both `.toml` providers parse correctly; `GET
/lip/identity` reports `healthy: true` + the real server name/version once
the handshake completes; the blob guard matched `verified_blob_sha` on
every call; a raw hand-rolled JSON-RPC client (bypassing kb-lip) proved
ruby-lsp itself resolves real answers against this app (`Widget` in
`app/models/policy.rb:29` → 10 real reopened-class definition locations,
plus the ruby-lsp-rails addon's full ActiveRecord schema hover). First-run
cost is dominated by ruby-lsp's own "composed bundle" bootstrap (10+
minutes on a cold repo whose Gemfile doesn't list `ruby-lsp`/`debug`
directly) — not a kb-lip bug; see prerequisite #2 above.

**What did NOT work, and two confirmed kb-lip bugs found by this smoke —
both FIXED in PRR-L4 below:**

1. **LocationLink parsing** — `/lip/definition` and `/lip/references` came
   back `{"results": [], ...}` for the SAME `Widget` query the raw probe
   resolved. Root cause: ruby-lsp replies to `textDocument/definition`
   with `LocationLink[]` (`targetUri`/`targetRange`/`targetSelectionRange`)
   regardless of client `linkSupport`, and `translate_locations` only read
   the `Location[]` shape (`uri`/`range`), silently dropping every item.
2. **Missing `.current_dir()`** — `LspClient::start` spawned the child with
   no `.current_dir()` call, so it inherited kb-lip's own process CWD
   rather than `workspace_root`, breaking ruby-lsp's Bundler/Gemfile
   detection whenever kb-lip is started from anywhere but the target repo.
3. An UNRESOLVED discrepancy: after manually fixing the CWD and
   re-testing, `/lip/hover` and `/lip/diagnostics` still returned empty for
   positions raw LSP resolved seconds earlier, including a plain intra-file
   Prism-only hover — reported honestly rather than hidden, with a
   candidate-next-step list (readiness/indexing, `didOpen` languageId,
   position-encoding negotiation, `didOpen` text drift, stdio reader
   desync) but no root cause confirmed at the time.

### PRR-L4 (2026-08-28, follow-up) — both bugs fixed + the discrepancy resolved

Both PRR-L3 bugs are now fixed in `crates/kb-lip` (`http.rs::
translate_locations`/`extract_uri_and_range`, `lsp.rs::LspClient::start`),
each with a fake-LSP-fixture regression test (`FAKE_LSP_DEFINITION_
LOCATION_LINK=1`, `FAKE_LSP_CWD_FILE`) plus unit tests for the pure
translation logic. The PRR-L3 discrepancy was investigated end-to-end
against the same real acme-shop checkout, working through the
candidate list in order:

**(a) Readiness/indexing — CONFIRMED as the dominant root cause, and
fixed (with one documented residual gap).** A raw JSON-RPC probe that
polled `textDocument/hover` on a trivial, Prism-only, intra-file position
(the `class Ability` declaration line itself — needs nothing beyond a
syntax parse) every ~20s while logging `$/progress` directly reproduced
the failure: hover returned `null` immediately after `didOpen` (before
ruby-lsp's indexing pass had even started signaling), then became
consistently non-null once observed mid-index (~20s into a run whose
`$/progress` cycle ran `begin`→`report×100`→`end` from t≈16s to t≈63s on
this repo) and stayed populated for the rest of a 255s observation window.
A second probe confirmed the PRECONDITION: ruby-lsp sends **zero**
`$/progress`/`window/workDoneProgress/create` traffic at all when the
client doesn't declare `window.workDoneProgress: true` in its `initialize`
capabilities — which is exactly what kb-lip sent pre-fix, meaning it was
flying completely blind to the indexing state it was racing against.

The fix (`crates/kb-lip/src/lsp.rs`): kb-lip now declares
`window.workDoneProgress: true`, tracks the generic (server-agnostic,
spec-defined — never a title/string match) `$/progress` begin/end
lifecycle per token in a new `ProgressTracker`, and surfaces it as
`LspClient::is_indexing()`. `GET /lip/identity` now reports an `indexing:
bool` field, and all five `/lip/*` endpoints (`http.rs::guarded_request`/
`guarded_diagnostics`, checked right after acquiring a live client) refuse
with `{"refused": "indexing"}` — HTTP 200, honest, never a guessed empty
answer — while any progress token is open.

Re-run against the real app with the fix (`providers/ruby-lsp.toml`-shaped
config, port 4851): `GET /lip/identity` read `indexing: true` at 11s
uptime; `/lip/hover`, `/lip/definition`, and `/lip/diagnostics` all
answered `{"refused": "indexing"}` during that window instead of a silent
empty result; polling `indexing` flipped to `false` at ~44s; **every
endpoint then returned real, populated data** (see below) — the
readiness gate closes the exact gap PRR-L3 found.

KNOWN RESIDUAL GAP (documented, not closed): the window between
`initialized` and the server's FIRST `$/progress` `begin` (~16s on this
repo's cold boot) is indistinguishable from "this server will never send
progress at all" — `is_indexing()` reads `false` in both cases, since
there's no active token either way. A query landing in that narrow
pre-begin window can still see a legitimate-looking-but-premature empty
result. Closing it fully needs either an operator-configurable settle
delay or a different heuristic; deferred to a follow-up unit rather than
guessing a magic timeout from one repo's timing.

**(b) `didOpen` languageId — RULED OUT.** `http.rs::guarded_request`/
`guarded_diagnostics` set `language_id` from `sup.config().lang_ids.first()`,
which for `ruby-lsp.toml` is `"ruby"` — the correct LSP languageId for
`.rb` files. Confirmed correct by reading the config + code; no live
reproduction needed.

**(c) Position-encoding negotiation — RULED OUT, confirmed live.** A raw
`initialize` probe using kb-lip's EXACT capabilities (no
`general.positionEncodings` declared) got back
`capabilities.positionEncoding: "utf-16"` from ruby-lsp 0.26.11 — spec-
compliant behavior (a client that doesn't declare `general.positionEncodings`
gets the mandatory UTF-16 default), matching kb-lip's own hardcoded UTF-16
assumption throughout `position.rs`. Not the cause.

**(d) `didOpen` text drift — RULED OUT by construction.** `guarded_request`/
`guarded_diagnostics` read the file ONCE (`blob::read_and_hash`) into
`pre: Blob`, then pass that SAME `Blob` (not a fresh re-read) into
`ensure_doc_open`, which sends `pre.bytes` verbatim as the `didOpen`/
`didChange` text — there is no code path where the bytes sent to the LSP
could differ from the bytes hashed. Confirmed by source reading alone.

**Stdio reader desync (the PRR-L3 write-up's own candidate next step) —
RULED OUT by code inspection.** `rpc::framed::read_message` reads one
`Content-Length`-framed message at a time with exact byte reads
(`read_exact`), and `reader_loop` processes messages strictly sequentially
(no concurrent/interleaved reads); nothing in that path is volume-
sensitive. No evidence of desync, and the readiness-gap reproduction above
fully accounts for the observed symptom without needing this hypothesis.

**NEW finding from this investigation — FIXED in PRR-L5 below:**
`/lip/diagnostics` structurally could not return a ruby-lsp diagnostic at
all, independent of the readiness fix. ruby-lsp 0.26.11 declares
`diagnosticProvider` in its `initialize` capabilities (LSP 3.17 PULL
diagnostics, `textDocument/diagnostic`) and — confirmed across two
independent observation windows (255s and 34s, both spanning well past
full indexing completion) — sends **zero** `textDocument/publishDiagnostics`
PUSH notifications, ever. A raw pull probe confirms `textDocument/
diagnostic` itself works and returns a well-formed `{"kind": "full",
"items": [...]}` response. But kb-lip's `/lip/diagnostics` pipeline
(`lsp.rs::DiagnosticsCache`, design-addendum-2.md §D) was PUSH-only at the
time — it never sent a pull request and only ever populated its cache from
`publishDiagnostics` notifications that this provider never emits. Against
ruby-lsp, `/lip/diagnostics` therefore always answered `results: []`,
regardless of whether the file actually had diagnostics — not merely
"empty because clean."

### PRR-L5 (2026-08-29) — pull-mode diagnostics support

Fixed by detecting the server's advertised diagnostics mode at
`initialize` time and branching `/lip/diagnostics`'s pipeline on it
(`crates/kb-lip/src/lsp.rs::ServerInfo::diagnostics_mode`,
`http.rs::guarded_diagnostics`). `GET /lip/identity` now additionally
reports `diagnostics_mode: "pull"|"push"|"none"` (`"none"` while
`healthy: false` — no client to ask, mirroring `indexing`'s own
convention). A server that advertises `capabilities.diagnosticProvider`
(anything other than absent/null/`false`) is queried via the LSP 3.17 pull
method, `textDocument/diagnostic` (`LspClient::pull_diagnostics`), on
every call — a `"full"` `DocumentDiagnosticReport` replaces kb-lip's own
per-uri cache (items + `resultId`); the NEXT pull sends that cached
`resultId` back as `previousResultId`, so an unchanged server can answer
cheaply with `{"kind": "unchanged"}` instead of resending the same
diagnostics, and kb-lip serves the previously-cached full result in that
case. A server that never advertises `diagnosticProvider` is served from
the original push cache, byte-for-byte unchanged. The blob guard (pre/post
hash) and the indexing gate run identically in both modes — only the
diagnostics-fetch step itself branches. Regression-tested end-to-end
against a fake-LSP fixture toggled into pull mode
(`FAKE_LSP_DIAG_PULL=1`, `crates/kb-lip/src/bin/fake_lsp.rs`): pull happy
path, unchanged-report-serves-cache, push fallback when pull isn't
advertised, blob guard, and the indexing gate — all pass
(`crates/kb-lip/tests/lip_adapter.rs`).

**Live re-smoke against the real acme-shop checkout: SKIPPED, not a
kb-lip issue.** Attempted against the same repo PRR-L3/L4 used
(`ruby-lsp` 0.26.11 at `~/.local/bin/ruby-lsp`), but the box's system Ruby
has since drifted to 3.4.10 while this repo's Gemfile still pins
`ruby "3.4.8"` — ruby-lsp's composed-bundle `bundle install` fails outright
with `Bundler::RubyVersionMismatch` before it ever reaches the LSP
handshake (confirmed with a direct hand-rolled `initialize` probe,
bypassing kb-lip entirely, to rule out an adapter-side cause). This is
local Ruby-version environment drift on this box since PRR-L4 — unrelated
to the pull-mode change above — not a regression introduced by this unit;
fixing it needs a version-pinned Ruby (mise/rbenv) or a Gemfile bump,
neither of which is this unit's scope. The fake-LSP-fixture coverage above
is therefore the only verification for this unit's diagnostics-mode logic.

### Re-verified endpoint results (PRR-L4, post-fix, post-indexing)

Against `app/models/policy.rb:29:17` (`Widget`), `blob_sha` via `git
hash-object`, port 4851:

- `GET /lip/identity` → `healthy: true`, `indexing: false`,
  `server_name: "Ruby LSP"`, `server_version: "0.26.11"`.
- `POST /lip/definition` → 10 populated results (reopened-class
  definitions across `app/models/widget.rb` + 9 `fulfillment/*`
  files), each with a real `path`/`line`/`col` — the LocationLink fix
  confirmed live, matching the raw probe's PRR-L3 finding exactly.
- `POST /lip/hover` → the full ruby-lsp-rails ActiveRecord schema
  annotation for table `widgets` (every column/index/FK) — populated,
  matching the raw probe's PRR-L3 finding exactly. This was the empty
  PRR-L3 discrepancy; now resolved by the readiness fix.
- `POST /lip/references` → 200 populated results (capped at
  `REFERENCES_CAP`; `Widget` is referenced far more than 200 times across
  the app), each with a real `path`/`line`/`col`.
- `POST /lip/diagnostics` → `results: []`, `refused` absent — a VALID
  `{diagnostics: []}` per design-addendum-2.md §D on its face, but per the
  NEW finding above (at the time of THIS PRR-L4 pass), this provider could
  not return anything else through kb-lip's then push-only pipeline
  regardless of the file's actual lint state — not strong evidence of file
  cleanliness the way it would be against a push-based server. **Fixed in
  PRR-L5** (see above) — this provider is now queried via LSP 3.17 pull
  instead of waiting on a push that never arrives.

**Bottom line**: both PRR-L3 bugs are fixed and regression-tested; the
readiness/indexing root cause behind the PRR-L3 hover/diagnostics
discrepancy is confirmed, fixed, and re-verified live (hover, definition,
and references all now return real, populated data against this large
Rails app); the push-vs-pull diagnostics protocol mismatch is fixed in
PRR-L5 (fake-LSP-fixture-verified; live re-smoke blocked by unrelated
Ruby-version drift on this box, see PRR-L5 above); one narrow residual
timing gap (the pre-first-`$/progress` window, PRR-L4 §a) remains open.

---

## S2-D: rust · typescript · python · go

The rest of this file adds one "Enable X lsp-live on the local daemon"
walkthrough plus a "Live smoke evidence" subsection per new language,
mirroring the ruby sections above. The generic material earlier in this
file (Files, "How to run a provider," "The blob guard," "What lsp-live
shows in the UI/CLI") applies unchanged to all four — only the
language-specific install step, config file, and port differ.

## Enable rust lsp-live on the local daemon — operator steps

1. Get rust-analyzer running against your target repo (`rustup component add
   rust-analyzer`, see `providers/rust-analyzer.toml`'s own prerequisites)
   and confirm it's healthy:

   ```bash
   kb-lip --config providers/rust-analyzer.toml &
   curl -s http://127.0.0.1:4845/lip/identity | jq '.healthy, .capabilities'
   ```

2. In your `kb-code.toml`, add:

   ```toml
   [[repos]]
   name = "my-rust-app"
   path = "/absolute/path/to/my-rust-app"

   [[intel.providers]]
   name  = "rust"
   url   = "http://127.0.0.1:4845"
   langs = ["rust"]
   repos = ["my-rust-app"]
   ```

3. Restart kb-code-server. Same lazy-handshake, cache-once-healthy,
   retry-on-failure posture as ruby's own steps above — see step 3 there
   for the full mechanics (identical across every provider; kb-code-server
   doesn't special-case by language).
4. Confirm: `GET /api/repos` should show `intel.alive: true` (or, for a
   repo with more than one configured provider, look in `intel_providers`
   — see "What lsp-live shows in the UI/CLI" above) for the repo, and a
   resolve/hover/usages call against a `.rs` file in that repo should
   surface a `lsp-live`/`exact` candidate as the top hit.

### Live smoke evidence (rust) — 2026-08-31 (S2 smoke, C1) — identity + readiness gate verified live; full probe suite BLOCKED by an external lock, not a kb-lip bug

`kb-lip --config providers/rust-analyzer.toml` (prebuilt `target/fast/kb-lip`,
port 4845) was run against `workspace_root = ~/project/kb` — the kb
repo itself, per this smoke's own scope — with rust-analyzer 1.96.0
(`ac68faa`, 2026-05-25) on `~/.cargo/bin` via `rustup component add
rust-analyzer`.

**What worked:** `GET /lip/identity` reported `healthy: true` with all six
capabilities, `diagnostics_mode: "pull"`, `server_name: "rust-analyzer"`,
`server_version: "1.96.0 (ac68faa 2026-05-25)"` — the handshake itself is
confirmed live. The readiness gate (PRR-L4's `$/progress` mechanism) is also
confirmed working end-to-end: every probe issued while `indexing: true`
(a `POST /lip/hover` against `crates/kb-lip/src/http.rs:277:20`,
`read_and_hash`) answered `{"refused": "indexing"}` in well under a
millisecond, honestly, for the entire observation window — never a guessed
or premature answer.

**What did NOT get verified, and why — an environment finding, not a
regression:** `indexing` never flipped to `false` across a 2588s (~43-minute)
observation window, so the hover/definition/references/symbols/diagnostics/
code-actions probe suite could not be run against real (non-refused) answers
in this pass. Process-tree inspection found the root cause: rust-analyzer's
own child, `cargo check --quiet --workspace --message-format=json
--manifest-path ~/project/kb/Cargo.toml --all-targets` (its
build-script/metadata pass, prerequisite #3 in `rust-analyzer.toml`), was
kernel-blocked the entire time — confirmed via `/proc/<pid>/wchan` reading
`locks_lock_inode_wait`, with `/proc/<pid>/io`'s `read_bytes` counter and
`/proc/<pid>/stat`'s `utime`/`stime` both flat across repeated 20–30s sampling
windows (ruling out "just slow disk I/O," which this box is otherwise prone
to — it's genuinely parked on a lock, not working). `ps aux` identified the
lock holder: a `cargo test -j…` process (started independently, well before
this smoke run) running against the SAME `target/` directory — i.e. exactly
the "test gate holds the build lock" this smoke unit's own hard rules warned
against invoking directly. rust-analyzer's own internal `cargo check` hit
that same lock from underneath, which this unit had no way to anticipate or
avoid short of not running rust-analyzer against this workspace at all.

**Bottom line (first pass):** identity/capabilities and the indexing-refusal
safety property were LIVE-VERIFIED; the probe suite was blocked by the lock.

**Re-run, same day (2026-08-31, lock released):** with the concurrent
`cargo test` finished, the SAME config went healthy with `indexing: false`
in ~109s (one later re-index cycle honestly re-refused mid-probe and
cleared in a couple of minutes — the readiness gate working as designed,
both directions). Full probe suite against `crates/kb-lip/src/blob.rs`
(`git_blob_sha1` call site at 56:15, blob-sha-pinned): **all six PASS** —
hover 0.67s (real signature `pub fn git_blob_sha1(bytes: &[u8]) -> String`),
definition 0.97ms (correct: `blob.rs:21:7`), references 0.49s (real
cross-file hits incl. `tests/lip_adapter.rs`), symbols 2.9ms, diagnostics
1.2ms (`results: []` with the matching `verified_blob_sha` — a clean file,
verified, not a guess), code-actions 0.19s returning a REAL rust-analyzer
inline-refactor `TextEdit` with `dropped_command_only: 0` /
`dropped_unsupported: 0`. rust on the kb repo itself is fully
LIVE-VERIFIED.

## Enable typescript lsp-live on the local daemon — operator steps

1. Get typescript-language-server running against your target repo (`npm
   install -g typescript-language-server typescript`, see
   `providers/typescript-language-server.toml`'s own prerequisites) and
   confirm it's healthy:

   ```bash
   kb-lip --config providers/typescript-language-server.toml &
   curl -s http://127.0.0.1:4847/lip/identity | jq '.healthy, .capabilities'
   ```

2. In your `kb-code.toml`, add:

   ```toml
   [[repos]]
   name = "my-app-web"
   path = "/absolute/path/to/my-app/web"

   [[intel.providers]]
   name  = "typescript"
   url   = "http://127.0.0.1:4847"
   langs = ["typescript", "typescriptreact", "javascript", "javascriptreact"]
   repos = ["my-app-web"]
   ```

3. Restart kb-code-server. Same lazy-handshake, cache-once-healthy,
   retry-on-failure posture as ruby's own steps above.
4. Confirm: `GET /api/repos` should show `intel.alive: true` (or
   `intel_providers`, for a repo with more than one configured provider —
   see "What lsp-live shows in the UI/CLI" above) for the repo, and a
   resolve/hover/usages call against a `.ts`/`.tsx`/`.js`/`.jsx` file in
   that repo should surface a `lsp-live`/`exact` candidate as the top hit.

### Live smoke evidence (typescript) — 2026-08-31 (S2 smoke, C1) — all six endpoints PASS

`kb-lip --config providers/typescript-language-server.toml` (port 4847) was
run with `workspace_root = ~/project/kb/web-code` (kb-code's own
SPA — its `tsconfig.json` root, per this file's own guidance), against
typescript-language-server 6.0.0 wrapping typescript 7.0.2 (both installed
under `~/.local/lib/node_modules`, npm prefix pointed at a user-owned dir per
prerequisite #1).

**What worked — everything.** `GET /lip/identity` reported `healthy: true`
with all six capabilities and `diagnostics_mode: "push"` (matches this
file's own comment: tsserver publishes, it doesn't pull). The first
`POST /lip/hover` (`src/components/annotations/AnnotationsPanel.tsx:97:18`,
the `buildCreatePayload` call site) succeeded on the very first request
(3.74s, the first-open/tsserver-project-load cost) — but the FOLLOWING
`/lip/definition` call landed mid-index and correctly answered `{"refused":
"indexing"}`; polling `GET /lip/identity` showed `indexing` flip back to
`false` roughly 90s later, after which every remaining probe against the
same symbol returned real data:

- `/lip/definition` → 1 result, `src/lib/annotations.ts:44:16` — 0.25s.
- `/lip/references` → 22 results across `AnnotationsPanel.tsx` (×3),
  `annotations.ts`, `reviewComments.ts` (×2), and `annotations.test.ts`
  (×16) — 0.12s.
- `/lip/symbols` (whole-file) → a populated, correctly-flattened
  `DocumentSymbol` tree (a few anonymous arrow-function callback entries
  render as `name: "<unknown>"` — a tsserver `DocumentSymbol` naming quirk
  for anonymous callbacks, not a kb-lip translation bug) — fast.
- `/lip/diagnostics` → `{"results": []}` for this clean file, after the
  full `diagnostics_wait_ms` (2.00s) — a valid "empty-after-wait" pass per
  design-addendum-2.md §D, not a refusal.
- `/lip/code-actions` (same position) → `{"results": [], "dropped_command_only":
  1}` — one command-only action offered by tsserver (almost certainly a
  workspace-wide refactor requiring `workspace/executeCommand`) was
  correctly dropped and counted rather than executed — 0.05s.

**One identity-field discrepancy, investigated and explained, not a bug:**
`server_name`/`server_version` both read `null` on `GET /lip/identity`.
Reading `crates/kb-lip/src/lsp.rs`'s `ServerInfo::from_initialize_result`
confirms it parses `result["serverInfo"]["name"]`/`["version"]` correctly
(the same field ruby-lsp/rust-analyzer/gopls all populate, PRR-L4/this
smoke) — typescript-language-server 6.0.0 simply doesn't send `serverInfo`
in its `initialize` response (an OPTIONAL field per the LSP spec), so `null`
is the honest reading, not a kb-lip parsing gap. Actual versions recorded
out-of-band from the installed npm packages: typescript-language-server
6.0.0, typescript 7.0.2.

**Bottom line:** all six lip/1 endpoints verified live and correct against
a real sub-project of this repo. Position encoding: per the "position
encoding" note in `typescript-language-server.toml`, kb-lip's hardcoded
UTF-16 assumption is the LSP-mandatory default and matches JS/TS's own
native UTF-16 string model; this smoke's probes were all against pure-ASCII
lines (the UTF-16 stress test is Go's, per §S2-D's own scoping — see below),
so no live multibyte counter-evidence was gathered here specifically, but
nothing in six passing endpoints contradicts it.

## Enable python lsp-live on the local daemon — operator steps

1. Get pyright running against your target repo (`npm install -g pyright`,
   see `providers/pyright.toml`'s own prerequisites — note pyright is an
   npm package despite analyzing Python) and confirm it's healthy:

   ```bash
   kb-lip --config providers/pyright.toml &
   curl -s http://127.0.0.1:4849/lip/identity | jq '.healthy, .capabilities'
   ```

2. In your `kb-code.toml`, add:

   ```toml
   [[repos]]
   name = "my-python-app"
   path = "/absolute/path/to/my-python-app"

   [[intel.providers]]
   name  = "python"
   url   = "http://127.0.0.1:4849"
   langs = ["python"]
   repos = ["my-python-app"]
   ```

3. Restart kb-code-server. Same lazy-handshake, cache-once-healthy,
   retry-on-failure posture as ruby's own steps above.
4. Confirm: `GET /api/repos` should show `intel.alive: true` (or
   `intel_providers`, for a repo with more than one configured provider —
   see "What lsp-live shows in the UI/CLI" above) for the repo, and a
   resolve/hover/usages call against a `.py` file in that repo should
   surface a `lsp-live`/`exact` candidate as the top hit.

### Live smoke evidence (python) — 2026-08-31 (S2 smoke, C1) — all six endpoints PASS, verified on a minimal sample (not a production repo)

`kb-lip --config providers/pyright.toml` (port 4849) was run against pyright
1.1.413 (the npm package's own `package.json`; `pyright-langserver` itself
prints no `--version` output when run bare — it just errors asking for
`--stdio`/`--node-ipc`, expected CLI behavior, not a bug) with
`workspace_root = /tmp/s2-smoke/py-sample`, a minimal two-file sample created
for this smoke run (venv-less, no `pyrightconfig.json`) — **honestly noted:
verified on a sample, not a production repo**, per this file's own scoping:

- `greet.py`: `def greet(name: str) -> str: return f"Hello, {name}!"`
- `main.py`: `from greet import greet` (cross-file import), `def add(a: int,
  b: int) -> int`, then `result: int = add(1, "two")` — the deliberate type
  error — and `print(greet("world"))`.

**What worked — everything, including the deliberate error.**
`GET /lip/identity` reported `healthy: true`, all six capabilities. All four
POST-with-position endpoints were probed against `main.py:9:6` (the `greet`
identifier in `print(greet("world"))`, resolving cross-file into `greet.py`):

- `/lip/hover` → `"(function) def greet(name: str) -> str"` — 7.67s (first-open
  cost; pyright's own workspace scan).
- `/lip/definition` → 1 result, `greet.py:1:4` (the `import greet` line —
  pyright/pyright-langserver's own convention of resolving a re-exported
  import to its `from` clause rather than the definition site three lines
  down; not a kb-lip artifact) — 0.012s.
- `/lip/references` → 3 results (`greet.py:1`, `main.py:1` the import, and
  `main.py:9` the call site) — 0.005s.
- `/lip/symbols` → 4 correctly-ranged entries (`add`, its two params `a`/`b`,
  and `result`) — 0.002s.
- `/lip/diagnostics` (whole-doc, `main.py`) → **caught the deliberate type
  error live**: `reportArgumentType`, `"Argument of type \"Literal['two']\"
  cannot be assigned to parameter \"b\" of type \"int\"…"`, severity 1,
  source `"Pyright"`, correctly ranged on the `"two"` literal — 0.057s.
- `/lip/code-actions` (point range on the same literal) → `{"results": [],
  "dropped_command_only": 0, "dropped_unsupported": 0}` — a genuinely EMPTY,
  non-refused answer (pyright didn't offer a quick fix for this particular
  type mismatch, which is normal — a type-mismatch on a literal usually
  isn't auto-fixable) — 0.003s. Per this smoke's own pass criterion, an
  empty-but-not-refused code-actions result is a PASS.

**One discrepancy worth flagging, investigated, not a kb-lip bug:**
`providers/pyright.toml`'s own comment asserts pyright is a PULL-diagnostics
server (`capabilities.diagnosticProvider` advertised at `initialize`). This
smoke observed `GET /lip/identity` report `diagnostics_mode: "push"` for
pyright-langserver 1.1.413 instead. `ServerInfo::diagnostics_mode()`
(`crates/kb-lip/src/lsp.rs`) is a straight, fixture-tested read of the
server's own advertised `capabilities.diagnosticProvider` — not a timing
issue — so this is either a version-specific change in pyright's own
behavior since that comment was written, or (more likely, reading
`LspClient::start`'s `initialize` capabilities) pyright only advertises pull
support when the CLIENT itself declares a `textDocument.diagnostic`
capability, which kb-lip's minimal client does not send. Either way, it did
NOT block correctness: the `/lip/diagnostics` call above still worked and
returned the real, correct error in 57ms — comfortably under
`diagnostics_wait_ms`'s 2000ms cap — via kb-lip's PUSH-cache path, meaning
pyright published the diagnostic almost immediately after `didOpen` for this
tiny file. Worth a follow-up look (declaring pull-diagnostics client
capabilities explicitly) but out of this smoke unit's scope.

**Bottom line:** all six lip/1 endpoints verified live and correct,
including the one deliberately-broken diagnostic this sample was built to
exercise. Position encoding: all probed lines were pure ASCII, so this run
adds no new multibyte counter-evidence for pyright specifically (Go's
sample carries the dedicated UTF-16 stress test, per §S2-D's own scoping,
below).

## Enable go lsp-live on the local daemon — operator steps

1. Get gopls running against your target repo (`go install
   golang.org/x/tools/gopls@latest`, see `providers/gopls.toml`'s own
   prerequisites) and confirm it's healthy:

   ```bash
   kb-lip --config providers/gopls.toml &
   curl -s http://127.0.0.1:4851/lip/identity | jq '.healthy, .capabilities'
   ```

2. In your `kb-code.toml`, add:

   ```toml
   [[repos]]
   name = "my-go-app"
   path = "/absolute/path/to/my-go-app"

   [[intel.providers]]
   name  = "go"
   url   = "http://127.0.0.1:4851"
   langs = ["go"]
   repos = ["my-go-app"]
   ```

3. Restart kb-code-server. Same lazy-handshake, cache-once-healthy,
   retry-on-failure posture as ruby's own steps above.
4. Confirm: `GET /api/repos` should show `intel.alive: true` (or
   `intel_providers`, for a repo with more than one configured provider —
   see "What lsp-live shows in the UI/CLI" above) for the repo, and a
   resolve/hover/usages call against a `.go` file in that repo should
   surface a `lsp-live`/`exact` candidate as the top hit.

### Live smoke evidence (go) — 2026-08-31 (S2 smoke, C1) — all six endpoints PASS; UTF-16 position encoding CONFIRMED CORRECT live

`kb-lip --config providers/gopls.toml` (port 4851) was run against gopls
v0.23.0 (`golang.org/x/tools/gopls`, Go 1.27.0 toolchain — `command` pointed
at the absolute `~/go/bin/gopls`, since it isn't on this shell's PATH) with
`workspace_root = /tmp/s2-smoke/go-sample`, a minimal module built for this
smoke run:

- `go.mod`: `module smoke`, `go 1.21`.
- `greet.go`: `func greet() string { return "hi from greet" }`.
- `main.go`: `func main() { fmt.Println("héllo wörld 你好世界", greet()) }` —
  a line deliberately containing multibyte runes (2-byte `é`/`ö`, 3-byte
  CJK `你好世界`) immediately BEFORE the `greet()` call on the same line, to
  make the byte-vs-UTF-16 column codec decisive (see below).

**What worked — everything.** `GET /lip/identity` reported `healthy: true`,
all six capabilities, `diagnostics_mode: "push"` (matches this file's own
comment), `server_name: "gopls"` — `server_version` is gopls's own full
`debug.BuildInfo` JSON blob rather than a plain semver string (noisier than
every other provider here, but it does contain the real version,
`v0.23.0`, inside `Main.Version` — a minor ergonomic wart in gopls's own
`initialize` response, not a kb-lip bug). `indexing` was observed `true` on
first launch and flipped to `false` within roughly two minutes for this
trivial two-file module (slower than it should be for a module this size —
this box was under heavy, documented I/O contention the whole session, see
the rust section above).

Sanity probes against the definition site (`greet.go:3:5`, pure ASCII):
`/lip/hover` → `"func greet() string"` (22.8s — first-open cost under this
run's box contention); `/lip/symbols` → 1 correctly-ranged entry (`greet`,
kind 12/function).

**The UTF-16 stress test — the decisive result.** On `main.go`'s line 6,
`\tfmt.Println("héllo wörld 你好世界", greet())`, the `greet` identifier
starts at BYTE offset 43 but UTF-16 CODE-UNIT offset 33 — a delta of 10
units (2 from `é`+`ö`, 8 from the four 3-byte CJK characters). The line is
only 41 UTF-16 units long in total, so a broken byte→UTF-16 translation
(e.g. one that fed the raw byte offset straight through as if it were
already a UTF-16 character index) would address position 43 in a 41-unit
line — structurally past the end of the line, guaranteed to miss `greet`
entirely. Sending kb-lip the CORRECT byte offset (43, per lip/1's own byte
column convention):

- `/lip/hover` → `"func greet() string"`, with the echoed `range` reading
  back `col: 43, end_col: 48` (kb-lip's own UTF-16→byte reverse-translation
  of gopls's answer, landing exactly on the 5-byte `greet` span) — 0.008s.
- `/lip/definition` → `greet.go:3:5`–`3:10`, the exact declaration span
  established by the sanity probe above — 0.001s.
- `/lip/references` → 2 results, `greet.go:3` (decl) + `main.go:6` (this
  call) — 0.001s.

This is a positive, decisive confirmation — not a lucky landing inside a
wide token — because the byte/UTF-16 delta (10) plus the line's own length
budget (41 units) leaves no slack for an off-by-N translation to
accidentally still land inside `greet`. **`gopls.toml`'s
`[UNVERIFIED-LIVE]` position-encoding note is de-flagged below as
CONFIRMED, dated 2026-08-31.**

Remaining probes, same position: `/lip/diagnostics` (whole-doc, clean file)
→ `{"results": []}` — 0.0003s, a valid pass. `/lip/code-actions` (point
range on `greet()`) → three real, correctly-ranged refactors (`"Extract
variable"`, `"Inline call to greet"`, `"Split arguments into separate
lines"`), plus `"dropped_command_only": 7` for actions gopls offered that
require `workspace/executeCommand` (correctly dropped and counted, never
executed, per design-s2.md §S2-C) — 13.0s (box contention again; still
correct).

**Bottom line:** all six lip/1 endpoints verified live and correct against
a real gopls handshake, and the UTF-16 position-encoding codec is now
LIVE-CONFIRMED for gopls specifically (previously only ruby-lsp had this,
PRR-L4 §c) — via a test constructed so a broken codec could not have
accidentally passed.
