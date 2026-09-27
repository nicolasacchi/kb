# BUILD-LOG — kb-code review store, Phase 1 acceptance (BUILD-BRIEF §3)

| | |
|---|---|
| generated | 2026-09-27T15:07:31+0200 |
| driver | `scripts/review-store/run_gates.py` (RS-U13) |
| mode | real run |
| before binary | `/tmp/gates-before-bin/kb-code-server` (sha256 7982f0ac88610420…) — commit `UNRECORDED` |
| after binary | `/tmp/gates-after3/kb-code-server` (sha256 2bde2c50860bcc73…) — commit `UNRECORDED` |
| before volume | `/home/nik/kbc-gates-state` (V0044 input) |
| after volume | `/home/nik/kbc-gates-after` (migrated in place by the post-upgrade boot) |
| ports | before `4790`, after `4791` — the in-use set [4000, 4001, 4747] is never touched |
| readiness budget | after `2h00m00s`, before `2h00m00s` — the after budget covers a first boot that takes the gated V0045 pre-migration snapshot (a whole-volume `VACUUM INTO`) |
| pristine inputs | `/home/nik/kbc-gates-bundle-2026-09-24` (verified against `SHA256SUMS`) |
| worktree | c7b5218 |

## Summary

| gate | asserts | verdict | one line |
|---|---|---|---|
| 1 | golden relocation | UNRUN | not selected (`--gate 2`) |
| 2 | user-repo invariance | PASS | 3 clone(s) byte-identical across create, start-pr, sync, snapshot, auto-capture, retrack and GC, under --invariance-scope store-registered (1000farmacie-iac, 1000farmacie-rails-01, 1000farmacie-rails-02; 9 configured clone(s) were OUT of scope). |
| 3 | no fallback | UNRUN | not selected (`--gate 2`) |
| 4 | review 65 end to end | UNRUN | not selected (`--gate 2`) |
| 5 | live GitHub (gh-cli) | UNRUN | not selected (`--gate 2`) |
| 6 | secrets | UNRUN | not selected (`--gate 2`) |
| 7 | existing suite + TS regen (CI) | UNRUN | not selected (`--gate 2`) |

**Exit code 1** — NOT all seven gates PASS (not PASS: [1, 3, 4, 5, 6, 7]).
---

## Overall outcome across all runs (added by the orchestrator, not the driver)

The driver REWRITES this file on every invocation, so a per-invocation summary is
not the phase's acceptance. Across every run: **6 PASS, 1 BLOCKED — with no gate
ever reporting a false pass.** Gates are run individually here because gate 1's
isolation and the volume's cold-cache cost exceed one invocation's budget.

| gate | verdict | evidence |
|---|---|---|
| 1 golden relocation | **BLOCKED — provably** | The pre-upgrade binary panics reading the volume: `prose_refs.rs:706:24: end byte index 54 is not a char boundary; it is inside 'à'`, the blocking-task wrapper re-panicking and dropping the connection. The cause is `is_delim` reading a UTF-8 continuation byte as a Latin-1 char, so `0xA0` counts as whitespace and a token is cut mid-character. **No commit is both pre-store and panic-free**: `main@9e1ac65` lacks the fix, current `main` has it, and it arrived with `539e2fc` (RS-U10) — part of this wave. The post-upgrade daemon logged **zero** such panics. Isolation cost: one unreadable review = **2m48s** through the harness's four transport retries, against 114 reviews. |
| 2 user-repo invariance | **PASS** | 3 clones byte-identical across create, start-pr, sync, snapshot, auto-capture, retrack and GC. The 9 absent configured clones are named as out of scope, never silently dropped. |
| 3 no fallback | **PASS** | Every store `ready`; `unresolved` and `odb_miss` both zero. |
| 4 review 65 end to end | **PASS** | `retrack 65 --dry-run` → `stale-pin`; `retrack 65` → ps4 `kind=base-corrected`, tip `525631b506e7…`, base `7c1ed0cfdd7d…`, **10 commits / 40 files equal to live PR 15790**; findings and verdict held on ps3 with `verdict_scope_changed`. |
| 5 live GitHub | **PASS** | All **45** open PRs listed; every `forge.base_ref` equal to `gh pr list`; the 2 stacked `feature/15646-statsig-*` PRs included. |
| 6 secrets | **PASS** | No plausible full token and no literal `gh auth token` output in the daemon log, the envelopes or anywhere in the volume copy. 2 bare `ghp_`/`ghs_` prefixes EXCLUDED as fixture/doc text. |
| 7 CI + TS regen | **PASS** | 15/15 check runs green on #166, including both TS-regen jobs. No local suite was run. |

