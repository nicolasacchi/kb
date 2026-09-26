# BUILD-LOG — kb-code review store, Phase 1 acceptance (BUILD-BRIEF §3)

| | |
|---|---|
| generated | 2026-09-26T18:08:22+0200 |
| driver | `scripts/review-store/run_gates.py` (RS-U13) |
| mode | real run |
| before binary | `/tmp/gates-before-bin/kb-code-server` (sha256 7982f0ac88610420…) — commit `UNRECORDED` |
| after binary | `/tmp/gates-after-bin/kb-code-server` (sha256 f4a847757dad64a9…) — commit `UNRECORDED` |
| before volume | `/home/nik/kbc-gates-state` (V0044 input) |
| after volume | `/home/nik/kbc-gates-after` (migrated in place by the post-upgrade boot) |
| ports | before `4790`, after `4791` — the in-use set [4000, 4001, 4747] is never touched |
| pristine inputs | `/home/nik/kbc-gates-bundle-2026-09-24` (verified against `SHA256SUMS`) |
| worktree | b437d3b |

## Summary

| gate | asserts | verdict | one line |
|---|---|---|---|
| 1 | golden relocation | FAIL | the before daemon was not ready within 300.0s (last: timed out) |
| 2 | user-repo invariance | FAIL | configured clone(s) this run must hash but that do not exist on this host: 1000farmacie-morning, 1000farmacie-rails-03, 1000farmacie-rails-04, 1000farmacie-rails-05, huddle, kb, kindle, pmx, research. A clone that cannot be hashed is a clone whose refs are unverified, so gate 2 cannot pass. Restore the clone, or re-run with --invariance-scope store-registered and say so here. |
| 3 | no fallback | FAIL | the after daemon was not ready within 300.0s (last: <urlopen error [Errno 111] Connection refused>) |
| 4 | review 65 end to end | FAIL | the after daemon was not ready within 300.0s (last: <urlopen error [Errno 111] Connection refused>) |
| 5 | live GitHub (gh-cli) | FAIL | the after daemon was not ready within 300.0s (last: <urlopen error [Errno 111] Connection refused>) |
| 6 | secrets | FAIL | 3 location(s) matched a secret pattern; locations and counts above, values never printed |
| 7 | existing suite + TS regen (CI) | FAIL | not every check is green: code-test (kb-code + kb-lip tests)=fail, code-lint (kb-code + kb-lip clippy)=fail |

**Exit code 1** — NOT all seven gates PASS (not PASS: [1, 2, 3, 4, 5, 6, 7]).

## Gate 1 — golden relocation
**Asserts.** Every existing review's files, per-file blob ids, diff stats,
comment/finding anchors and verdict patchset are IDENTICAL before and after the
V0044 -> V0045 upgrade, and the only differences are new envelope fields
(`base{…}`, `warnings[]`, `minted`, per-patchset `kind`/`base_tip_sha`) — each
one enumerated below, because "we allowed the new keys" is only a result if the
list is printed. Also surfaces the two facts that prove the migration really ran:
the gated `index.db.pre-V0045.bak` snapshot, and the V0044 binary's refusal to
open the migrated volume.

**FAIL** — the before daemon was not ready within 300.0s (last: timed out)

### Commands

```
$ /tmp/gates-before-bin/kb-code-server --config /home/nik/kbc-gates-state/config/kb-code.toml
    # gate 1 · start the before daemon · exit 0
```

### Evidence

```
bundle SHA256SUMS:
  6 file(s) verified; NO DRIFT
before volume:
  /home/nik/kbc-gates-state/state/kb-code/index.db · sha256 55299fec3273f855… · 5540667392 bytes
before volume epoch:
  refinery_schema_history max = 44; no pre-V0045.bak
after volume:
  /home/nik/kbc-gates-after/state/kb-code/index.db · sha256 846aa82009517d55… · 5540667392 bytes
after volume epoch:
  refinery_schema_history max = 44; no pre-V0045.bak
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

**FAIL** — configured clone(s) this run must hash but that do not exist on this host: 1000farmacie-morning, 1000farmacie-rails-03, 1000farmacie-rails-04, 1000farmacie-rails-05, huddle, kb, kindle, pmx, research. A clone that cannot be hashed is a clone whose refs are unverified, so gate 2 cannot pass. Restore the clone, or re-run with --invariance-scope store-registered and say so here.

### Commands

```
(no command was run)
```

### Evidence

```
scope:
  configured — 12 configured clone(s) in scope
configured clones:
  kb=MISSING; kindle=MISSING; huddle=MISSING; pmx=MISSING; research=MISSING; 1000farmacie-iac=present; 1000farmacie-morning=MISSING; 1000farmacie-rails-01=present; 1000farmacie-rails-02=present; 1000farmacie-rails-03=MISSING; 1000farmacie-rails-04=MISSING; 1000farmacie-rails-05=MISSING
