# BUILD-LOG — kb-code review store, Phase 1 acceptance (BUILD-BRIEF §3)

| | |
|---|---|
| generated | 2026-09-26T21:17:03+0200 |
| driver | `scripts/review-store/run_gates.py` (RS-U13) |
| mode | real run |
| before binary | `/tmp/gates-before-bin/kb-code-server` (sha256 7982f0ac88610420…) — commit `UNRECORDED` |
| after binary | `/tmp/gates-after-bin/kb-code-server` (sha256 f4a847757dad64a9…) — commit `UNRECORDED` |
| before volume | `/home/nik/kbc-gates-state` (V0044 input) |
| after volume | `/home/nik/kbc-gates-after` (migrated in place by the post-upgrade boot) |
| ports | before `4790`, after `4791` — the in-use set [4000, 4001, 4747] is never touched |
| readiness budget | after `2h00m00s`, before `2h00m00s` — the after budget covers a first boot that takes the gated V0045 pre-migration snapshot (a whole-volume `VACUUM INTO`) |
| pristine inputs | `/home/nik/kbc-gates-bundle-2026-09-24` (verified against `SHA256SUMS`) |
| worktree | bfa0f0c |

## Summary

| gate | asserts | verdict | one line |
|---|---|---|---|
| 1 | golden relocation | FAIL | step 'golden snapshot of the before volume' failed (exit 2): error: GET /api/reviews/48/comments?ps=1 -> Remote end closed connection without response |
| 2 | user-repo invariance | FAIL | operation(s) that did not complete: sync (exit 3: { |
| 3 | no fallback | PASS | every store is ready and both fallback counters are zero. |
| 4 | review 65 end to end | PASS | retrack 65: dry-run stale-pin, ps4 kind=base-corrected tip 525631b506e7… base 7c1ed0cfdd7d…, 10 commits / 40 files equal to live PR 15790; findings and verdict stayed on ps3. |
| 5 | live GitHub (gh-cli) | PASS | all 45 open PRs listed by `review sync --open --dry-run`, every forge.base_ref equal to `gh pr list`, including 2 stacked PR(s) on feature/15646-statsig-*. |
| 6 | secrets | PASS | no plausible full token and no literal `gh auth token` output in 3 artefact(s) or anywhere in the volume copy; 2 bare-prefix occurrence(s) seen and excluded as fixture/doc text (listed above), so this is a scan that ran rather than a bare zero. |
| 7 | existing suite + TS regen (CI) | PASS | all 15 check runs green on nicolasacchi/kb#166 (head rs/final), including the TS-regen job(s) drift (TS wire bindings), code-drift (kb-code TS wire bindings). No local suite was run. |

**Exit code 1** — NOT all seven gates PASS (not PASS: [1, 2]).

## Gate 1 — golden relocation
**Asserts.** Every existing review's files, per-file blob ids, diff stats,
comment/finding anchors and verdict patchset are IDENTICAL before and after the
V0044 -> V0045 upgrade, and the only differences are new envelope fields
(`base{…}`, `warnings[]`, `minted`, per-patchset `kind`/`base_tip_sha`) — each
one enumerated below, because "we allowed the new keys" is only a result if the
list is printed. Also surfaces the two facts that prove the migration really ran:
the gated pre-migration snapshot of this volume, and the V0044 binary's refusal to
open the migrated volume.

The snapshot is checked through the product's OWN receipt (`backup.marker` beside
the volume), not through a hardcoded file name: the gate asserts the claim — a
non-empty snapshot of THIS volume, taken while it was still at epoch 44, whose
recorded byte count still matches the file on disk — and prints whatever the
product named it. It is named for the volume's epoch at the time of the snapshot,
because that is the epoch a restore of it lands on, so a V0044 → V0045 crossing
writes `index.db.pre-V0044.bak`.

**FAIL** — step 'golden snapshot of the before volume' failed (exit 2): error: GET /api/reviews/48/comments?ps=1 -> Remote end closed connection without response

### Commands

```
$ /tmp/gates-before-bin/kb-code-server --config /home/nik/kbc-gates-state/config/kb-code.toml
    # gate 1 · start the before daemon · exit 0
$ GET http://127.0.0.1:4790/api/identity
    # gate 1 · before daemon ready · exit 200
$ /usr/bin/python3 /home/nik/project/kb-rs-final/scripts/review-store/review_snapshot.py snapshot --base http://127.0.0.1:4790 -o /tmp/gates-run-2/json/gate1-before.json --timeout 60.0
    # gate 1 · golden snapshot of the before volume · exit 2
$ SIGTERM 2900045
    # gate 1 · stop the before daemon · exit 0
```

### Evidence

```
bundle SHA256SUMS:
  6 file(s) verified; NO DRIFT
before volume:
  /home/nik/kbc-gates-state/state/kb-code/index.db · sha256 55299fec3273f855… · 5540667392 bytes
before volume epoch:
  refinery_schema_history max = 44; no gated pre-migration snapshot recorded beside this volume
after volume:
  /home/nik/kbc-gates-after/state/kb-code/index.db · sha256 846aa82009517d55… · 5540667392 bytes
after volume epoch:
  refinery_schema_history max = 45; gated pre-migration snapshot recorded by the product: index.db.pre-V0044.bak (5282185216 bytes, of volume epoch 44)
before config drift vs kb-code.toml.orig:
  kb_daemon: '<absent>' -> {'enabled': False}; server.addr: '127.0.0.1:4747' -> '127.0.0.1:4790'; transcripts: '<absent>' -> {'enabled': False}
after config drift vs kb-code.toml.orig:
  kb_daemon: '<absent>' -> {'enabled': False}; review: '<absent>' -> {'store': {'seed_on_boot': True, 'allow_inherited_credentials': False}, 'repos': [{'name': '1000farmacie-rails-01', 'credential': 'gh-cli', 'gh_user': 'nicolasacchi'}, {'name': '1000farmacie-rails-02', 'credential': 'gh-cli', 'gh_user': 'nicolasacchi'}, {'name': '1000farmacie-iac', 'credential': 'gh-cli', 'gh_user': 'nicolasacchi'}]}; server.addr: '127.0.0.1:4747' -> '127.0.0.1:4791'; transcripts: '<absent>' -> {'enabled': False}
before volume epoch (pre-boot):
  refinery_schema_history max = 44
```

## Gate 2 — user-repo invariance
**Asserts.** Across create, start-pr, sync, snapshot, auto-capture, retrack
and GC, no registered clone's `for-each-ref`, `packed-refs` or `refs/` tree
changes. `repo_invariance.py record` runs BEFORE the first operation and `check`
after the last. A configured-but-missing clone FAILS this gate by name: a clone
that cannot be hashed is a clone whose refs are unverified.

**FAIL** — operation(s) that did not complete: sync (exit 3: {
  "error": {
    "code": "urn:kb:errors:store-seeding",
    "hint": null,
    "message": "the review store for this repo is seeding; retry in 30s",
    "next": [
      [
        "kb-code",
        "); snapshot (exit 3: {
  "error": {
    "code": "urn:kb:errors:store-seeding",
    "hint": null,
    "message": "the review store for this repo is seeding; retry in 30s",
    "next": []
  },
  "ok": false
}); auto-capture (store sync) (exit 3: {
  "error": {
    "code": "store-seeding",
    "hint": "the store is seeding in the background; retry shortly",
    "message": "review store unavailable: store-seeding"
  },
  "ok": false
}); retrack (exit 3: {
  "error": {
    "code": "urn:kb:errors:store-seeding",
    "hint": null,
    "message": "the review store for this repo is seeding; retry in 30s",
    "next": []
  },
  "ok": false
}); gc (patchsets) (exit 1: Error: review gc failed (503 Service Unavailable): the review store for this repo is seeding; retry in 30s); gc (store refs) (exit 3: {
  "error": {
    "code": "store-seeding",
    "hint": "the store is seeding in the background; retry shortly",
    "message": "review store unavailable: store-seeding"
  },
  "ok": false
})

### Commands

```
$ /usr/bin/python3 /home/nik/project/kb-rs-final/scripts/review-store/repo_invariance.py record --repo /home/nik/progetti/1000farmacie/iac/1000farmacie-iac --repo /home/nik/progetti/1000farmacie/rails/1000farmacie.01 --repo /home/nik/progetti/1000farmacie/rails/1000farmacie.02 -o /tmp/gates-run-2/gate2/baseline.json
    # gate 2 · invariance baseline, BEFORE any operation · exit 0
$ /tmp/gates-after-bin/kb-code-server --config /home/nik/kbc-gates-after/config/kb-code.toml
    # gate 2 · start the after daemon · exit 0
$ GET http://127.0.0.1:4791/api/identity
    # gate 2 · after daemon ready · exit 200
$ /tmp/gates-after-bin/kb-code review start refs/kbc/pr/15873 --repo 1000farmacie-rails-01 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: create · exit 0
$ /tmp/gates-after-bin/kb-code review start-pr --repo 1000farmacie-rails-01 --pr 15873 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: start-pr · exit 0
$ /tmp/gates-after-bin/kb-code review sync --repo 1000farmacie-rails-01 --pr 15873 --json --wait=900 --daemon http://127.0.0.1:4791
    # gate 2 · operation: sync · exit 3
$ /tmp/gates-after-bin/kb-code review snapshot 115 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: snapshot · exit 3
$ /tmp/gates-after-bin/kb-code store sync --repo 1000farmacie-rails-01 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: auto-capture (store sync) · exit 3
$ /tmp/gates-after-bin/kb-code review retrack 115 --repo 1000farmacie-rails-01 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: retrack · exit 3
$ /tmp/gates-after-bin/kb-code review gc --review 115 --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: gc (patchsets) · exit 1
$ /tmp/gates-after-bin/kb-code store gc --repo 1000farmacie-rails-01 --yes --json --daemon http://127.0.0.1:4791
    # gate 2 · operation: gc (store refs) · exit 3
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
  /tmp/gates-run-2/gate2/baseline.json
operation sequence:
  create: exit 0; start-pr: exit 0; sync: exit 3; snapshot: exit 3; auto-capture (store sync): exit 3; retrack: exit 3; gc (patchsets): exit 1; gc (store refs): exit 3
auto-capture:
  driven through `store sync`, the store-side ref move that publishes repo.head_moved. The auto-capture WORKER only fires for a move in a USER clone, which this gate forbids; the capture path it shares is the `review snapshot` step above.
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
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-rails-01/store
    # gate 3 · store card: 1000farmacie-rails-01 · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-rails-02/store
    # gate 3 · store card: 1000farmacie-rails-02 · exit 200
$ GET http://127.0.0.1:4791/api/repos/1000farmacie-iac/store
    # gate 3 · store card: 1000farmacie-iac · exit 200
$ /tmp/gates-after-bin/kb-code review files 65 --json --daemon http://127.0.0.1:4791
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
store state:
  1000farmacie-rails-01=ready, 1000farmacie-rails-02=ready, 1000farmacie-iac=ready
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

**PASS** — retrack 65: dry-run stale-pin, ps4 kind=base-corrected tip 525631b506e7… base 7c1ed0cfdd7d…, 10 commits / 40 files equal to live PR 15790; findings and verdict stayed on ps3.

### Commands

```
$ /tmp/gates-after-bin/kb-code review retrack 65 --repo 1000farmacie-rails-01 --dry-run --json --daemon http://127.0.0.1:4791
    # gate 4 · retrack dry-run classification · exit 0
$ /tmp/gates-after-bin/kb-code review retrack 65 --repo 1000farmacie-rails-01 --json --daemon http://127.0.0.1:4791
    # gate 4 · retrack (applies) · exit 0
$ /tmp/gates-after-bin/kb-code review show 65 --json --daemon http://127.0.0.1:4791
    # gate 4 · review state after retrack · exit 0
$ gh pr view 15790 -R 1000farmacie/1000farmacie --json number,files,commits
    # gate 4 · live GitHub PR (ground truth) · exit 0
$ /tmp/gates-after-bin/kb-code review files 65 --ps 4 --json --daemon http://127.0.0.1:4791
    # gate 4 · changed files of ps4 · exit 0
$ /tmp/gates-after-bin/kb-code review findings list 65 --ps 3 --json --daemon http://127.0.0.1:4791
    # gate 4 · findings on ps3 · exit 0
$ /tmp/gates-after-bin/kb-code review findings list 65 --ps 4 --json --daemon http://127.0.0.1:4791
    # gate 4 · findings on ps4 · exit 0
```

### Evidence

```
retrack --dry-run class:
  stale-pin
retrack outcome:
  minted=True ps=4 kind=base-corrected class=stale-pin verdict_scope_changed=True
ps4:
  tip 525631b506e7… base 7c1ed0cfdd7d… commits 10
ps4 files:
  40
gh pr view 15790:
  10 commits / 40 files
verdict:
  state=comment ps=3 verdict_scope_changed=True
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
$ /tmp/gates-after-bin/kb-code review sync --repo 1000farmacie-rails-01 --open --dry-run --json --daemon http://127.0.0.1:4791
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
$ sqlite3 /home/nik/kbc-gates-after/state/kb-code/index.db ".backup '/tmp/gates-run-2/gate6/after-volume-copy.db'"
    # gate 6 · consistent COPY of the after volume (sqlite3 .backup, never cp) · exit 0
```

### Evidence

```
credential source:
  `gh auth token --user nicolasacchi` answered (40 chars; the value is never printed, logged or passed in argv) — its exact literal is a needle
volume copy:
  /tmp/gates-run-2/gate6/after-volume-copy.db (5540667392 bytes, via `sqlite3 .backup`)
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
  embedder-lint (ORT surface — clippy)=pass; drift (TS wire bindings)=pass; code-lint (kb-code + kb-lip clippy)=pass; gates-binary (kb-code-server + kb-code artifact)=pass; code-drift (kb-code TS wire bindings)=pass; e2e (Playwright iframe smoke)=pass; workspace-lint (fmt + clippy)=pass; embedder-test (ORT surface — tests)=pass; workspace-test (nextest + doctests + API route table)=pass; code-test (kb-code + kb-lip tests)=pass; code-spa (web-code build + theme lint + vitest)=pass; code-e2e (kb-code Search-Everywhere Playwright smoke)=pass; web (vitest + typecheck)=pass; supply-chain (cargo-deny — licenses + advisories)=pass; Check DCO sign-off=pass
TS regen jobs:
  drift (TS wire bindings)=pass; code-drift (kb-code TS wire bindings)=pass
local suites:
  NOT RUN here, by design: this driver never invokes cargo or npm. `cargo test` and the web-code tests are the CI jobs on nicolasacchi/kb#166.
```

## Driver notes (migrate-first phase, readiness, migration evidence)

```
migrate first: SKIPPED — /home/nik/kbc-gates-after/state/kb-code/index.db is already at refinery_schema_history max = 45, so the gated V0045 snapshot was taken by an earlier run and every gate below sees a warm volume
readiness: the after daemon became ready after 6m07s, of which 0.0s was spent inside the gated pre-migration snapshot (index.db.pre-V0044.bak at 5282185216 bytes)
```