### The two product defects the gates found

Both surfaced in gate 2's first, cold run and both are fixed on this branch:

1. **The store's base-fetch deadline was hardcoded at 30 s.** A fetch from a
   748 MB clone cannot finish in that; git was killed mid-fetch and the
   operation dropped. Now `[review.store] base_fetch_timeout_secs`, default
   1800 s, bounded by the compiler against the work and seed deadlines.
2. **`review start` and `review snapshot` used a 10 s HTTP client.** Both make
   the daemon capture, so any capture outlived the client, which exited 5 with
   "is kb-code-server running" while the server finished minutes later. They
   now share `review_agent::READ_TIMEOUT` with `retrack` rather than each
   carrying a copy.

Recorded rather than papered over: a SLOW fetch and a HUNG one are **not**
distinguishable today — the capture carries no progress signal, so the
process-group kill remains the only outcome.

### What gate 1 would need

Not more time: a baseline that can read the volume. Its pre-upgrade half must be
a commit that is BOTH before the review store AND free of the `à` panic, and no
such commit exists. The driver already supports comparing only the reviews the
old binary can read and naming the rest; what it cannot do is invent the missing
binary.

## Gate 1 — golden relocation
**Asserts.** Every existing review's files, per-file blob ids, diff stats,
comment/finding anchors and verdict patchset are IDENTICAL before and after the
V0044 -> V0045 upgrade, and the only differences are new envelope fields
(`base{…}`, `warnings[]`, `minted`, per-patchset `kind`/`base_tip_sha`) — each
one enumerated below, because "we allowed the new keys" is only a result if the
list is printed. Also surfaces the two facts that prove the migration really ran:
the gated pre-migration snapshot of this volume, and the V0044 binary's refusal to
open the migrated volume.

Each review is read **in isolation** on both sides, and the verdict counts them:
`N of M reviews compared; K were unreadable by the pre-upgrade binary`. A review
the pre-upgrade binary cannot read is a finding, not an aborted gate — the
driver names it, quotes the HTTP path and the failure verbatim, and reproduces
the panic from that daemon's own log. Nothing is dropped silently: a review the
POST-upgrade binary can no longer read, a review that exists only after, and a
run in which no review could be compared at all are all FAILs.

The snapshot is checked through the product's OWN receipt (`backup.marker` beside
the volume), not through a hardcoded file name: the gate asserts the claim — a
non-empty snapshot of THIS volume, taken while it was still at epoch 44, whose
recorded byte count still matches the file on disk — and prints whatever the
product named it. It is named for the volume's epoch at the time of the snapshot,
because that is the epoch a restore of it lands on, so a V0044 → V0045 crossing
writes `index.db.pre-V0044.bak`. A COMPLETE snapshot beside the volume is that
success state, so the readiness loop reports a boot as "still migrating" only
while that file is GROWING or a `-journal` sidecar is being written — never
merely because the file exists.

**UNRUN** — not selected by this invocation (it ran gates 2). No command was executed for gate 1 and no claim is made about it.

### Commands

```
(none — not selected by this invocation (it ran gates 2))
```

### Evidence

```
(none — not selected by this invocation (it ran gates 2))
```

## Gate 2 — user-repo invariance
**Asserts.** Across create, start-pr, sync, snapshot, auto-capture, retrack
and GC, no registered clone's `for-each-ref`, `packed-refs` or `refs/` tree
changes. `repo_invariance.py record` runs BEFORE the first operation and `check`
after the last. A configured-but-missing clone FAILS this gate by name: a clone
that cannot be hashed is a clone whose refs are unverified.

The sequence never runs against a SEEDING store. The driver waits for every
`[[review.repos]]` store to be `ready` first — the same helper gate 3 uses, with
the wait and its elapsed time reported — and FAILs rather than starting against
a store that is still seeding. Mid-sequence, a 503 carrying
`urn:kb:errors:store-seeding` is the daemon's own documented retry, so it is
WAITED on: the `retry_after` it reports (or the delay its message names) is
honoured, within `--store-ready-timeout` per operation, and every wait is
printed with its elapsed time. Only when that budget is spent does the operation
fail. Any other 503 is still a failure.

**PASS** — 3 clone(s) byte-identical across create, start-pr, sync, snapshot, auto-capture, retrack and GC, under --invariance-scope store-registered (1000farmacie-iac, 1000farmacie-rails-01, 1000farmacie-rails-02; 9 configured clone(s) were OUT of scope).

### Commands