```

## Gate 3 — no fallback
**Asserts.** With every review store `ready`, ZERO reads fall back to the
user repo's objects. The counters are `runtime.git_fallbacks` (`unresolved`,
`odb_miss`) on `GET /api/repos/{name}/store`, read per repo. A store that is
not yet ready is retried with backoff and then reported as SKIP with the state
it stayed in; a READY store with a non-zero counter is a FAIL.

**FAIL** — the after daemon was not ready within 300.0s (last: <urlopen error [Errno 111] Connection refused>)

### Commands

```
$ /tmp/gates-after-bin/kb-code-server --config /home/nik/kbc-gates-after/config/kb-code.toml
    # gate 3 · start the after daemon · exit 0
```

### Evidence

```
(none recorded)
```

## Gate 4 — review 65 end to end
**Asserts.** Review 65 (repo `1000farmacie-rails-01`, PR 15790, squash-merged
2026-09-24) end to end: `retrack 65 --dry-run` classifies `stale-pin`;
`retrack 65` gives ps4 `kind=base-corrected` with tip `525631b506e7…`,
base `7c1ed0cfdd…`, 10 commits / 40 files, equal to LIVE
`gh pr view 15790 --json files,commits`; findings and the verdict stay on
ps3 with `verdict_scope_changed`.

**FAIL** — the after daemon was not ready within 300.0s (last: <urlopen error [Errno 111] Connection refused>)

### Commands

```
$ /tmp/gates-after-bin/kb-code-server --config /home/nik/kbc-gates-after/config/kb-code.toml
    # gate 4 · start the after daemon · exit 0
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

**FAIL** — the after daemon was not ready within 300.0s (last: <urlopen error [Errno 111] Connection refused>)

### Commands

```
$ /tmp/gates-after-bin/kb-code-server --config /home/nik/kbc-gates-after/config/kb-code.toml
    # gate 5 · start the after daemon · exit 0
```

### Evidence

```
(none recorded)
```

## Gate 6 — secrets
**Asserts.** No GitHub credential leaks. The daemon's stdout+stderr are
captured to a file (this box has no daemon log file — the driver creates one),
and that log, every gate's JSON output and a `sqlite3 .backup` COPY of the after
volume (read-only SQL, never `cp`, never the live DB) are scanned for `gho_`,
`ghp_`, `ghu_`, `ghs_`, a token-shaped string and the literal output of
`gh auth token --user nicolasacchi`. Matches are reported as location + count; the
value is never printed or logged.

**FAIL** — 3 location(s) matched a secret pattern; locations and counts above, values never printed

### Commands

```
$ sqlite3 /home/nik/kbc-gates-after/state/kb-code/index.db ".backup '/tmp/gates-run-1/gate6/after-volume-copy.db'"
    # gate 6 · consistent COPY of the after volume (sqlite3 .backup, never cp) · exit 0
```

### Evidence

```
credential source:
  `gh auth token --user nicolasacchi` answered (40 chars; the value is never printed, logged or passed in argv) — its literal is one of the needles below
volume copy:
  /tmp/gates-run-1/gate6/after-volume-copy.db (5540667392 bytes, via `sqlite3 .backup`)
needles:
  gho_*, ghp_*, ghu_*, ghs_*, <token-shaped string>, <literal gh auth token output>
scanned:
  2 file(s) + the volume copy
MATCH (redacted):
  volume (read-only SQL): occurrences: 6 row(s) contain a token prefix or the literal token — 6 occurrence(s)
MATCH (redacted):
  volume (read-only SQL): call_sites: 8 row(s) contain a token prefix or the literal token — 8 occurrence(s)
MATCH (redacted):
  volume (read-only SQL): comments: 2 row(s) contain a token prefix or the literal token — 2 occurrence(s)
```

## Gate 7 — existing suite + TS regen (CI)
**Asserts.** The existing suite passes and the TypeScript types are
regenerated with no unrelated diff. This gate IS CI — the driver never runs
`cargo test` or the web-code tests locally. It records the check-run table for
`nicolasacchi/kb#166` and requires every check green, including the `drift` /
`code-drift` TS-regen jobs, which is what makes "regenerated with no unrelated
diff" evidence rather than a claim.

**FAIL** — not every check is green: code-test (kb-code + kb-lip tests)=fail, code-lint (kb-code + kb-lip clippy)=fail

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
  embedder-test (ORT surface — tests)=pass; code-test (kb-code + kb-lip tests)=fail; code-spa (web-code build + theme lint + vitest)=pass; gates-binary (kb-code-server + kb-code artifact)=pass; code-drift (kb-code TS wire bindings)=pass; supply-chain (cargo-deny — licenses + advisories)=pass; embedder-lint (ORT surface — clippy)=pass; code-e2e (kb-code Search-Everywhere Playwright smoke)=pass; e2e (Playwright iframe smoke)=pass; code-lint (kb-code + kb-lip clippy)=fail; workspace-lint (fmt + clippy)=pass; web (vitest + typecheck)=pass; workspace-test (nextest + doctests + API route table)=pass; drift (TS wire bindings)=pass; Check DCO sign-off=pass
TS regen jobs:
  code-drift (kb-code TS wire bindings)=pass; drift (TS wire bindings)=pass
```
