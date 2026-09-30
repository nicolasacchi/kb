# Adversarial review — 2026-09-30

Twelve reviewers attacked this week's work: six on the changes committed in the
session that produced them (`omp`), six on already-merged code. Every finding below
was verified against the tree before being recorded. Fixed ones say so.

## Fixed in the same session

**F1 BLOCKER — `X-Forwarded-For` disabled the DNS-rebinding guard.** `host_guard`
derived "loopback peer" from `request_is_loopback`, which reads `X-Forwarded-For` when
the peer is a trusted hop. Correct for auth; fatal for the guard, because that header
is not forbidden to scripts — a rebound page sets it, `host_gate_applies` goes false,
and the hole reopens on the `KB_ALLOW_NO_AUTH=1` deployment the carve-out already
leaves open. Now reads the raw TCP peer (`peer_is_trusted`); test
`a_forged_x_forwarded_for_does_not_disable_the_guard` pins it. 50/50 middleware tests.

**F2 BLOCKER — the `e2e` job's Playwright failures were swallowed by `tee`.** The step
relied on pipefail; Actions' unspecified Linux default is `bash -e {0}`, which has
none, so a pipeline reports the LAST command's status. Measured: `bash -e` → 0,
`bash -eo pipefail` → 7. Every spec could fail and `ci` would still conclude
`success` — which is what `build-image.yml` gates an image publish on.

**F3 BLOCKER — DCO passed having checked nothing.** `for sha in $(git rev-list …)`
is a word-list; command substitution there is not covered by `set -e`, so an
unresolvable range produced zero commits and the job printed "All commits carry a
DCO sign-off." with exit 0. Now the range is resolved once and an unresolvable or
empty range is a hard failure. DCO also grepped the raw body (a pasted `git log`
excerpt satisfied it) and never checked identity (`--author=Real` +
`Signed-off-by: Impostor` passed). Now reads the real trailer block and requires it
to equal the author.

**F4 MAJOR — the `code-*` path filter could not see its own definition.** Neither
`.github/workflows/ci.yml`, `rust-toolchain.toml`, nor `scripts/review-store/` was
matched, so a PR whose only diff was ci.yml could neuter every code-* job and all
five would report success. All three added.

**F5 MAJOR — kb-code's production `HostPolicy::from_config` never stripped the port**,
so the one entry shape `docs/configuration.md:41` blesses produced a silent 403 on
every /api call with no boot warning. Both constructors now share
`normalize_host_label`. (A first attempt fixed only the test-only constructor —
which is exactly why this survived a review that looked closely at the wrong one.)

**F6 MAJOR — `BatchOp::SetMeta` skipped the owner gate the PATCH twin enforces** for
the same disclosure, so a non-owner could un-private someone else's note via
`POST …/apply`.

**F7 MAJOR — `PrReviewsOut` existed twice with different shapes.** `ts(export)` sat
on the client-side value while the wire body is `routes::PrReviewsResponse` (with
`schema` + `unavailable_reason`), and `gen-ts-code-check` cannot see hand-written
types. Export moved to the wire type. This surfaced a real type error the collision
had hidden: hand-written `unavailable_reason?: string` vs generated
`string | null`.

**F8 — the dependabot React-19 ignore missed `@types/react` and `@types/react-dom`**,
which are separate dependency names, so a weekly run would still open exactly the red
PR the block exists to prevent. Its "much larger piece of work" justification was
also overstated: the break surface is two `JSX.Element` sites.

## Status as of 2026-09-30