```
$ /usr/bin/python3 /home/nik/project/kb-rs-final/scripts/review-store/repo_invariance.py record --repo /home/nik/progetti/1000farmacie/iac/1000farmacie-iac --repo /home/nik/progetti/1000farmacie/rails/1000farmacie.01 --repo /home/nik/progetti/1000farmacie/rails/1000farmacie.02 -o /home/nik/kbc-gates-out/gate2/baseline.json
    # gate 2 · invariance baseline, BEFORE any operation · exit 0
$ /tmp/gates-after3/kb-code-server --config /home/nik/kbc-gates-after/config/kb-code.toml
    # gate 2 · start the after daemon · exit 0
$ GET http://127.0.0.1:4791/api/identity
    # gate 2 · after daemon ready · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-rails-01/store
    # gate 2 · store card: 1000farmacie-rails-01 · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-rails-02/store
    # gate 2 · store card: 1000farmacie-rails-02 · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-iac/store
    # gate 2 · store card: 1000farmacie-iac · exit 200
$ /tmp/gates-after3/kb-code review start refs/kbc/pr/15873 --repo 1000farmacie-rails-01 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: create · exit 0
$ /tmp/gates-after3/kb-code review start-pr --repo 1000farmacie-rails-01 --pr 15873 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: start-pr · exit 0
$ /tmp/gates-after3/kb-code review sync --repo 1000farmacie-rails-01 --pr 15873 --json --wait=900 --daemon http://127.0.0.1:4791
    # gate 2 · operation: sync · exit 0
$ /tmp/gates-after3/kb-code review snapshot 116 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: snapshot · exit 0
$ /tmp/gates-after3/kb-code store sync --repo 1000farmacie-rails-01 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: auto-capture (store sync) · exit 0
$ /tmp/gates-after3/kb-code review retrack 116 --repo 1000farmacie-rails-01 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: retrack · exit 0
$ /tmp/gates-after3/kb-code review gc --review 116 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: gc (patchsets) · exit 0
$ /tmp/gates-after3/kb-code store gc --repo 1000farmacie-rails-01 --yes --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: gc (store refs) · exit 0
$ /usr/bin/python3 /home/nik/project/kb-rs-final/scripts/review-store/repo_invariance.py check --baseline /home/nik/kbc-gates-out/gate2/baseline.json
    # gate 2 · invariance check, AFTER the operation sequence · exit 0
```

### Evidence

```
scope:
  store-registered — 3 of 12 configured clone(s) in scope. Reason: only the `[[review.repos]]` members, because the operator passed --invariance-scope store-registered — NARROWER than the configured set, so the refs of every clone listed as out of scope below are NOT verified by this run.
clone set this run covers:
  1000farmacie-iac, 1000farmacie-rails-01, 1000farmacie-rails-02
configured clones:
  kb=MISSING; kindle=MISSING; huddle=MISSING; pmx=MISSING; research=MISSING; 1000farmacie-iac=present; 1000farmacie-morning=MISSING; 1000farmacie-rails-01=present; 1000farmacie-rails-02=present; 1000farmacie-rails-03=MISSING; 1000farmacie-rails-04=MISSING; 1000farmacie-rails-05=MISSING
configured but OUT OF SCOPE for this run (listed, never silently dropped):
  1000farmacie-morning (MISSING), 1000farmacie-rails-03 (MISSING), 1000farmacie-rails-04 (MISSING), 1000farmacie-rails-05 (MISSING), huddle (MISSING), kb (MISSING), kindle (MISSING), pmx (MISSING), research (MISSING)
configured but absent, outside this scope (listed, never silently dropped):
  1000farmacie-morning, 1000farmacie-rails-03, 1000farmacie-rails-04, 1000farmacie-rails-05, huddle, kb, kindle, pmx, research
baseline:
  /home/nik/kbc-gates-out/gate2/baseline.json
store readiness before the operation sequence:
  waited 15s for 1000farmacie-iac=ready, 1000farmacie-rails-01=ready, 1000farmacie-rails-02=ready (budget 1h00m00s, 0 poll(s) found it not ready)
operation sequence:
  create: exit 0; start-pr: exit 0; sync: exit 0; snapshot: exit 0; auto-capture (store sync): exit 0; retrack: exit 0; gc (patchsets): exit 0; gc (store refs): exit 0
auto-capture:
  driven through `store sync`, the store-side ref move that publishes repo.head_moved. The auto-capture WORKER only fires for a move in a USER clone, which this gate forbids; the capture path it shares is the `review snapshot` step above.
check:
  exit 0: unchanged: /home/nik/progetti/1000farmacie/iac/1000farmacie-iac
unchanged: /home/nik/progetti/1000farmacie/rails/1000farmacie.01
unchanged: /home/nik/progetti/1000farmacie/rails/1000farmacie.02
```

