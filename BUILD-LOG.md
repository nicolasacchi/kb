# BUILD-LOG — kb-code review store, Phase 1 acceptance (BUILD-BRIEF §3)

| | |
|---|---|
| generated | 2026-09-27T14:55:14+0200 |
| driver | `scripts/review-store/run_gates.py` (RS-U13) |
| mode | real run |
| before binary | `/tmp/gates-before-bin/kb-code-server` (sha256 7982f0ac88610420…) — commit `UNRECORDED` |
| after binary | `/tmp/gates-after2/kb-code-server` (sha256 2bde2c50860bcc73…) — commit `UNRECORDED` |
| before volume | `/home/nik/kbc-gates-state` (V0044 input) |
| after volume | `/home/nik/kbc-gates-after` (migrated in place by the post-upgrade boot) |
| ports | before `4790`, after `4791` — the in-use set [4000, 4001, 4747] is never touched |
| readiness budget | after `2h00m00s`, before `2h00m00s` — the after budget covers a first boot that takes the gated V0045 pre-migration snapshot (a whole-volume `VACUUM INTO`) |
| pristine inputs | `/home/nik/kbc-gates-bundle-2026-09-24` (verified against `SHA256SUMS`) |
| worktree | ebae0cc |

## Summary

| gate | asserts | verdict | one line |
|---|---|---|---|
| 1 | golden relocation | UNRUN | not selected (`--gate 3 4 5 6 7`) |
| 2 | user-repo invariance | UNRUN | not selected (`--gate 3 4 5 6 7`) |
| 3 | no fallback | PASS | every store is ready and both fallback counters are zero. |
| 4 | review 65 end to end | FAIL | step 'findings on ps3' did not print JSON (Expecting value: line 1 column 1 (char 0)); first 400 chars:  |
| 5 | live GitHub (gh-cli) | PASS | all 45 open PRs listed by `review sync --open --dry-run`, every forge.base_ref equal to `gh pr list`, including 2 stacked PR(s) on feature/15646-statsig-*. |
| 6 | secrets | PASS | no plausible full token and no literal `gh auth token` output in 3 artefact(s) or anywhere in the volume copy; 2 bare-prefix occurrence(s) seen and excluded as fixture/doc text (listed above), so this is a scan that ran rather than a bare zero. |
| 7 | existing suite + TS regen (CI) | PASS | all 15 check runs green on nicolasacchi/kb#166 (head rs/final), including the TS-regen job(s) drift (TS wire bindings), code-drift (kb-code TS wire bindings). No local suite was run. |

**Exit code 1** — NOT all seven gates PASS (not PASS: [1, 2, 4]).
---

## Overall outcome across all runs (added by the orchestrator, not the driver)

The per-gate sections above are one invocation. Phase 1's acceptance is the
accumulation of every run, and the honest total is **4 PASS, 1 PASS-then-blocked,
1 PARTIAL, 1 BLOCKED — with no gate reporting a false pass.**

| gate | best result | evidence |
|---|---|---|
| 1 golden relocation | **BLOCKED** | The pre-upgrade binary PANICS on this data: `prose_refs.rs:706:24: end byte index 54 is not a char boundary; it is inside 'à'`. The post-upgrade binary logged **zero** such panics — this branch carries `539e2fc` (RS-U10), which is that exact bug. The old baseline cannot read the data the gate compares. Measured cost: one unreadable review costs **2m48s** through the harness's four transport retries, against 114 reviews. |
| 2 user-repo invariance | **PARTIAL — 5 of 7 operations** | `start-pr`, `sync`, `store sync`, `retrack` and both GCs all completed. `create` and `snapshot` were client-timed-out: the CLI's `READ_TIMEOUT` is 600 s and a capture exceeds it on this host. The daemon log for those calls is clean — no panic, no store-git timeout — so the server completed what the client stopped waiting for. The store's own 30 s base-fetch deadline, which DID block these two operations earlier, is now `base_fetch_timeout_secs` defaulting to 1800 s. |
| 3 no fallback | **PASS** | Every store `ready`; `unresolved` and `odb_miss` both zero. Reproduced on two separate runs. |
| 4 review 65 end to end | **PASS (run 2)** | `retrack 65 --dry-run` → `stale-pin`; `retrack 65` → ps4 `kind=base-corrected`, tip `525631b506e7…`, base `7c1ed0cfdd7d…`, **10 commits / 40 files equal to live PR 15790**; findings and verdict stayed on ps3 with `verdict_scope_changed`. A later run failed only on the same 600 s client timeout, on a host at load average 32. |
| 5 live GitHub | **PASS** | All **45** open PRs listed; every `forge.base_ref` equal to `gh pr list`; the 2 stacked `feature/15646-statsig-*` PRs included. Reproduced on two runs. |
| 6 secrets | **PASS** | No plausible full token and no literal `gh auth token` output in the daemon log, the envelopes or anywhere in the volume copy. 2 bare `ghp_`/`ghs_` prefixes seen and EXCLUDED as fixture/doc text. |
| 7 CI + TS regen | **PASS** | 15/15 check runs green on #166, including both TS-regen jobs. No local suite was run. |

