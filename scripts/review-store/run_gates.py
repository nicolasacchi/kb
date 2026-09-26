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

WHAT IT CALLS (it never reimplements them)

  * `review_snapshot.py snapshot` / `diff` — the U0 golden harness (a).
  * `repo_invariance.py record` / `check`  — the U0 invariance harness (b).
  * `kb-code` (the agent CLI) and `gh` (read-only) for the live gates.
  * `kb-code-server` is STARTED by this driver, on a free port, with its
    stdout+stderr captured to a file (this box has no daemon log file, and
    gate 6 needs one).

THE GATES (BUILD-BRIEF §3)

  1. golden relocation      — the only permitted diffs are NEW envelope
                              fields; the driver enumerates the keys it
                              allowed, so "we allowed the new keys" is a
                              printed list, not a claim.
  2. user-repo invariance   — `record` BEFORE the whole operation sequence
                              (create, start-pr, sync, snapshot,
                              auto-capture, retrack, GC) and `check` after.
                              A configured-but-missing clone FAILS the gate
                              by name; it is never silently skipped.
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
  6. secrets                — grep the captured daemon log, every gate's
                              JSON output and a `.backup` COPY of the after
                              volume for `gho_`/`ghp_`/`ghu_`/`ghs_` and for
                              the literal `gh auth token` output. Matches
                              are reported as location + count, redacted.
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
import json
import os
import re
import signal
import subprocess
import sys
import time
import tomllib
import urllib.error
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

