# Critical review — features merged in the week to 2026-09-28

Six independent review lenses over the 128 non-merge commits that reached `main` in the
7 days to 2026-09-28, plus an adversarial self-review of the CI/licensing work committed
in the same session. Every finding below was verified against the tree before it was
recorded; findings that did not reproduce were dropped rather than reported.

**Status of this document:** findings only. Nothing here has been fixed except where a
commit message says otherwise. They are ordered by user-visible impact, not by file.

## Verification pass

Every finding below was then put through an adversarial pass whose only job was to REFUTE it.
Two did not survive, and are marked ~~struck through~~ where they appear — **M8** (a deliberate,
unit-pinned design) and **M11** (the function is covered by 9 unit cases and 2 e2e specs; only
one call site is a real hole). Do not act on those two.

Nine survived. Three of those carry corrections to the original text, applied inline:
- **M1** — the `SQLITE_BUSY` trigger is unreachable (`Store` holds a single `Mutex<Connection>`
  and `lock()` is `parking_lot`). The reachable errors are I/O and rusqlite row-decode failures.
  Still a fail-open on read error, narrower trigger than first written.
- **M2** — the semantics are confirmed but no reachable panic source in the closure was found,
  so this is defensive hardening rather than a demonstrable live bug.
- **M4** — survives on `seed.rs` only. The `capture.rs` half is **refuted** (its N is at most 2,
  not the store's branch count), and "a complete network fetch per spec" is wrong: git raises
  `couldn't find remote ref` during ref negotiation, before pack transfer, so retries move a
  delta. The *time* multiplication `1 + N` × 1800 s is real; the byte multiplication is not.

Severity changes: **M11** drops from MAJOR to MINOR. **M12** stays MAJOR, with one correction —
a 400 renders as a permanent `pending` tier, not as "no grammar", so the symptom is a spinner
that never resolves. **M7**'s FSRS impact is behind `scoring_v2_stability`, which defaults to
**false**; the display paths (census, `/recalled-by`, `/sessions/{id}/recalls`) are hit
unconditionally. **M6** has a partial mitigation: `scripts/kb-backup-cron.sh` drives
`kb backup --all`, which does call `run_remote_copy` — but that is the author's host cron, not a
shipped mitigation, and the defect is specific to the new in-process `[backup] schedule_hours`.

---

## BLOCKER

### B1. kb-server has no `Host` allowlist — DNS rebinding is an unauthenticated auth bypass

`crates/kb-server/src/middleware.rs:99-104`

`origin_allowed` admits any state-changing request whose `Origin` host:port equals the
request's own `Host`. The comment above it reasons that "a genuine cross-site request
carries the *victim's* origin, which won't equal an attacker-chosen `Host`". That holds
for cross-site CSRF and **fails under DNS rebinding**, where the attacker's page origin
*is* the rebound name.

Against the default `127.0.0.1:4000` deployment:

1. Attacker serves a page at `http://evil.example:4000`; the victim loads it.
2. That page's own script calls `fetch('/api/…', {method:'POST'})` — relative, so
   same-origin to the browser.
3. Attacker flips DNS `evil.example → 127.0.0.1`. The browser now talks to kb.
4. The request arrives with `Host: evil.example:4000` and
   `Origin: http://evil.example:4000`, so `rest == host` → **allowed**.
5. The peer is `127.0.0.1` → `is_loopback_origin` true →
   `request_is_admitted` returns true at `middleware.rs:287-289` with **no credential**.
6. Same-origin, so CORS is irrelevant. The attacker's JS reads and writes the whole
   corpus, including `DELETE /api/kb`.

The SPA fallback serves the shell on any non-artifact `Host`
(`crates/kb-server/src/routes/dispatch.rs:46-51`), so the daemon answers the rebound name.

**The fix already exists in the sibling daemon.**
`crates/kb-code-server/src/security/origin.rs` implements `host_allowed`, and its own
module doc says of kb-server that it "(c) never checks `Host` at all" and that checking
`Host` on every method "is the whole DNS-rebinding defence". kb-code closes this by
default, refusing a rebound `Host` unconditionally for a loopback peer with no config
required. kb-server has no `hostnames` key (`crates/kb-core/src/config.rs:756-800`) and
no host gate anywhere under `crates/kb-server/src/` — `grep -rn hostnames
crates/kb-server/src/` returns nothing.

**Fix:** port kb-code's `host_allowed` to kb-server; add `[server] hostnames`; enforce on
every peer (unconditionally for a loopback peer); stop treating `Origin == Host` as
sufficient on its own.

---

## MAJOR

### M1. A bound review store can read the forge with an *ambient* credential

`crates/kb-code-server/src/reviews.rs:1836-1841`

```rust
let recorded = st.store.get_review_store(id).ok().flatten().and_then(|r| r.cred_account);
let bound = settings.gh_user.is_some() || recorded.is_some();
```

`get_review_store` returns a `Result`; `.ok()` collapses a `SQLITE_BUSY`/I/O error into
`None`, so `recorded` becomes `None`. For a store bound only by a recorded
`cred_account` — the common case, written at `registry.rs:1354-1359` — `bound` flips
false, `ApiBinding::Unbound` is chosen, and `resolve_github_token_with`
(`github.rs:777-786`) puts `[github] token_file` first and `KB_CODE_GITHUB_TOKEN` second.

The daemon then FETCHES as pinned account A and READS PR metadata as whatever account
the ambient file or env names — the exact identity swap `github.rs:37-52` says D12
forbids. Completely silent: `warnings` stays empty and `store show`'s `cred_account`
still says A, so the audit trail misreports the identity that read the forge.

**Fix:** treat "could not read the row" as *bound* (fail closed), or propagate the DB
error and skip the API read with a warning.

### M2. The `spawn_blocking` join fallback defaults to the permissive binding

`crates/kb-code-server/src/reviews.rs:1866`

```rust
.await
.unwrap_or((ApiBinding::Unbound, None, vec![]));
```

The failure arm of the D12 rule picks `Unbound`, the one value that re-admits the
ambient rungs. Any panic in the closure (the `gh` spawn path, OOM) silently converts a
bound store into an unbound one with zero warnings, so the operator sees a normal review
envelope. Same class as M1, opposite trigger.

**Fix:** compute `bound` outside the blocking task and default the join error to `Bound`
plus a warning, or to "no credential" plus a warning.

### M3. The `start-pr` fallback path fetches with no deadline and no git hardening

`crates/kb-code-server/src/reviews.rs:421-436` (used by `start_pr_base` (at :1191, via `fetch_remote_base` at :1090)) and
`crates/kb-code-server/src/github.rs:203-210`

`run_git` is a bare `std::process::Command::new("git")…output()`: no timeout, no
process-group kill, and — unlike the store's own `StoreGit::build`
(`review_store/git.rs:70-76`) — no `GIT_TERMINAL_PROMPT=0`, no
`SSH_ASKPASS_REQUIRE=never`, no `ssh -o BatchMode=yes`.

This week raised `base_fetch_timeout_secs` from 30 s to 1800 s, but that value is read
by `StoreGit::fetch` and reaches only `seed::fetch_base_branches` and
`capture::fetch_forge` — repos whose store is `ready`. The fallback path that the same
wave's commit message calls "the common case" got none of it.

**Concrete failure.** `POST /api/reviews/pr?async=1` on a repo with no ready store,
against a remote that accepts TCP then stalls: git blocks. With a controlling tty it
blocks on the credential prompt; without one, on the socket, forever. `settled` is never
written (`review_jobs.rs:426`), so the entry stays `running` for the full
`STUCK_JOB_HORIZON_SECS` = 21 600 s, 409-ing every later admission on its
`(start-pr, repo, pr)` key while holding `review_sync::repo_guard`, which also blocks
`review sync` for that repo. `sweep`'s `abort()` cannot land: the body is inside
`spawn_blocking`, which `review_jobs.rs:249-254` names as the case only a git deadline
can produce — and this path has no deadline.

**Fix:** route both fetches through the existing `review_store::proc::run` (process-group
kill + bounded capture), or at minimum set `GIT_TERMINAL_PROMPT=0` and add a wall
deadline that SIGKILLs the group, classifying the timeout into the existing
`FailureClass::Timeout` so `start_pr_base` falls to its next rung.

### M4. The vanished-ref recovery multiplies the new deadline by the store's branch count

`crates/kb-code-server/src/review_store/seed.rs:656-681` and
`crates/kb-code-server/src/review_base/capture.rs:782-816`

`git fetch` with several refspecs aborts the whole call on one missing ref, which
`classify.rs:241-243` maps to `FailureClass::Vanished`. Both recovery loops then re-fetch
each spec alone — a *complete* network fetch of the base graph per spec — and each
iteration passes `git.base_fetch_timeout()` (1800 s) unchanged.

`N` is the distinct base-branch count across the store's open reviews
(`registry.rs:1318`). A store with 8 base branches where one was deleted on the forge
gives 1 + 8 = 9 sequential fetches ≈ 4.5 h, with no ceiling of its own, holding the
store's `base` and every `work-<id>` fetch lock (`routes.rs:637-651`, a route with no
request-level timeout).