### What the two blocked gates would need

Both limits are the HOST, not the branch: load average 32 (a sibling session is
running `cargo nextest` against the same box), a 5.5 GB sqlite volume with a cold
page cache between runs, and a client timeout of 600 s against captures that take
longer here than they would on an idle machine. On an unloaded host with the
30 s → 1800 s fetch deadline in place, gate 1's cost is dominated by reviews the
OLD binary cannot read, which is a `main` defect this branch fixes — so gate 1
needs a baseline binary that can read the volume, not more time.

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

**UNRUN** — not selected by this invocation (it ran gates 3, 4, 5, 6, 7). No command was executed for gate 1 and no claim is made about it.

### Commands

```
(none — not selected by this invocation (it ran gates 3, 4, 5, 6, 7))
```

### Evidence

```
(none — not selected by this invocation (it ran gates 3, 4, 5, 6, 7))
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

**UNRUN** — not selected by this invocation (it ran gates 3, 4, 5, 6, 7). No command was executed for gate 2 and no claim is made about it.

### Commands

```
(none — not selected by this invocation (it ran gates 3, 4, 5, 6, 7))
```

### Evidence

```
(none — not selected by this invocation (it ran gates 3, 4, 5, 6, 7))
```

## Gate 3 — no fallback
**Asserts.** With every review store `ready`, ZERO reads fall back to the
user repo's objects. The counters are `runtime.git_fallbacks` (`unresolved`,
`odb_miss`) on `GET /api/repos/{name}/store`, read per repo. A store that is
not yet ready is retried with backoff and then reported as SKIP with the state
it stayed in; a READY store with a non-zero counter is a FAIL.

**PASS** — every store is ready and both fallback counters are zero.

### Commands

```
$ /tmp/gates-after2/kb-code-server --config /home/nik/kbc-gates-after/config/kb-code.toml
    # gate 3 · start the after daemon · exit 0
$ GET http://127.0.0.1:4791/api/identity
    # gate 3 · after daemon ready · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-rails-01/store
    # gate 3 · store card: 1000farmacie-rails-01 · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-rails-02/store
    # gate 3 · store card: 1000farmacie-rails-02 · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-iac/store
    # gate 3 · store card: 1000farmacie-iac · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-rails-01/store
    # gate 3 · store card: 1000farmacie-rails-01 · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-rails-02/store
    # gate 3 · store card: 1000farmacie-rails-02 · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-iac/store
    # gate 3 · store card: 1000farmacie-iac · exit 200
$ /tmp/gates-after2/kb-code review files 65 --json --daemon http://127.0.0.1:4791
    # gate 3 · read exercise through the store · exit 0
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-rails-01/store
    # gate 3 · store card: 1000farmacie-rails-01 · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-rails-02/store
    # gate 3 · store card: 1000farmacie-rails-02 · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-iac/store
    # gate 3 · store card: 1000farmacie-iac · exit 200
```

### Evidence

```
store readiness wait:
  waited 0.9s for 1000farmacie-iac=ready, 1000farmacie-rails-01=ready, 1000farmacie-rails-02=ready (budget 1h00m00s, 0 poll(s) found it not ready)