**All eleven are FIXED and merged** (PR #187, `fix: the eleven open findings from the
adversarial review`; follow-up `eb7646df` for three defects CI found in it). 13 of 14 CI
jobs green. The one failure, `e2e`, is PRE-EXISTING ON MAIN — proven by a control run
(`gh workflow run ci.yml --ref main`, run 36741375544) in which `e2e` was the only failing
job, with none of these changes present.

Two agents **overruled parts of the brief on evidence**, and were right:

- O9 as written said to admit `*.localhost`. It does not: that name's loopback-ness is a
  RESOLVER CONVENTION, not a DNS fact (musl, an older libc, or a `search` domain can
  resolve it to a public name the attacker owns), and an allowlist must not admit a name
  whose loopback-ness depends on who resolved it. The project's only `*.localhost` is the
  artifact-iframe host, which must stay refused.
- O6's first half (`gc_manifest` on the public->private transition) is a provable no-op:
  the note still references its own aids, so `gc_plan` keeps every one. Implementing it
  would delete the operator's own screenshot on a REVERSIBLE toggle. The serve-side check
  was done instead, which is the half that also covers blobs never re-gced.

Two more defects were found only because the new tests were run at all, and BOTH were in
the tests, not the fixes: the O4 fixture hand-joined paths onto `state` while `KbPaths`
resolves `<state>/<kb>/{.review,lance}`, and its `touch_newer` stamped the file at the
tarball's own second against a `mtime > since` comparison. The predicate was correct the
whole time — its own test was lying. That is the same failure class as the XFF bypass and
the port-strip bug: something asserted, and the assertion was not evidence.

## Originally open — needs a decision (all since fixed)

**O1 HIGH — the ETag is an existence oracle for private notes.** `etag_for` hashes the
UNFILTERED sidecar (`kb-core/src/review.rs:1701`), and the same token is inlined into
the `?cm=on` payload, which is served **unauthenticated** outside the /api nest
(`routes/artifact.rs:968`). So anyone who can resolve `<id>.artifacts.localhost` can
detect every private-note create and edit, forever, with a conditional GET — no
content needed. Fix: derive the ETag from the FILTERED bytes for public reads, keeping
the disk token for `If-Match` on writes.

**O2 HIGH — the scheduled backup still never copies off-host.** `run_remote_copy` has
one caller: the `kb backup` CLI verb. `[backup] schedule_hours` writes a local tarball,
logs success and emits `maintenance.backup.written`. `kb doctor` reports
`backup-age: PASS` because it only reads `<state>/exports/`. Contradicts
`self-host.md:667` ("Backups must leave the box"). Also: `{dest}` is rewritten by
`--all` to `{dest}/{tarball_basename}`, which two docs call verbatim; and
`schedule_hours` is documented in zero operator docs.

**O3 HIGH — `memory_recalls` is in none of the three lifecycle registries
invariant #2 mandates.** Not in `CASCADE_STEPS`, `SWEEP_TABLES`, or the
`cascade_relocate_doc` rekey transaction, so relocating a session silently drops its
entire recall ledger, and `cascade_delete_doc` leaks the rows permanently. The
`sessions_delete` written to prevent the second half has zero callers.

**O4 MEDIUM — the backup skip predicate watches one of four things the tarball
contains.** `write_kb_export` packs `index.db`, `lance/`, `.review/` and `slates/`, but
`should_skip_scheduled_backup` only stats the sqlite db, so `.review/` and `slates/`
edits are never backed up.

**O5 MEDIUM — single-comment `resolve` / `set_anchor` / `add_reply` mutate a private
note**, while the BATCH path deliberately skips notes ("a local agent could quietly
resolve the operator's private reminders"). Note ids are enumerable by design
(`/api/anchors/stale` returns them fleet-wide).

**O6 MEDIUM — flipping a comment to private does not un-publish its attachment
blobs.** `set_comment_meta` never calls `gc_manifest`, and `attachments::serve` has no
visibility check and serves with a one-year immutable cache header. The already-
published URL keeps working forever.

**O7 MEDIUM — the guard's LAYERING is tested only by a hand-built miniature.** No test
exercises `host_guard` through the real `build_router`; deleting the `/capture`
`route_layer`, or moving the layer under `auth_bearer`, fails nothing.

**O8 MEDIUM — `rust-toolchain.toml` is not read by CI.** The comments say "pin to
rust-toolchain.toml's version" but every job hardcodes `dtolnay/rust-toolchain@1.96.0`,
so the two can diverge silently.

**O9 LOW/MEDIUM — loopback-name spellings that used to work now 403** with no boot
warning: `localhost.` (trailing dot), `127.0.0.2`, `*.localhost` aliases.

**O10 LOW — `host_allowed(None) == true` is bypassable over HTTP/2**, where `:authority`
carries the name and no `Host` header is sent — so a proxy with an HTTP/2 upstream
would silently get no enforcement.

**O11 — the `file:line` anchors in docs/configuration.md are ~30 of 47 wrong.** A
prior round "fixed" 12 and broke 1. The real problem is that nothing checks them; a CI
gate extracting every `` `file.rs:N` `` and asserting `N <= len` would end the
recurrence.

## Confirmed sound (so it is not re-reviewed)

- **tree-sitter 0.27 migration**: behaviour-identical, proven two ways — sha256 of both
  pre-migration bodies matched, and a 51-input differential harness showed 0
  divergences. The commit message's "LanguageRef gaining a lifetime" claim is **wrong**
  (it already existed in 0.26.11); the other 0.27 breaks do not affect the crate.
- **kb-server Host guard, post-fix**: no documented deployment is bricked — the
  Dockerfile healthcheck is outside the /api nest, the mDNS LAN recipe is non-loopback
  (gate off by default + warning fires), and the reference Traefik/Authelia deploy is
  likewise.
- **Architecture invariants**: a full 35-invariant audit found zero violations,
  including the comment-tags feature's hard parts.
- **comment-tags read side**: every renderer, lister, indexer, counter and export is
  public-only; no full-text index of a note body exists.
- **The gate and `auth_bearer` sharing `request_is_loopback`** is why a configured
  token turns the XFF trick into a 401 rather than a bypass.

## Process note

Two of my own earlier "fixes" in this session were incomplete in ways only an
adversarial pass caught: the parser unification (which I described as closing a
divergence while the production constructor still diverged) and a security guard whose
stated invariant was false for the deployment class it most needed to cover. The
comment-writing habit that makes a codebase reviewable is the same habit that makes a
wrong claim survive — a comment asserting a property is not evidence of it.