This also invalidates the pin the ×60 raise rests on: `git.rs:148-151` justifies the
ceiling against `SEED_FETCH_TIMEOUT_SECS` (3600 s), but the *pass* is 4.5× that. The
compile-time assertion cannot see the multiplier.

**Fix:** one shared deadline for the pass — `let until = Instant::now() +
git.base_fetch_timeout();` before the first attempt, each retry bounded by what
remains — or bound the *pass* at `SEED_FETCH_TIMEOUT` and state that as the invariant.

### M5. The job map's honesty guarantee does not cover the store's own locks

`crates/kb-code-server/src/review_jobs.rs:263-288` with
`crates/kb-code-server/src/review_base/capture.rs:758-759`

`review_jobs.rs:93-100` promises that a corpse "can never keep holding
`crate::review_sync::repo_guard` … while the map has already forgotten it". True for
`repo_guard` (an async guard held by the task's future, so `abort()` drops it). False for
the store's locks: `fetch_forge` takes `lock.blocking_lock()` — a `std` guard in a pool
thread that `abort()` cannot reach. Same for `ops_lock` and the store `flock`.

At the 6 h horizon, `sweep` calls `task.abort()`; the task is awaiting a `spawn_blocking`
`JoinHandle`, so the abort drops the future and the next `sweep` sees
`is_finished() == true` while the blocking closure still runs. `sweep` drops the entry
and the 404 tells the poller exactly what `review_jobs.rs:90-91` prescribes: re-submit.
The new job passes the now-free `repo_guard` and blocks on the corpse's store lock; when
the corpse returns it has written refs and DB rows for a job the map forgot.

**Fix:** register the `spawn_blocking` `JoinHandle`'s `AbortHandle` in the `JobHandle`
so `sweep` sees unfinished work, or track outstanding blocking work per job and refuse
to drop the entry while it is non-zero (re-submit then 409s instead of double-writing).

### M6. The daemon's new backup schedule never copies off-host, and reports success

`crates/kb-server/src/lib.rs:1266-1285`;
`kb_core::storage::backup::run_remote_copy` (`crates/kb-core/src/storage/backup.rs:71`)
has **no caller in the daemon** — its only call site is
`crates/kb-cli/src/commands/backup.rs:117`.

The new in-process scheduler's success arm logs "backup schedule: wrote export tarball"
and emits `maintenance.backup.written` after writing a local tarball and nothing else.
`BackupSection`'s own doc (`config.rs:592-596`) says a schedule "with no `remote_cmd`
still writes local tarballs and WARNs that the off-host copy will not run" — but that
warning is conditional on `remote_cmd` being ABSENT (`config.rs:1990`). An operator with
a fully configured `remote_cmd` + `remote_dest` + `schedule_hours` gets **no warning and
no off-host copy**, contradicting `docs/self-host.md:667` ("Backups must leave the box").

This is a regression the wave introduced: `56f94c2` deleted
`warn_backup_schedule_without_remote_cmd` and replaced an honest warning with a silent
local-only schedule.

**Fix:** call `run_remote_copy` in the `Written` arm (via `spawn_blocking`) and log its
`Failed` outcome; until then, warn whenever `schedule_hours` is set that the daemon
schedule is local-only.

### M7. The usage ledger counts every recall twice

`crates/kb-core/src/storage/sqlite.rs:4646` (also :4581, :4680, :4802-4811)

`served_artifact_pred` (added by `3138304`, defined at `sqlite.rs:84-86`) ORs live-serve
rows into every read beside the newest-capture predicate. One injection is written
twice, by two writers, into the same table: at serve time
(`routes/memory.rs:631` → `record_served_recalls`, stamped `served-…`) and at Stop-hook
capture time (`sessions/view.rs:2835 derive_memory_recalls`, which walks every injected
`<!--kb-recall/1 …-->` marker). Neither read path excludes the other, and
`memory_recalls_prune_served` only ages serve rows out at the 30-day retention cutoff.

**Concrete failure.** After a session is captured, `GET /api/sessions/{sid}/recalls`
returns the same memory twice — once with `turn_id = "t-3"`, once with `turn_id = NULL`;
`GET /api/memory/recalled-by` lists the same session twice; `memory_recalls_counts_for_ids`
`COUNT(*)` = 2, so the census and the FSRS stability term
(`scoring_v2_stability`, `config.rs:659`) score a single recall as twice-used. The test
added at `sqlite.rs:11694` uses a *different* `memory_id`, so it never exercises the
overlap.

**Fix:** make the writers mutually exclusive at read time, or have
`memory_recalls_replace` delete the superseded `served-` rows for the same
`(session_id, memory_id, pos)` in the same transaction. Add a regression test writing
both rows for the SAME memory.

### M8. ~~A rejected registry secret is admitted and attributed to `operator`~~ — REFUTED, do not "fix"

An adversarial verification pass tried to kill this one and could not — but then found it is the
**documented, unit-pinned design**, not an oversight. Recorded here so nobody re-raises it.

The mechanism is real: `registry_match` (`middleware.rs:379-391`) returns `None` on a miss,
`request_is_admitted` falls through to `allow_no_auth` (`:306-312`), and `resolve_identity`
ends at the `Loopback` arm (`:369-373`) → `auth.operator`. So with a token registry configured
AND `KB_ALLOW_NO_AUTH=1`, a wrong `X-Kb-Token` does get a 200.

That is the chosen posture, in three places:
- the comment at `middleware.rs:306-311` states the decision — honour the override
  "REGARDLESS of registry emptiness … must never tighten admission for browser users carrying
  only Remote-User";
- `docs/architecture-invariants.md:237-242` — "**Attribution never gates.** … a non-empty token
  registry … must NEVER tighten `KB_ALLOW_NO_AUTH=1` … (unit-pinned)";
- a unit test at `middleware.rs:1528-1557`
  (`request_is_admitted_registry_never_tightens_allow_no_auth`) whose doc names this trade-off.

`docs/self-host.md:84-88` documents `KB_ALLOW_NO_AUTH=1` as the posture where "an **upstream proxy
is the authentication gate** … you deliberately run kb token-less behind it". The proposed fix —
reject a presented-but-wrong credential — would 401 exactly those browser users the invariant
protects. The "forged attribution" half also does not arise in the documented deployment: that
same traefik setup forwards `Remote-User` (`self-host.md:800`), so the ladder resolves to the
real user at `middleware.rs:337-355`.

**Residuals worth a doc line, not a behaviour change:** the test pins only the *absent*-credential
case, so the wrong-token case is unpinned; and in a gate that forwards no identity header,
mutations are recorded as `operator`. Fail-closed is preserved by default — registry set,
`KB_ALLOW_NO_AUTH` unset, non-loopback peer, wrong token → 401.

### M9. ~~One documented exit-code table, two implementations that disagree~~ — FIXED (`e245b1b` doc, follow-up commit)

`crates/kb-code-cli/src/envelope.rs:204-220` vs
`crates/kb-code-cli/src/review_agent.rs:200-232`; table at `docs/kb-code.md:1542-1552`

`exit_code_for` (wired once, `main.rs:7426`, for **every** non-review verb) has arms for
only 401/403 and 409. `AgentError::from_http` (the review/agent verbs) also maps
404 → 8 and 503 → 3. The documented table describes the second.

**Reachable today:** a revspec that does not resolve is answered
`404 urn:kb:errors:unknown-ref` (`kb-code-server/src/frames.rs:240`), and that status is
deliberately kept in the `anyhow` chain (`main.rs:7857-7862`) so `exit_code_for` can
read it. It finds no arm, and the process exits **1**. A 503
(`urn:kb:errors:store-seeding`, `review_store/registry.rs:76`) likewise. The table says
8 and 3.

**Fix (applied):** `exit_code_for` now carries `404 => EXIT_NOT_FOUND` and
`503 => EXIT_CONFLICT`, so the one documented table is true for the whole CLI. Verified with
`cargo check -p kb-code-cli`.

### M10. ~~`EXIT_NOT_FOUND`'s doc asserts the opposite of the shipped behaviour~~ — FIXED

`crates/kb-code-cli/src/envelope.rs:77-80`

The doc says it is "only used by verbs whose route answers an unambiguous not-found
body". `review_agent.rs:219-225` maps **every** 404, including the bodiless
loopback-refusal 404, and `docs/kb-code.md:1551` documents it that way while warning
that 8 is structurally ambiguous. An integrator reading the constant concludes exit 8
means "definitely absent" and writes exactly the wrong-but-confident branch.

**Fix (applied):** the doc now states the shipped truth — every 404 on the status alone,
including a loopback-only route's bodiless refusal, and that 8 is structurally ambiguous — and
names `review_agent::AgentError::from_http` as the mapper.

### M11. ~~`wireSpansToLineMap` has zero test coverage~~ — REFUTED as stated; a narrower MINOR survives

The headline was wrong and is retracted. `wireSpansToLineMap`
(`web-code/src/lib/paintSpans.ts:65`) is called from *inside* `paintSpans` at
`paintSpans.ts:97`, so `paintSpans` is not a separate code path — it is a thin wrapper over the
function the finding called untested. Every `paintSpans` assertion IS a `wireSpansToLineMap`
assertion.

It is covered by **9 unit cases** (`paintSpans.test.ts:15-29`;
`highlightConsumers.test.ts:17-24, 26-32, 34-41, 43-51, 53-59`;
`suggestionPaint.test.ts:60, 71`) and **2 e2e specs** the original finding did not mention:
`web-code/e2e/highlight.spec.ts:68` and `web-code/e2e/diff-highlight-surfaces.spec.ts:217`, the
latter asserting `[data-kbc-hl]` through `SuggestionDiff.tsx:166`, which calls the function
*directly* — one of the three production sites named. Replacing `paintSpans.ts:97` with
`new Map()` fails 9 unit assertions and 2 e2e specs, not zero.

**What survives, at MINOR:** the specific call at
`web-code/src/hooks/useDiffHighlights.ts:148` is genuinely unexercised. Its test file's `seed`
is typed `ReadonlyArray<readonly [readonly unknown[], FileResponse]>`
(`useDiffHighlights.test.ts:69`), so no test can seed a `HighlightBatchOut`; `snippet.byId` is
empty in all five cases and `sideFromSnippet` always takes the degraded branch at `:144-146`.
No e2e reaches it either — the snippet fallback needs an unindexed/oversize/404 blob, and
`diff-syntax.spec.ts:53-65` deliberately 404s and asserts the *unpainted* outcome, so it would
not catch the mutation. Secondary: all three wire-path test files are pure ASCII, so the
multi-byte column mapping is untested through this entry point (the mapper itself is covered at
`decorations.test.ts:10-51` and the parallel path at `diffHighlight.test.ts:48-71`).

### M12. The per-snippet byte cap lives in one caller, not in the hook

`web-code/src/hooks/useHighlight.ts:34-45`; clamped only at
`web-code/src/hooks/useDiffHighlights.ts:199,218`

The server refuses any item over `MAX_SNIPPET_BYTES = 256 * 1024` with a 400 that
rejects the **whole batch** (`kb-code-server/src/highlight.rs:751-759`). Only
`useDiffHighlights` clamps. `HighlightedSnippet.tsx:43-45`,
`PseudoFileView.tsx:62` and `SuggestionDiff.tsx:42` send unclamped text. `useHighlight`
returns only `{ byId, isLoading }` — no `isError` — so a 400 renders identically to "no
grammar for this language": plain text, permanently, with no diagnostic and no retry. A
markdown fence containing a minified bundle is > 256 KiB and is entirely ordinary.

**Fix:** move the guard into `useHighlight`'s `uniqueHighlightItems` (measure with
`utf8LengthOf`), cap the list at `MAX_BATCH_ITEMS` (64), and add `isError` so callers can
distinguish a refusal from a missing grammar.