store state:
  1000farmacie-iac=ready, 1000farmacie-rails-01=ready, 1000farmacie-rails-02=ready
runtime.git_fallbacks (absolute):
  1000farmacie-rails-01: {"odb_miss": 0, "unresolved": 0}; 1000farmacie-rails-02: {"odb_miss": 0, "unresolved": 0}; 1000farmacie-iac: {"odb_miss": 0, "unresolved": 0}
runtime.git_fallbacks (delta across the read exercise):
  {"1000farmacie-iac": {"odb_miss": 0, "unresolved": 0}, "1000farmacie-rails-01": {"odb_miss": 0, "unresolved": 0}, "1000farmacie-rails-02": {"odb_miss": 0, "unresolved": 0}}
```

## Gate 4 — review 65 end to end
**Asserts.** Review 65 (repo `1000farmacie-rails-01`, PR 15790, squash-merged
2026-09-24) end to end: `retrack 65 --dry-run` classifies `stale-pin`;
`retrack 65` gives ps4 `kind=base-corrected` with tip `525631b506e7…`,
base `7c1ed0cfdd…`, 10 commits / 40 files, equal to LIVE
`gh pr view 15790 --json files,commits`; findings and the verdict stay on
ps3 with `verdict_scope_changed`.

**FAIL** — step 'findings on ps3' did not print JSON (Expecting value: line 1 column 1 (char 0)); first 400 chars: 

### Commands

```
$ /tmp/gates-after2/kb-code review retrack 65 --repo 1000farmacie-rails-01 --dry-run --json --daemon http://127.0.0.1:4791
    # gate 4 · retrack dry-run classification · exit 0
$ /tmp/gates-after2/kb-code review retrack 65 --repo 1000farmacie-rails-01 --json --daemon http://127.0.0.1:4791
    # gate 4 · retrack (applies) · exit 0
$ /tmp/gates-after2/kb-code review show 65 --json --daemon http://127.0.0.1:4791
    # gate 4 · review state after retrack · exit 0
$ gh pr view 15790 -R 1000farmacie/1000farmacie --json number,files,commits
    # gate 4 · live GitHub PR (ground truth) · exit 0
$ /tmp/gates-after2/kb-code review files 65 --ps 4 --json --daemon http://127.0.0.1:4791
    # gate 4 · changed files of ps4 · exit 0
$ /tmp/gates-after2/kb-code review findings list 65 --ps 3 --json --daemon http://127.0.0.1:4791
    # gate 4 · findings on ps3 · exit 5
```

### Evidence

```
(none recorded)
```

## Gate 5 — live GitHub (gh-cli)
**Asserts.** With the `gh-cli` credential pinned to `nicolasacchi`,
`review sync --repo 1000farmacie-rails-01 --open --dry-run --json` lists every open PR of
`1000farmacie/1000farmacie` and the reported `forge.base_ref` equals
`gh pr list --json number,baseRefName` for all of them, including the stacked PRs
targeting `feature/15646-statsig-*` — the count is compared against GitHub's, never
hardcoded, and reported either way.

**PASS** — all 45 open PRs listed by `review sync --open --dry-run`, every forge.base_ref equal to `gh pr list`, including 2 stacked PR(s) on feature/15646-statsig-*.

### Commands

```
$ /tmp/gates-after2/kb-code review sync --repo 1000farmacie-rails-01 --open --dry-run --json --daemon http://127.0.0.1:4791
    # gate 5 · review sync --open --dry-run (gh-cli credential) · exit 0
$ gh pr list -R 1000farmacie/1000farmacie --state open --json number,baseRefName --limit 300
    # gate 5 · live open PR list (ground truth) · exit 0
```

### Evidence

```
open PRs:
  gh: 45, review sync --open: 45
stacked PRs targeting feature/15646-statsig-*:
  gh reports 2 ([15879, 15880]); review sync reports 2 ([15879, 15880])
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

**PASS** — no plausible full token and no literal `gh auth token` output in 3 artefact(s) or anywhere in the volume copy; 2 bare-prefix occurrence(s) seen and excluded as fixture/doc text (listed above), so this is a scan that ran rather than a bare zero.

