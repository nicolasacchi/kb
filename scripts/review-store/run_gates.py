#!/usr/bin/env python3
"""run_gates.py — RS-U13 acceptance driver for BUILD-BRIEF §3.

ONE entry point that runs the seven Phase-1 acceptance gates against a
VOLUME COPY and a test daemon on a FREE PORT, and writes every command,
verdict and piece of evidence into `BUILD-LOG.md`. Stdlib only (no
third-party deps — BUILDER-RULES / README §9), Python 3.11+ (`tomllib`).

    USAGE

    Print the whole plan and execute nothing (this is how the driver is
    validated before any volume is touched):
        python3 run_gates.py --dry-run

    Run all seven gates:
        python3 run_gates.py \\
            --before-server /tmp/gates-bin-before/kb-code-server \\
            --after-server  /tmp/gates-bin-after/kb-code-server \\
            --before-root /home/nik/kbc-gates-state \\
            --after-root  /home/nik/kbc-gates-after \\
            --before-port 4790 --after-port 4791 \\
            --out /home/nik/kbc-gates-out

    Re-run ONE gate (gates 4 and 5 need a live `gh` and a warm store, so
    they are meant to be re-runnable alone):
        python3 run_gates.py --gate 5 ...same flags...

    Prove the five safety rails without touching anything:
        python3 run_gates.py --self-test

    Rewrite BUILD-LOG.md as the all-UNRUN template:
        python3 run_gates.py --render-template

MIGRATE FIRST

  A first boot of the post-upgrade daemon against a volume that has not
  crossed V0045 takes the gated pre-migration snapshot — a `VACUUM INTO` of
  the whole volume — which is ~95 minutes on a 5.5 GB volume, not seconds.
  So the driver pays that ONCE, up front, visibly, and every gate afterwards
  starts against a warm migrated volume:

    1. if the after volume is already at epoch >= 45, say so and skip;
    2. otherwise start the after daemon ONCE, wait for it to be genuinely
       ready (default 7200 s), report the epoch before/after, the snapshot
       file and the elapsed time, and stop it;
    3. only then start the gates.

  `--skip-migrate-first` turns step 2 off; `--ready-timeout` /
  `--before-ready-timeout` are the budgets (both default to 7200 s — the
  first real run found even the non-migrating pre-upgrade daemon not ready
  within 300 s on this 5.5 GB volume).
  Readiness distinguishes "still migrating" from "not coming up", and the
  migrating signal means it: a boot is reported as migrating while the
  snapshot file beside the volume is GROWING (its size moved since the last
  probe) or a `-journal` sidecar is being written — NOT merely because a
  `*.pre-V*.bak` file exists. A completed snapshot is the SUCCESS state for
  gate 1's migration proof, and this box produced a 5,282,185,216-byte `.bak`
  that sat unchanged for six minutes while the daemon booted perfectly
  normally; a presence-based signal read that as "still migrating" and the
  gate waited on a lie. Any readiness failure carries the elapsed time and
  the last line the daemon printed.

PER-REVIEW SNAPSHOT ISOLATION (gate 1)

  The pre-upgrade binary PANICS on part of this data — `prose_refs.rs` slices
  a string at a byte index inside `'à'`, the blocking-task wrapper re-panics
  and the connection drops — so `review_snapshot.py snapshot` (one document
  for every review, all-or-nothing) exits 2 and takes the whole comparison
  with it. The post-upgrade branch carries the fix
  (`539e2fc fix(kb-code): stop prose_refs splitting non-ASCII characters
  (RS-U10)`), so the failure IS the finding: the pre-upgrade binary cannot
  read data the post-upgrade one can.

  The driver therefore reads ONE REVIEW AT A TIME, in process, through the
  U0 harness's own per-review entry point, and records each failure with the
  review id, the HTTP path and the failure verbatim instead of aborting. It
  then compares every review readable on BOTH sides and states the outcome
  precisely ("N of M reviews compared; K were unreadable by the pre-upgrade
  binary"), with the panic text from the daemon log as the evidence. A run
  in which nothing could be compared is a FAIL, and a review the AFTER
  binary can no longer read is a FAIL. Nothing is dropped silently.

WHAT IT CALLS (it never reimplements them)

  * `review_snapshot.py` — the U0 golden harness (a), used two ways: its
    `diff` mode is called as a subprocess, and for the snapshot itself the
    driver imports it as a module and calls its own per-review read. Its
    `snapshot` CLI is ALL-OR-NOTHING (one review it cannot read aborts the
    whole document), which is exactly what gate 1 must not be, so the tool
    is NOT edited and NOT weakened — the driver isolates each review around
    it. If the harness ever stops exposing a per-review entry point, gate 1
    says so by name instead of quietly falling back to a weaker comparison.
  * `repo_invariance.py record` / `check`  — the U0 invariance harness (b).
  * `kb-code` (the agent CLI) and `gh` (read-only) for the live gates.
  * `kb-code-server` is STARTED by this driver, on a free port, with its
    stdout+stderr captured to a file (this box has no daemon log file, and
    gate 6 needs one).

THE GATES (BUILD-BRIEF §3)
  1. golden relocation      — the only permitted diffs are NEW envelope
                             fields; the driver enumerates the keys it
                             allowed, so "we allowed the new keys" is a
                             printed list, not a claim. Each review is read
                             in isolation, so one review the pre-upgrade
                             binary cannot read is a named, evidenced
                             finding rather than an aborted gate.
  2. user-repo invariance   — `record` BEFORE the whole operation sequence
                             (create, start-pr, sync, snapshot,
                             auto-capture, retrack, GC) and `check` after.
                             A configured-but-missing clone FAILS the gate
                             by name; it is never silently skipped. The
                             sequence never runs against a SEEDING store:
                             the driver waits for `ready` first, and a 503
                             `urn:kb:errors:store-seeding` mid-sequence is
                             the daemon's own documented retry — honoured
                             within the budget and logged with its elapsed
                             time.
  3. no fallback            — `runtime.git_fallbacks` on
                              `GET /api/repos/{name}/store` must be zero on
                              a READY store. Not-ready is a backoff+SKIP
                              with the state it stayed in; ready+non-zero
                              is a FAIL.
  4. review 65 end to end   — retrack dry-run class, ps4 kind/tip/base,
                              10 commits / 40 files vs LIVE `gh`, and
                              findings+verdict left on ps3.
  5. live GitHub `gh-cli`   — `review sync --open --dry-run` lists every
                              open PR and `forge.base_ref` equals
                              `gh pr list --json number,baseRefName`.
  6. secrets                — the same scan the product's own redactor
                             would do (`redact.rs`): a CREDENTIAL, i.e. a
                             plausible full token for its prefix family or
                             the literal `gh auth token` output. A bare
                             `gho_`/`ghp_`/`ghu_`/`ghs_` is NOT a needle —
                             it is counted and reported as discounted
                             fixture/doc text, so the log shows what the
                             scan ran and what it excluded rather than a
                             bare zero. Locations and counts are reported;
                             a matched VALUE never is.
  7. existing suite + TS    — THIS ONE IS CI. The driver never runs
                              `cargo`/`npm`; it records the check-run table
                              for a named PR and requires every check green,
                              including the `drift` / `code-drift` TS-regen
                              jobs, which is how "TS types regenerated with
                              no unrelated diff" is evidenced.

SAFETY RAILS (each reachable from `--self-test`, see RAIL SELF-TESTS below)

  R1 ports     — a port in the configured in-use set {4000, 4001, 4747} is
                 refused outright, and `ss -ltn` must show the port FREE
                 before a daemon is started on it.
  R2 paths     — every filesystem path the driver touches must be under one
                 of the two named environment roots (or the output dir /
                 the bundle); anything else aborts.
  R3 user git  — no git WRITE is ever run against a clone under
                 `/home/nik/progetti/`. Every git argv passes a guard that
                 refuses a mutating subcommand on such a path.
  R4 no token  — every argv passes a guard that refuses a GitHub-token-
                 shaped string, so a token can never reach `ps`/history.
  R5 inputs    — before either volume is touched, the pristine bundle is
                 verified against `SHA256SUMS`, the live volumes' digests
                 are recorded and compared with any previous run, and the
                 test configs are diffed against `kb-code.toml.orig`.

EXIT CODE

  0 only when all seven gates PASS. 1 when at least one gate is not PASS
  (FAIL, SKIP or UNRUN). 2 on a driver/rail abort. `--dry-run`,
  `--self-test` and `--render-template` exit 0 on success.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import inspect
import json
import os
import re
import signal
import subprocess
import sqlite3
import sys
import time
import tempfile
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Sequence


def git_subcommand_and_target(argv: Sequence[str]) -> tuple[str, str | None]:
    """(subcommand, repository path) out of a `git` argv. Option VALUES are
    skipped, so `git -C /path fetch origin` yields ("fetch", "/path") and not
    ("/path", …) — which is what a naive "first non-flag argument" returns,
    and why the R3 guard must not be that naive."""
    value_opts = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path", "--config-env"}
    path_opts = {"-C", "--git-dir", "--work-tree"}
    sub = ""
    target: str | None = None
    i = 1
    while i < len(argv):
        a = argv[i]
        if a in path_opts:
            if i + 1 < len(argv):
                target = argv[i + 1]
            i += 2
            continue
        if a in value_opts:
            i += 2
            continue
        if not a.startswith("-"):
            sub = a
            break
        i += 1
    return sub, target

# --------------------------------------------------------------------------
# constants
# --------------------------------------------------------------------------

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parents[1]
SNAPSHOT_TOOL = HERE / "review_snapshot.py"
INVARIANCE_TOOL = HERE / "repo_invariance.py"

# R1 — the configured in-use set. Never start a daemon here, never talk to
# a daemon here. (4000/4001 = prod kb, 4747 = live kb-code.)
FORBIDDEN_PORTS = (4000, 4001, 4747)

# R3 — the operator's real repositories.
USER_CLONE_PREFIX = Path("/home/nik/progetti")

# R2 — the LIVE kb / kb-code state root. No flag value can make it an allowed
# root: pointing --before-root/--after-root here is refused, because a wrong
# flag must not turn a read-only gate into a write on the real volume.
LIVE_STATE_PREFIX = Path.home() / ".local" / "state" / "kb"

# THE TOKEN SHAPES — transcribed from the product's own high-precision
# redactor, `crates/kb-code-server/src/review_store/redact.rs` (its `RULES`
# table). This repository has exactly ONE notion of "this looks like a GitHub
# token", and the driver borrows it rather than inventing a second one: the
# right alphabet AND a plausible length for the prefix family. `_` is
# deliberately NOT in the classic-token alphabet, which is what separates a
# credential from (a) the repo's own `ghp_TESTTOKEN_FAKE…sekrit` fixtures and
# (b) a sentence that merely NAMES a shape (`secrets — high-precision known
# token shapes (sk-…, ghp_…)`). The design's rule, which the redactor's own
# header states: a secret is a real credential, not a string that starts with
# a token prefix.
SECRET_SHAPES: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("classic GitHub token (ghp_/gho_/ghu_/ghs_/ghr_)", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{16,}")),
    ("fine-grained PAT (github_pat_)", re.compile(r"\bgithub_pat_[A-Za-z0-9_]{20,}")),
    ("GitLab PAT (glpat-)", re.compile(r"\bglpat-[A-Za-z0-9_\-]{16,}")),
)

# R4 — every shape above, matched against every argv this driver builds.
TOKEN_RE = re.compile("(?:" + "|".join(p.pattern for _, p in SECRET_SHAPES) + ")")

# Gate 6 — the BARE prefixes. These are NOT needles: a bare prefix is not a
# credential. They are counted only so the log can say what the scan
# discounted (a fixture string, a doc sentence) instead of printing a bare
# zero, and so an operator can see the run scanned at all.
SECRET_PREFIXES = ("gho_", "ghp_", "ghu_", "ghs_")

# --------------------------------------------------------------------------
# readiness (the V0044 -> V0045 first boot)
# --------------------------------------------------------------------------

# Crossing the V0045 epoch triggers the gated pre-migration snapshot
# (`backup::GATED_EPOCHS` = [40, 45]): a page-by-page `VACUUM INTO` of the whole
# volume, measured at ~57 MB/min on a loaded box — so ~95 minutes for a 5.5 GB
# first boot, after which boots are fast because the volume is already
# migrated. The post-upgrade daemon's default readiness budget must fit THAT,
# not an ordinary service start. 7200 s = 2 h, ~25% headroom over the measured
# first boot.
DEFAULT_AFTER_READY_TIMEOUT = 7200.0

# The PRE-upgrade daemon migrates nothing, so it was given a short budget of
# its own on the theory that it boots in ~85 s. The first real run disproved
# that: gate 1 failed with "the before daemon was not ready within 300.0s",
# i.e. on a 5.5 GB volume on this box the V0044 boot did NOT answer in five
# minutes either — it moves the same bytes, so it is the same order of work.
# The two budgets are therefore equal by default, kept as separate flags so an
# operator who knows their before-volume boots fast can tighten one of them.
DEFAULT_BEFORE_READY_TIMEOUT = 7200.0

# Lines in the daemon's own output that mean "this boot is doing the gated
# snapshot", transcribed from `backup.rs`:
#   "kb-code: took a pre-migration snapshot before crossing a gated schema epoch"
#   "kb-code: reusing the existing pre-migration snapshot"
#   "kb-code: refusing to snapshot before VACUUM: needs … has …"
MIGRATION_LOG_SIGNALS = (
    "pre-migration snapshot",
    "gated schema epoch",
    "snapshot before vacuum",
)

# How often the readiness loop looks for migration evidence, and how often it
# prints a heartbeat while a long migration is in flight. An operator should
# not have to stare at a silent terminal for 95 minutes.
READY_POLL_SECS = 0.5
MIGRATION_PROBE_SECS = 5.0
MIGRATION_HEARTBEAT_SECS = 60.0

# git subcommands that WRITE to a repository. Anything not in this set is
# treated as read-only by the R3 guard.
GIT_WRITE_SUBCOMMANDS = frozenset(
    {
        "add", "am", "apply", "branch", "checkout", "cherry-pick", "clean",
        "clone", "commit", "fetch", "gc", "merge", "mv", "pull", "push",
        "rebase", "reflog", "remote", "repack", "reset", "restore", "revert",
        "rm", "stash", "submodule", "switch", "tag", "update-index",
        "update-ref", "worktree",
    }
)

# Gate 1 — the ONLY envelope keys a V0044 -> V0045 relocation may add.
# `review_snapshot.py diff --allow-new-keys` excuses every added key without
# saying which, so the driver computes the added-key set itself and requires
# each one to match this list: the allowlist is enforced, not trusted.
ALLOWED_NEW_KEYS = ("base", "warnings", "minted", "kind", "base_tip_sha")

# Gate 2 — the daemon's OWN retry contract. While a review store is seeding,
# `review sync`/`store sync` answer HTTP 503 with this error code and the
# message "the review store for this repo is seeding; retry in 30s" plus a
# `next` suggestion. The CLI maps that documented, self-describing answer to
# exit 3 (conflict) — correctly, per its own docs. So a 503 carrying THIS
# code is a WAIT, not a failed operation; any other 503 — or this code with
# no delay the caller can honour — is still a failure.
STORE_SEEDING_CODE = "urn:kb:errors:store-seeding"
# The delay the product's own message names. Used ONLY when the envelope
# carries the code but neither a `retry_after` field nor a "retry in Ns"
# message — the code IS the contract, so the product's own constant is the
# faithful fallback, and every wait that uses it says so in the log.
STORE_SEEDING_FALLBACK_RETRY = 30.0

# Gate 1 — a daemon panic, quoted as evidence when the pre-upgrade binary
# cannot read a review. The bytes the driver quotes are the daemon's own
# words; the driver adds nothing to them.
PANIC_MARKERS = ("panicked at", "not a char boundary", "stack backtrace")

GATE_NAMES = {
    1: "golden relocation",
    2: "user-repo invariance",
    3: "no fallback",
    4: "review 65 end to end",
    5: "live GitHub (gh-cli)",
    6: "secrets",
    7: "existing suite + TS regen (CI)",
}

DEFAULT_REPO = "1000farmacie-rails-01"
DEFAULT_FORGE = "1000farmacie/1000farmacie"
DEFAULT_REVIEW = 65
DEFAULT_PR = 15790
DEFAULT_CI_REPO = "nicolasacchi/kb"
DEFAULT_CI_PR = 166
DEFAULT_GH_USER = "nicolasacchi"
STACKED_BASE_PREFIX = "feature/15646-statsig-"

EXIT_OK = 0
EXIT_GATES = 1
EXIT_ABORT = 2

REDACTED = "<REDACTED>"


class RailError(RuntimeError):
    """A safety rail refused. Aborts the whole driver with exit 2."""


class GateAbort(RuntimeError):
    """A gate could not complete. Recorded as FAIL with the reason."""


# --------------------------------------------------------------------------
# small helpers
# --------------------------------------------------------------------------


def sh_quote(argv: Sequence[str]) -> str:
    """Render an argv list for a human-readable log line. Display only —
    every real invocation goes through `subprocess.run([...])`."""
    out = []
    for a in argv:
        s = str(a)
        out.append(s if re.fullmatch(r"[\w@%+=:,./{}\[\]-]+", s) else json.dumps(s))
    return " ".join(out)


def sha256_file(path: Path, chunk: int = 1 << 20) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        while True:
            b = fh.read(chunk)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


def now_iso() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%S%z")


def _hms(seconds: float) -> str:
    """`95m03s` / `1h35m03s` / `12.3s` — a readiness budget and an elapsed time
    are unreadable as bare seconds once one of them is ninety minutes."""
    s = int(round(seconds))
    if s < 60:
        return f"{seconds:.1f}s" if seconds < 10 else f"{s}s"
    m, rem = divmod(s, 60)
    return f"{m}m{rem:02d}s" if m < 60 else f"{m // 60}h{m % 60:02d}m{rem:02d}s"


def listening_ports() -> set[int]:
    """Ports `ss -ltn` reports as LISTEN on this host."""
    proc = subprocess.run(["ss", "-ltn"], capture_output=True, text=True, check=False)
    ports: set[int] = set()
    for line in proc.stdout.splitlines():
        m = re.search(r":(\d+)\s", line)
        if m:
            ports.add(int(m.group(1)))
    return ports


def read_config(config: Path) -> dict:
    with open(config, "rb") as fh:
        return tomllib.load(fh)


def configured_repos(config: Path) -> list[tuple[str, str]]:
    return [(r["name"], r["path"]) for r in read_config(config).get("repos", [])]


def store_repos(config: Path) -> list[str]:
    """The `[[review.repos]]` members — the clones a review store is
    registered for. `[review.store]` is the sibling table of scalars."""
    return [r["name"] for r in read_config(config).get("review", {}).get("repos", [])]


def resolve_binaries(args: argparse.Namespace) -> tuple[Path, Path, Path, Path]:
    def cli_for(server: str) -> Path:
        return Path(server).parent / "kb-code"

    return (
        Path(args.before_server).resolve(),
        Path(args.after_server).resolve(),
        Path(args.before_cli).resolve() if args.before_cli else cli_for(args.before_server),
        Path(args.after_cli).resolve() if args.after_cli else cli_for(args.after_server),
    )


# --------------------------------------------------------------------------
# results
# --------------------------------------------------------------------------


@dataclass
class Step:
    """One command the driver ran — or would run, in --dry-run."""

    gate: int
    label: str
    argv: list[str]
    returncode: int | None = None
    planned: bool = False
    stdout: str = ""
    stderr: str = ""
    seconds: float = 0.0
    out_file: str | None = None

    def render(self) -> str:
        rc = "n/a" if self.planned else str(self.returncode)
        return f"$ {sh_quote(self.argv)}\n    # gate {self.gate} · {self.label} · exit {rc}"


@dataclass
class GateResult:
    number: int
    name: str
    status: str  # PASS | FAIL | SKIP | UNRUN
    reason: str
    evidence: list[tuple[str, str]] = field(default_factory=list)
    steps: list[Step] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return self.status == "PASS"


# --------------------------------------------------------------------------
# the run context: rails, execution, daemons
# --------------------------------------------------------------------------


class Ctx:
    """Everything a gate needs, plus the rails every action passes."""

    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.dry_run = bool(args.dry_run)
        self.out = Path(args.out).resolve()
        self.before_root = Path(args.before_root).resolve()
        self.after_root = Path(args.after_root).resolve()
        self.bundle = Path(args.bundle).resolve()
        self.before_port = int(args.before_port)
        self.after_port = int(args.after_port)
        self.log_path = Path(args.build_log).resolve()
        self.selected: list[int] = sorted(set(args.gate)) if args.gate else list(range(1, 8))
        self.template = False
        self.steps: list[Step] = []
        self.notes: list[str] = []
        self.daemons: dict[str, "Daemon"] = {}
        # The literal token, read ONCE into memory for gate 6's grep. It is
        # never printed, never written to the log, never put in argv.
        self._token: str | None = None
        self._token_loaded = False

    # -- R2 paths ------------------------------------------------------

    def allowed_roots(self) -> list[Path]:
        return [self.before_root, self.after_root, self.out, self.bundle]

    def check_path(self, path: str | Path, why: str) -> Path:
        p = Path(path).resolve()
        if p == LIVE_STATE_PREFIX or LIVE_STATE_PREFIX in p.parents:
            raise RailError(
                f"R2: refusing to {why} {p}: it is inside the LIVE kb state root "
                f"({LIVE_STATE_PREFIX}), which no --before-root/--after-root value can unlock"
            )
        for root in self.allowed_roots():
            if p == root or root in p.parents:
                return p
        raise RailError(
            f"R2: refusing to {why} {p}: it is not under any of "
            + ", ".join(str(r) for r in self.allowed_roots())
        )

    def check_clone_path(self, path: str | Path, name: str) -> Path:
        """A CONFIGURED clone: it must exist, and it is only ever READ (R3
        enforces that on every git argv). A user clone under
        /home/nik/progetti is deliberately admissible here — it is the very
        thing gate 2 hashes — while any other path outside the two environment
        roots is refused like any other out-of-scope path."""
        p = Path(path)
        if not p.is_dir():
            raise GateAbort(
                f"configured clone {name!r} points at {p}, which does not exist on this host"
            )
        resolved = p.resolve()
        if resolved == USER_CLONE_PREFIX or USER_CLONE_PREFIX in resolved.parents:
            return resolved
        return self.check_path(resolved, f"hash the clone of {name!r}")

    # -- R1 ports ------------------------------------------------------

    def check_port(self, port: int, why: str) -> None:
        if port in FORBIDDEN_PORTS:
            raise RailError(
                f"R1: refusing to {why} on port {port}: it is in the configured in-use "
                f"set {list(FORBIDDEN_PORTS)} (prod kb / live kb-code)"
            )
        busy = listening_ports()
        if port in busy:
            raise RailError(
                f"R1: refusing to {why} on port {port}: `ss -ltn` already reports it "
                "LISTENing. Stop that process, or pass a different port."
            )

    # -- R3 + R4 argv guards -------------------------------------------

    def guard_argv(self, argv: Sequence[str]) -> list[str]:
        argv = [str(a) for a in argv]
        for a in argv:
            if TOKEN_RE.search(a):
                raise RailError(
                    "R4: refusing to run a command whose argv contains a GitHub-token-"
                    f"shaped string: {sh_quote(argv)}"
                )
        prog = Path(argv[0]).name if argv else ""
        if prog == "git":
            sub, target = git_subcommand_and_target(argv)
            if sub in GIT_WRITE_SUBCOMMANDS and target is not None:
                t = Path(target).resolve()
                if t == USER_CLONE_PREFIX or USER_CLONE_PREFIX in t.parents:
                    raise RailError(
                        f"R3: refusing `git {sub}` against {t}: it is a user clone under "
                        f"{USER_CLONE_PREFIX}. The gates only ever READ these."
                    )
        return argv

    # -- execution -----------------------------------------------------

    def placeholder(self, real: str, name: str) -> str:
        """In --dry-run a value that is only known at run time is shown as a
        named placeholder, so the plan is complete without inventing data."""
        return name if self.dry_run else real

    def exec(
        self,
        gate: int,
        label: str,
        argv: Sequence[str],
        *,
        check: bool = True,
        timeout: float = 3600.0,
        out_file: str | None = None,
    ) -> Step:
        argv = self.guard_argv(argv)
        step = Step(gate=gate, label=label, argv=argv, out_file=out_file)
        self.steps.append(step)
        if self.dry_run:
            step.planned = True
            return step
        t0 = time.time()
        proc = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, check=False)
        step.seconds = time.time() - t0
        step.returncode = proc.returncode
        step.stdout = proc.stdout
        step.stderr = proc.stderr
        if check and proc.returncode != 0:
            raise GateAbort(
                f"step {label!r} failed (exit {proc.returncode}): "
                f"{self.redact((proc.stderr or proc.stdout).strip()[:600])}"
            )
        return step

    def exec_json(self, gate: int, label: str, argv: Sequence[str], **kw: Any) -> tuple[Step, Any]:
        step = self.exec(gate, label, argv, **kw)
        if self.dry_run:
            return step, None
        try:
            return step, json.loads(step.stdout)
        except json.JSONDecodeError as e:
            raise GateAbort(
                f"step {label!r} did not print JSON ({e}); first 400 chars: "
                f"{self.redact(step.stdout[:400])}"
            ) from e

    def http_get(
        self, gate: int, label: str, url: str, *, timeout: float = 120.0
    ) -> tuple[int, Any, str]:
        """One GET that REPORTS the status instead of raising: `(status, body,
        raw)`. A 503 is an answer from this daemon — the store-seeding one is
        a state, not a transport failure — so a caller that has to tell
        `ready` from `seeding` needs the status, not an exception. A
        connection that never produced a response still raises."""
        step = Step(gate=gate, label=label, argv=["GET", url])
        self.steps.append(step)
        if self.dry_run:
            step.planned = True
            return 200, {}, ""
        req = urllib.request.Request(url, headers={"Accept": "application/json"})
        tok = os.environ.get("KB_CODE_TOKEN")
        if tok:
            req.add_header("Authorization", f"Bearer {tok}")
        t0 = time.time()
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                status = resp.status
                raw = resp.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as e:
            status = e.code
            raw = e.read()[:600].decode("utf-8", "replace")
        except Exception as e:  # noqa: BLE001 — reported, never swallowed
            step.returncode = -1
            raise GateAbort(f"GET {url} failed: {e}") from e
        step.returncode = status
        step.seconds = time.time() - t0
        if status == 200:
            step.stdout = self.redact(raw[:4000])
        try:
            body = json.loads(raw) if raw.strip() else {}
        except json.JSONDecodeError:
            body = {}
        return status, body, raw

    def http_json(self, gate: int, label: str, url: str, *, timeout: float = 120.0) -> Any:
        status, body, raw = self.http_get(gate, label, url, timeout=timeout)
        if status != 200:
            raise GateAbort(f"GET {url} -> HTTP {status}: {self.redact(raw[:300])}")
        return body

    # -- gate 6 helpers ------------------------------------------------

    def gh_token(self) -> str | None:
        """`gh auth token --user <u>` read ONCE into memory. The command takes
        no token in argv; its OUTPUT is never printed, logged or persisted."""
        if self._token_loaded:
            return self._token
        self._token_loaded = True
        if self.dry_run:
            return None
        proc = subprocess.run(
            ["gh", "auth", "token", "--user", self.args.gh_user],
            capture_output=True, text=True, check=False,
        )
        tok = proc.stdout.strip() if proc.returncode == 0 else ""
        self._token = tok or None
        return self._token

    def redact(self, text: str) -> str:
        if self._token:
            text = text.replace(self._token, REDACTED)
        return TOKEN_RE.sub(REDACTED, text)

    # -- daemons -------------------------------------------------------

    def start_daemon(self, gate: int, name: str, binary: Path, root: Path, port: int) -> "Daemon":
        d = Daemon(self, gate, name, binary, root, port)
        # Registered BEFORE the boot, not after: a daemon that fails readiness
        # is still a live process, and `stop_daemon`/`stop_all` must be able to
        # reach it. Registering afterwards leaked the process on every failed
        # boot — which is exactly what a 95-minute migration budget makes
        # likely to hit.
        self.daemons[name] = d
        try:
            d.start()
        except BaseException:
            self.stop_daemon(name)
            raise
        return d

    def stop_daemon(self, name: str) -> None:
        d = self.daemons.pop(name, None)
        if d:
            d.stop()

    def stop_all(self) -> None:
        for name in list(self.daemons):
            self.stop_daemon(name)

    def ensure_after_daemon(self, gate: int) -> "Daemon":
        """Gates 2-6 share one long-lived after-daemon (gate 3 needs a warm
        store; gates 4/5 need a warm daemon)."""
        d = self.daemons.get("after")
        if d is not None and (self.dry_run or d.alive()):
            return d
        return self.start_daemon(
            gate, "after", Path(self.args.after_server), self.after_root, self.after_port
        )

    def migrate_first(self) -> None:
        """Pay the V0044 -> V0045 first-boot cost ONCE, before any gate runs.

        The post-upgrade daemon's first boot on an unmigrated volume takes the
        gated pre-migration snapshot (`backup::GATED_EPOCHS`): a `VACUUM INTO`
        of the whole volume, ~95 minutes at the ~57 MB/min this box sustains
        on a 5.5 GB volume. Left to the first gate that boots the daemon, that
        cost lands inside a 300 s readiness budget and is indistinguishable
        from a crash — which is exactly what the first real run showed: four
        gates FAIL for one benign cause.

        So it is done HERE, once, with the full budget and a report, and every
        gate afterwards starts against a warm migrated volume. Reuses the
        ordinary start/stop plumbing (`start_daemon`/`stop_daemon`) — there is
        no second daemon lifecycle here.

        A volume already at epoch >= 45 is not re-migrated: the one-off cost
        was paid by an earlier run, and saying so is the whole report.
        """
        needs_after = any(n in (1, 2, 3, 4, 5, 6) for n in self.selected)
        if not needs_after:
            self.notes.append(
                f"migrate first: SKIPPED — the selected gate(s) {self.selected} never boot the "
                "after daemon, so there is no volume to migrate"
            )
            return
        if self.dry_run:
            self.notes.append(
                "migrate first: would boot the post-upgrade daemon once (budget "
                f"{_hms(self.args.ready_timeout)}) to take the gated V0045 pre-migration snapshot, "
                "then stop it and start the gates against a warm volume"
            )
            self.steps.append(
                Step(
                    0,
                    f"migrate first: boot the after daemon once (budget "
                    f"{_hms(self.args.ready_timeout)}), then stop it",
                    [str(self.args.after_server), "--config",
                     str(self.after_root / "config" / "kb-code.toml")],
                    planned=True,
                )
            )
            return
        if self.args.skip_migrate_first:
            self.notes.append(
                "migrate first: SKIPPED by --skip-migrate-first — the first gate that boots the "
                "after daemon will pay the gated V0045 snapshot instead"
            )
            return

        after_db = self.after_root / "state" / "kb-code" / "index.db"
        epoch_before = read_volume_epoch(after_db)
        if isinstance(epoch_before, int) and epoch_before >= 45:
            self.notes.append(
                f"migrate first: SKIPPED — {after_db} is already at refinery_schema_history "
                f"max = {epoch_before}, so the gated V0045 snapshot was taken by an earlier run "
                "and every gate below sees a warm volume"
            )
            return

        print(
            f"→ migrate first: {after_db} is at epoch {epoch_before}; booting the post-upgrade "
            f"daemon ONCE to take the gated V0045 pre-migration snapshot (budget "
            f"{self.args.ready_timeout}s — a first boot is ~95 min, not seconds).",
            flush=True,
        )
        t0 = time.time()
        try:
            self.start_daemon(
                0, "after", Path(self.args.after_server), self.after_root, self.after_port
            )
        finally:
            elapsed = time.time() - t0
            self.stop_daemon("after")
            epoch_after = read_volume_epoch(after_db)
            snap = read_gated_snapshot(after_db)
            snapshots = stray_snapshots(after_db)
            # The note is written in a `finally`, so it runs on the failure path
            # too — and a note that says "migrated" after a boot that never
            # completed would be worse than no note at all.
            self.notes.append(
                "migrate first: the after volume is at epoch "
                f"{epoch_before} before the boot and {epoch_after} after it, "
                + (
                    f"migrated in {_hms(elapsed)}"
                    if epoch_after != epoch_before
                    else (
                        f"NOT migrated — the boot did not complete within {_hms(elapsed)}, so the "
                        "phase did not succeed (see the abort message and the daemon log)"
                    )
                )
                + "; pre-migration snapshot(s) beside it: "
                + (
                    ", ".join(
                        f"{p.name} ({p.stat().st_size} bytes"
                        + (f", the name the product's {BACKUP_MARKER} recorded" if snap and p == snap.path else "")
                        + ")"
                        for p in snapshots
                    )
                    or "(none — the gate-1 migration proof will say so)"
                )
            )

    # -- files ---------------------------------------------------------

    def art(self, *parts: str) -> Path:
        """Path to an artefact. In --dry-run the directory is NOT created:
        a plan that touches the filesystem is not a plan."""
        p = self.out.joinpath(*parts)
        if not self.dry_run:
            p.parent.mkdir(parents=True, exist_ok=True)
        return p

    # -- the plan ------------------------------------------------------

    def plan_text(self) -> str:
        lines: list[str] = []
        pre = [s for s in self.steps if s.gate == 0]
        if pre:
            # Gate 0 is the migrate-first phase, not a gate: it boots the
            # post-upgrade daemon once so the V0045 pre-migration snapshot is
            # paid BEFORE the gates, not inside a gate's readiness budget.
            lines.append("── migrate first (before any gate) " + "─" * 20)
            for s in pre:
                lines.append("   $ " + sh_quote(s.argv))
                lines.append(f"     · {s.label}")
            lines.append("")
        for n in range(1, 8):
            mine = [s for s in self.steps if s.gate == n]
            lines.append(f"── gate {n}: {GATE_NAMES[n]} " + "─" * max(0, 60 - len(GATE_NAMES[n])))
            if not mine:
                lines.append("   (no command — nothing to do, or the gate is not selected)")
            for s in mine:
                lines.append("   $ " + sh_quote(s.argv))
                lines.append(f"     · {s.label}")
            lines.append("")
        return "\n".join(lines)


class Daemon:
    """A test `kb-code-server` this driver owns: it starts it, waits for
    readiness, captures stdout+stderr to a file (this box has no daemon log
    file, and gate 6 needs one) and stops it on the way out."""

    def __init__(self, ctx: Ctx, gate: int, name: str, binary: Path, root: Path, port: int):
        self.ctx = ctx
        self.gate = gate
        self.name = name
        self.binary = binary
        self.root = root
        self.port = port
        self.proc: subprocess.Popen | None = None
        self.log: Path | None = None
        self._snapshot_sizes: dict[str, int] = {}
        # Consecutive probes on which a snapshot file's size did NOT move: the
        # snapshot is finished. Growth resets it.
        self._snapshot_stable: dict[str, int] = {}

    @property
    def base(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def alive(self) -> bool:
        return self.proc is not None and self.proc.poll() is None

    def start(self) -> None:
        ctx = self.ctx
        config = self.root / "config" / "kb-code.toml"
        argv = ctx.guard_argv([str(self.binary), "--config", str(config)])
        if ctx.dry_run:
            ctx.steps.append(Step(self.gate, f"start the {self.name} daemon", argv, planned=True))
            return
        ctx.check_path(self.root, f"boot the {self.name} daemon on")
        ctx.check_port(self.port, f"start the {self.name} daemon")
        if not self.binary.is_file():
            raise GateAbort(f"binary not found: {self.binary}")
        if not config.is_file():
            raise GateAbort(f"no test config at {config}")
        addr = str(read_config(config).get("server", {}).get("addr", ""))
        if addr != f"127.0.0.1:{self.port}":
            raise GateAbort(
                f"{config} binds {addr}, but this run was given port {self.port}. Fix the "
                "config (the driver never rewrites it) or pass the matching port."
            )
        env = {k: v for k, v in os.environ.items()}
        for k in ("KB_HOME", "KB_STATE_DIR", "KB_CONFIG_DIR", "KB_CACHE_DIR", "KB_CODE_GITHUB_TOKEN", "KB_GITHUB_TOKEN"):
            env.pop(k, None)
        env.update(
            {
                "KB_STATE_DIR": str(self.root / "state"),
                "KB_CONFIG_DIR": str(self.root / "config"),
                "KB_CACHE_DIR": str(ctx.out / "cache" / self.name),
                "RUST_LOG": ctx.args.rust_log,
            }
        )
        self.log = ctx.art("logs", f"{self.name}-daemon.log")
        with open(self.log, "w", encoding="utf-8") as fh:
            self.proc = subprocess.Popen(
                argv, stdout=fh, stderr=subprocess.STDOUT, env=env,
                cwd=str(ctx.out), start_new_session=True,
            )
        ctx.steps.append(
            Step(self.gate, f"start the {self.name} daemon", argv, returncode=0, out_file=str(self.log))
        )
        self.wait_ready()

    @property
    def ready_timeout(self) -> float:
        """The post-upgrade daemon gets the migration budget (a first boot is
        a whole-volume `VACUUM INTO`); the pre-upgrade one keeps a short
        budget, because it migrates nothing and a stuck boot must fail fast."""
        return float(
            self.ctx.args.before_ready_timeout
            if self.name == "before"
            else self.ctx.args.ready_timeout
        )

    @property
    def db_path(self) -> Path:
        return self.root / "state" / "kb-code" / "index.db"

    def last_output_line(self) -> str:
        """The last non-empty line the daemon printed, redacted. On any
        readiness failure this is what tells the operator whether the boot is
        deep in a `VACUUM INTO` or died on one line of error — the first real
        run's FAIL said neither."""
        if not self.log or not self.log.exists():
            return "(the daemon produced no output)"
        try:
            lines = [
                ln.strip()
                for ln in self.log.read_text(encoding="utf-8", errors="replace").splitlines()
                if ln.strip()
            ]
        except OSError as e:
            return f"(the daemon log could not be read: {e})"
        if not lines:
            return "(the daemon produced no output)"
        return self.ctx.redact(lines[-1])[:300]

    def migration_state(self) -> tuple[bool, str]:
        """Is this boot doing the gated pre-migration snapshot?

        Two independent signals, either of which is enough:

          * a line in the daemon's own output (the `backup.rs` messages), or
          * the `VACUUM INTO` destination itself — `index.db.pre-V<e>.bak`
            GROWING, or its `-journal` sidecar present — beside the volume.
            The sidecar is SQLite's rollback journal for the destination, so
            its presence means the copy is in flight right now.

        PRESENCE IS NOT THE SIGNAL, and that is the whole point. A finished
        snapshot sits beside the volume forever — it is the SUCCESS state for
        gate 1's migration proof, and the operator is meant to read it there.
        This box grew a 5,282,185,216-byte `index.db.pre-V0044.bak` that then
        sat unchanged for six minutes while the daemon booted perfectly
        normally, and a presence-based signal called that "still migrating",
        so the readiness loop reported a lie and waited on it. So a snapshot
        file counts as in-flight only while its size MOVES between two
        consecutive probes; a file whose size has not moved is reported as
        complete, and the first sighting of one is neither (growth is not yet
        established either way).

        The daemon's own output is the second signal and is deliberately the
        weaker one: `backup.rs` says "pre-migration snapshot" both when it
        takes a snapshot and when it reuses one, and that line then stays in
        the log for the rest of the boot. So it marks a boot as
        migration-RELATED and is reported as such — but only the file MOVING
        is what "in progress" means.

        Returns `(in_progress, human-readable detail)`.
        """
        active: list[str] = []
        detail: list[str] = []
        if self.log and self.log.exists():
            try:
                tail = self.log.read_text(encoding="utf-8", errors="replace")[-200_000:].lower()
            except OSError:
                tail = ""
            for signal_text in MIGRATION_LOG_SIGNALS:
                if signal_text in tail:
                    detail.append(f"daemon output mentions {signal_text!r}")
                    active.append(f"the daemon's own output mentions {signal_text!r}")
        try:
            for p in sorted(self.db_path.parent.glob("index.db.pre-V*.bak*")):
                try:
                    size = p.stat().st_size
                except OSError:
                    continue
                if p.name.endswith("-journal"):
                    detail.append(f"{p.name} present ({size} bytes) — the snapshot copy is in flight")
                    active.append(f"{p.name} is being written")
                    continue
                # "growing" needs a previous sample to compare against: the
                # first sighting of a snapshot is not evidence of growth, and
                # an unchanged size across probes is evidence it is FINISHED.
                previous = self._snapshot_sizes.get(p.name)
                self._snapshot_sizes[p.name] = size
                if previous is None:
                    detail.append(
                        f"{p.name} at {size} bytes (first sighting — growth not yet established)"
                    )
                elif size != previous:
                    detail.append(f"{p.name} at {size} bytes, up from {previous} — GROWING")
                    active.append(f"{p.name} grew {previous} -> {size} bytes")
                    self._snapshot_stable.pop(p.name, None)
                else:
                    stable = self._snapshot_stable.get(p.name, 0) + 1
                    self._snapshot_stable[p.name] = stable
                    detail.append(
                        f"{p.name} at {size} bytes, unchanged across {stable} consecutive "
                        "probe(s) — the snapshot is COMPLETE, not in flight"
                    )
        except OSError:
            pass
        return bool(active), "; ".join(detail)

    def wait_ready(self, timeout: float | None = None) -> None:
        """Poll `GET /api/identity` until the daemon answers.

        "Slow" and "dead" are different verdicts and get different words:
        a boot that is alive and visibly inside the gated pre-migration
        snapshot is reported as such and waited out, while a process that
        exited is reported with its exit code. Either way a failure carries
        the ELAPSED time and the LAST line the daemon printed, so the log
        distinguishes "hung in a 95-minute migration" from "exited
        immediately with an error".
        """
        timeout = float(timeout) if timeout is not None else self.ready_timeout
        started = time.time()
        deadline = started + timeout
        last = ""
        migrating: str | None = None
        migrating_since: float | None = None
        next_probe = 0.0
        next_beat = started + MIGRATION_HEARTBEAT_SECS
        while True:
            if self.proc is not None and self.proc.poll() is not None:
                raise GateAbort(
                    f"the {self.name} daemon exited (code {self.proc.returncode}) after "
                    f"{_hms(time.time() - started)} — it never answered GET /api/identity. "
                    f"Last line: {self.last_output_line()}"
                )
            try:
                with urllib.request.urlopen(self.base + "/api/identity", timeout=5) as r:
                    if r.status == 200:
                        self.ctx.steps.append(
                            Step(self.gate, f"{self.name} daemon ready",
                                 ["GET", self.base + "/api/identity"], returncode=200)
                        )
                        if migrating is not None:
                            self.ctx.notes.append(
                                f"readiness: the {self.name} daemon became ready after "
                                f"{_hms(time.time() - started)}, of which "
                                f"{_hms((migrating_since or time.time()) - started)} was spent "
                                f"inside the gated pre-migration snapshot ({migrating})"
                            )
                        return
            except Exception as e:  # noqa: BLE001 — not ready yet
                last = str(e)
            now = time.time()
            if now >= next_probe:
                next_probe = now + MIGRATION_PROBE_SECS
                active, detail = self.migration_state()
                if active:
                    migrating = detail
                    if migrating_since is None:
                        migrating_since = now
                    if now >= next_beat:
                        next_beat = now + MIGRATION_HEARTBEAT_SECS
                        print(
                            f"  ⏳ {self.name} daemon: {_hms(now - started)} elapsed, still "
                            f"migrating — {detail} · last: {self.last_output_line()}",
                            flush=True,
                        )
            if now >= deadline:
                break
            time.sleep(READY_POLL_SECS)
        elapsed = time.time() - started
        budget_flag = "--before-ready-timeout" if self.name == "before" else "--ready-timeout"
        if migrating is not None:
            raise GateAbort(
                f"the {self.name} daemon was still inside the gated pre-migration snapshot after "
                f"{_hms(elapsed)} and never answered GET /api/identity (budget {_hms(timeout)}): "
                f"{migrating}. The snapshot is a whole-volume `VACUUM INTO`; on a 5.5 GB volume "
                f"it is measured at tens of minutes, so this is a budget, not a crash. "
                f"Raise {budget_flag} (currently {timeout:.0f}s) or wait. Last line: "
                f"{self.last_output_line()}"
            )
        raise GateAbort(
            f"the {self.name} daemon was not ready within {_hms(timeout)} (waited "
            f"{_hms(elapsed)}) and no gated migration was in progress, so this is a boot "
            f"failure, not a slow one. Last probe: {last}. Last line: {self.last_output_line()}"
        )

    def stop(self) -> None:
        if self.proc is None:
            return
        try:
            os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            self.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
            self.proc.wait(timeout=30)
        self.ctx.steps.append(
            Step(self.gate, f"stop the {self.name} daemon", ["SIGTERM", str(self.proc.pid)], returncode=0)
        )
        self.proc = None