---

## MINOR

- **N1.** `crates/kb-code-server/src/review_jobs.rs:420-462` — the job task's
  `JoinHandle` is kept only as an `AbortHandle`; a panic anywhere in the body skips
  `settled`, leaving `status: "running"` for 6 h with no error, and a retry silently
  re-attaches. `boot.rs:81` and `maint.rs:1867-1873` both log their panics; this spawn
  does not.
- **N2.** `crates/kb-code-server/src/review_store/registry.rs:1834` (also :1529, :1553) —
  `let _ =` discards the error on the final state write of `sync_ready`, so a DB error
  returns `{"action":"synced"}` 200 while the row still disagrees. The fresh-read
  comment at :1809-1814 explains that this exact class of loss is why it re-reads.
- **N3.** `crates/kb-cli/src/commands/backup.rs:412-417` — a non-zero `tar` exit returns
  the error **without removing `out_path`**, so a truncated tarball is left where both
  the scheduler and the doctor will report a healthy backup. The kb-core writer does
  remove it (`backup.rs:326-329`).
- **N4.** `crates/kb-code-server/src/review_store/` has **no `#[ts(export)]` anywhere**,
  so its wire types sit outside the `code-drift` gate and
  `web-code/src/api/types.ts:2054-2119` hand-mirrors them. The mirror has already
  drifted: `routes.rs:331-338` injects `runtime.git_fallbacks`, which the TS interface
  does not declare, so a shipped feature is invisible to the SPA and `tsc` cannot catch
  it.
