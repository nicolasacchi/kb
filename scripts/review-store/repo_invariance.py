#!/usr/bin/env python3
"""repo_invariance.py — RS-U0 golden harness, part (b).

Proves BUILD-BRIEF §3 gate 2 ("no registered user clone's refs change"):
for a list of registered user-repo clones, hash `git for-each-ref`,
`packed-refs`, and every file under the git common dir's `refs/` tree,
before and after a review-store operation. `record` writes a baseline;
`check` recomputes and diffs against it, naming exactly what changed.

Works against a WORKING TREE OR A BARE CLONE — resolves each repo's own
`git rev-parse --git-common-dir` first (worktree-safe, same convention the
Phase-1 design docs use throughout, e.g. README §6's default-branch probe),
so a repo passed as a linked worktree is checked at its real shared ref
store, not a worktree-private stub.

Stdlib only (no third-party deps — BUILDER-RULES / README §9).

    USAGE

    Record a baseline for one or more repos (paths, not names — this tool
    never talks to a daemon, only to git):
        python3 repo_invariance.py record \\
            --repo /path/to/rails-01 --repo /path/to/rails-02 \\
            -o baseline.json

    Or discover every repo path from a running kb-code-server's own
    `GET /api/repos` (so the operator doesn't have to hand-list them):
        python3 repo_invariance.py record --daemon-base http://127.0.0.1:PORT \\
            -o baseline.json

    Recompute and diff, after the operation under test:
        python3 repo_invariance.py check --baseline baseline.json
        # exit 0 = every repo's refs are byte-identical to the baseline
        # exit 1 = at least one repo changed — the report below names
        #          exactly which ref/path, added/removed/changed
        # exit 2 = usage / read error (e.g. a repo path from the baseline
        #          no longer exists)

    `check` re-reads each repo PATH recorded in the baseline (not a fresh
    --repo list) — the whole point is "did THESE clones change", so there
    is no separate repo-selection step at check time.

    WHAT IS HASHED, PER REPO

      1. `for_each_ref`  — sha256 of `git for-each-ref --sort=refname
         --format='%(refname) %(objectname)'` output, PLUS the raw sorted
         line list (so `check` can name exactly which ref moved/appeared/
         vanished, not just "it changed").
      2. `packed_refs`   — "absent" when the common dir has no
         `packed-refs` file, else sha256 of its bytes PLUS the raw
         non-comment lines (same reasoning as (1)).
      3. `refs_tree`     — a sorted (path, sha256-of-content) listing for
         every regular file under `<common-dir>/refs/` (paths are POSIX,
         relative to `refs/`), PLUS an aggregate sha256 over that whole
         canonical listing for a cheap one-line pass/fail per repo.

    A repo with no `refs/` directory at all (freshly `git init --bare`,
    everything packed) records an empty `refs_tree` list — not an error.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import urllib.request
from pathlib import Path
from typing import Any

SCHEMA = "kbrs-repo-invariance/1"


class InvarianceError(RuntimeError):
    pass


def _run_git(cwd: Path, args: list[str]) -> str:
    proc = subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise InvarianceError(
            f"git -C {cwd} {' '.join(args)} failed ({proc.returncode}): {proc.stderr.strip()}"
        )
    return proc.stdout


def _common_dir(repo_path: str) -> Path:
    """Resolves the repo's real shared ref store — worktree-safe (the
    Phase-1 design docs' own convention, e.g. README §6's `ls-remote
    --symref` default-branch probe uses the same `--git-common-dir`
    reasoning)."""
    out = _run_git(Path(repo_path), ["rev-parse", "--git-common-dir"]).strip()
    p = Path(out)
    if not p.is_absolute():
        p = (Path(repo_path) / p).resolve()
    return p


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _sha256_text(text: str) -> str:
    return _sha256_bytes(text.encode("utf-8"))


def _for_each_ref(repo_path: Path) -> dict:
    out = _run_git(
        repo_path, ["for-each-ref", "--sort=refname", "--format=%(refname) %(objectname)"]
    )
    lines = [line for line in out.splitlines() if line]
    return {"sha256": _sha256_text("\n".join(lines)), "lines": lines}


def _packed_refs(common_dir: Path) -> dict:
    p = common_dir / "packed-refs"
    if not p.exists():
        return {"state": "absent", "sha256": None, "lines": []}
    data = p.read_bytes()
    lines = [
        line
        for line in data.decode("utf-8", "replace").splitlines()
        if line and not line.startswith("#")
    ]
    return {"state": "present", "sha256": _sha256_bytes(data), "lines": lines}


def _refs_tree(common_dir: Path) -> dict:
    refs_dir = common_dir / "refs"
    entries: list[dict] = []
    if refs_dir.is_dir():
        for p in sorted(refs_dir.rglob("*")):
            if p.is_file():
                rel = p.relative_to(refs_dir).as_posix()
                entries.append({"path": rel, "sha256": _sha256_bytes(p.read_bytes())})
    entries.sort(key=lambda e: e["path"])
    aggregate = _sha256_text(
        "\n".join(f"{e['path']}\t{e['sha256']}" for e in entries)
    )
    return {"sha256": aggregate, "entries": entries}


def snapshot_repo(repo_path: str) -> dict:
    common_dir = _common_dir(repo_path)
    return {
        "repo_path": repo_path,
        "common_dir": str(common_dir),
        "for_each_ref": _for_each_ref(common_dir),
        "packed_refs": _packed_refs(common_dir),
        "refs_tree": _refs_tree(common_dir),
    }


def _discover_repo_paths(daemon_base: str, timeout: float) -> list[str]:
    url = daemon_base.rstrip("/") + "/api/repos"
    with urllib.request.urlopen(url, timeout=timeout) as resp:  # noqa: S310 (fixed http/https base, operator-supplied)
        body = json.load(resp)
    return [r["path"] for r in body.get("repos", [])]


# --------------------------------------------------------------------------
# diff
# --------------------------------------------------------------------------


def _diff_lines(before: list[str], after: list[str]) -> list[str]:
    b, a = set(before), set(after)
    out = [f"    - {line}" for line in sorted(b - a)]
    out += [f"    + {line}" for line in sorted(a - b)]
    return out


def _diff_entries(before: list[dict], after: list[dict]) -> list[str]:
    b = {e["path"]: e["sha256"] for e in before}
    a = {e["path"]: e["sha256"] for e in after}
    out = []
    for path in sorted(set(b) - set(a)):
        out.append(f"    - {path}")
    for path in sorted(set(a) - set(b)):
        out.append(f"    + {path}")
    for path in sorted(set(a) & set(b)):
        if a[path] != b[path]:
            out.append(f"    ~ {path} (content changed)")
    return out


def diff_repo(before: dict, after: dict) -> list[str]:
    lines: list[str] = []
    if before["for_each_ref"]["sha256"] != after["for_each_ref"]["sha256"]:
        lines.append("  for-each-ref changed:")
        lines.extend(_diff_lines(before["for_each_ref"]["lines"], after["for_each_ref"]["lines"]))
    if before["packed_refs"]["sha256"] != after["packed_refs"]["sha256"]:
        if before["packed_refs"]["state"] != after["packed_refs"]["state"]:
            lines.append(
                f"  packed-refs: {before['packed_refs']['state']} -> {after['packed_refs']['state']}"
            )
        else:
            lines.append("  packed-refs changed:")
            lines.extend(_diff_lines(before["packed_refs"]["lines"], after["packed_refs"]["lines"]))
    if before["refs_tree"]["sha256"] != after["refs_tree"]["sha256"]:
        lines.append("  refs/ tree changed:")
        lines.extend(_diff_entries(before["refs_tree"]["entries"], after["refs_tree"]["entries"]))
    return lines


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def cmd_record(args: argparse.Namespace) -> int:
    repo_paths = list(args.repo or [])
    if args.daemon_base:
        repo_paths.extend(_discover_repo_paths(args.daemon_base, args.timeout))
    if not repo_paths:
        raise InvarianceError("no repos given (--repo, or --daemon-base with a non-empty fleet)")
    repos = [snapshot_repo(p) for p in repo_paths]
    doc = {"schema": SCHEMA, "repos": repos}
    text = json.dumps(doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    if args.out:
        with open(args.out, "w", encoding="utf-8") as fh:
            fh.write(text)
    else:
        sys.stdout.write(text)
    return 0


def cmd_check(args: argparse.Namespace) -> int:
    with open(args.baseline, "r", encoding="utf-8") as fh:
        baseline = json.load(fh)
    any_changed = False
    for before in baseline["repos"]:
        after = snapshot_repo(before["repo_path"])
        lines = diff_repo(before, after)
        if lines:
            any_changed = True
            print(f"CHANGED: {before['repo_path']}")
            for line in lines:
                print(line)
        else:
            print(f"unchanged: {before['repo_path']}")
    return 1 if any_changed else 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(
        prog="repo_invariance.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    pr = sub.add_parser("record", help="hash for-each-ref + packed-refs + refs/ tree for a baseline")
    pr.add_argument("--repo", action="append", help="repo path (working tree or bare clone; repeatable)")
    pr.add_argument("--daemon-base", help="also discover repo paths from GET /api/repos on this daemon")
    pr.add_argument("--timeout", type=float, default=30.0)
    pr.add_argument("-o", "--out", help="output path (default: stdout)")
    pr.set_defaults(func=cmd_record)

    pc = sub.add_parser("check", help="recompute + diff against a recorded baseline")
    pc.add_argument("--baseline", required=True)
    pc.set_defaults(func=cmd_check)

    args = p.parse_args(argv)
    try:
        return args.func(args)
    except InvarianceError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    except (OSError, json.JSONDecodeError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