# --------------------------------------------------------------------------
# R5 — verify the inputs before touching either volume
# --------------------------------------------------------------------------


def require_sums(bundle: Path) -> None:
    if not (bundle / "SHA256SUMS").is_file():
        raise RailError(f"R5: no SHA256SUMS in {bundle} — refusing to touch a volume")


def read_volume_epoch(db: Path) -> int | str:
    """The volume's schema epoch, read-only, no `immutable=1` (a daemon may
    legitimately be running against it)."""
    try:
        out = subprocess.run(
            ["sqlite3", f"file:{db}?mode=ro", "select max(version) from refinery_schema_history;"],
            capture_output=True, text=True, check=False,
        )
        if out.returncode == 0 and out.stdout.strip().isdigit():
            return int(out.stdout.strip())
        return f"<unreadable: {out.stderr.strip()[:80]}>"
    except FileNotFoundError:
        return "<sqlite3 not installed>"


def config_drift_summary(orig: Path, cfg: Path) -> str:
    """One line per differing key — enough to show the gates' host overrides
    without dumping a whole file into the log."""
    try:
        a, b = read_config(orig), read_config(cfg)
    except Exception as e:  # noqa: BLE001
        return f"<unparseable: {e}>"
    diffs: list[str] = []

    def walk(x: Any, y: Any, path: str) -> None:
        if isinstance(x, dict) and isinstance(y, dict):
            for k in sorted(set(x) | set(y)):
                walk(x.get(k, "<absent>"), y.get(k, "<absent>"), f"{path}.{k}" if path else k)
        elif x != y:
            diffs.append(f"{path}: {x!r} -> {y!r}")

    walk(a, b, "")
    return "; ".join(diffs) if diffs else "identical to kb-code.toml.orig"