- **N5.** `FailureClass`'s 21 slugs (`classify.rs:115-135`) reach the store card
  (`routes.rs:197-199`) but 13 of them appear **zero times** in `docs/kb-code.md`. An
  operator reading `host-key-mismatch` or `disk-full` has no documented meaning.
- **N6.** `review_store/routes.rs:164-166` — `StoreSettings::warnings` is
  boot-global but is pushed onto every repo's store card, so one bad
  `base_fetch_timeout_secs` renders as N identical `config` findings with no way to tell
  daemon-scoped from repo-scoped.
- **N7.** `docs/configuration.md` review-store section — **fixed in this session**
  (commit `9dcee6e`): twelve `file:line` anchors were 15–46 lines stale and several
  named the wrong struct. Recorded here because the underlying rot is unfixed — nothing
  enforces these citations. A test extracting every `X.rs:N` from the file and asserting
  the cited line is near the named identifier would keep them honest.
- **N5b.** ~~`docs/kb-code.md`'s `base_fetch_timeout_secs` paragraph had a four-line block spliced
  into the middle of its first sentence~~ — FIXED. "bounds ONE base" was cut off and resumed four
  lines later at "(network) fetch". Reordered so the sentence is whole and the range warning is its
  own paragraph.
- **N8.** `crates/kb-code-server/src/review_store/registry.rs:1834` and the
  `code-*` path filters: `rust-toolchain.toml` and `.github/workflows/ci.yml` match no
  branch of the filter, so a PR that bumps the pinned toolchain or edits the `code-*`
  wiring itself still skips all five jobs it just changed.