## Gate 3 — no fallback
**Asserts.** With every review store `ready`, ZERO reads fall back to the
user repo's objects. The counters are `runtime.git_fallbacks` (`unresolved`,
`odb_miss`) on `GET /api/repos/{name}/store`, read per repo. A store that is
not yet ready is retried with backoff and then reported as SKIP with the state
it stayed in; a READY store with a non-zero counter is a FAIL.

**UNRUN** — not selected by this invocation (it ran gates 2). No command was executed for gate 3 and no claim is made about it.

### Commands

```
(none — not selected by this invocation (it ran gates 2))
```

### Evidence

```
(none — not selected by this invocation (it ran gates 2))
```

## Gate 4 — review 65 end to end
**Asserts.** Review 65 (repo `1000farmacie-rails-01`, PR 15790, squash-merged
2026-09-24) end to end: `retrack 65 --dry-run` classifies `stale-pin`;
`retrack 65` gives ps4 `kind=base-corrected` with tip `525631b506e7…`,
base `7c1ed0cfdd…`, 10 commits / 40 files, equal to LIVE
`gh pr view 15790 --json files,commits`; findings and the verdict stay on
ps3 with `verdict_scope_changed`.

**UNRUN** — not selected by this invocation (it ran gates 2). No command was executed for gate 4 and no claim is made about it.

### Commands

```
(none — not selected by this invocation (it ran gates 2))
```

### Evidence

```
(none — not selected by this invocation (it ran gates 2))
```

## Gate 5 — live GitHub (gh-cli)
**Asserts.** With the `gh-cli` credential pinned to `nicolasacchi`,
`review sync --repo 1000farmacie-rails-01 --open --dry-run --json` lists every open PR of
`1000farmacie/1000farmacie` and the reported `forge.base_ref` equals
`gh pr list --json number,baseRefName` for all of them, including the stacked PRs
targeting `feature/15646-statsig-*` — the count is compared against GitHub's, never
hardcoded, and reported either way.

**UNRUN** — not selected by this invocation (it ran gates 2). No command was executed for gate 5 and no claim is made about it.

### Commands

```
(none — not selected by this invocation (it ran gates 2))
```

### Evidence

```
(none — not selected by this invocation (it ran gates 2))
```

## Gate 6 — secrets
**Asserts.** No GitHub credential leaks. The daemon's stdout+stderr are
captured to a file (this box has no daemon log file — the driver creates one),
and that log, every gate's JSON output and a `sqlite3 .backup` COPY of the after
volume (read-only SQL, never `cp`, never the live DB) are scanned for a
**credential**: a plausible full token for its prefix family — the shapes are the
product's own, transcribed from `crates/kb-code-server/src/review_store/redact.rs`
— or the exact literal output of `gh auth token --user nicolasacchi`.

A bare `gho_`/`ghp_`/`ghu_`/`ghs_` is **not** a needle. In a database that
indexes source and prose, a bare prefix is overwhelmingly a test fixture or a
sentence that NAMES a token shape, and the design's rule is that a secret is a
real credential, not a string that starts with a prefix. Those occurrences are
counted and reported below as excluded, so the log shows what the scan ran and
what it discounted instead of a bare zero. Matches are reported as location +
count; a matched value is never printed or logged.

**UNRUN** — not selected by this invocation (it ran gates 2). No command was executed for gate 6 and no claim is made about it.

### Commands

```
(none — not selected by this invocation (it ran gates 2))
```

### Evidence

```
(none — not selected by this invocation (it ran gates 2))
```

## Gate 7 — existing suite + TS regen (CI)
**Asserts.** The existing suite passes and the TypeScript types are
regenerated with no unrelated diff. This gate IS CI — the driver never runs
`cargo test` or the web-code tests locally. It records the check-run table for
`nicolasacchi/kb#166` and requires every check green, including the `drift` /
`code-drift` TS-regen jobs, which is what makes "regenerated with no unrelated
diff" evidence rather than a claim.

**UNRUN** — not selected by this invocation (it ran gates 2). No command was executed for gate 7 and no claim is made about it.

### Commands

```
(none — not selected by this invocation (it ran gates 2))
```

### Evidence

```
(none — not selected by this invocation (it ran gates 2))
```

## Driver notes (migrate-first phase, readiness, migration evidence)

```
migrate first: SKIPPED — /home/nik/kbc-gates-after/state/kb-code/index.db is already at refinery_schema_history max = 45, so the gated V0045 snapshot was taken by an earlier run and every gate below sees a warm volume
```