def verify_bundle(bundle: Path, max_bytes: int | None = None) -> tuple[int, list[str]]:
    """Check every entry of `SHA256SUMS`. `max_bytes` skips the multi-hundred-
    megabyte tarballs (the --self-test uses it so the rail can be proved in a
    second; a real run verifies everything). Returns (checked, problems)."""
    require_sums(bundle)
    sums = bundle / "SHA256SUMS"
    bad: list[str] = []
    checked = 0
    for line in sums.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        digest, _, name = line.partition("  ")
        target = bundle / name.strip()
        if not target.is_file():
            bad.append(f"{name}: MISSING from the bundle")
            continue
        if max_bytes is not None and target.stat().st_size > max_bytes:
            continue
        actual = sha256_file(target)
        checked += 1
        if actual != digest:
            bad.append(f"{name}: sha256 drift (expected {digest[:16]}…, got {actual[:16]}…)")
    return checked, bad


def verify_inputs(ctx: Ctx, hash_volumes: bool = True) -> list[tuple[str, str]]:
    """R5: verify the pristine bundle, fingerprint the live volumes, and diff
    the test configs against the bundle's original."""
    require_sums(ctx.bundle)
    rows: list[tuple[str, str]] = []
    if ctx.dry_run:
        rows.append(("bundle SHA256SUMS", f"would verify every file listed in {ctx.bundle}/SHA256SUMS"))
        rows.append(("volumes", f"would fingerprint {ctx.before_root} and {ctx.after_root} index.db"))
        return rows

    checked, bad = verify_bundle(ctx.bundle)
    rows.append(
        (
            "bundle SHA256SUMS",
            f"{checked} file(s) verified; " + ("NO DRIFT" if not bad else "DRIFT: " + "; ".join(bad)),
        )
    )
    if not hash_volumes:
        return rows

    baseline_file = ctx.out / "volumes.sha256"
    previous: dict[str, str] = {}
    if baseline_file.is_file():
        for line in baseline_file.read_text(encoding="utf-8").splitlines():
            d, _, n = line.partition("  ")
            if n:
                previous[n.strip()] = d
    fresh: list[str] = []
    for label, root in (("before", ctx.before_root), ("after", ctx.after_root)):
        db = root / "state" / "kb-code" / "index.db"
        if not db.is_file():
            raise RailError(f"R5: no volume at {db}")
        digest = sha256_file(db)
        fresh.append(f"{digest}  {db}")
        note = ""
        if str(db) in previous:
            note = " = the digest recorded by a previous run" if previous[str(db)] == digest else "  DRIFT vs a previous run"
        snap = read_gated_snapshot(db)
        rows.append((f"{label} volume", f"{db} · sha256 {digest[:16]}… · {db.stat().st_size} bytes{note}"))
        rows.append(
            (
                f"{label} volume epoch",
                f"refinery_schema_history max = {read_volume_epoch(db)}"
                + (
                    f"; gated pre-migration snapshot recorded by the product: {snap.path.name} "
                    f"({snap.bytes_on_disk} bytes, of volume epoch {snap.volume_epoch})"
                    if snap
                    else "; no gated pre-migration snapshot recorded beside this volume"
                ),
            )
        )
    baseline_file.parent.mkdir(parents=True, exist_ok=True)
    baseline_file.write_text("\n".join(fresh) + "\n", encoding="utf-8")

    orig = ctx.bundle / "kb-code.toml.orig"
    for label, root in (("before", ctx.before_root), ("after", ctx.after_root)):
        cfg = root / "config" / "kb-code.toml"
        if orig.is_file() and cfg.is_file():
            rows.append((f"{label} config drift vs kb-code.toml.orig", config_drift_summary(orig, cfg)))
    return rows


# `backup.rs` names a gated pre-migration snapshot for the volume's epoch AT THE
# TIME OF THE SNAPSHOT — the epoch a restore of that file lands on — so a
# V0044 -> V0045 crossing writes `index.db.pre-V0044.bak`, NOT
# `pre-V0045.bak`. That is a considered decision with a test pinning it
# (`crossing_v0045_from_v0044_writes_the_v0045_snapshot` asserts
# `ends_with("index.db.pre-V0044.bak")`): the file IS the rollback target for
# the volume as it stands, so naming it after the epoch it does not contain
# would be wrong.
#
# So the driver does not hardcode the name. `backup::take` records the name it
# actually used, beside the snapshot, in `<db dir>/backup.marker` — and that
# record is exactly the CLAIM gate 1 must check: "the gated-epoch snapshot of
# THIS volume was taken before the crossing". The name is read from the
# product, asserted against the product's own db_path/epoch/bytes fields, and
# then printed, because the operator is the one who has to recognise it.
BACKUP_MARKER = "backup.marker"


@dataclass
class GatedSnapshot:
    """The product's own record of the pre-migration snapshot beside a volume."""

    path: Path
    db_path: str
    volume_epoch: int | None
    recorded_bytes: int
    bytes_on_disk: int
    taken_at: int
    schema: str

    def problems_for(self, db: Path, expected_epoch: int) -> list[str]:
        """Every way this fails to be a snapshot OF `db` taken before the
        crossing of `expected_epoch`. An empty list means the claim holds."""
        bad: list[str] = []
        if not self.path.is_file():
            bad.append(f"{self.path} is recorded but is not on disk")
        elif self.bytes_on_disk == 0:
            bad.append(f"{self.path} is empty — a snapshot holding nothing restores nothing")
        if self.recorded_bytes != self.bytes_on_disk:
            bad.append(
                f"{self.path} is {self.bytes_on_disk} bytes on disk but the receipt recorded "
                f"{self.recorded_bytes}: the copy is truncated, or the file was replaced after "
                "the fact"
            )
        try:
            same_volume = Path(self.db_path).resolve() == db.resolve()
        except OSError:
            same_volume = False
        if not same_volume:
            bad.append(
                f"the receipt names {self.db_path} as its source, not {db}: this snapshot is of "
                "a different volume"
            )
        if self.volume_epoch != expected_epoch:
            bad.append(
                f"the receipt records the volume at epoch {self.volume_epoch} when the snapshot "
                f"was taken, expected {expected_epoch} (the pre-migration epoch)"
            )
        return bad

    def describe(self) -> str:
        where = (
            f"{self.path.name} ({self.bytes_on_disk} bytes, sha256 {sha256_file(self.path)[:16]}…)"
            if self.bytes_on_disk > 0
            else f"{self.path.name} ({self.bytes_on_disk} bytes on disk)"
        )
        return (
            f"{where}; the product's own receipt ({self.schema}) records it as taken from "
            f"{self.db_path} at epoch {self.volume_epoch}, {self.recorded_bytes} bytes — a "
            "V0044 volume's pre-migration snapshot is named for V0044 because that is the epoch "
            "a restore of it lands on"
        )