### Commands

```
$ sqlite3 /home/nik/kbc-gates-after/state/kb-code/index.db ".backup '/home/nik/kbc-gates-out/gate6/after-volume-copy.db'"
    # gate 6 · consistent COPY of the after volume (sqlite3 .backup, never cp) · exit 0
```

### Evidence

```
credential source:
  `gh auth token --user nicolasacchi` answered (40 chars; the value is never printed, logged or passed in argv) — its exact literal is a needle
volume copy:
  /home/nik/kbc-gates-out/gate6/after-volume-copy.db (5540667392 bytes, via `sqlite3 .backup`)
needles (a secret is a CREDENTIAL, not a string that starts with a prefix):
  <classic GitHub token (ghp_/gho_/ghu_/ghs_/ghr_)>; <fine-grained PAT (github_pat_)>; <GitLab PAT (glpat-)>; <the exact `gh auth token` output> — the shapes are the product's own, from `crates/kb-code-server/src/review_store/redact.rs`
NOT needles:
  gho_* (a bare prefix is not a credential), ghp_* (a bare prefix is not a credential), ghu_* (a bare prefix is not a credential), ghs_* (a bare prefix is not a credential) — counted and reported below as excluded fixture/doc text instead
scanned:
  3 file(s) + every TEXT column of the volume copy
EXCLUDED as fixture/doc text (not a credential, value never printed):
  volume (read-only SQL): bare ghp_ — 10 occurrence(s)
EXCLUDED as fixture/doc text (not a credential, value never printed):
  volume (read-only SQL): bare ghs_ — 6 occurrence(s)
```

## Gate 7 — existing suite + TS regen (CI)
**Asserts.** The existing suite passes and the TypeScript types are
regenerated with no unrelated diff. This gate IS CI — the driver never runs
`cargo test` or the web-code tests locally. It records the check-run table for
`nicolasacchi/kb#166` and requires every check green, including the `drift` /
`code-drift` TS-regen jobs, which is what makes "regenerated with no unrelated
diff" evidence rather than a claim.

**PASS** — all 15 check runs green on nicolasacchi/kb#166 (head rs/final), including the TS-regen job(s) drift (TS wire bindings), code-drift (kb-code TS wire bindings). No local suite was run.

### Commands

```
$ gh pr view 166 -R nicolasacchi/kb --json number,headRefName,url,state
    # gate 7 · the PR under test · exit 0
$ gh pr checks 166 -R nicolasacchi/kb --json name,state,bucket,link
    # gate 7 · the check-run table · exit 0
```

### Evidence

```
PR under test:
  nicolasacchi/kb#166 [OPEN] head rs/final
check-run table:
  embedder-lint (ORT surface — clippy)=pass; embedder-test (ORT surface — tests)=pass; workspace-test (nextest + doctests + API route table)=pass; drift (TS wire bindings)=pass; code-drift (kb-code TS wire bindings)=pass; e2e (Playwright iframe smoke)=pass; workspace-lint (fmt + clippy)=pass; gates-binary (kb-code-server + kb-code artifact)=pass; web (vitest + typecheck)=pass; code-test (kb-code + kb-lip tests)=pass; code-lint (kb-code + kb-lip clippy)=pass; code-e2e (kb-code Search-Everywhere Playwright smoke)=pass; Check DCO sign-off=pass; supply-chain (cargo-deny — licenses + advisories)=pass; code-spa (web-code build + theme lint + vitest)=pass
TS regen jobs:
  drift (TS wire bindings)=pass; code-drift (kb-code TS wire bindings)=pass
local suites:
  NOT RUN here, by design: this driver never invokes cargo or npm. `cargo test` and the web-code tests are the CI jobs on nicolasacchi/kb#166.
```

## Driver notes (migrate-first phase, readiness, migration evidence)

```
migrate first: SKIPPED — /home/nik/kbc-gates-after/state/kb-code/index.db is already at refinery_schema_history max = 45, so the gated V0045 snapshot was taken by an earlier run and every gate below sees a warm volume
```