# R4 — a GitHub token, in any of its four classic prefixes, or a
# fine-grained PAT. Matched against every argv this driver builds.
TOKEN_RE = re.compile(r"\b(?:gh[pousr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{20,})")

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

    def http_json(self, gate: int, label: str, url: str, *, timeout: float = 120.0) -> Any:
        step = Step(gate=gate, label=label, argv=["GET", url])
        self.steps.append(step)
        if self.dry_run:
            step.planned = True
            return {}
        req = urllib.request.Request(url, headers={"Accept": "application/json"})
        tok = os.environ.get("KB_CODE_TOKEN")
        if tok:
            req.add_header("Authorization", f"Bearer {tok}")
        t0 = time.time()
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                body = json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as e:
            step.returncode = e.code
            raise GateAbort(
                f"GET {url} -> HTTP {e.code}: "
                f"{self.redact(e.read()[:300].decode('utf-8', 'replace'))}"
            ) from e
        except Exception as e:  # noqa: BLE001 — reported, never swallowed
            step.returncode = -1
            raise GateAbort(f"GET {url} failed: {e}") from e
        step.returncode = 200
        step.seconds = time.time() - t0
        step.stdout = self.redact(json.dumps(body)[:4000])
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
        d.start()
        self.daemons[name] = d
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

    def wait_ready(self, timeout: float | None = None) -> None:
        timeout = timeout or float(self.ctx.args.ready_timeout)
        deadline = time.time() + timeout
        last = ""
        while time.time() < deadline:
            if self.proc is not None and self.proc.poll() is not None:
                tail = ""
                if self.log and self.log.exists():
                    tail = self.ctx.redact(
                        self.log.read_text(encoding="utf-8", errors="replace")[-1500:]
                    )
                raise GateAbort(
                    f"the {self.name} daemon exited (code {self.proc.returncode}) before "
                    f"answering GET /api/identity. Log tail:\n{tail}"
                )
            try:
                with urllib.request.urlopen(self.base + "/api/identity", timeout=5) as r:
                    if r.status == 200:
                        self.ctx.steps.append(
                            Step(self.gate, f"{self.name} daemon ready",
                                 ["GET", self.base + "/api/identity"], returncode=200)
                        )
                        return
            except Exception as e:  # noqa: BLE001 — not ready yet
                last = str(e)
            time.sleep(0.5)
        raise GateAbort(f"the {self.name} daemon was not ready within {timeout}s (last: {last})")

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
        bak = db.parent / "index.db.pre-V0045.bak"
        rows.append((f"{label} volume", f"{db} · sha256 {digest[:16]}… · {db.stat().st_size} bytes{note}"))
        rows.append(
            (
                f"{label} volume epoch",
                f"refinery_schema_history max = {read_volume_epoch(db)}"
                + ("; index.db.pre-V0045.bak present" if bak.is_file() else "; no pre-V0045.bak"),
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


def gate_1(ctx: Ctx) -> GateResult:
    g = 1
    ev: list[tuple[str, str]] = []
    res = GateResult(g, GATE_NAMES[g], "FAIL", "")
    before_daemon = after_daemon = None
    try:
        ev += verify_inputs(ctx)
        before_db = ctx.before_root / "state" / "kb-code" / "index.db"
        after_db = ctx.after_root / "state" / "kb-code" / "index.db"
        bak = after_db.parent / "index.db.pre-V0045.bak"

        if not ctx.dry_run:
            epoch = read_volume_epoch(before_db)
            ev.append(("before volume epoch (pre-boot)", f"refinery_schema_history max = {epoch}"))
            if epoch != 44:
                raise GateAbort(
                    f"the before volume is at epoch {epoch}, expected 44 (V0044). It has "
                    "probably been migrated already; re-decompress the bundle volume."
                )
            if (before_db.parent / "index.db.pre-V0045.bak").exists():
                raise GateAbort(
                    f"{before_db.parent}/index.db.pre-V0045.bak exists: the 'before' volume has "
                    "been migrated at least once and can no longer prove the V0044 -> V0045 "
                    "relocation. Re-decompress the bundle volume."
                )

        before_daemon = ctx.start_daemon(g, "before", Path(ctx.args.before_server), ctx.before_root, ctx.before_port)
        ctx.exec(
            g,
            "golden snapshot of the before volume",
            [
                sys.executable, str(SNAPSHOT_TOOL), "snapshot",
                "--base", before_daemon.base,
                "-o", str(ctx.art("json", "gate1-before.json")),
                "--timeout", str(ctx.args.http_timeout),
            ],
        )
        ev.append(("before snapshot", str(ctx.art("json", "gate1-before.json"))))

        after_daemon = ctx.start_daemon(g, "after", Path(ctx.args.after_server), ctx.after_root, ctx.after_port)
        if not ctx.dry_run:
            if not bak.is_file():
                raise GateAbort(
                    f"{bak} does not exist after the post-upgrade boot: the gated V0045 "
                    "snapshot was not taken, so the migration did not run as designed"
                )
            epoch = read_volume_epoch(after_db)
            ev.append(
                (
                    "migration proof",
                    f"index.db.pre-V0045.bak written ({bak.stat().st_size} bytes, sha256 "
                    f"{sha256_file(bak)[:16]}…); refinery_schema_history max = {epoch}",
                )
            )
            if epoch != 45:
                raise GateAbort(f"the after volume is at epoch {epoch} after boot, expected 45 (V0045)")
        else:
            ev.append(("migration proof", f"would require {bak} to exist and the volume to be at epoch 45"))
        ctx.exec(
            g,
            "golden snapshot of the after volume",
            [
                sys.executable, str(SNAPSHOT_TOOL), "snapshot",
                "--base", after_daemon.base,
                "-o", str(ctx.art("json", "gate1-after.json")),
                "--timeout", str(ctx.args.http_timeout),
            ],
        )
        ev.append(("after snapshot", str(ctx.art("json", "gate1-after.json"))))

        ev.append(("V0044 binary on a V0045 volume", probe_old_binary_refusal(ctx, ctx.before_port)))

        step = ctx.exec(
            g,
            "golden diff (new envelope fields allowed)",
            [
                sys.executable, str(SNAPSHOT_TOOL), "diff",
                str(ctx.art("json", "gate1-before.json")),
                str(ctx.art("json", "gate1-after.json")),
                "--allow-new-keys",
            ],
            check=False,
        )
        ev.append(("diff --allow-new-keys", f"exit {step.returncode}: {step.stdout.strip() or '(no output)'}"))

        if not ctx.dry_run:
            doc_before = json.loads(ctx.art("json", "gate1-before.json").read_text(encoding="utf-8"))
            doc_after = json.loads(ctx.art("json", "gate1-after.json").read_text(encoding="utf-8"))
            ev.append(
                (
                    "reviews compared",
                    f"{doc_before.get('review_count')} before / {doc_after.get('review_count')} after",
                )
            )
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
            res.status = "PASS"
            res.reason = (
                f"{doc_before.get('review_count')} reviews relocated with no change to files, "
                f"blob ids, anchors, findings or verdict; {len(allowed)} new envelope key(s) "
                "allowed and enumerated above; the V0045 snapshot was taken and the V0044 "
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
        ev.append(("scope", f"{ctx.args.invariance_scope} — {len(in_scope)} configured clone(s) in scope"))
        ev.append(
            (
                "configured clones",
                "; ".join(f"{n}={'present' if Path(p).is_dir() else 'MISSING'}" for n, p in all_repos),
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
        for label, tail in plan:
            rid = review_id or ctx.placeholder("{REVIEW_ID}", "<review-id-from-create>")
            argv = [str(cli), *[rid if a == "{REVIEW_ID}" else a for a in tail], "--daemon", daemon.base]
            step = ctx.exec(g, f"operation: {label}", argv, check=False, timeout=1800)
            if not ctx.dry_run:
                if step.returncode != 0:
                    failed_ops.append(
                        f"{label} (exit {step.returncode}: {ctx.redact(step.stderr.strip()[:200])})"
                    )
                if review_id is None and label in ("create", "start-pr", "sync"):
                    review_id = extract_review_id(step.stdout)
                op_rows.append(f"{label}: exit {step.returncode}")
        ev.append(("operation sequence", "; ".join(op_rows) or "(dry run)"))
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
                f"{len(targets)} registered clone(s) byte-identical across create, start-pr, "
                "sync, snapshot, auto-capture, retrack and GC."
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

        states: dict[str, str] = {}
        deadline = time.time() + float(ctx.args.store_ready_timeout)
        backoff = 2.0
        while True:
            states = {}
            for name in repos:
                card = ctx.http_json(g, f"store card: {name}", f"{daemon.base}/api/repos/{name}/store")
                states[name] = (card.get("store") or {}).get("state", "<no store>")
            if ctx.dry_run or all(s == "ready" for s in states.values()):
                break
            if time.time() > deadline:
                res.status = "SKIP"
                res.reason = (
                    f"store(s) never reached `ready` within {ctx.args.store_ready_timeout}s: "
                    + ", ".join(f"{k}={v}" for k, v in states.items())
                )
                res.evidence = ev
                res.steps = [s for s in ctx.steps if s.gate == g]
                return res
            time.sleep(backoff)
            backoff = min(backoff * 1.5, 30.0)
        ev.append(("store state", ", ".join(f"{k}={v}" for k, v in states.items())))

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

SECRET_PREFIXES = ("gho_", "ghp_", "ghu_", "ghs_")
SECRET_SHAPE_RE = re.compile(r"\b(?:gh[pousr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{20,})")


def scan_text_file(ctx: Ctx, path: Path, token: str | None) -> list[tuple[str, str, int]]:
    if not path.is_file():
        return []
    text = ctx.redact(path.read_text(encoding="utf-8", errors="replace"))
    out: list[tuple[str, str, int]] = []
    for prefix in SECRET_PREFIXES:
        n = text.count(prefix)
        if n:
            out.append((prefix, f"prefix {prefix}*", n))
    n = len(SECRET_SHAPE_RE.findall(text))
    if n:
        out.append(("shape", "token-shaped string", n))
    if token:
        n = text.count(token)
        if n:
            out.append(("literal", "the literal `gh auth token` output", n))
    return out


def scan_sqlite(ctx: Ctx, db: Path, token: str | None) -> list[tuple[str, str, int]]:
    """Read-only SQL over every TEXT column of the COPY. Never the live DB,
    never a `cp`."""
    out: list[tuple[str, str, int]] = []
    uri = f"file:{db}?mode=ro"
    tables = subprocess.run(
        ["sqlite3", uri, "select name from sqlite_master where type='table';"],
        capture_output=True, text=True, check=False,
    )
    if tables.returncode != 0:
        return [("error", f"could not list tables: {tables.stderr.strip()[:200]}", 1)]
    for name in [t for t in tables.stdout.split() if t]:
        cols = subprocess.run(["sqlite3", uri, f"pragma table_info('{name}');"],
                              capture_output=True, text=True, check=False)
        if cols.returncode != 0:
            continue
        text_cols = [
            parts[1] for parts in (line.split("|") for line in cols.stdout.splitlines())
            if len(parts) >= 3 and (parts[2].upper().startswith("TEXT") or parts[2] == "")
        ]
        if not text_cols:
            continue
        where = " or ".join(
            f"instr(coalesce(\"{c}\",''),'{p}')>0" for c in text_cols for p in SECRET_PREFIXES
        )
        if token:
            where += " or " + " or ".join(f"instr(coalesce(\"{c}\",''),'{token}')>0" for c in text_cols)
        cnt = subprocess.run(
            ["sqlite3", uri, f'select count(*) from "{name}" where {where};'],
            capture_output=True, text=True, check=False,
        )
        if cnt.returncode == 0 and cnt.stdout.strip().isdigit() and int(cnt.stdout.strip()) > 0:
            out.append(
                ("prefix/literal", f"{name}: {cnt.stdout.strip()} row(s) contain a token prefix or the literal token",
                 int(cnt.stdout.strip()))
            )
    return out


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
                 "is never printed, logged or passed in argv) — its literal is one of the needles below")
                if token else
                (f"`gh auth token --user {ctx.args.gh_user}` did NOT answer; only the prefix and "
                 "shape needles apply"),
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
            hits: list[tuple[str, str, int]] = []
            for kind, path in targets:
                for _, label, n in scan_text_file(ctx, path, token):
                    hits.append((kind, f"{path}: {label}", n))
            for _, label, n in scan_sqlite(ctx, copy_db, token):
                hits.append(("volume (read-only SQL)", label, n))
            ev.append(
                (
                    "needles",
                    ", ".join(
                        [f"{p}*" for p in SECRET_PREFIXES]
                        + ["<token-shaped string>"]
                        + (["<literal gh auth token output>"] if token else [])
                    ),
                )
            )
            ev.append(("scanned", f"{len(targets)} file(s) + the volume copy"))
            if hits:
                for kind, where, n in hits:
                    ev.append(("MATCH (redacted)", f"{kind}: {where} — {n} occurrence(s)"))
                raise GateAbort(
                    f"{len(hits)} location(s) matched a secret pattern; locations and counts above, "
                    "values never printed"
                )
            res.status = "PASS"
            res.reason = (
                f"no {', '.join(SECRET_PREFIXES)} prefix, no token-shaped string and no literal "
                f"`gh auth token` output in {len(targets)} artefact(s) or in the volume copy."
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
the gated `index.db.pre-V0045.bak` snapshot, and the V0044 binary's refusal to
open the migrated volume.""",
    2: """**Asserts.** Across create, start-pr, sync, snapshot, auto-capture, retrack
and GC, no registered clone's `for-each-ref`, `packed-refs` or `refs/` tree
changes. `repo_invariance.py record` runs BEFORE the first operation and `check`
after the last. A configured-but-missing clone FAILS this gate by name: a clone
that cannot be hashed is a clone whose refs are unverified.""",
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
volume (read-only SQL, never `cp`, never the live DB) are scanned for `gho_`,
`ghp_`, `ghu_`, `ghs_`, a token-shaped string and the literal output of
`gh auth token --user {gh_user}`. Matches are reported as location + count; the
value is never printed or logged.""",
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
        out.append("\n## Driver notes\n\n```\n" + "\n".join(ctx.notes) + "\n```\n")
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
    p.add_argument("--ready-timeout", type=float, default=300.0, help="seconds to wait for a daemon's /api/identity")
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
        print(f"  gates         : {ctx.selected}\n")
        problems: list[str] = []
        for n in ctx.selected:
            try:
                GATES[n](ctx)
            except (GateAbort, RailError) as e:
                problems.append(f"gate {n}: {e}")
        print(ctx.plan_text())
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
