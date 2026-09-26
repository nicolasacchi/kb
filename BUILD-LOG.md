# BUILD-LOG — kb-code review store, Phase 1 acceptance (BUILD-BRIEF §3)

| | |
|---|---|
| generated | 2026-09-26T15:27:01+0200 |
| driver | `scripts/review-store/run_gates.py` (RS-U13) |
| mode | TEMPLATE — no gate has been run |
| before binary | `<V0044 kb-code-server from PR #167>` (NOT PRESENT — gate could not have run) — commit `UNRECORDED` |
| after binary | `<post-upgrade kb-code-server from PR #166>` (NOT PRESENT — gate could not have run) — commit `UNRECORDED` |
| before volume | `/home/nik/kbc-gates-state` (V0044 input) |
| after volume | `/home/nik/kbc-gates-after` (migrated in place by the post-upgrade boot) |
| ports | before `4790`, after `4791` — the in-use set [4000, 4001, 4747] is never touched |
| pristine inputs | `/home/nik/kbc-gates-bundle-2026-09-24` (verified against `SHA256SUMS`) |
| worktree | 55e69eb |

## Summary

| gate | asserts | verdict | one line |
|---|---|---|---|
| 1 | golden relocation | UNRUN | template — nothing has been run yet |
| 2 | user-repo invariance | UNRUN | template — nothing has been run yet |
| 3 | no fallback | UNRUN | template — nothing has been run yet |
| 4 | review 65 end to end | UNRUN | template — nothing has been run yet |
| 5 | live GitHub (gh-cli) | UNRUN | template — nothing has been run yet |
| 6 | secrets | UNRUN | template — nothing has been run yet |
| 7 | existing suite + TS regen (CI) | UNRUN | template — nothing has been run yet |

**Exit code n/a (template — nothing was run)** — TEMPLATE — no gate has been run; nothing here is a result.

## Gate 1 — golden relocation
**Asserts.** Every existing review's files, per-file blob ids, diff stats,
comment/finding anchors and verdict patchset are IDENTICAL before and after the
V0044 -> V0045 upgrade, and the only differences are new envelope fields
(`base{…}`, `warnings[]`, `minted`, per-patchset `kind`/`base_tip_sha`) — each
one enumerated below, because "we allowed the new keys" is only a result if the
list is printed. Also surfaces the two facts that prove the migration really ran:
the gated `index.db.pre-V0045.bak` snapshot, and the V0044 binary's refusal to
open the migrated volume.

**UNRUN** — this file is the TEMPLATE: no gate has been run yet. No command was executed for gate 1 and no claim is made about it.

### Commands

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

### Evidence

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

## Gate 2 — user-repo invariance
**Asserts.** Across create, start-pr, sync, snapshot, auto-capture, retrack
and GC, no registered clone's `for-each-ref`, `packed-refs` or `refs/` tree
changes. `repo_invariance.py record` runs BEFORE the first operation and `check`
after the last. A configured-but-missing clone FAILS this gate by name: a clone
that cannot be hashed is a clone whose refs are unverified.

**UNRUN** — this file is the TEMPLATE: no gate has been run yet. No command was executed for gate 2 and no claim is made about it.

### Commands

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

### Evidence

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

## Gate 3 — no fallback
**Asserts.** With every review store `ready`, ZERO reads fall back to the
user repo's objects. The counters are `runtime.git_fallbacks` (`unresolved`,
`odb_miss`) on `GET /api/repos/{name}/store`, read per repo. A store that is
not yet ready is retried with backoff and then reported as SKIP with the state
it stayed in; a READY store with a non-zero counter is a FAIL.

**UNRUN** — this file is the TEMPLATE: no gate has been run yet. No command was executed for gate 3 and no claim is made about it.

### Commands

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

### Evidence

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

## Gate 4 — review 65 end to end
**Asserts.** Review 65 (repo `1000farmacie-rails-01`, PR 15790, squash-merged
2026-09-24) end to end: `retrack 65 --dry-run` classifies `stale-pin`;
`retrack 65` gives ps4 `kind=base-corrected` with tip `525631b506e7…`,
base `7c1ed0cfdd…`, 10 commits / 40 files, equal to LIVE
`gh pr view 15790 --json files,commits`; findings and the verdict stay on
ps3 with `verdict_scope_changed`.

**UNRUN** — this file is the TEMPLATE: no gate has been run yet. No command was executed for gate 4 and no claim is made about it.

### Commands

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

### Evidence

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

## Gate 5 — live GitHub (gh-cli)
**Asserts.** With the `gh-cli` credential pinned to `nicolasacchi`,
`review sync --repo 1000farmacie-rails-01 --open --dry-run --json` lists every open PR of
`1000farmacie/1000farmacie` and the reported `forge.base_ref` equals
`gh pr list --json number,baseRefName` for all of them, including the stacked PRs
targeting `feature/15646-statsig-*` — the count is compared against GitHub's, never
hardcoded, and reported either way.

**UNRUN** — this file is the TEMPLATE: no gate has been run yet. No command was executed for gate 5 and no claim is made about it.

### Commands

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

### Evidence

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

## Gate 6 — secrets
**Asserts.** No GitHub credential leaks. The daemon's stdout+stderr are
captured to a file (this box has no daemon log file — the driver creates one),
and that log, every gate's JSON output and a `sqlite3 .backup` COPY of the after
volume (read-only SQL, never `cp`, never the live DB) are scanned for `gho_`,
`ghp_`, `ghu_`, `ghs_`, a token-shaped string and the literal output of
`gh auth token --user nicolasacchi`. Matches are reported as location + count; the
value is never printed or logged.

**UNRUN** — this file is the TEMPLATE: no gate has been run yet. No command was executed for gate 6 and no claim is made about it.

### Commands

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

### Evidence

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

## Gate 7 — existing suite + TS regen (CI)
**Asserts.** The existing suite passes and the TypeScript types are
regenerated with no unrelated diff. This gate IS CI — the driver never runs
`cargo test` or the web-code tests locally. It records the check-run table for
`nicolasacchi/kb#166` and requires every check green, including the `drift` /
`code-drift` TS-regen jobs, which is what makes "regenerated with no unrelated
diff" evidence rather than a claim.

**UNRUN** — this file is the TEMPLATE: no gate has been run yet. No command was executed for gate 7 and no claim is made about it.

### Commands

```
(none — this file is the TEMPLATE: no gate has been run yet)
```

### Evidence

```
(none — this file is the TEMPLATE: no gate has been run yet)
```