def read_gated_snapshot(db: Path) -> GatedSnapshot | None:
    """The snapshot the product itself recorded beside `db`, or None if it
    recorded none. Read-only: never writes, never deletes."""
    marker = db.parent / BACKUP_MARKER
    if not marker.is_file():
        return None
    try:
        doc = json.loads(marker.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    backup_path = str(doc.get("backup_path") or "")
    if not backup_path:
        return None
    try:
        on_disk = Path(backup_path).stat().st_size
    except OSError:
        on_disk = -1
    return GatedSnapshot(
        path=Path(backup_path),
        db_path=str(doc.get("db_path") or ""),
        volume_epoch=doc.get("volume_epoch"),
        recorded_bytes=int(doc.get("bytes") or 0),
        bytes_on_disk=on_disk,
        taken_at=int(doc.get("taken_at") or 0),
        schema=str(doc.get("schema") or "(no schema)"),
    )


def stray_snapshots(db: Path) -> list[Path]:
    """Every `index.db.pre-V*.bak*` beside `db` — snapshot files and any stray
    `-journal` sidecar. A PRISTINE volume must have none."""
    return sorted(p for p in db.parent.glob("index.db.pre-V*.bak*") if p.is_file())


# --------------------------------------------------------------------------
# gate 1 — golden relocation
# --------------------------------------------------------------------------


def added_key_paths(before: Any, after: Any, path: str = "") -> list[str]:
    """Every dict key present in `after` but not in `before`. The driver does
    this itself, because `review_snapshot.py diff --allow-new-keys` excuses
    added keys without ever saying which ones."""
    out: list[str] = []
    if isinstance(before, dict) and isinstance(after, dict):
        for k in sorted(set(after) - set(before)):
            out.append(f"{path}.{k}" if path else k)
        for k in sorted(set(before) & set(after)):
            out.extend(added_key_paths(before[k], after[k], f"{path}.{k}" if path else k))
    elif isinstance(before, list) and isinstance(after, list) and len(before) == len(after):
        for i, (b, a) in enumerate(zip(before, after)):
            out.extend(added_key_paths(b, a, f"{path}[{i}]"))
    return out


def key_is_allowed(dotted: str) -> bool:
    leaf = dotted.rsplit(".", 1)[-1].split("[", 1)[0]
    return leaf in ALLOWED_NEW_KEYS


def toml_dump_minimal(data: dict) -> str:
    """Enough TOML for a throwaway probe config (server + repos + the two
    off-switches), so the real test config is never rewritten."""
    lines: list[str] = []
    if data.get("server"):
        lines.append("[server]")
        for k, v in data["server"].items():
            lines.append(f"{k} = {json.dumps(v)}")
    for repo in data.get("repos", []):
        lines.append("")
        lines.append("[[repos]]")
        for k, v in repo.items():
            lines.append(f"{k} = {json.dumps(v)}")
    for key in ("kb_daemon", "transcripts"):
        if key in data:
            lines.append("")
            lines.append(f"[{key}]")
            for k, v in data[key].items():
                lines.append(f"{k} = {json.dumps(v)}")
    return "\n".join(lines) + "\n"


def probe_old_binary_refusal(ctx: Ctx, port: int) -> str:
    """U1's other half: the pre-upgrade binary must REFUSE a V0045 volume.
    Runs the before binary against a THROWAWAY config (the after config is
    never rewritten) and reports exactly what it said."""
    if ctx.dry_run:
        return "would boot the V0044 binary against the migrated V0045 volume and require a refusal"
    probe_dir = ctx.out / "probe-v0044"
    probe_dir.mkdir(parents=True, exist_ok=True)
    data = read_config(ctx.after_root / "config" / "kb-code.toml")
    data.setdefault("server", {})["addr"] = f"127.0.0.1:{port}"
    cfg_path = probe_dir / "kb-code.toml"
    cfg_path.write_text(toml_dump_minimal(data), encoding="utf-8")
    env = {k: v for k, v in os.environ.items()}
    for k in ("KB_HOME", "KB_STATE_DIR", "KB_CONFIG_DIR", "KB_CACHE_DIR", "KB_CODE_GITHUB_TOKEN"):
        env.pop(k, None)
    env.update(
        {
            "KB_STATE_DIR": str(ctx.after_root / "state"),
            "KB_CONFIG_DIR": str(probe_dir),
            "KB_CACHE_DIR": str(ctx.out / "cache" / "probe-v0044"),
        }
    )
    log = ctx.art("logs", "probe-v0044.log")
    argv = ctx.guard_argv([str(ctx.args.before_server), "--config", str(cfg_path)])
    ctx.steps.append(
        Step(1, "V0044 binary against the V0045 volume (must refuse)", argv, out_file=str(log))
    )
    with open(log, "w", encoding="utf-8") as fh:
        proc = subprocess.Popen(
            argv, stdout=fh, stderr=subprocess.STDOUT, env=env,
            cwd=str(ctx.out), start_new_session=True,
        )
        try:
            rc = proc.wait(timeout=min(180.0, float(ctx.args.ready_timeout)))
        except subprocess.TimeoutExpired:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            proc.wait(timeout=30)
            return "FAIL — the V0044 binary BOOTED a V0045 volume instead of refusing it"
    tail = ctx.redact(log.read_text(encoding="utf-8", errors="replace").strip()[-400:])
    if rc == 0:
        return f"FAIL — the V0044 binary exited 0 on a V0045 volume (tail: {tail})"
    return f"refused as designed (exit {rc}): {tail}"


# --------------------------------------------------------------------------
# gate 1 — per-review snapshot isolation
# --------------------------------------------------------------------------


def load_snapshot_tool() -> Any:
    """The U0 golden harness as a MODULE.

    `review_snapshot.py snapshot` builds ONE document for every review and
    aborts all of it on the first review it cannot read — which is exactly the
    case this gate has to survive, because the pre-upgrade binary panics on
    part of the data the gate compares. The harness already contains the
    per-review read that document is assembled from, so the driver calls THAT,
    one review at a time. The tool is not edited, not forked and not
    weakened: it is the same code path, entered per review instead of per
    store, and its `diff` mode still does the comparing.

    A harness that stops exposing that entry point is reported by name. The
    driver does NOT fall back to the all-or-nothing document, because that
    would be a quieter gate than the one the brief asks for.
    """
    if not SNAPSHOT_TOOL.is_file():
        raise GateAbort(f"the U0 golden harness is missing: {SNAPSHOT_TOOL}")
    spec = importlib.util.spec_from_file_location("rs_u0_review_snapshot", SNAPSHOT_TOOL)
    if spec is None or spec.loader is None:
        raise GateAbort(f"could not load the U0 golden harness at {SNAPSHOT_TOOL}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def tool_entry(mod: Any, name: str) -> Any:
    """One entry point of the U0 harness, or a named GateAbort. Used for the
    harness's per-review read and its diff; a missing one is a maintenance
    signal, never a silent downgrade."""
    fn = getattr(mod, name, None)
    if fn is None:
        raise GateAbort(
            f"{SNAPSHOT_TOOL.name} no longer exposes `{name}`, which is how the driver isolates "
            "each review. A `--review` filter on `snapshot` would serve the same purpose; until "
            "the harness offers one, gate 1 cannot read this data review by review, and it will "
            "not pretend otherwise by falling back to the all-or-nothing document."
        )
    return fn


def snapshot_token() -> str | None:
    """The bearer token the harness's own CLI would have used: `KB_CODE_TOKEN`
    from the environment, never argv (R4). The driver passes no `--token-file`
    today, so this is exactly what the subprocess call it replaces saw."""
    return (os.environ.get("KB_CODE_TOKEN") or "").strip() or None


# The canonical document's `schema` value, the harness's own constant. The
# harness is the source of truth (a sweep reads `SCHEMA` off it); this is only
# what the driver writes when it cannot see the harness.
SNAPSHOT_SCHEMA = "kbrs-golden-review-snapshot/1"


@dataclass
class ReviewRead:
    """One review's snapshot attempt: the document, or why there is none."""

    repo: str
    review_id: int
    doc: dict | None = None
    error: str | None = None

    @property
    def ok(self) -> bool:
        return self.doc is not None

    def http_path(self) -> str:
        """The path the harness named in the failure, when it named one."""
        m = re.search(r"GET (\S+?) ->", self.error or "")
        return m.group(1) if m else "(the harness did not name a path)"

    def describe(self) -> str:
        if self.ok:
            return f"review {self.review_id} (repo {self.repo}): read"
        return f"review {self.review_id} (repo {self.repo}): {self.http_path()} -> {self.error}"


@dataclass
class SideSnapshot:
    """One side's per-review sweep: what was read, what was not, and why."""

    side: str
    path: Path
    schema: str = SNAPSHOT_SCHEMA
    repos: list[str] = field(default_factory=list)
    total: int = 0
    reads: list[ReviewRead] = field(default_factory=list)

    @property
    def ok_ids(self) -> set[int]:
        return {r.review_id for r in self.reads if r.ok}

    @property
    def failures(self) -> list[ReviewRead]:
        return [r for r in self.reads if not r.ok]

    def document(self, only: Sequence[int] | None = None) -> dict:
        """The canonical document, exactly the shape the harness writes, for
        the reviews named in `only` (all of them when it is None)."""
        keep = None if only is None else set(only)
        docs = [
            r.doc
            for r in self.reads
            if r.ok and (keep is None or r.review_id in keep)
        ]
        docs.sort(key=lambda d: (d.get("repo") or "", d.get("id") or 0))
        return {
            "schema": self.schema,
            "repos": self.repos,
            "review_count": len(docs),
            "reviews": docs,
        }


def sweep_reviews(
    ctx: Ctx, g: int, side: str, daemon: "Daemon", out_path: Path, tool: Any
) -> SideSnapshot:
    """Read EVERY review on `daemon`, ONE AT A TIME, and write the canonical
    document for the ones that came back.

    A review that cannot be read is RECORDED — its id, the HTTP path, the
    failure verbatim — and the sweep continues. That is the whole difference
    from the harness's `snapshot` subcommand, which exits 2 on the first bad
    review and takes every other review down with it. The failure is the
    finding; the gate has to be able to say which review produced it and what
    the binary said while producing it.
    """
    if ctx.dry_run:
        ctx.steps.append(
            Step(
                g,
                f"golden snapshot of the {side} volume, one isolated read per review (in process, "
                f"through the U0 harness — the all-or-nothing CLI argv below is the entry point it "
                f"replaces per review), written to {out_path.name}",
                [
                    sys.executable, str(SNAPSHOT_TOOL), "snapshot",
                    "--base", daemon.base, "-o", str(out_path),
                    "--timeout", str(ctx.args.http_timeout),
                ],
                planned=True,
            )
        )
        return SideSnapshot(side=side, path=out_path)
    get_json = tool_entry(tool, "_get_json")
    snap_review = tool_entry(tool, "_snapshot_review")
    discover = tool_entry(tool, "_discover_repos")
    token = snapshot_token()
    timeout = float(ctx.args.http_timeout)
    try:
        repos = list(discover(daemon.base, token, timeout))
    except Exception as e:  # noqa: BLE001 — no repo list, so no review can be named
        raise GateAbort(
            f"the {side} daemon would not list its repos, so not one review could be named: {e}"
        ) from e
    snap = SideSnapshot(
        side=side, path=out_path, schema=str(getattr(tool, "SCHEMA", SNAPSHOT_SCHEMA)), repos=repos
    )
    for repo in repos:
        q = urllib.parse.urlencode({"repo": repo})
        try:
            body = get_json(daemon.base, f"/api/reviews?{q}", token, timeout)
        except Exception as e:  # noqa: BLE001
            raise GateAbort(
                f"the {side} daemon would not list the reviews of repo {repo}, so not one of them "
                f"could be read: {e}"
            ) from e
        for row in (body or {}).get("reviews", []):
            rid = int(row["id"])
            try:
                snap.reads.append(
                    ReviewRead(repo, rid, doc=snap_review(daemon.base, token, timeout, repo, rid))
                )
            except Exception as e:  # noqa: BLE001 — recorded verbatim, never swallowed
                snap.reads.append(ReviewRead(repo, rid, error=str(e)))
    snap.total = len(snap.reads)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        json.dumps(snap.document(), indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return snap


def compare_sides(before: SideSnapshot, after: SideSnapshot) -> tuple[list[int], list[str]]:
    """`(compared review ids, everything that makes this comparison unusable)`.

    The rules live here, together and pure, because each of them is a way this
    gate could otherwise pass for the wrong reason:

      * only a review readable on BOTH sides is comparable — for it,
        "identical" is a statement about the data, not about which binary
        survived;
      * a review the AFTER binary cannot read but the pre-upgrade one could is
        a REGRESSION: the relocation is only a pass if every review survives
        it, so that is a problem, not a footnote;
      * a review readable after and never enumerated before means the two
        sides are not snapshots of the same review set at all;
      * NO comparable review is a problem. A run in which nothing could be
        compared is a FAIL, never a pass with a caveat — which is what an
        all-or-nothing snapshot degenerates into when the data is hostile.

    A review the PRE-upgrade binary cannot read is NOT a problem: that is the
    finding, named and evidenced by the caller.
    """
    compared = sorted(before.ok_ids & after.ok_ids)
    problems: list[str] = []
    regressed = [r for r in after.failures if r.review_id in before.ok_ids]
    if regressed:
        problems.append(
            f"the post-upgrade binary could not read {len(regressed)} review(s) the pre-upgrade "
            "one could: "
            + "; ".join(r.describe() for r in regressed[:10])
            + ". The relocation is only a pass if every review survives it."
        )
    appeared = sorted(after.ok_ids - before.ok_ids - {r.review_id for r in before.failures})
    if appeared:
        problems.append(
            f"review(s) {appeared} are readable after the relocation but were never in the before "
            "snapshot, so the two sides are not snapshots of the same review set"
        )
    if not compared:
        problems.append(
            f"NOT ONE review could be read on both sides (before: {len(before.ok_ids)}/"
            f"{before.total} readable; after: {len(after.ok_ids)}/{after.total}). A relocation "
            "claim needs a review to compare, so this is a FAIL, not a quiet pass — the named "
            "failures above say why."
        )
    return compared, problems


def panic_evidence(ctx: Ctx, daemon: "Daemon", limit: int = 6) -> list[str]:
    """Panic lines from a daemon's own log, verbatim and redacted. When the
    pre-upgrade binary cannot read a review, the reason is in ITS log, and a
    gate that names the failure without quoting that sends the reader off to
    hunt for it."""
    log = daemon.log
    if not log or not log.exists():
        return []
    try:
        lines = log.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return []
    out: list[str] = []
    for line in lines:
        if any(marker in line for marker in PANIC_MARKERS):
            out.append(ctx.redact(line.strip())[:300])
            if len(out) >= limit:
                break
    return out


def gate_1(ctx: Ctx) -> GateResult:
    g = 1
    ev: list[tuple[str, str]] = []
    res = GateResult(g, GATE_NAMES[g], "FAIL", "")
    before_daemon = after_daemon = None
    try:
        ev += verify_inputs(ctx)
        before_db = ctx.before_root / "state" / "kb-code" / "index.db"
        after_db = ctx.after_root / "state" / "kb-code" / "index.db"
        # The snapshot's NAME is the product's to choose; the CLAIM that a
        # gated-epoch pre-migration snapshot of this volume was taken before
        # the crossing is gate 1's to check. Both are read from the receipt the
        # product writes beside the volume — see GatedSnapshot.
        PRE_MIGRATION_EPOCH = 44

        if not ctx.dry_run:
            epoch = read_volume_epoch(before_db)
            ev.append(("before volume epoch (pre-boot)", f"refinery_schema_history max = {epoch}"))
            if epoch != PRE_MIGRATION_EPOCH:
                raise GateAbort(
                    f"the before volume is at epoch {epoch}, expected {PRE_MIGRATION_EPOCH} "
                    "(V0044). It has probably been migrated already; re-decompress the bundle "
                    "volume."
                )
            # A pristine V0044 volume has taken no gated-epoch snapshot, so it
            # has no `index.db.pre-V*.bak` beside it at ANY epoch — the name is
            # the product's to choose, so the check is for the presence of
            # the class, not of one spelling.
            strays = stray_snapshots(before_db)
            if strays or (before_db.parent / BACKUP_MARKER).is_file():
                found = ", ".join(p.name for p in strays) or "(none)"
                raise GateAbort(
                    f"the 'before' volume already carries gated-epoch snapshot evidence beside it "
                    f"({found}"
                    + (f", plus {BACKUP_MARKER}" if (before_db.parent / BACKUP_MARKER).is_file() else "")
                    + f"). It has been snapshotted/migrated at least once and can no longer prove "
                    "the V0044 -> V0045 relocation. Re-decompress the bundle volume."
                )

        before_daemon = ctx.start_daemon(g, "before", Path(ctx.args.before_server), ctx.before_root, ctx.before_port)
        # One isolated read per review on BOTH sides. The pre-upgrade binary
        # panics on part of this data, and the harness's own all-or-nothing
        # `snapshot` would have turned that single review into "exit 2" and
        # taken the whole comparison with it.
        tool = None if ctx.dry_run else load_snapshot_tool()
        before_side = sweep_reviews(
            ctx, g, "before", before_daemon, ctx.art("json", "gate1-before.json"), tool
        )
        ev.append(
            (
                "before snapshot (one isolated read per review)",
                f"{len(before_side.ok_ids)} of {before_side.total} review(s) read into "
                f"{before_side.path}"
                + (
                    f"; {len(before_side.failures)} could NOT be read — named with the reason below"
                    if before_side.failures
                    else ""
                ),
            )
        )

        after_daemon = ctx.start_daemon(g, "after", Path(ctx.args.after_server), ctx.after_root, ctx.after_port)
        if not ctx.dry_run:
            snap = read_gated_snapshot(after_db)
            if snap is None:
                raise GateAbort(
                    f"the post-upgrade boot left no gated-epoch pre-migration snapshot recorded "
                    f"beside {after_db} (no {BACKUP_MARKER}, and "
                    f"{[p.name for p in stray_snapshots(after_db)] or 'no index.db.pre-V*.bak'}): "
                    "the backup gate did not fire, so the migration did not run as designed"
                )
            epoch = read_volume_epoch(after_db)
            ev.append(
                (
                    "migration proof — gated pre-migration snapshot",
                    f"{snap.describe()}; refinery_schema_history max = {epoch} after the boot",
                )
            )
            problems = snap.problems_for(after_db, PRE_MIGRATION_EPOCH)
            if problems:
                raise GateAbort(
                    "the gated-epoch pre-migration snapshot does not hold up as a snapshot of THIS "
                    "volume taken before the V0044 -> V0045 crossing: " + "; ".join(problems)
                )
            if epoch != 45:
                raise GateAbort(f"the after volume is at epoch {epoch} after boot, expected 45 (V0045)")
        else:
            ev.append(
                (
                    "migration proof",
                    f"would require a {BACKUP_MARKER} beside {after_db} naming a non-empty "
                    f"index.db.pre-V*.bak of this volume taken at epoch {PRE_MIGRATION_EPOCH}, and "
                    "the volume to be at epoch 45. The snapshot's NAME is whatever the product "
                    "recorded — a V0044 volume's pre-migration snapshot is named for V0044, the "
                    "epoch a restore of it lands on",
                )
            )
        after_side = sweep_reviews(
            ctx, g, "after", after_daemon, ctx.art("json", "gate1-after.json"), tool
        )
        ev.append(
            (
                "after snapshot (one isolated read per review)",
                f"{len(after_side.ok_ids)} of {after_side.total} review(s) read into {after_side.path}"
                + (
                    f"; {len(after_side.failures)} could NOT be read — named with the reason below"
                    if after_side.failures
                    else ""
                ),
            )
        )

        ev.append(("V0044 binary on a V0045 volume", probe_old_binary_refusal(ctx, ctx.before_port)))

        # The comparison is over the reviews BOTH sides could read: the only
        # set for which "identical" says something about the data instead of
        # about which binary survived. Everything outside it is named below,
        # with the reason, and never quietly dropped.
        compared_before = ctx.art("json", "gate1-compared-before.json")
        compared_after = ctx.art("json", "gate1-compared-after.json")
        compared, comparison_problems = (
            compare_sides(before_side, after_side) if not ctx.dry_run else ([], [])
        )
        if not ctx.dry_run:
            for side_snap, path in ((before_side, compared_before), (after_side, compared_after)):
                path.write_text(
                    json.dumps(side_snap.document(compared), indent=2, sort_keys=True, ensure_ascii=False)
                    + "\n",
                    encoding="utf-8",
                )
        step = ctx.exec(
            g,
            f"golden diff over the {len(compared) or 'compared'} review(s) (new envelope fields allowed)",
            [
                sys.executable, str(SNAPSHOT_TOOL), "diff",
                str(compared_before),
                str(compared_after),
                "--allow-new-keys",
            ],
            check=False,
        )
        ev.append(("diff --allow-new-keys", f"exit {step.returncode}: {step.stdout.strip() or '(no output)'}"))

        if not ctx.dry_run:
            doc_before = json.loads(compared_before.read_text(encoding="utf-8"))
            doc_after = json.loads(compared_after.read_text(encoding="utf-8"))
            unreadable_before = before_side.failures
            unreadable_after = after_side.failures
            ev.append(
                (
                    "reviews enumerated",
                    f"{before_side.total} before (across {len(before_side.repos)} repo(s)) / "
                    f"{after_side.total} after",
                )
            )
            ev.append(
                (
                    "reviews compared (readable on BOTH sides)",
                    f"{len(compared)} of {before_side.total}: {compared}"
                    if len(compared) <= 40
                    else f"{len(compared)} of {before_side.total}: {compared[:40]} … (+{len(compared) - 40} more)",
                )
            )
            for r in unreadable_before:
                ev.append((f"UNREADABLE by the pre-upgrade binary — review {r.review_id}", r.describe()))
            for r in unreadable_after:
                ev.append((f"UNREADABLE by the post-upgrade binary — review {r.review_id}", r.describe()))
            if unreadable_before:
                panics = panic_evidence(ctx, before_daemon)
                ev.append(
                    (
                        "why, from the pre-upgrade daemon's own log (verbatim)",
                        "\n".join(panics)
                        or "(no panic line in the log — the failures above are the whole evidence)",
                    )
                )
            if comparison_problems:
                raise GateAbort("; ".join(comparison_problems))
            added = added_key_paths(doc_before, doc_after)
            allowed = [p for p in added if key_is_allowed(p)]
            refused = [p for p in added if not key_is_allowed(p)]
            ev.append(
                (
                    "keys this run allowed to be new",
                    ", ".join(allowed) if allowed
                    else "(none — the snapshot document is byte-identical)",
                )
            )
            if refused:
                raise GateAbort(
                    f"unexplained NEW keys beyond {list(ALLOWED_NEW_KEYS)}: " + ", ".join(refused[:20])
                )
            if step.returncode != 0:
                raise GateAbort(
                    "review_snapshot.py diff reported differences beyond the allowed new keys "
                    f"(exit {step.returncode}):\n{ctx.redact(step.stdout[:2000])}"
                )
            # The harness's own diff, per review, so a difference is attributed
            # to the review that carries it rather than to a document index.
            diff_snapshots = tool_entry(tool, "diff_snapshots")
            by_id_before = {r.review_id: r.doc for r in before_side.reads if r.ok}
            by_id_after = {r.review_id: r.doc for r in after_side.reads if r.ok}
            differing = {
                rid: diff_snapshots(by_id_before[rid], by_id_after[rid], True)
                for rid in compared
            }
            differing = {k: v for k, v in differing.items() if v}
            if differing:
                raise GateAbort(
                    "review(s) differ beyond the allowed new envelope keys: "
                    + "; ".join(
                        f"review {rid}: " + " | ".join(lines[:3]) for rid, lines in sorted(differing.items())[:10]
                    )
                )
            ev.append(
                (
                    "review-by-review diff (harness `diff_snapshots`, new keys allowed)",
                    f"{len(compared)} review(s) compared individually, 0 with a difference beyond "
                    f"{list(ALLOWED_NEW_KEYS)}",
                )
            )
            res.status = "PASS"
            res.reason = (
                f"{len(compared)} of {before_side.total} reviews compared and relocated with no "
                "change to files, blob ids, anchors, findings or verdict; "
                + (
                    f"{len(unreadable_before)} of {before_side.total} were UNREADABLE by the "
                    "pre-upgrade binary ("
                    + ", ".join(str(r.review_id) for r in unreadable_before[:10])
                    + (", …" if len(unreadable_before) > 10 else "")
                    + ") — each named above with its HTTP path, the failure verbatim and the "
                    "daemon's own panic, which is the finding this gate is built to survive; "
                    if unreadable_before
                    else "every review was readable on both sides; "
                )
                + f"{len(allowed)} new envelope key(s) allowed and enumerated above; the gated "
                f"pre-migration snapshot ({snap.path.name if snap else 'MISSING'}) of this volume "
                f"was taken at epoch {PRE_MIGRATION_EPOCH} before the crossing, and the V0044 "
                "binary refused the migrated volume."
            )
        else:
            res.status = "UNRUN"
            res.reason = "dry run — planned only"
    except RailError:
        raise  # a safety rail refusal is not a gate verdict: it aborts the run
    except GateAbort as e:
        res.status = "FAIL"
        res.reason = str(e)
    finally:
        # Gate 1 owns the volume's first migration: it starts and stops BOTH
        # daemons, so gates 2-6 start from a migrated but idle volume.
        if after_daemon is not None and ctx.daemons.get("after") is after_daemon:
            ctx.daemons.pop("after", None)
        for d in (after_daemon, before_daemon):
            if d is not None:
                d.stop()
    res.evidence = ev
    res.steps = [s for s in ctx.steps if s.gate == g]
    return res


# --------------------------------------------------------------------------
# store readiness + the daemon's own retry contract (gate 2, and gate 3)
# --------------------------------------------------------------------------



def seeding_retry_seconds(text: str) -> float | None:
    """How long the daemon asked the caller to wait, or None if `text` carries
    no store-seeding meaning at all.

    The contract is the daemon's own error envelope: code
    `urn:kb:errors:store-seeding`, an optional `retry_after`, and the message
    "the review store for this repo is seeding; retry in 30s". `retry_after`
    wins; otherwise the delay the message names is read back out of it; and
    only if the envelope carries the code and neither is present does the
    product's own constant stand in — every wait that takes that branch says
    so in the log.

    None means "this is not a seeding answer": any other 503, or any other
    failure, and the caller must treat it as a failure. Parsed out of the raw
    text on purpose, so a CLI that pretty-prints or truncates the envelope
    still yields the code and the delay.
    """
    if STORE_SEEDING_CODE not in text:
        return None
    m = re.search(r'"retry_after"\s*:\s*"?(\d+(?:\.\d+)?)', text)
    if m:
        return float(m.group(1))
    m = re.search(r"retry in\s+(\d+(?:\.\d+)?)\s*s", text)
    if m:
        return float(m.group(1))
    return STORE_SEEDING_FALLBACK_RETRY


def seeding_sleep(
    returncode: int, text: str, spent: float, budget: float
) -> tuple[float | None, str]:
    """What to do about a failed operation, given the daemon's answer:
    `(seconds to wait, the line to log)`, or `(None, …)` when the operation is
    a failure and the retry loop must stop.

    A store-seeding 503 is a WAIT as long as the per-operation budget lasts;
    the wait is the daemon's own (`retry_after`, else the delay its message
    names) and is never allowed to run past the budget. A budget that is spent
    is the ONLY way a seeding answer becomes a failure, and it says so. Any
    other failure carries no note: it was never a wait.
    """
    said = seeding_retry_seconds(text)
    if said is None:
        return None, ""
    left = float(budget) - float(spent)
    if left <= 0:
        return None, (
            f"the store was still seeding after {_hms(spent)} — the {_hms(budget)} per-operation "
            "seeding budget is spent, so the operation is a failure now"
        )
    nap = min(said, left)
    return nap, (
        f"the daemon answered {STORE_SEEDING_CODE} (exit {returncode}) — the store is still "
        f"seeding, so this is a WAIT, not a failed operation; sleeping {_hms(nap)} as the daemon "
        f"asked ({_hms(spent)} of the {_hms(budget)} budget used)"
    )


def read_store_states(
    ctx: Ctx, g: int, daemon: "Daemon", repos: Sequence[str]
) -> dict[str, str]:
    """`GET /api/repos/{name}/store` per repo, as a STATE per repo.

    A 503 carrying the store-seeding code IS the state (`seeding`) — it is the
    daemon saying "not yet", not a failed read. Any other non-200, or a card
    with no state, raises: an unreadable store card must never read as
    `ready`.
    """
    states: dict[str, str] = {}
    for name in repos:
        status, body, raw = ctx.http_get(g, f"store card: {name}", f"{daemon.base}/api/repos/{name}/store")
        if status == 200:
            states[name] = str((body.get("store") or {}).get("state", "<no state>"))
        elif seeding_retry_seconds(raw) is not None:
            states[name] = "seeding"
        else:
            raise GateAbort(
                f"GET /api/repos/{name}/store -> HTTP {status}: {ctx.redact(raw[:300])}. The store's "
                "state could not be read, so this run cannot claim the store is ready."
            )
    return states


def wait_for_store_ready(
    ctx: Ctx, g: int, daemon: "Daemon", repos: Sequence[str], *, timeout: float
) -> tuple[dict[str, str], float, list[str]]:
    """Wait until EVERY repo's store is `ready`. Returns `(states, elapsed
    seconds, waits)`, where `waits` is one printable line per wait, each with
    the elapsed time at that moment — a wait an operator cannot see is a wait
    nobody can trust.

    ONE definition of readiness, used by gate 3 (whose fallback counters only
    mean something on a ready store) and by gate 2 (which must not run its
    operation sequence against a seeding store). The caller decides what a
    store that never got ready means: gate 3 SKIPs with the state it stayed
    in, gate 2 FAILs, because its operations would not have run.
    """
    started = time.time()
    backoff = 2.0
    states: dict[str, str] = {}
    waits: list[str] = []
    while True:
        states = read_store_states(ctx, g, daemon, repos)
        elapsed = time.time() - started
        if ctx.dry_run or all(s == "ready" for s in states.values()):
            return states, elapsed, waits
        if elapsed >= float(timeout):
            return states, elapsed, waits
        line = (
            f"{_hms(elapsed)} in: store(s) not ready yet — "
            + ", ".join(f"{k}={v}" for k, v in sorted(states.items()))
            + f"; waiting {_hms(backoff)} (budget {_hms(float(timeout))})"
        )
        waits.append(line)
        print(f"  ⏳ gate {g}: {line}", flush=True)
        time.sleep(backoff)
        backoff = min(backoff * 1.5, 30.0)


# --------------------------------------------------------------------------
# gate 2 — user-repo invariance
# --------------------------------------------------------------------------


def extract_review_id(stdout: str) -> str | None:
    try:
        doc = json.loads(stdout)
    except json.JSONDecodeError:
        return None
    data = doc.get("data", doc)
    for key in ("id", "review_id"):
        if isinstance(data.get(key), int):
            return str(data[key])
    return None


def gate_2(ctx: Ctx) -> GateResult:
    g = 2
    ev: list[tuple[str, str]] = []
    res = GateResult(g, GATE_NAMES[g], "FAIL", "")
    try:
        config = ctx.check_path(ctx.after_root / "config" / "kb-code.toml", "read the after config")
        all_repos = configured_repos(config)
        members = store_repos(config)
        in_scope = set(members) if ctx.args.invariance_scope == "store-registered" else {n for n, _ in all_repos}
        paths = dict(all_repos)
        missing_scope = sorted(n for n in in_scope if not Path(paths.get(n, "")).is_dir())
        missing_other = sorted(n for n, p in all_repos if n not in in_scope and not Path(p).is_dir())
        # The scope is part of the verdict, so it is printed WITH its reason
        # and with the exact clone set it covers. A PASS that does not say
        # which clones it hashed is not readable as a PASS.
        scope_reason = (
            "every `[[repos]]` entry in the after config — the default, and the only scope in "
            "which a configured-but-absent clone FAILS this gate"
            if ctx.args.invariance_scope == "configured"
            else "only the `[[review.repos]]` members, because the operator passed "
            "--invariance-scope store-registered — NARROWER than the configured set, so the "
            "refs of every clone listed as out of scope below are NOT verified by this run"
        )
        ev.append(
            (
                "scope",
                f"{ctx.args.invariance_scope} — {len(in_scope)} of {len(all_repos)} configured "
                f"clone(s) in scope. Reason: {scope_reason}.",
            )
        )
        ev.append(
            (
                "clone set this run covers",
                ", ".join(sorted(in_scope)) or "(none)",
            )
        )
        ev.append(
            (
                "configured clones",
                "; ".join(f"{n}={'present' if Path(p).is_dir() else 'MISSING'}" for n, p in all_repos),
            )
        )
        out_of_scope = sorted(n for n, _ in all_repos if n not in in_scope)
        if out_of_scope:
            ev.append(
                (
                    "configured but OUT OF SCOPE for this run (listed, never silently dropped)",
                    ", ".join(
                        f"{n} ({'present' if Path(dict(all_repos)[n]).is_dir() else 'MISSING'})"
                        for n in out_of_scope
                    ),
                )
            )
        if missing_other:
            ev.append(
                (
                    "configured but absent, outside this scope (listed, never silently dropped)",
                    ", ".join(missing_other),
                )
            )
        if not ctx.dry_run and missing_scope:
            raise GateAbort(
                "configured clone(s) this run must hash but that do not exist on this host: "
                f"{', '.join(missing_scope)}. A clone that cannot be hashed is a clone whose "
                "refs are unverified, so gate 2 cannot pass. Restore the clone, or re-run "
                "with --invariance-scope store-registered and say so here."
            )
        targets = [
            (n, ctx.check_clone_path(paths[n], n))
            for n in sorted(in_scope)
            if Path(paths.get(n, "")).is_dir()
        ]

        record_argv = [sys.executable, str(INVARIANCE_TOOL), "record"]
        for _, p in targets:
            record_argv += ["--repo", str(p)]
        record_argv += ["-o", str(ctx.art("gate2", "baseline.json"))]
        ctx.exec(g, "invariance baseline, BEFORE any operation", record_argv)
        ev.append(("baseline", str(ctx.art("gate2", "baseline.json"))))

        daemon = ctx.ensure_after_daemon(g)
        _, _, _, cli = resolve_binaries(ctx.args)
        repo = ctx.args.repo
        pr = ctx.args.ops_pr

        # The operation sequence must not run against a seeding store. The
        # daemon says so itself — 503 `urn:kb:errors:store-seeding`, "the
        # review store for this repo is seeding; retry in 30s" — and the second
        # real run proved the point by having `sync` refuse exactly that way.
        # So the store is waited out FIRST, with the same readiness helper
        # gate 3 uses (one definition, not two), and what it waited for and
        # for how long is reported whether or not it had to wait.
        store_names = store_repos(config)
        if not ctx.dry_run and not store_names:
            ev.append(
                (
                    "store readiness before the operation sequence",
                    "the after config registers no `[[review.repos]]` store, so there is no store "
                    "that could be seeding",
                )
            )
        elif store_names:
            states, waited, waits = wait_for_store_ready(
                ctx, g, daemon, store_names, timeout=float(ctx.args.store_ready_timeout)
            )
            ev.append(
                (
                    "store readiness before the operation sequence",
                    f"waited {_hms(waited)} for "
                    + ", ".join(f"{k}={v}" for k, v in sorted(states.items()))
                    + f" (budget {_hms(float(ctx.args.store_ready_timeout))}, "
                    f"{len(waits)} poll(s) found it not ready)",
                )
            )
            for line in waits:
                ev.append(("store wait", line))
                ctx.notes.append(f"gate 2 store readiness: {line}")
            if not ctx.dry_run and not all(s == "ready" for s in states.values()):
                raise GateAbort(
                    f"the review store was still not `ready` after {_hms(waited)} ("
                    + ", ".join(f"{k}={v}" for k, v in sorted(states.items()))
                    + f", budget {_hms(float(ctx.args.store_ready_timeout))}). The operation "
                    "sequence was NOT run: against a seeding store every operation the gate is "
                    "about to make is refused by the daemon, so a run that started here would "
                    "prove nothing."
                )
        review_id: str | None = None
        plan = [
            ("create", ["review", "start", f"refs/kbc/pr/{pr}", "--repo", repo, "--json"]),
            ("start-pr", ["review", "start-pr", "--repo", repo, "--pr", str(pr), "--json"]),
            ("sync", ["review", "sync", "--repo", repo, "--pr", str(pr), "--json", "--wait=900"]),
            ("snapshot", ["review", "snapshot", "{REVIEW_ID}", "--json"]),
            ("auto-capture (store sync)", ["store", "sync", "--repo", repo, "--json"]),
            ("retrack", ["review", "retrack", "{REVIEW_ID}", "--repo", repo, "--json"]),
            ("gc (patchsets)", ["review", "gc", "--review", "{REVIEW_ID}", "--json"]),
            ("gc (store refs)", ["store", "gc", "--repo", repo, "--yes", "--json"]),
        ]
        op_rows: list[str] = []
        failed_ops: list[str] = []
        seeding_waits: list[str] = []
        for label, tail in plan:
            rid = review_id or ctx.placeholder("{REVIEW_ID}", "<review-id-from-create>")
            argv = [str(cli), *[rid if a == "{REVIEW_ID}" else a for a in tail], "--daemon", daemon.base]
            # A store-seeding 503 is the daemon's own documented retry, so it
            # is a WAIT inside the gate's budget, not a failed operation. The
            # budget is per operation (--store-ready-timeout, the same budget
            # as the readiness wait above); when it runs out the operation
            # fails exactly as it does for any other reason, and the log says
            # which budget it was. Any other 503 has no retry meaning and
            # fails immediately.
            seed_budget = float(ctx.args.store_ready_timeout)
            seed_started = time.time()
            step = None
            while True:
                step = ctx.exec(g, f"operation: {label}", argv, check=False, timeout=1800)
                if ctx.dry_run:
                    break
                if step.returncode == 0:
                    break
                nap, note = seeding_sleep(
                    step.returncode,
                    (step.stderr or "") + "\n" + (step.stdout or ""),
                    time.time() - seed_started,
                    seed_budget,
                )
                if note:
                    seeding_waits.append(f"{label}: {note}")
                    print(f"  ⏳ gate {g}: {label}: {note}", flush=True)
                if nap is None:
                    break
                time.sleep(nap)
            if step is None:
                continue
            if not ctx.dry_run:
                if step.returncode != 0:
                    budget_note = (
                        f" [the store was still seeding when this operation's "
                        f"{_hms(seed_budget)} seeding budget ran out]"
                        if any(
                            w.startswith(f"{label}:") and "budget is spent" in w
                            for w in seeding_waits
                        )
                        else ""
                    )
                    failed_ops.append(
                        f"{label} (exit {step.returncode}: "
                        f"{ctx.redact(step.stderr.strip()[:200])}{budget_note})"
                    )
                if review_id is None and label in ("create", "start-pr", "sync"):
                    review_id = extract_review_id(step.stdout)
                op_rows.append(f"{label}: exit {step.returncode}")
        ev.append(("operation sequence", "; ".join(op_rows) or "(dry run)"))
        if seeding_waits:
            ev.append(
                (
                    f"store-seeding waits during the operation sequence ({len(seeding_waits)})",
                    "\n".join(f"  {w}" for w in seeding_waits),
                )
            )
            ctx.notes.extend(f"gate 2 seeding wait: {w}" for w in seeding_waits)
        ev.append(
            (
                "auto-capture",
                "driven through `store sync`, the store-side ref move that publishes "
                "repo.head_moved. The auto-capture WORKER only fires for a move in a USER "
                "clone, which this gate forbids; the capture path it shares is the "
                "`review snapshot` step above.",
            )
        )
        if failed_ops:
            raise GateAbort("operation(s) that did not complete: " + "; ".join(failed_ops))

        step = ctx.exec(
            g,
            "invariance check, AFTER the operation sequence",
            [sys.executable, str(INVARIANCE_TOOL), "check", "--baseline", str(ctx.art("gate2", "baseline.json"))],
            check=False,
        )
        ev.append(("check", f"exit {step.returncode}: {ctx.redact(step.stdout.strip()[:2000])}"))
        if not ctx.dry_run:
            if step.returncode != 0:
                raise GateAbort(f"a registered clone's refs changed: {ctx.redact(step.stdout.strip()[:2000])}")
            res.status = "PASS"
            res.reason = (
                f"{len(targets)} clone(s) byte-identical across create, start-pr, "
                f"sync, snapshot, auto-capture, retrack and GC, under "
                f"--invariance-scope {ctx.args.invariance_scope} "
                f"({', '.join(sorted(in_scope))}"
                + (f"; {len(out_of_scope)} configured clone(s) were OUT of scope" if out_of_scope else "")
                + ")."
            )
        else:
            res.status = "UNRUN"
            res.reason = "dry run — planned only"
    except RailError:
        raise  # a safety rail refusal is not a gate verdict: it aborts the run
    except GateAbort as e:
        res.status = "FAIL"
        res.reason = str(e)
    res.evidence = ev
    res.steps = [s for s in ctx.steps if s.gate == g]
    return res


# --------------------------------------------------------------------------
# gate 3 — no fallback
# --------------------------------------------------------------------------


def read_fallback_counters(ctx: Ctx, g: int, daemon: "Daemon", repos: Sequence[str]) -> dict[str, dict[str, int]]:
    out: dict[str, dict[str, int]] = {}
    for name in repos:
        card = ctx.http_json(g, f"store card: {name}", f"{daemon.base}/api/repos/{name}/store")
        fb = ((card.get("runtime") or {}).get("git_fallbacks")) or {}
        out[name] = {"unresolved": int(fb.get("unresolved", 0)), "odb_miss": int(fb.get("odb_miss", 0))}
    return out


def gate_3(ctx: Ctx) -> GateResult:
    g = 3
    ev: list[tuple[str, str]] = []
    res = GateResult(g, GATE_NAMES[g], "FAIL", "")
    try:
        daemon = ctx.ensure_after_daemon(g)
        config = ctx.after_root / "config" / "kb-code.toml"
        repos = store_repos(config) or [n for n, _ in configured_repos(config)]
        if not repos:
            res.status = "SKIP"
            res.reason = "no repo is configured with a review store, so there is no counter to read"
            res.evidence = ev
            res.steps = [s for s in ctx.steps if s.gate == g]
            return res

        states, waited, waits = wait_for_store_ready(
            ctx, g, daemon, repos, timeout=float(ctx.args.store_ready_timeout)
        )
        ev.append(
            (
                "store readiness wait",
                f"waited {_hms(waited)} for "
                + ", ".join(f"{k}={v}" for k, v in sorted(states.items()))
                + f" (budget {_hms(float(ctx.args.store_ready_timeout))}, "
                f"{len(waits)} poll(s) found it not ready)",
            )
        )
        for line in waits:
            ev.append(("store wait", line))
        if not ctx.dry_run and not all(s == "ready" for s in states.values()):
            res.status = "SKIP"
            res.reason = (
                f"store(s) never reached `ready` within {ctx.args.store_ready_timeout}s: "
                + ", ".join(f"{k}={v}" for k, v in sorted(states.items()))
            )
            res.evidence = ev
            res.steps = [s for s in ctx.steps if s.gate == g]
            return res
        ev.append(("store state", ", ".join(f"{k}={v}" for k, v in sorted(states.items()))))

        before_counters = read_fallback_counters(ctx, g, daemon, repos)
        _, _, _, cli = resolve_binaries(ctx.args)
        ctx.exec(
            g,
            "read exercise through the store",
            [str(cli), "review", "files", str(ctx.args.review), "--json", "--daemon", daemon.base],
            check=False,
        )
        after_counters = read_fallback_counters(ctx, g, daemon, repos)
        ev.append(
            (
                "runtime.git_fallbacks (absolute)",
                "; ".join(f"{k}: " + json.dumps(v, sort_keys=True) for k, v in after_counters.items()),
            )
        )
        deltas = {
            k: {m: after_counters[k][m] - before_counters[k][m] for m in after_counters[k]}
            for k in after_counters
        }
        ev.append(("runtime.git_fallbacks (delta across the read exercise)", json.dumps(deltas, sort_keys=True)))
        if not ctx.dry_run:
            offenders = [
                f"{repo}.{metric}={value}"
                for repo, counters in after_counters.items()
                for metric, value in counters.items()
                if value != 0
            ]
            if offenders:
                raise GateAbort(
                    "a READY store still served reads through a fallback: " + ", ".join(offenders)
                )
            res.status = "PASS"
            res.reason = "every store is ready and both fallback counters are zero."
        else:
            res.status = "UNRUN"
            res.reason = "dry run — planned only"
    except RailError:
        raise  # a safety rail refusal is not a gate verdict: it aborts the run
    except GateAbort as e:
        res.status = "FAIL"
        res.reason = str(e)
    res.evidence = ev
    res.steps = [s for s in ctx.steps if s.gate == g]
    return res


# --------------------------------------------------------------------------
# gate 4 — review 65 end to end
# --------------------------------------------------------------------------


def gate_4(ctx: Ctx) -> GateResult:
    g = 4
    ev: list[tuple[str, str]] = []
    res = GateResult(g, GATE_NAMES[g], "FAIL", "")
    try:
        daemon = ctx.ensure_after_daemon(g)
        _, _, _, cli = resolve_binaries(ctx.args)
        repo, rid, pr = ctx.args.repo, ctx.args.review, ctx.args.pr

        _, dry = ctx.exec_json(
            g,
            "retrack dry-run classification",
            [str(cli), "review", "retrack", str(rid), "--repo", repo, "--dry-run", "--json", "--daemon", daemon.base],
        )
        _, applied = ctx.exec_json(
            g,
            "retrack (applies)",
            [str(cli), "review", "retrack", str(rid), "--repo", repo, "--json", "--daemon", daemon.base],
            timeout=1800,
        )
        _, show = ctx.exec_json(
            g, "review state after retrack", [str(cli), "review", "show", str(rid), "--json", "--daemon", daemon.base]
        )
        _, gh_view = ctx.exec_json(
            g,
            "live GitHub PR (ground truth)",
            ["gh", "pr", "view", str(pr), "-R", ctx.args.forge, "--json", "number,files,commits"],
        )
        # The patchset number comes from the retrack envelope, not from a
        # hardcoded 4; in --dry-run it is shown as a named placeholder.
        ps_hint = ctx.placeholder(
            str(((applied or {}).get("data") or {}).get("ps_number") or 0),
            "<ps-number-from-retrack>",
        )
        _, files_body = ctx.exec_json(
            g,
            f"changed files of ps{ps_hint}",
            [str(cli), "review", "files", str(rid), "--ps", ps_hint, "--json", "--daemon", daemon.base],
        )
        for ps_n in (ctx.args.expect_verdict_ps, ps_hint):
            ctx.exec_json(
                g,
                f"findings on ps{ps_n}",
                [str(cli), "review", "findings", "list", str(rid), "--ps", str(ps_n), "--json",
                 "--daemon", daemon.base],
                check=False,
            )

        if ctx.dry_run:
            res.status = "UNRUN"
            res.reason = "dry run — planned only"
        else:
            data = (applied or {}).get("data") or {}
            klass = (dry or {}).get("data", {}).get("class")
            ev.append(("retrack --dry-run class", str(klass)))
            if klass != "stale-pin":
                raise GateAbort(f"retrack {rid} --dry-run classified {klass!r}, expected 'stale-pin'")
            ev.append(
                (
                    "retrack outcome",
                    f"minted={data.get('minted')} ps={data.get('ps_number')} kind={data.get('kind')} "
                    f"class={data.get('class')} verdict_scope_changed={data.get('verdict_scope_changed')}",
                )
            )
            if data.get("minted") is not True:
                raise GateAbort("retrack did not mint a patchset")
            if data.get("kind") != "base-corrected":
                raise GateAbort(f"ps kind is {data.get('kind')!r}, expected 'base-corrected'")
            if data.get("verdict_scope_changed") is not True:
                raise GateAbort("verdict_scope_changed is not true after a base-only correction")
            ps_no = int(data.get("ps_number") or 0)
            psets = {p["ps_number"]: p for p in (show or {}).get("patchsets", [])}
            ps = psets.get(ps_no)
            if ps is None:
                raise GateAbort(f"patchset {ps_no} is missing from GET /api/reviews/{rid}")
            tip = str(ps.get("tip_sha_full") or "")
            base = str(ps.get("base_sha_full") or "")
            commits = int(ps.get("commit_count") or 0)
            ev.append((f"ps{ps_no}", f"tip {tip[:12]}… base {base[:12]}… commits {commits}"))
            if not tip.startswith(ctx.args.expect_tip):
                raise GateAbort(f"ps{ps_no} tip is {tip}, expected to start with {ctx.args.expect_tip}")
            if not base.startswith(ctx.args.expect_base):
                raise GateAbort(f"ps{ps_no} base is {base}, expected to start with {ctx.args.expect_base}")
            n_files = len((files_body or {}).get("files", []))
            ev.append((f"ps{ps_no} files", str(n_files)))
            if (commits, n_files) != (ctx.args.expect_commits, ctx.args.expect_files):
                raise GateAbort(
                    f"kb-code says {commits} commits / {n_files} files, expected "
                    f"{ctx.args.expect_commits} / {ctx.args.expect_files}"
                )
            gh_files = len((gh_view or {}).get("files", []))
            gh_commits = len((gh_view or {}).get("commits", []))
            ev.append((f"gh pr view {pr}", f"{gh_commits} commits / {gh_files} files"))
            if (commits, n_files) != (gh_commits, gh_files):
                raise GateAbort(
                    f"kb-code {commits}/{n_files} != live GitHub {gh_commits}/{gh_files} for PR {pr}"
                )
            verdict = (show or {}).get("verdict") or {}
            ev.append(
                (
                    "verdict",
                    f"state={verdict.get('state')} ps={verdict.get('ps')} "
                    f"verdict_scope_changed={(show or {}).get('verdict_scope_changed')}",
                )
            )
            if verdict.get("ps") != ctx.args.expect_verdict_ps:
                raise GateAbort(
                    f"the verdict moved to ps{verdict.get('ps')}, expected it to stay on "
                    f"ps{ctx.args.expect_verdict_ps}"
                )
            if n_files == 0:
                raise GateAbort("the changed-file list for the new patchset is empty")
            res.status = "PASS"
            res.reason = (
                f"retrack {rid}: dry-run stale-pin, ps{ps_no} kind=base-corrected tip "
                f"{tip[:12]}… base {base[:12]}…, {commits} commits / {n_files} files equal to "
                f"live PR {pr}; findings and verdict stayed on ps{ctx.args.expect_verdict_ps}."
            )
    except RailError:
        raise  # a safety rail refusal is not a gate verdict: it aborts the run
    except GateAbort as e:
        res.status = "FAIL"
        res.reason = str(e)
    res.evidence = ev
    res.steps = [s for s in ctx.steps if s.gate == g]
    return res


# --------------------------------------------------------------------------
# gate 5 — live GitHub through the gh-cli credential
# --------------------------------------------------------------------------


def gate_5(ctx: Ctx) -> GateResult:
    g = 5
    ev: list[tuple[str, str]] = []
    res = GateResult(g, GATE_NAMES[g], "FAIL", "")
    try:
        daemon = ctx.ensure_after_daemon(g)
        _, _, _, cli = resolve_binaries(ctx.args)
        _, sync = ctx.exec_json(
            g,
            "review sync --open --dry-run (gh-cli credential)",
            [str(cli), "review", "sync", "--repo", ctx.args.repo, "--open", "--dry-run", "--json",
             "--daemon", daemon.base],
            timeout=1800,
        )
        _, gh_list = ctx.exec_json(
            g,
            "live open PR list (ground truth)",
            ["gh", "pr", "list", "-R", ctx.args.forge, "--state", "open",
             "--json", "number,baseRefName", "--limit", "300"],
        )

        if ctx.dry_run:
            res.status = "UNRUN"
            res.reason = "dry run — planned only"
        else:
            data = (sync or {}).get("data") or {}
            items = data.get("items") or []
            listed = {
                int(i["pr_number"]): ((i.get("forge") or {}).get("base_ref"))
                for i in items if i.get("ok", True)
            }
            failed = [i for i in items if i.get("ok") is False]
            if failed:
                raise GateAbort(
                    f"{len(failed)} of {len(items)} PR(s) failed to sync: "
                    + ", ".join(f"#{i.get('pr_number')}: {(i.get('error') or {}).get('code')}" for i in failed[:10])
                )
            if data.get("truncated"):
                raise GateAbort("the forge listed more PRs than one sync reads; the comparison would be partial")
            truth = {int(p["number"]): p["baseRefName"] for p in (gh_list or [])}
            ev.append(("open PRs", f"gh: {len(truth)}, review sync --open: {len(listed)}"))
            missing = sorted(set(truth) - set(listed))
            extra = sorted(set(listed) - set(truth))
            if missing:
                raise GateAbort(f"`review sync --open` did not list {len(missing)} open PR(s): {missing[:20]}")
            if extra:
                raise GateAbort(
                    f"`review sync --open` listed {len(extra)} PR(s) GitHub does not report open: {extra[:20]}"
                )
            mismatch = [(n, listed[n], truth[n]) for n in sorted(truth) if listed[n] != truth[n]]
            if mismatch:
                raise GateAbort(
                    f"forge.base_ref disagrees with GitHub for {len(mismatch)} PR(s): "
                    + "; ".join(f"#{n}: {a!r} != {b!r}" for n, a, b in mismatch[:10])
                )
            gh_stacked = sorted(n for n, b in truth.items() if b.startswith(STACKED_BASE_PREFIX))
            sync_stacked = sorted(n for n, b in listed.items() if (b or "").startswith(STACKED_BASE_PREFIX))
            ev.append(
                (
                    f"stacked PRs targeting {STACKED_BASE_PREFIX}*",
                    f"gh reports {len(gh_stacked)} ({gh_stacked}); review sync reports "
                    f"{len(sync_stacked)} ({sync_stacked})",
                )
            )
            if gh_stacked != sync_stacked:
                raise GateAbort(f"stacked PR sets differ: gh {gh_stacked} vs sync {sync_stacked}")
            res.status = "PASS"
            res.reason = (
                f"all {len(truth)} open PRs listed by `review sync --open --dry-run`, every "
                f"forge.base_ref equal to `gh pr list`, including {len(gh_stacked)} stacked PR(s) "
                f"on {STACKED_BASE_PREFIX}*."
            )
    except RailError:
        raise  # a safety rail refusal is not a gate verdict: it aborts the run
    except GateAbort as e:
        res.status = "FAIL"
        res.reason = str(e)
    res.evidence = ev
    res.steps = [s for s in ctx.steps if s.gate == g]
    return res


# --------------------------------------------------------------------------
# gate 6 — secrets
# --------------------------------------------------------------------------

def _q(identifier: str) -> str:
    return '"' + identifier.replace('"', '""') + '"'


@dataclass
class SecretScan:
    """One artefact's worth of secret-scan result.

    `hits` are CREDENTIALS — a plausible full token for its prefix family
    (`redact.rs`'s shapes, see `SECRET_SHAPES`) or an exact match against the
    live `gh auth token` value. `excluded` is what the scan saw and
    deliberately did NOT count: a bare token PREFIX, which in a database that
    indexes source and prose is overwhelmingly a test fixture
    (`ghp_TESTTOKEN_FAKE…sekrit`) or a sentence that NAMES a token shape
    ("high-precision known token shapes (sk-…, ghp_…)"). A bare prefix is not
    a credential.

    Reporting the exclusions is not politeness: a bare "no matches" is
    indistinguishable from a scan that never ran. The driver prints both.

    The scan runs on RAW text, deliberately. Redacting first would replace a
    real token with the redaction marker before the shape rule ever saw it,
    and the gate could then never report the very thing it exists to report.
    What keeps the value unprintable is that this structure holds only needle
    NAMES and COUNTS — there is nowhere for a value to survive to.
    """

    hits: list[tuple[str, int]] = field(default_factory=list)      # (needle, count)
    excluded: list[tuple[str, int]] = field(default_factory=list)  # (bare prefix, count)

    def merge(self, other: "SecretScan") -> None:
        for label, n in other.hits:
            self.hits = _add_count(self.hits, label, n)
        for label, n in other.excluded:
            self.excluded = _add_count(self.excluded, label, n)


def _add_count(rows: list[tuple[str, int]], label: str, n: int) -> list[tuple[str, int]]:
    for i, (existing, count) in enumerate(rows):
        if existing == label:
            return rows[:i] + [(label, count + n)] + rows[i + 1:]
    return rows + [(label, n)]


def unshaped_prefix_count(text: str) -> dict[str, int]:
    """Bare-prefix occurrences that do NOT begin a plausible full token, per
    prefix. This is the exclusion accounting, and it is what tells a fixture
    string apart from a credential: `ghp_TESTTOKEN_FAKE…sekrit` has a `_`
    inside its first sixteen characters, which the real alphabet forbids."""
    out: dict[str, int] = {}
    for prefix in SECRET_PREFIXES:
        start = 0
        while True:
            i = text.find(prefix, start)
            if i < 0:
                break
            start = i + len(prefix)
            if not any(p.match(text, i) for _, p in SECRET_SHAPES):
                out[prefix] = out.get(prefix, 0) + 1
    return out


def scan_blob(text: str, token: str | None) -> SecretScan:
    """The scan itself, over one blob of RAW text. Nothing here keeps a
    matched value — only a needle's name and how many times it matched."""
    scan = SecretScan()
    for label, pattern in SECRET_SHAPES:
        n = len(pattern.findall(text))
        if n:
            scan.hits.append((f"a plausible {label}", n))
    if token:
        n = text.count(token)
        if n:
            scan.hits.append(("the literal `gh auth token` output", n))
    for prefix, n in unshaped_prefix_count(text).items():
        scan.excluded.append((prefix, n))
    return scan


def scan_text_file(ctx: Ctx, path: Path, token: str | None) -> SecretScan:
    if not path.is_file():
        return SecretScan()
    # RAW, not `ctx.redact(...)`: redacting first would erase the very match
    # this gate exists to report. `scan_blob` keeps only labels and counts.
    return scan_blob(path.read_text(encoding="utf-8", errors="replace"), token)


def _text_columns(conn: sqlite3.Connection, table: str) -> list[str]:
    try:
        info = conn.execute(f"PRAGMA table_info({_q(table)})").fetchall()
    except sqlite3.Error:
        return []
    return [r[1] for r in info if len(r) >= 3 and (r[2].upper().startswith("TEXT") or r[2] == "")]


def scan_sqlite(ctx: Ctx, db: Path, token: str | None) -> SecretScan:
    """Read-only SQL over every TEXT column of the COPY, with the SHAPE
    decision made in Python on the candidate rows.

    The SQL only NARROWS (`GLOB` for each prefix family, `instr` for the exact
    literal): deciding "is this a credential" is a regex against the
    product's own alphabet, and SQLite has no regex. Narrowing in SQL and
    deciding in Python is also what keeps a value unprintable — a candidate
    cell is counted and dropped, never echoed.
    """
    scan = SecretScan()
    try:
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        conn.execute("PRAGMA query_only=1")
    except sqlite3.Error as e:
        scan.hits.append((f"the volume copy could not be opened read-only: {e}", 1))
        return scan
    try:
        try:
            tables = [
                r[0]
                for r in conn.execute(
                    "select name from sqlite_master "
                    "where type='table' and name not like 'sqlite_%' order by name;"
                )
            ]
        except sqlite3.Error as e:
            scan.hits.append((f"could not list tables: {e}", 1))
            return scan
        for table in tables:
            cols = _text_columns(conn, table)
            if not cols:
                continue
            where: list[str] = []
            params: list[str] = []
            for col in cols:
                q = _q(col)
                # One GLOB per column per prefix family: a bare prefix, or
                # anything that could still become a plausible full token.
                for needle in ("*gh[pousr]_*", "*github_pat_*", "*glpat-*"):
                    where.append(f"{q} GLOB ?")
                    params.append(needle)
                if token:
                    where.append(f"instr(coalesce({q},''),?)>0")
                    params.append(token)
            selected = ", ".join(_q(c) for c in cols)
            try:
                cursor = conn.execute(
                    f"select {selected} from {_q(table)} where {' or '.join(where)};", params
                )
                for row in cursor:
                    for cell in row:
                        if cell is None or isinstance(cell, bytes):
                            continue
                        # RAW cell, decided and counted, then dropped. The
                        # cell is never echoed, so no value can escape.
                        scan.merge(scan_blob(str(cell), token))
            except sqlite3.Error as e:
                scan.hits.append((f"{table} could not be scanned: {e}", 1))
    finally:
        conn.close()
    return scan


def gate_6(ctx: Ctx) -> GateResult:
    g = 6
    ev: list[tuple[str, str]] = []
    res = GateResult(g, GATE_NAMES[g], "FAIL", "")
    try:
        token = ctx.gh_token()
        ev.append(
            (
                "credential source",
                (f"`gh auth token --user {ctx.args.gh_user}` answered ({len(token)} chars; the value "
                 "is never printed, logged or passed in argv) — its exact literal is a needle")
                if token else
                (f"`gh auth token --user {ctx.args.gh_user}` did NOT answer; only the token SHAPES "
                 "apply"),
            )
        )
        # The after daemon is stopped first so the copy is consistent and quiescent.
        ctx.stop_daemon("after")
        src_db = ctx.after_root / "state" / "kb-code" / "index.db"
        copy_db = ctx.out / "gate6" / "after-volume-copy.db"
        if not ctx.dry_run:
            copy_db.parent.mkdir(parents=True, exist_ok=True)
            if copy_db.exists():
                copy_db.unlink()
        step = ctx.exec(
            g,
            "consistent COPY of the after volume (sqlite3 .backup, never cp)",
            ["sqlite3", str(src_db), f".backup '{copy_db}'"],
            check=False,
            timeout=7200,
        )
        if ctx.dry_run:
            res.status = "UNRUN"
            res.reason = "dry run — planned only"
        else:
            if step.returncode != 0 or not copy_db.is_file():
                raise GateAbort(
                    f"could not make the read-only copy of the after volume (exit {step.returncode}): "
                    f"{ctx.redact(step.stderr.strip()[:300])}"
                )
            ev.append(("volume copy", f"{copy_db} ({copy_db.stat().st_size} bytes, via `sqlite3 .backup`)"))
            targets: list[tuple[str, Path]] = []
            if (ctx.out / "logs").is_dir():
                targets += [("daemon log", p) for p in sorted((ctx.out / "logs").glob("*.log"))]
            if (ctx.out / "json").is_dir():
                targets += [("gate output", p) for p in sorted((ctx.out / "json").rglob("*.json"))]
            targets += [("gate output", p) for p in sorted(ctx.out.glob("gate*/*.json"))]
            hits: list[tuple[str, int]] = []
            excluded: list[tuple[str, int]] = []
            scanned = 0
            for kind, path in targets:
                scan = scan_text_file(ctx, path, token)
                scanned += 1
                for label, n in scan.hits:
                    hits = _add_count(hits, f"{kind}: {path}: {label}", n)
                for label, n in scan.excluded:
                    excluded = _add_count(excluded, f"{kind}: {path}: bare {label}", n)
            volume = scan_sqlite(ctx, copy_db, token)
            for label, n in volume.hits:
                hits = _add_count(hits, f"volume (read-only SQL): {label}", n)
            for label, n in volume.excluded:
                excluded = _add_count(excluded, f"volume (read-only SQL): bare {label}", n)
            ev.append(
                (
                    "needles (a secret is a CREDENTIAL, not a string that starts with a prefix)",
                    "; ".join(
                        [f"<{label}>" for label, _ in SECRET_SHAPES]
                        + (["<the exact `gh auth token` output>"] if token else [])
                    )
                    + f" — the shapes are the product's own, from "
                    f"`crates/kb-code-server/src/review_store/redact.rs`",
                )
            )
            ev.append(
                (
                    "NOT needles",
                    ", ".join(f"{p}* (a bare prefix is not a credential)" for p in SECRET_PREFIXES)
                    + " — counted and reported below as excluded fixture/doc text instead",
                )
            )
            ev.append(("scanned", f"{scanned} file(s) + every TEXT column of the volume copy"))
            if excluded:
                for where, n in excluded:
                    ev.append(
                        (
                            "EXCLUDED as fixture/doc text (not a credential, value never printed)",
                            f"{where} — {n} occurrence(s)",
                        )
                    )
            else:
                ev.append(
                    (
                        "EXCLUDED as fixture/doc text",
                        "none: no bare token prefix appeared in anything scanned",
                    )
                )
            if hits:
                for where, n in hits:
                    ev.append(("MATCH (redacted)", f"{where} — {n} occurrence(s)"))
                raise GateAbort(
                    f"{len(hits)} location(s) held a plausible full token or the literal "
                    f"`gh auth token` output; locations and counts above, values never printed. "
                    f"{len(excluded)} bare-prefix occurrence(s) were excluded as fixture/doc text "
                    "and are listed above — a prefix alone is not a credential."
                )
            res.status = "PASS"
            res.reason = (
                f"no plausible full token and no literal `gh auth token` output in {scanned} "
                f"artefact(s) or anywhere in the volume copy; {len(excluded)} bare-prefix "
                f"occurrence(s) seen and excluded as fixture/doc text (listed above), so this is "
                "a scan that ran rather than a bare zero."
            )
    except RailError:
        raise  # a safety rail refusal is not a gate verdict: it aborts the run
    except GateAbort as e:
        res.status = "FAIL"
        res.reason = str(e)
    res.evidence = ev
    res.steps = [s for s in ctx.steps if s.gate == g]
    return res


# --------------------------------------------------------------------------
# gate 7 — the existing suite + TS regen, which IS CI
# --------------------------------------------------------------------------

DRIFT_JOBS = ("drift", "code-drift")


def gate_7(ctx: Ctx) -> GateResult:
    g = 7
    ev: list[tuple[str, str]] = []
    res = GateResult(g, GATE_NAMES[g], "FAIL", "")
    try:
        if ctx.args.ci_pr is None:
            res.status = "UNRUN"
            res.reason = "no --ci-pr given: name the PR that carries the binaries' CI"
            res.evidence = ev
            res.steps = [s for s in ctx.steps if s.gate == g]
            return res
        _, head = ctx.exec_json(
            g, "the PR under test",
            ["gh", "pr", "view", str(ctx.args.ci_pr), "-R", ctx.args.ci_repo,
             "--json", "number,headRefName,url,state"],
        )
        _, checks = ctx.exec_json(
            g, "the check-run table",
            ["gh", "pr", "checks", str(ctx.args.ci_pr), "-R", ctx.args.ci_repo,
             "--json", "name,state,bucket,link"],
        )
        if ctx.dry_run:
            res.status = "UNRUN"
            res.reason = "dry run — planned only"
        else:
            head = head or {}
            checks = checks or []
            ev.append(
                (
                    "PR under test",
                    f"{ctx.args.ci_repo}#{head.get('number')} [{head.get('state')}] head "
                    f"{head.get('headRefName')}",
                )
            )
            if ctx.args.ci_branch and head.get("headRefName") != ctx.args.ci_branch:
                raise GateAbort(
                    f"PR #{head.get('number')} is on branch {head.get('headRefName')!r}, expected "
                    f"{ctx.args.ci_branch!r}"
                )
            if not checks:
                raise GateAbort("GitHub reports no check runs for this PR")
            rows, bad = [], []
            for c in checks:
                bucket = c.get("bucket") or c.get("state")
                rows.append(f"{c.get('name')}={bucket}")
                if bucket not in ("pass", "success", "skip"):
                    bad.append(f"{c.get('name')}={bucket}")
            ev.append(("check-run table", "; ".join(rows)))
            drift = [c for c in checks if any(d in (c.get("name") or "").lower() for d in DRIFT_JOBS)]
            if not drift:
                raise GateAbort(
                    f"neither TS-regen job {list(DRIFT_JOBS)} appears in the check-run table, so "
                    "'TS types regenerated with no unrelated diff' is unevidenced"
                )
            ev.append(
                ("TS regen jobs", "; ".join(f"{c.get('name')}={c.get('bucket') or c.get('state')}" for c in drift))
            )
            if bad:
                raise GateAbort("not every check is green: " + ", ".join(bad))
            ev.append(
                (
                    "local suites",
                    f"NOT RUN here, by design: this driver never invokes cargo or npm. `cargo "
                    f"test` and the web-code tests are the CI jobs on {ctx.args.ci_repo}#{ctx.args.ci_pr}.",
                )
            )
            res.status = "PASS"
            res.reason = (
                f"all {len(checks)} check runs green on {ctx.args.ci_repo}#{ctx.args.ci_pr} (head "
                f"{head.get('headRefName')}), including the TS-regen job(s) "
                + ", ".join(c.get("name", "") for c in drift)
                + ". No local suite was run."
            )
    except RailError:
        raise  # a safety rail refusal is not a gate verdict: it aborts the run
    except GateAbort as e:
        res.status = "FAIL"
        res.reason = str(e)
    res.evidence = ev
    res.steps = [s for s in ctx.steps if s.gate == g]
    return res


# --------------------------------------------------------------------------
# BUILD-LOG.md
# --------------------------------------------------------------------------

LOG_HEADER = """# BUILD-LOG — kb-code review store, Phase 1 acceptance (BUILD-BRIEF §3)

| | |
|---|---|
| generated | {generated} |
| driver | `scripts/review-store/run_gates.py` (RS-U13) |
| mode | {mode} |
| before binary | `{before_server}`{before_sha} — commit `{before_commit}` |
| after binary | `{after_server}`{after_sha} — commit `{after_commit}` |
| before volume | `{before_root}` (V0044 input) |
| after volume | `{after_root}` (migrated in place by the post-upgrade boot) |
| ports | before `{before_port}`, after `{after_port}` — the in-use set {forbidden} is never touched |
| readiness budget | after `{ready_timeout}`, before `{before_ready_timeout}` — the after budget covers a first boot that takes the gated V0045 pre-migration snapshot (a whole-volume `VACUUM INTO`) |
| pristine inputs | `{bundle}` (verified against `SHA256SUMS`) |
| worktree | {git_head} |

## Summary

| gate | asserts | verdict | one line |
|---|---|---|---|
{summary}

**Exit code {exit_code}** — {verdict_line}
"""

GATE_INTRO = {
    1: """**Asserts.** Every existing review's files, per-file blob ids, diff stats,
comment/finding anchors and verdict patchset are IDENTICAL before and after the
V0044 -> V0045 upgrade, and the only differences are new envelope fields
(`base{{…}}`, `warnings[]`, `minted`, per-patchset `kind`/`base_tip_sha`) — each
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
merely because the file exists.""",
    2: """**Asserts.** Across create, start-pr, sync, snapshot, auto-capture, retrack
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
fail. Any other 503 is still a failure.""",
    3: """**Asserts.** With every review store `ready`, ZERO reads fall back to the
user repo's objects. The counters are `runtime.git_fallbacks` (`unresolved`,
`odb_miss`) on `GET /api/repos/{{name}}/store`, read per repo. A store that is
not yet ready is retried with backoff and then reported as SKIP with the state
it stayed in; a READY store with a non-zero counter is a FAIL.""",
    4: """**Asserts.** Review {review} (repo `{repo}`, PR {pr}, squash-merged
2026-09-24) end to end: `retrack {review} --dry-run` classifies `stale-pin`;
`retrack {review}` gives ps{vps_plus} `kind=base-corrected` with tip `{tip}…`,
base `{base}…`, {commits} commits / {files} files, equal to LIVE
`gh pr view {pr} --json files,commits`; findings and the verdict stay on
ps{vps} with `verdict_scope_changed`.""",
    5: """**Asserts.** With the `gh-cli` credential pinned to `{gh_user}`,
`review sync --repo {repo} --open --dry-run --json` lists every open PR of
`{forge}` and the reported `forge.base_ref` equals
`gh pr list --json number,baseRefName` for all of them, including the stacked PRs
targeting `{stacked}*` — the count is compared against GitHub's, never
hardcoded, and reported either way.""",
    6: """**Asserts.** No GitHub credential leaks. The daemon's stdout+stderr are
captured to a file (this box has no daemon log file — the driver creates one),
and that log, every gate's JSON output and a `sqlite3 .backup` COPY of the after
volume (read-only SQL, never `cp`, never the live DB) are scanned for a
**credential**: a plausible full token for its prefix family — the shapes are the
product's own, transcribed from `crates/kb-code-server/src/review_store/redact.rs`
— or the exact literal output of `gh auth token --user {gh_user}`.

A bare `gho_`/`ghp_`/`ghu_`/`ghs_` is **not** a needle. In a database that
indexes source and prose, a bare prefix is overwhelmingly a test fixture or a
sentence that NAMES a token shape, and the design's rule is that a secret is a
real credential, not a string that starts with a prefix. Those occurrences are
counted and reported below as excluded, so the log shows what the scan ran and
what it discounted instead of a bare zero. Matches are reported as location +
count; a matched value is never printed or logged.""",
    7: """**Asserts.** The existing suite passes and the TypeScript types are
regenerated with no unrelated diff. This gate IS CI — the driver never runs
`cargo test` or the web-code tests locally. It records the check-run table for
`{ci_repo}#{ci_pr}` and requires every check green, including the `drift` /
`code-drift` TS-regen jobs, which is what makes "regenerated with no unrelated
diff" evidence rather than a claim.""",
}


def md_cell(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ") or "—"


def render_log(ctx: Ctx, results: Sequence[GateResult], exit_code: int) -> str:
    args = ctx.args

    def sha_of(p: str) -> str:
        path = Path(p)
        return f" (sha256 {sha256_file(path)[:16]}…)" if path.is_file() else " (NOT PRESENT — gate could not have run)"

    head = subprocess.run(
        ["git", "-C", str(REPO_ROOT), "rev-parse", "--short", "HEAD"],
        capture_output=True, text=True, check=False,
    )
    is_template = getattr(ctx, "template", False)
    summary_rows = []
    for n in range(1, 8):
        r = next((x for x in results if x.number == n), None)
        if r is None:
            why = "template — nothing has been run yet" if is_template else (
                f"not selected (`--gate {' '.join(map(str, ctx.selected))}`)"
            )
            summary_rows.append(f"| {n} | {GATE_NAMES[n]} | UNRUN | {why} |")
        else:
            summary_rows.append(
                f"| {n} | {GATE_NAMES[n]} | {r.status} | {md_cell(r.reason.splitlines()[0] if r.reason else '')} |"
            )
    not_pass = [r.number for r in results if not r.ok]
    unselected = [] if is_template else [n for n in range(1, 8) if n not in ctx.selected]
    verdict_line = (
        "TEMPLATE — no gate has been run; nothing here is a result."
        if is_template
        else (
            "all seven gates PASS." if exit_code == EXIT_OK
            else f"NOT all seven gates PASS (not PASS: {sorted(not_pass + unselected)})."
        )
    )
    out = [
        LOG_HEADER.format(
            generated=now_iso(),
            mode=(
                "TEMPLATE — no gate has been run"
                if is_template
                else ("DRY RUN — nothing was executed" if ctx.dry_run else "real run")
            ),
            before_server=args.before_server,
            after_server=args.after_server,
            before_sha=sha_of(args.before_server),
            after_sha=sha_of(args.after_server),
            before_commit=args.before_commit or "UNRECORDED",
            after_commit=args.after_commit or "UNRECORDED",
            before_root=ctx.before_root,
            after_root=ctx.after_root,
            before_port=ctx.before_port,
            after_port=ctx.after_port,
            forbidden=list(FORBIDDEN_PORTS),
            ready_timeout=_hms(args.ready_timeout),
            before_ready_timeout=_hms(args.before_ready_timeout),
            bundle=ctx.bundle,
            git_head=head.stdout.strip() if head.returncode == 0 else "unknown",
            summary="\n".join(summary_rows),
            exit_code="n/a (template — nothing was run)" if is_template else exit_code,
            verdict_line=verdict_line,
        )
    ]
    if ctx.dry_run:
        out.append("\n## Plan (dry run — nothing was executed)\n\n```\n" + ctx.plan_text() + "\n```\n")
    for n in range(1, 8):
        r = next((x for x in results if x.number == n), None)
        out.append(f"\n## Gate {n} — {GATE_NAMES[n]}\n")
        out.append(
            GATE_INTRO[n].format(
                review=args.review, repo=args.repo, pr=args.pr, vps=args.expect_verdict_ps,
                vps_plus=args.expect_verdict_ps + 1, tip=args.expect_tip, base=args.expect_base,
                commits=args.expect_commits, files=args.expect_files, gh_user=args.gh_user,
                forge=args.forge, stacked=STACKED_BASE_PREFIX, ci_repo=args.ci_repo, ci_pr=args.ci_pr,
            )
        )
        out.append("\n\n")
        if r is None:
            why = (
                "this file is the TEMPLATE: no gate has been run yet"
                if is_template
                else (
                    f"not selected by this invocation (it ran gates "
                    f"{', '.join(map(str, ctx.selected)) or 'none'})"
                )
            )
            out.append(
                f"**UNRUN** — {why}. No command was executed for gate {n} and no claim "
                "is made about it.\n"
            )
            out.append(f"\n### Commands\n\n```\n(none — {why})\n```\n")
            out.append(f"\n### Evidence\n\n```\n(none — {why})\n```\n")
            continue
        out.append(f"**{r.status}** — {r.reason}\n")
        out.append("\n### Commands\n\n```\n")
        out.append("\n".join(s.render() for s in r.steps) if r.steps else "(no command was run)")
        out.append("\n```\n")
        out.append("\n### Evidence\n\n```\n")
        if r.evidence:
            for label, value in r.evidence:
                out.append(f"{label}:\n  {value}\n")
        else:
            out.append("(none recorded)\n")
        out.append("```\n")
    if ctx.notes:
        out.append(
            "\n## Driver notes (migrate-first phase, readiness, migration evidence)\n\n```\n"
            + "\n".join(ctx.notes)
            + "\n```\n"
        )
    return "".join(out)


def write_log(ctx: Ctx, results: Sequence[GateResult], exit_code: int) -> None:
    path = Path(ctx.log_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(render_log(ctx, results, exit_code), encoding="utf-8")
    print(f"wrote {path}")


# --------------------------------------------------------------------------
# self-test — each safety rail, reachable and provable
# --------------------------------------------------------------------------


def self_test(args: argparse.Namespace) -> int:
    """Exercise all five rails with inputs that MUST be refused (and controls
    that must be allowed), touching no volume, daemon or clone."""
    checks: list[tuple[str, bool, str]] = []

    def expect_refuse(name: str, fn: Callable[[], Any]) -> None:
        try:
            fn()
        except RailError as e:
            checks.append((name, True, str(e)[:150]))
        except Exception as e:  # noqa: BLE001
            checks.append((name, False, f"wrong exception {type(e).__name__}: {e}"))
        else:
            checks.append((name, False, "was NOT refused"))

    def expect_allow(name: str, fn: Callable[[], Any]) -> None:
        try:
            fn()
        except Exception as e:  # noqa: BLE001
            checks.append((name, False, f"wrongly refused: {e}"))
        else:
            checks.append((name, True, "allowed, as it should be"))

    ctx = Ctx(args)
    ctx.dry_run = True
    print("run_gates.py --self-test — the five safety rails\n")

    print("R1 (ports)")
    expect_refuse("R1 port 4747 (live kb-code)", lambda: ctx.check_port(4747, "start a test daemon"))
    expect_refuse("R1 port 4000 (prod kb)", lambda: ctx.check_port(4000, "start a test daemon"))
    busy = sorted(listening_ports())
    if busy:
        expect_refuse(
            f"R1 port {busy[0]} while `ss -ltn` reports it LISTENing",
            lambda: ctx.check_port(busy[0], "start a test daemon"),
        )
    expect_allow("R1 free port 4791", lambda: ctx.check_port(4791, "start a test daemon"))

    print("R2 (paths)")
    expect_refuse("R2 the live kb-code volume", lambda: ctx.check_path("/home/nik/.local/state/kb/kb-code/index.db", "read"))
    expect_refuse("R2 an unrelated path", lambda: ctx.check_path("/etc/passwd", "read"))
    expect_allow("R2 the before volume", lambda: ctx.check_path(ctx.before_root / "state/kb-code/index.db", "read"))
    expect_allow("R2 the after volume", lambda: ctx.check_path(ctx.after_root, "read"))
    expect_allow("R2 the output dir", lambda: ctx.check_path(ctx.out / "x.json", "write"))
    expect_allow("R2 a CONFIGURED user clone (read-only hashing, gate 2's subject)",
                 lambda: ctx.check_clone_path("/home/nik/progetti/1000farmacie/rails/1000farmacie.01", "1000farmacie-rails-01"))
    hostile = Ctx(argparse.Namespace(**{**vars(args), "before_root": str(LIVE_STATE_PREFIX)}))
    expect_refuse("R2 even when a flag makes the LIVE state root an allowed root",
                  lambda: hostile.check_path(LIVE_STATE_PREFIX / "kb-code" / "index.db", "read"))

    print("R3 (no git write on a user clone)")
    clone = "/home/nik/progetti/1000farmacie/rails/1000farmacie.01"
    expect_refuse("R3 `git fetch` on a user clone", lambda: ctx.guard_argv(["git", "-C", clone, "fetch", "origin"]))
    expect_refuse("R3 `git gc` on a user clone", lambda: ctx.guard_argv(["git", "-C", "/home/nik/progetti/kb", "gc"]))
    expect_refuse("R3 `git worktree prune` on a user clone", lambda: ctx.guard_argv(["git", "-C", clone, "worktree", "prune"]))
    expect_allow("R3 `git for-each-ref` on a user clone (read-only)", lambda: ctx.guard_argv(["git", "-C", clone, "for-each-ref"]))
    expect_allow("R3 `git fetch` into a scratch repo", lambda: ctx.guard_argv(["git", "-C", str(ctx.out / "scratch"), "fetch", "origin"]))

    print("R4 (no token in argv)")
    expect_refuse("R4 a gho_ token in argv", lambda: ctx.guard_argv(["curl", "-H", "Authorization: Bearer gho_" + "A" * 36]))
    expect_refuse("R4 a github_pat_ token in argv", lambda: ctx.guard_argv(["gh", "api", "-H", "token: github_pat_" + "b" * 40]))
    expect_allow(
        "R4 `gh auth token` (takes no token; its output is read into memory)",
        lambda: ctx.guard_argv(["gh", "auth", "token", "--user", "nicolasacchi"]),
    )

    print("R5 (verify the inputs before touching a volume)")
    expect_refuse("R5 a bundle with no SHA256SUMS", lambda: require_sums(ctx.bundle / "does-not-exist"))
    expect_allow("R5 the real bundle has SHA256SUMS", lambda: require_sums(ctx.bundle))
    if (ctx.bundle / "SHA256SUMS").is_file():
        # The small entries only: a real run verifies the 2 GB of tarballs too,
        # but --self-test must stay a one-second check.
        checked, bad = verify_bundle(ctx.bundle, max_bytes=1_000_000)
        checks.append(
            (
                "R5 bundle verified against SHA256SUMS",
                not bad,
                f"{checked} small file(s) verified, no drift" if not bad else "; ".join(bad)[:150],
            )
        )
        # …and prove the rail actually reports drift, on a throwaway copy.
        fake = ctx.out / "self-test-drift"
        fake.mkdir(parents=True, exist_ok=True)
        (fake / "payload").write_text("hello\n", encoding="utf-8")
        (fake / "SHA256SUMS").write_text(
            hashlib.sha256(b"not this\n").hexdigest() + "  payload\n", encoding="utf-8"
        )
        _, fake_bad = verify_bundle(fake)
        checks.append(
            ("R5 drift is reported, not swallowed", bool(fake_bad), fake_bad[0] if fake_bad else "no drift reported")
        )
    else:
        checks.append(("R5 bundle verified against SHA256SUMS", True, f"skipped: {ctx.bundle} has no SHA256SUMS"))

    # ------------------------------------------------------------------
    # G6 — the secret needle is a CREDENTIAL, not a token prefix. The
    # shapes come from the product's own redactor; this proves both the
    # fixture/doc exclusions and that a plausible full token still counts.
    # ------------------------------------------------------------------

    print("G6 (a secret is a credential, not a prefix)")
    FIXTURE = "ghp_TESTTOKEN_FAKE_does_not_match_a_real_alphabet_sekrit"
    DOC = "secrets — high-precision known token shapes (sk-…, ghp_…, AWS, gho_…)"
    REAL = "ghp_1a2B3c4D5e6F7g8H9i0JkLmNoPqRsTuVwXyZ0123"

    fixture_scan = scan_blob(f"let token = {FIXTURE}; // fixture", None)
    checks.append(
        (
            "G6 a `ghp_TESTTOKEN_FAKE…sekrit` fixture does NOT count as a secret",
            not fixture_scan.hits and bool(fixture_scan.excluded),
            f"hits={fixture_scan.hits or 'none'}, excluded={fixture_scan.excluded}",
        )
    )
    doc_scan = scan_blob(f"//! {DOC}", None)
    checks.append(
        (
            "G6 a doc sentence that NAMES a token shape does NOT count",
            not doc_scan.hits and bool(doc_scan.excluded),
            f"hits={doc_scan.hits or 'none'}, excluded={doc_scan.excluded}",
        )
    )
    real_scan = scan_blob(f"remote: https://x-access-token:{REAL}@github.com/acme/widgets.git", None)
    checks.append(
        (
            "G6 a plausible full token DOES count",
            sum(n for _, n in real_scan.hits) == 1,
            f"hits={real_scan.hits or 'none'}, excluded={real_scan.excluded or 'none'}",
        )
    )
    checks.append(
        (
            "G6 a matched value is never carried into the report",
            REAL not in repr(real_scan) and FIXTURE not in repr(fixture_scan),
            "the scan structure holds needle names and counts only",
        )
    )
    # A GHE/Gitea token the shape rules do not know is still caught, because
    # the caller already holds it: the exact literal is a needle of its own.
    OPAQUE = "0a1b2c3d4e5fTOKENOFNOSHAPE00"
    literal_scan = scan_blob(f"pushed with {OPAQUE} as the credential", OPAQUE)
    checks.append(
        (
            "G6 an exact `gh auth token` literal counts whatever its shape",
            literal_scan.hits == [("the literal `gh auth token` output", 1)]
            and not any("classic" in label for label, _ in literal_scan.hits),
            f"hits={literal_scan.hits or 'none'}",
        )
    )
    # The shapes must be the PRODUCT's shapes, not a second invention: read
    # them back out of redact.rs and compare. If the redactor's alphabet ever
    # changes, this check is what makes the driver follow it.
    redact_src = REPO_ROOT / "crates" / "kb-code-server" / "src" / "review_store" / "redact.rs"
    if redact_src.is_file():
        wanted = {
            r"\bgh[pousr]_[A-Za-z0-9]{16,}",
            r"\bgithub_pat_[A-Za-z0-9_]{20,}",
            r"\bglpat-[A-Za-z0-9_\-]{16,}",
        }
        checks.append(
            (
                "G6 the token shapes are the ones in redact.rs (one convention)",
                {p.pattern for _, p in SECRET_SHAPES} == wanted,
                f"{sorted(p.pattern for _, p in SECRET_SHAPES)}",
            )
        )
    else:
        checks.append(
            ("G6 the token shapes are the ones in redact.rs (one convention)", False,
 f"redact.rs not found at {redact_src}")
        )

    # ------------------------------------------------------------------
    # Readiness — "slow" and "dead" must get different words, and every
    # failure must carry the elapsed time and the daemon's last line.
    # ------------------------------------------------------------------

    print("Readiness (a slow migration is not a crash)")
    with tempfile.TemporaryDirectory(prefix="run-gates-selftest-") as tmp:
        root = Path(tmp)
        vol = root / "state" / "kb-code"
        vol.mkdir(parents=True)
        log = root / "after-daemon.log"

        log.write_text(
            "kb-code: booting\n"
            "kb-code: took a pre-migration snapshot before crossing a gated schema epoch "
            "[45] — an older binary will refuse this volume afterwards\n",
            encoding="utf-8",
        )
        migrating = Daemon(ctx, 0, "after", Path("/bin/true"), root, 4791)
        migrating.log = log
        active, detail = migrating.migration_state()
        checks.append(
            (
                "readiness a migration line in the daemon output is recognised",
                active and "pre-migration snapshot" in detail,
                detail or "no migration signal found",
            )
        )
        try:
            migrating.wait_ready(timeout=1.0)
        except GateAbort as e:
            slow_msg = str(e)
        else:
            slow_msg = ""
        checks.append(
            (
                "readiness a migrating boot says MIGRATING, not crashed",
                "still inside the gated pre-migration snapshot" in slow_msg
                and "not a crash" in slow_msg,
                slow_msg.splitlines()[0][:150] if slow_msg else "wait_ready returned instead of failing",
            )
        )
        checks.append(
            (
                "readiness a failure carries the elapsed time AND the last output line",
                "Last line:" in slow_msg
                and "pre-migration snapshot" in slow_msg
                and (bool(re.search(r"after \d", slow_msg)) or "waited " in slow_msg),
                slow_msg.splitlines()[-1][:150] if slow_msg else "no failure raised",
            )
        )

        # A growing `*.bak` beside the volume is the same signal, from the
        # filesystem, with no log line at all.
        log.write_text("kb-code: booting\n", encoding="utf-8")
        bak = vol / "index.db.pre-V0044.bak"
        bak.write_bytes(b"x" * 4096)
        journal = vol / "index.db.pre-V0044.bak-journal"
        journal.write_bytes(b"y" * 64)
        active2, detail2 = migrating.migration_state()
        checks.append(
            (
                "readiness a growing *.bak / *.bak-journal beside the volume is recognised",
                active2 and "bak-journal" in detail2,
                detail2 or "no snapshot evidence found",
            )
        )

        # No migration evidence at all: that is a boot failure, and it says so.
        log.write_text("error: config parse failed at line 12\n", encoding="utf-8")
        bak.unlink()
        journal.unlink()
        dead = Daemon(ctx, 0, "after", Path("/bin/true"), root, 4791)
        dead.log = log
        try:
            dead.wait_ready(timeout=1.0)
        except GateAbort as e:
            dead_msg = str(e)
        else:
            dead_msg = ""
        checks.append(
            (
                "readiness a boot with no migration evidence is a BOOT FAILURE, not slow",
                "boot failure, not a slow one" in dead_msg
                and "config parse failed at line 12" in dead_msg,
                dead_msg.splitlines()[0][:150] if dead_msg else "wait_ready returned instead of failing",
            )
        )

        # A COMPLETE, non-growing `.bak` beside a healthy volume is the SUCCESS
        # state for gate 1's migration proof — and it must not also read as
        # "still working". The second real run grew a 5,282,185,216-byte
        # snapshot that then sat unchanged for six minutes while the daemon
        # booted perfectly normally; a presence-based signal called that a
        # migration in flight and the readiness loop waited on the lie. Two
        # probes of an unmoving file, then a verdict.
        quiet_root = root / "quiet-vol"
        vol2 = quiet_root / "state" / "kb-code"
        vol2.mkdir(parents=True)
        quiet_log = root / "quiet-daemon.log"
        quiet_log.write_text("kb-code: ready\n", encoding="utf-8")
        done_bak = vol2 / "index.db.pre-V0044.bak"
        done_bak.write_bytes(b"z" * (1 << 20))
        quiet = Daemon(ctx, 0, "after", Path("/bin/true"), quiet_root, 4791)
        quiet.log = quiet_log
        seen_first, detail_first = quiet.migration_state()
        seen_second, detail_second = quiet.migration_state()
        checks.append(
            (
                "readiness a COMPLETE, non-growing *.bak is not 'still migrating'",
                not seen_first
                and "growth not yet established" in detail_first
                and not seen_second
                and "COMPLETE" in detail_second,
                detail_second or "no snapshot evidence found",
            )
        )
        try:
            quiet.wait_ready(timeout=1.0)
        except GateAbort as e:
            quiet_msg = str(e)
        else:
            quiet_msg = ""
        checks.append(
            (
                "readiness a complete snapshot beside a live daemon gives a BOOT verdict, not a wait",
                "boot failure, not a slow one" in quiet_msg
                and "still inside the gated pre-migration snapshot" not in quiet_msg,
                quiet_msg.splitlines()[0][:150] if quiet_msg else "wait_ready returned instead of failing",
            )
        )
        # …and the same file, GROWING between probes, is in flight. Growth, not
        # presence: that is the only thing that makes a snapshot in progress.
        with done_bak.open("ab") as fh:
            fh.write(b"z" * 8192)
        growing_active, growing_detail = quiet.migration_state()
        checks.append(
            (
                "readiness a *.bak whose size MOVED between probes is GROWING",
                growing_active and "GROWING" in growing_detail,
                growing_detail or "no snapshot evidence found",
            )
        )

    # The 300 s default is not coming back: the first real run failed gate 1 on
    # the BEFORE daemon with exactly that budget, so both defaults must now
    # exceed it, and the after default must additionally fit a 95 min first
    # boot.
    checks.append(
        (
            "readiness no daemon is back on the 300 s budget the real run disproved",
            args.ready_timeout > 300.0 and args.before_ready_timeout > 300.0,
            f"after {_hms(args.ready_timeout)} ({args.ready_timeout:.0f}s), "
            f"before {_hms(args.before_ready_timeout)} ({args.before_ready_timeout:.0f}s)",
        )
    )
    checks.append(
        (
            "readiness the after-daemon default fits a ~95 min first boot",
            args.ready_timeout >= 95 * 60,
            f"--ready-timeout default {_hms(args.ready_timeout)}",
        )
    )
    checks.append(
        (
            "readiness budgets are printed in the log header",
            "{ready_timeout}" in LOG_HEADER and "{before_ready_timeout}" in LOG_HEADER,
            "readiness budget row present",
        )
    )

    # ------------------------------------------------------------------
    # Gate 1's migration proof. The snapshot's NAME is the product's to
    # choose (a V0044 volume's pre-migration snapshot is named for V0044);
    # the CLAIM is gate 1's to check, and it is checked from the receipt the
    # product writes beside the volume.
    # ------------------------------------------------------------------

    print("Gate 1 migration proof (the claim, not the spelling)")
    with tempfile.TemporaryDirectory(prefix="run-gates-snapshot-") as tmp:
        vol = Path(tmp) / "state" / "kb-code"
        vol.mkdir(parents=True)
        db = vol / "index.db"
        db.write_bytes(b"live volume bytes" * 64)
        # Exactly what `backup::take` records for a V0044 -> V0045 crossing.
        good_bak = vol / "index.db.pre-V0044.bak"
        good_bak.write_bytes(b"snapshot bytes" * 64)
        receipt = {
            "schema": "kbc-backup/1",
            "db_path": str(db),
            "backup_path": str(good_bak),
            "volume_epoch": 44,
            "bytes": good_bak.stat().st_size,
            "taken_at": 1758900000,
        }
        (vol / BACKUP_MARKER).write_text(json.dumps(receipt), encoding="utf-8")
        good = read_gated_snapshot(db)
        checks.append(
            (
                "gate 1 a snapshot named for the PRE-migration epoch is accepted",
                good is not None and good.problems_for(db, 44) == [],
                f"{good.path.name if good else 'no receipt'} -> "
                + (str(good.problems_for(db, 44)) if good else "no receipt beside the volume"),
            )
        )
        checks.append(
            (
                "gate 1 the name comes from the receipt, never a hardcoded spelling",
                not any(
                    isinstance(c, str) and "index.db.pre-V0045.bak" in c
                    for c in gate_1.__code__.co_consts
                ),
                f"gate 1 read {good.path.name} from {BACKUP_MARKER} if it exists",
            )
        )
        checks.append(
            (
                "gate 1 a snapshot of a DIFFERENT volume is refused",
                bool(good and any("different volume" in p
                                  for p in good.problems_for(Path(tmp) / "elsewhere" / "index.db", 44))),
                "the receipt's db_path must match the volume under test",
            )
        )
        checks.append(
            (
                "gate 1 a snapshot taken at the WRONG epoch is refused",
                bool(good and good.problems_for(db, 40)),
                "the receipt's volume_epoch must be the pre-migration epoch (44)",
            )
        )
        good_bak.write_bytes(b"short")  # a torn copy
        truncated = read_gated_snapshot(db)
        checks.append(
            (
                "gate 1 a TRUNCATED snapshot is refused (recorded bytes != bytes on disk)",
                bool(truncated and any("truncated" in p for p in truncated.problems_for(db, 44))),
                f"recorded {truncated.recorded_bytes if truncated else '?'} bytes, "
                f"{truncated.bytes_on_disk if truncated else '?'} on disk",
            )
        )
        good_bak.unlink()
        gone = read_gated_snapshot(db)
        checks.append(
            (
                "gate 1 a receipt whose snapshot file is gone is refused",
                bool(gone and any("not on disk" in p for p in gone.problems_for(db, 44))),
                "a receipt whose file was deleted is not a backup",
            )
        )
        (vol / BACKUP_MARKER).unlink()
        checks.append(
            (
                "gate 1 no receipt and no snapshot file = NO migration proof, not a silent pass",
                read_gated_snapshot(db) is None and stray_snapshots(db) == [],
                f"no {BACKUP_MARKER} and no index.db.pre-V*.bak beside the volume",
            )
        )

    # ------------------------------------------------------------------
    # Gate 1's per-review isolation. The pre-upgrade binary panics on
    # `prose_refs.rs` slicing a string at byte 54 — inside `'à'` — the
    # blocking-task wrapper re-panics, and the connection drops. The harness's
    # own `snapshot` would have exited 2 on that one review and taken every
    # other review with it, so the driver reads one review at a time and
    # records the failure instead of aborting on it.
    # ------------------------------------------------------------------

    print("Gate 1 per-review isolation (one bad review is a finding, not an abort)")
    PANIC = (
        "GET /api/reviews/48/comments?ps=1 -> Remote end closed connection without response"
    )

    class _StubHarness:
        """The U0 harness's shape, with review 48 unreadable — the real
        pre-upgrade failure, reproduced without a daemon."""

        SCHEMA = "kbrs-golden-review-snapshot/1"

        @staticmethod
        def _discover_repos(base, token, timeout):
            return ["acme-widgets"]

        @staticmethod
        def _get_json(base, path, token, timeout, **kw):
            if path.startswith("/api/reviews?"):
                return {"reviews": [{"id": 48}, {"id": 65}]}
            return {}

        @staticmethod
        def _snapshot_review(base, token, timeout, repo, review_id):
            if review_id == 48:
                raise RuntimeError(PANIC)
            return {"id": review_id, "repo": repo, "patchsets": [{"ps_number": 1, "files": []}]}

    class _StubDaemon:
        base = "http://127.0.0.1:4790"

    with tempfile.TemporaryDirectory(prefix="run-gates-isolation-") as tmp:
        live = argparse.Namespace(**{**vars(args), "dry_run": False, "out": str(Path(tmp) / "out")})
        live_ctx = Ctx(live)
        out_json = Path(tmp) / "out" / "json" / "gate1-before.json"
        out_json.parent.mkdir(parents=True, exist_ok=True)
        stub_side = sweep_reviews(live_ctx, 1, "before", _StubDaemon(), out_json, _StubHarness())
        written = json.loads(out_json.read_text(encoding="utf-8"))
        checks.append(
            (
                "gate 1 one unreadable review does NOT abort the other reviews",
                stub_side.ok_ids == {65} and stub_side.total == 2 and len(stub_side.failures) == 1,
                f"read {sorted(stub_side.ok_ids)} of {stub_side.total}; "
                f"{[f.review_id for f in stub_side.failures]} unreadable",
            )
        )
        checks.append(
            (
                "gate 1 an unreadable review is named with its HTTP path and the failure verbatim",
                stub_side.failures[0].http_path() == "/api/reviews/48/comments?ps=1"
                and stub_side.failures[0].error == PANIC,
                stub_side.failures[0].describe()[:150] if stub_side.failures else "no failure recorded",
            )
        )
        checks.append(
            (
                "gate 1 the written document holds the READABLE reviews, count and all",
                written["review_count"] == 1
                and [r["id"] for r in written["reviews"]] == [65]
                and written["schema"] == _StubHarness.SCHEMA,
                f"review_count={written['review_count']}, ids={[r['id'] for r in written['reviews']]}",
            )
        )
        # The rules that keep a hostile volume from turning into a pass.
        def _side(side_name, ok_ids, fail_ids, total=None):
            return SideSnapshot(
                side=side_name,
                path=Path(f"{side_name}.json"),
                repos=["acme-widgets"],
                total=total if total is not None else len(ok_ids) + len(fail_ids),
                reads=[
                    ReviewRead("acme-widgets", i, doc={"id": i, "repo": "acme-widgets"})
                    for i in ok_ids
                ]
                + [ReviewRead("acme-widgets", i, error=PANIC) for i in fail_ids],
            )

        found_before = _side("before", [65], [48])
        found_after = _side("after", [65, 48], [])
        picked, problems = compare_sides(found_before, found_after)
        checks.append(
            (
                "gate 1 K unreadable on the before side is a NAMED finding, not a pass-with-caveat",
                picked == [65] and problems == [],
                f"compared {picked}, problems {problems or 'none'}",
            )
        )
        none_comparable = compare_sides(_side("before", [], [48, 65]), _side("after", [48, 65], []))
        checks.append(
            (
                "gate 1 a run where NOTHING could be compared is a FAIL, never a pass",
                none_comparable[0] == []
                and any("NOT ONE review could be read on both sides" in p for p in none_comparable[1]),
                "; ".join(none_comparable[1])[:150] or "no problem reported",
            )
        )
        regressed = compare_sides(_side("before", [65, 48], []), _side("after", [65], [48]))
        checks.append(
            (
                "gate 1 a review the AFTER binary can no longer read is a FAIL",
                any("could not read" in p for p in regressed[1]),
                "; ".join(regressed[1])[:150] or "no problem reported",
            )
        )
        appeared = compare_sides(_side("before", [65], []), _side("after", [65, 99], []))
        checks.append(
            (
                "gate 1 a review that exists only after is reported, not ignored",
                any("99" in p for p in appeared[1]),
                "; ".join(appeared[1])[:150] or "no problem reported",
            )
        )

    # ------------------------------------------------------------------
    # Gate 2 — the daemon's own retry contract. A 503
    # `urn:kb:errors:store-seeding` is a WAIT; anything else is a failure.
    # ------------------------------------------------------------------

    print("Gate 2 store readiness and the daemon's retry contract")
    SEEDING_503 = (
        '{"error":{"code":"urn:kb:errors:store-seeding","hint":null,"message":"the review '
        'store for this repo is seeding; retry in 30s","next":[["wait","the store is '
        'seeding"]]}}'
    )
    checks.append(
        (
            "gate 2 the real seeding 503 is a WAIT, and the delay is the one the daemon names",
            seeding_retry_seconds(SEEDING_503) == 30.0,
            f"retry in {seeding_retry_seconds(SEEDING_503)}s from the message the daemon sent",
        )
    )
    checks.append(
        (
            "gate 2 an explicit `retry_after` in the envelope wins over the message",
            seeding_retry_seconds('{"code":"urn:kb:errors:store-seeding","retry_after":12}')
            == 12.0,
            "retry_after=12s honoured",
        )
    )
    checks.append(
        (
            "gate 2 a seeding code with no delay anywhere falls back to the product's own 30s",
            seeding_retry_seconds('{"code":"urn:kb:errors:store-seeding"}')
            == STORE_SEEDING_FALLBACK_RETRY,
            f"fallback {STORE_SEEDING_FALLBACK_RETRY}s, and the wait says so",
        )
    )
    for label, other in (
        ("another 503", '{"error":{"code":"urn:kb:errors:store-unavailable","message":"try later"}}'),
        ("a conflict", '{"error":{"code":"urn:kb:errors:conflict","message":"review exists"}}'),
        ("a plain failure", "error: the daemon is not listening"),
    ):
        checks.append(
            (
                f"gate 2 {label} with no seeding meaning is still a FAILURE",
                seeding_retry_seconds(other) is None,
                f"no retry honoured for {other[:60]}",
            )
        )
    nap, note = seeding_sleep(3, SEEDING_503, 0.0, 3600.0)
    checks.append(
        (
            "gate 2 a seeding 503 inside the budget sleeps the daemon's own delay and logs it",
            nap == 30.0 and "WAIT, not a failed operation" in note and "sleeping 30s" in note,
            note[:150] or "no wait note produced",
        )
    )
    nap, note = seeding_sleep(3, SEEDING_503, 3590.0, 3600.0)
    checks.append(
        (
            "gate 2 the wait never sleeps PAST the per-operation budget",
            nap == 10.0 and "sleeping 10s" in note,
            f"asked for 30s with 10s of budget left -> slept {nap}s",
        )
    )
    nap, note = seeding_sleep(3, SEEDING_503, 3600.0, 3600.0)
    checks.append(
        (
            "gate 2 only an exhausted budget turns a seeding 503 into a failure",
            nap is None and "budget is spent" in note,
            note[:150] or "a spent budget produced no note",
        )
    )
    nap, note = seeding_sleep(3, '{"error":{"code":"urn:kb:errors:conflict"}}', 0.0, 3600.0)
    checks.append(
        (
            "gate 2 a non-seeding failure is never dressed up as a wait",
            nap is None and note == "",
            f"no wait, no note (nap={nap!r})",
        )
    )
    src2, src3 = inspect.getsource(gate_2), inspect.getsource(gate_3)
    card_read = "/api/repos/{name}/store"
    checks.append(
        (
            "gate 2 and gate 3 share ONE store-readiness helper, not two",
            "wait_for_store_ready(" in src2
            and "wait_for_store_ready(" in src3
            # the per-repo store card is read in the helper only: neither gate
            # carries a readiness loop of its own
            and card_read not in src2
            and card_read not in src3,
            "both gates call wait_for_store_ready, and the store-card read exists only there",
        )
    )

    print()
    width = max(len(c[0]) for c in checks)
    failed = 0
    for name, ok, detail in checks:
        if not ok:
            failed += 1
        print(f"[{'ok  ' if ok else 'FAIL'}] {name.ljust(width)}  {detail}")
    print()
    if failed:
        print(f"{failed} rail check(s) FAILED")
        return EXIT_ABORT
    print(f"all {len(checks)} rail checks behaved as specified")
    return EXIT_OK


# --------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------

GATES: dict[int, Callable[[Ctx], GateResult]] = {
    1: gate_1,
    2: gate_2,
    3: gate_3,
    4: gate_4,
    5: gate_5,
    6: gate_6,
    7: gate_7,
}


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="run_gates.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument("--before-server", required=True, help="pre-upgrade (V0044) kb-code-server binary")
    p.add_argument("--after-server", required=True, help="post-upgrade kb-code-server binary")
    p.add_argument("--before-cli", help="pre-upgrade kb-code CLI (default: kb-code beside --before-server)")
    p.add_argument("--after-cli", help="post-upgrade kb-code CLI (default: kb-code beside --after-server)")
    p.add_argument("--before-root", default="/home/nik/kbc-gates-state", help="'before' KB_HOME root")
    p.add_argument("--after-root", default="/home/nik/kbc-gates-after", help="'after' KB_HOME root")
    p.add_argument("--before-port", type=int, default=4790)
    p.add_argument("--after-port", type=int, default=4791)
    p.add_argument("--out", default="/home/nik/kbc-gates-out", help="output dir (artefacts, logs, plan)")
    p.add_argument("--build-log", default=str(REPO_ROOT / "BUILD-LOG.md"), help="where to write BUILD-LOG.md")
    p.add_argument("--bundle", default="/home/nik/kbc-gates-bundle-2026-09-24")
    p.add_argument("--gate", type=int, action="append", choices=sorted(GATES),
                   help="run only this gate (repeatable); default: all seven")
    p.add_argument("--dry-run", action="store_true", help="print the full plan and execute nothing")
    p.add_argument("--self-test", action="store_true", help="exercise the five safety rails and exit")
    p.add_argument("--no-log", action="store_true", help="do not write BUILD-LOG.md")
    p.add_argument("--render-template", action="store_true", help="write the all-UNRUN BUILD-LOG.md and exit")
    p.add_argument("--invariance-scope", choices=("configured", "store-registered"), default="configured",
                   help="gate 2's clone set: every configured [[repos]] entry (default), or only the [[review.repos]] members")
    p.add_argument("--repo", default=DEFAULT_REPO, help="the repo the live gates operate on")
    p.add_argument("--review", type=int, default=DEFAULT_REVIEW, help="gate 4's review id")
    p.add_argument("--pr", type=int, default=DEFAULT_PR, help="gate 4's GitHub PR number")
    p.add_argument("--ops-pr", type=int, default=15873,
                   help="gate 2's PR for the operation sequence (never gate 4's review, so gate 4 stays pristine)")
    p.add_argument("--expect-tip", default="525631b506e7", help="gate 4: expected ps tip prefix")
    p.add_argument("--expect-base", default="7c1ed0cfdd", help="gate 4: expected ps base prefix")
    p.add_argument("--expect-commits", type=int, default=10)
    p.add_argument("--expect-files", type=int, default=40)
    p.add_argument("--expect-verdict-ps", type=int, default=3, help="gate 4: the ps findings+verdict must stay on")
    p.add_argument("--forge", default=DEFAULT_FORGE, help="GitHub project for gates 4/5")
    p.add_argument("--gh-user", default=DEFAULT_GH_USER, help="the gh-cli credential's pinned user (D12)")
    p.add_argument("--ci-repo", default=DEFAULT_CI_REPO)
    p.add_argument("--ci-pr", type=int, default=DEFAULT_CI_PR,
                   help="gate 7: the PR whose CI IS the gate; 0 means 'not named', which gate 7 reports as UNRUN")
    p.add_argument("--ci-branch", default="rs/final", help="gate 7: the branch that PR must be on")
    p.add_argument("--before-commit", default="", help="commit the before binary was built from (recorded in the log)")
    p.add_argument("--after-commit", default="", help="commit the after binary was built from (recorded in the log)")
    p.add_argument("--ready-timeout", type=float, default=DEFAULT_AFTER_READY_TIMEOUT,
                   help="seconds to wait for the POST-UPGRADE daemon's /api/identity. The default "
                        f"is {_hms(DEFAULT_AFTER_READY_TIMEOUT)} because its first boot on an "
                        "unmigrated volume takes the gated V0045 pre-migration snapshot — a "
                        "whole-volume VACUUM INTO, ~95 min for 5.5 GB")
    p.add_argument("--before-ready-timeout", type=float, default=DEFAULT_BEFORE_READY_TIMEOUT,
                   help="seconds to wait for the PRE-upgrade daemon. It migrates nothing, but the "
                        "first real run found it still not ready after 300 s on the 5.5 GB volume, "
                        f"so the default matches the after budget (default "
                        f"{_hms(DEFAULT_BEFORE_READY_TIMEOUT)}); lower it if you know it boots fast")
    p.add_argument("--skip-migrate-first", action="store_true",
                   help="do not boot the post-upgrade daemon up front to pay the gated V0045 "
                        "snapshot; the first gate that needs the daemon will pay it instead")
    p.add_argument("--store-ready-timeout", type=float, default=3600.0, help="gate 3: seconds to wait for every store to be ready")
    p.add_argument("--http-timeout", type=float, default=60.0, help="per-HTTP-request timeout for the golden snapshot")
    p.add_argument("--rust-log", default=os.environ.get("RUST_LOG", "info"))
    return p


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.self_test:
        return self_test(args)
    if not args.ci_pr:  # `--ci-pr 0` is not a PR number: it means "operator has not named one"
        args.ci_pr = None

    ctx = Ctx(args)
    if args.render_template:
        ctx.template = True
        write_log(ctx, [], EXIT_GATES)
        print(f"template written: every gate reads UNRUN until a real run fills it in ({ctx.log_path})")
        return EXIT_OK
    if args.dry_run:
        print("run_gates.py — DRY RUN. No volume, no daemon, no clone and no file is touched.\n")
        print(f"  before server : {args.before_server}")
        print(f"  after  server : {args.after_server}")
        print(f"  before volume : {ctx.before_root}")
        print(f"  after  volume : {ctx.after_root}")
        print(f"  ports         : {ctx.before_port} / {ctx.after_port} (forbidden: {list(FORBIDDEN_PORTS)})")
        print(f"  output        : {ctx.out}")
        print(f"  build log     : {ctx.log_path}")
        print(f"  gates         : {ctx.selected}")
        print(
            f"  readiness     : after {_hms(args.ready_timeout)} / before "
            f"{_hms(args.before_ready_timeout)} — the after default covers a first boot that "
            f"takes the gated V0045 pre-migration snapshot"
        )
        print(f"  migrate first : {'skipped (--skip-migrate-first)' if args.skip_migrate_first else 'yes'}\n")
        problems: list[str] = []
        ctx.migrate_first()
        for n in ctx.selected:
            try:
                GATES[n](ctx)
            except (GateAbort, RailError) as e:
                problems.append(f"gate {n}: {e}")
        print(ctx.plan_text())
        for note in ctx.notes:
            print(f"note: {note}\n")
        if problems:
            print("PREFLIGHT PROBLEM (a real run would abort here):")
            for p in problems:
                print(f"  {p}")
            return EXIT_ABORT
        print("Dry run complete: the plan above is what a real run would execute, in this order.")
        return EXIT_OK

    ctx.out.mkdir(parents=True, exist_ok=True)
    for root in (ctx.before_root, ctx.after_root):
        if not root.is_dir():
            raise RailError(f"environment root {root} does not exist")
    results: list[GateResult] = []
    try:
        # The V0044 -> V0045 first boot is a whole-volume VACUUM INTO (~95 min on
        # a 5.5 GB volume). Pay it ONCE, here, with the full budget and a
        # report — so no gate can FAIL for being slow, and each gate starts
        # against a warm volume. Same start/stop plumbing as the gates.
        try:
            ctx.migrate_first()
        except GateAbort as e:
            # A readiness failure in the migrate-first phase is a DRIVER abort,
            # not a gate verdict: no gate has run, so no gate may report FAIL.
            print(f"\nABORTED before any gate: {e}", file=sys.stderr)
            if not args.no_log:
                write_log(ctx, results, EXIT_ABORT)
            return EXIT_ABORT
        for n in ctx.selected:
            print(f"→ gate {n}: {GATE_NAMES[n]}", flush=True)
            r = GATES[n](ctx)
            results.append(r)
            print(f"  {r.status}: {r.reason.splitlines()[0] if r.reason else ''}", flush=True)
    except RailError as e:
        print(f"\nABORTED by a safety rail: {e}", file=sys.stderr)
        if not args.no_log:
            write_log(ctx, results, EXIT_ABORT)
        return EXIT_ABORT
    finally:
        ctx.stop_all()

    missing = [n for n in sorted(GATES) if n not in ctx.selected]
    not_pass = [r.number for r in results if not r.ok]
    exit_code = EXIT_OK if not not_pass and not missing else EXIT_GATES
    if not args.no_log:
        write_log(ctx, results, exit_code)
    for n in missing:
        print(f"→ gate {n}: {GATE_NAMES[n]}\n  UNRUN: not selected by --gate", flush=True)
    print(f"\nexit {exit_code}")
    return exit_code


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RailError as e:
        print(f"ABORTED by a safety rail: {e}", file=sys.stderr)
        sys.exit(EXIT_ABORT)