## NIT

- **T1.** `deny.toml` — `bans.skip` is **crate-name scoped, not version scoped**: an
  entry suppresses every duplicate error for that name, and the `version` field is
  documentation only (verified: naming `9.9.9` suppresses just as well). A future third
  `html5ever` line would pass unnoticed. Now documented in the file's header (commit
  `f2dc569`).
- **T2.** `deny.toml` — `string_cache 0.9.0` is described as a build dep of
  `web_atoms 0.2.5`; it is a normal dep (only `string_cache_codegen`/`phf_codegen` are
  build deps).
- **T3.** `web/package.json` — the `@vitejs/plugin-react` 4→5 bump silently raises
  web/'s Node floor from `>=16` to `^20.19 || >=22.12`, and the repo has no `.nvmrc`, no
  `engines` field, and no doc stating a Node version. Every current consumer pins
  node 22, so nothing breaks today; a contributor on node 18 gets only a warning.

---

## Disposition — what was fixed here and what was not, and why

**Fixed in the same session** (each a fact that was wrong, so no behaviour decision):
M9, M10, N5b, N7, T1, plus the five `code-*` skip messages that named four of seven prefixes.
M9 is the only CODE change: `exit_code_for` gained the 404/503 arms the documented table already
promised, verified with `cargo check -p kb-code-cli`.

**Deliberately NOT fixed, because fixing needs a decision that is not mine to make:**

- **B1 (BLOCKER)** — porting kb-code's `host_allowed` to kb-server means adding a `[server]
  hostnames` key and deciding the fail-open/fail-closed boundary for a non-loopback peer behind a
  trusted proxy. kb-code's own module doc records that getting this wrong "takes a deployed
  daemon down on upgrade — a hardening unit that bricks the deployment it hardens is not a
  hardening unit." That trade-off is a product call. It is also the highest-value item here and
  should be the next piece of work.
- **M6** — whether the in-process scheduler *should* copy off-host, or whether `remote_cmd` is
  CLI-only by design, is a config-contract decision. The current behaviour contradicts
  `self-host.md:667` either way, so it needs an owner.
- **M7** — making the serve and capture writers exclusive is a data-model change with a retention
  dimension (`memory_recalls_prune_served` is age-based by design). Which record wins is a
  product question about what "a recall" means.
- **M1 / M2** — both fail *open* to a more privileged credential. The fix is to fail closed, which
  is right for a security rule but changes behaviour for an operator whose store currently reads
  fine off an ambient token. That trade should be named, not slipped in.
- **M3 / M4 / M5** — real defects, but each is a concurrency design change in a subsystem that
  just shipped, and each needs its own tests. Folding them into a docs-and-CI commit would make
  the change harder to review, not easier.
- **M12** — the fix belongs in the hook and changes a component's public return type, so it
  touches four call sites.

Every one of these is a *unit of work*, not a line. The point of this document is that the next
person starts from evidence rather than from scratch.

---

## Checked and found clean

Recorded so a future reviewer does not redo the work.

- **The review store's own git is fully bounded.** Every `StoreGit` call goes through
  `proc::run` with an explicit `RunSpec.timeout`: base fetches 1800 s, `ls-remote` 15 s,
  `credential fill` 10 s, local plumbing 60 s, work/member 120 s, maintenance 30 min,
  seed import 3600 s. No `RunSpec` is unbounded. `proc.rs` is leak-free: own process
  group, `killpg` on every exit path, `waitid(WNOWAIT)` so the pgid cannot be recycled,
  capped output sinks drained on dedicated threads, `pgid <= 1` refused.
- **No lock is held across an `.await`** anywhere in the store path. Every `tokio` guard
  is `lock_owned().await` then held across exactly one `spawn_blocking`, or a
  `blocking_lock()` taken *inside* the blocking closure. Every `parking_lot` guard is a
  single statement.
- **No unbounded collections.** `stats_cache`, `seeding`, `held`, `fetch_locks`,
  `ops_locks`, `forge_cache` are all keyed by an existing entity; `forge_cache` is
  bounded by total PR count because a repeat put overwrites.
- **TS binding generation is real and complete** for what it covers — all 55
  `#[ts(export)]` sites have a generated file, no orphans either way, and
  `just gen-ts-code-check` does `rm -rf` + regenerate + porcelain diff. (M11/N4 are about
  what it does *not* cover.)
- **Byte→column is genuinely unified** — `lineStartByteOffsets`, `lineIndexAt`,
  `makeByteToUtf16Mapper`, `utf8ByteLength`, `utf8LengthOf` each have exactly one
  definition and are imported, not copied.
- **The event schema is gated** — `maintenance.backup.written` is declared, has a
  `per_type` arm, is emitted, and `every_emitted_kind_is_declared_in_v0_0_1_types`
  (`schema.rs:557`) walks both crates' `src/`.
- **Boot crash recovery is sound**; a `seeding` row is never permanently wedged; the
  per-store `flock` is non-blocking and reports `store-locked` rather than parking.
- **The 2026-09 NOTICE/licensing work is correct.** The regenerated
  `THIRD-PARTY-LICENSES.md` is a byte-identical clean render (826 crates, balanced
  fences, no truncation, no stale versions, self-consistent Overview). The seven
  `deny.toml` skips are load-bearing: removing any one turns the gate red on the bumped
  lock.
- **`git fsck` clean** after removing 24 worktrees.
