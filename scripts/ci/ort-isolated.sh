#!/usr/bin/env bash
# Invariant #26 gate -- ONNX Runtime (ort / ort-sys / fastembed) is reachable
# from `kb-embedder` ONLY. Compile-free: it RESOLVES (cargo metadata / cargo
# tree), it never builds.
#
# Why a gate exists: until 2026-08 the Dockerfile built the daemon and the
# sidecar in ONE `cargo build -p kb-cli ... -p kb-embedder ...`. Cargo unifies
# features across a single invocation, so kb-core's `local-embedder` leaked into
# the shipped `kb` binary (filed as a P0; split into two RUNs on 2026-08-13).
# That is the #26(b) unification hazard: one cargo invocation that mixes
# kb-embedder with anything else (or builds every member without excluding it)
# shares one kb-core rlib with the feature on.
#
# Two checks:
#   reachability  every workspace member except the owner is resolved ALONE
#                 (`cargo tree -p <member>`, normal+build+dev edges, all
#                 targets) at default features and then at --all-features
#                 (the feature home only at default features); none may list
#                 a banned crate. A positive control proves the owner still
#                 reaches every banned crate, so a rename cannot pass vacuously.
#   topology      every cargo build/test/clippy/... command line in the
#                 Dockerfiles, workflows, justfile and scripts is linted:
#                   (a) kb-embedder must be alone in its invocation;
#                   (b) --workspace needs --exclude kb-embedder, and a bare
#                       root command (no -p / --workspace / --manifest-path)
#                       selects every member;
#                   (c) local-embedder / --all-features only with -p kb-core
#                       or -p kb-embedder.
#                 A tripwire fails the run when too few commands were linted.
#
# NOT proven here: symbols in a linked binary (LTO / link behaviour), cargo
# arguments that come from shell variables, cargo commands outside the scanned
# files.
#
# Usage: ort-isolated.sh [--manifest-path <Cargo.toml>] [--root <dir>]
#                        [--only reachability|topology]
# Exit: 0 clean, 1 violation, 2 usage/tool error (2 wins over 1).
#
# SELFTEST-ONLY environment knobs: ORT_ISOLATED_OWNER (kb-embedder),
# ORT_ISOLATED_BANNED (ort,ort-sys,fastembed), ORT_ISOLATED_FEATURE_HOME
# (kb-core), ORT_ISOLATED_MIN_CARGO_LINES (10).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
python3 -I - "$here" "$@" <<'PY'
import glob, json, os, re, shlex, subprocess, sys

here = sys.argv[1]
args = sys.argv[2:]
manifest = os.path.join(here, "..", "..", "Cargo.toml")
root = os.path.join(here, "..", "..")
only = None
i = 0
while i < len(args):
    a = args[i]
    if a in ("--manifest-path", "--root", "--only") and i + 1 < len(args):
        v = args[i + 1]
        if a == "--manifest-path":
            manifest = v
        elif a == "--root":
            root = v
        else:
            if v not in ("reachability", "topology"):
                print(f"ort-isolated: bad --only value {v!r}", file=sys.stderr)
                sys.exit(2)
            only = v
        i += 2
    else:
        print(f"ort-isolated: unknown argument {a!r}", file=sys.stderr)
        sys.exit(2)

OWNER = os.environ.get("ORT_ISOLATED_OWNER", "kb-embedder")
BANNED = [b for b in os.environ.get("ORT_ISOLATED_BANNED", "ort,ort-sys,fastembed").split(",") if b]
HOME = os.environ.get("ORT_ISOLATED_FEATURE_HOME", "kb-core")
# The floor exists because a wrong --root or a broken glob would otherwise
# lint zero commands and pass vacuously.
MIN_CARGO_LINES = 10
MIN_LINES = int(os.environ.get("ORT_ISOLATED_MIN_CARGO_LINES", MIN_CARGO_LINES))

violations = []
fatal = []
summary = []


def run(cmd):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True)
    except FileNotFoundError:
        fatal.append("cargo not found on PATH")
        return None
    if p.returncode != 0:
        fatal.append(f"`{' '.join(cmd)}` exited {p.returncode}: {p.stderr.strip()}")
        return None
    return p.stdout


def reachability():
    out = run(["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked",
               "--manifest-path", manifest])
    if out is None:
        return
    try:
        members = [p["name"] for p in json.loads(out)["packages"]]
    except Exception as e:
        fatal.append(f"unparsable cargo metadata JSON: {e}")
        return
    if OWNER not in members:
        fatal.append(f"owner {OWNER} is not a workspace member")
        return
    others = [m for m in members if m != OWNER]
    if not others:
        fatal.append("no workspace member other than the owner")
        return

    def tree(member, all_features):
        cmd = ["cargo", "tree", "--locked", "--manifest-path", manifest, "-p", member,
               "--target", "all", "-e", "normal,build,dev", "--prefix", "none",
               "--format", "{p}"]
        if all_features:
            cmd.append("--all-features")
        o = run(cmd)
        if o is None:
            return None
        return {ln.split()[0] for ln in o.splitlines() if ln.strip()}

    n_default = n_all = 0
    for m in others:
        names = tree(m, False)
        if names is not None:
            n_default += 1
            for b in BANNED:
                if b in names:
                    violations.append(f"  - {m}: reaches {b} (default)")
        if m == HOME:
            continue
        names = tree(m, True)
        if names is not None:
            n_all += 1
            for b in BANNED:
                if b in names:
                    violations.append(f"  - {m}: reaches {b} (all-features)")
    ctl = tree(OWNER, False)
    if ctl is not None:
        missing = [b for b in BANNED if b not in ctl]
        if missing:
            fatal.append(f"control failed: {OWNER} no longer reaches {','.join(missing)}; "
                         "the gate would pass vacuously (renamed crate?)")
    summary.append(f"{n_default} members at default features, {n_all} at all features, "
                   f"control {OWNER} reaches {','.join(BANNED)}")


CARGO_RE = re.compile(r"\bcargo\s+(\+\S+\s+)?(build|b|test|t|clippy|nextest|install|check|c|run|r|doc|bench|rustc|mutants)\b")


def scan_files():
    pats = ["Dockerfile*", "justfile", ".github/workflows/*.yml", ".github/actions/**/*.yml",
            ".github/actions/**/*.yaml", "scripts/**/*.sh"]
    seen = []
    for pat in pats:
        for f in sorted(glob.glob(os.path.join(root, pat), recursive=True)):
            if os.path.isfile(f) and not os.path.basename(f).startswith("ort-isolated") and f not in seen:
                seen.append(f)
    return seen


def logical_lines(path):
    yml = path.endswith((".yml", ".yaml"))
    out = []
    pending = None
    for no, raw in enumerate(open(path, encoding="utf-8", errors="replace").read().splitlines(), 1):
        s = raw.strip()
        if s.startswith("#"):
            continue
        if yml and re.match(r"^-?\s*name:\s", s):
            continue
        line = re.sub(r"\s#\s.*$", "", raw)
        if pending is not None:
            start, text = pending
            text += " " + line.strip()
        else:
            start, text = no, line
        if text.rstrip().endswith("\\"):
            pending = (start, text.rstrip()[:-1])
        else:
            pending = None
            out.append((start, text))
    if pending is not None:
        out.append(pending)
    return out


def topology():
    files = scan_files()
    n_cmds = 0
    for path in files:
        rel = os.path.relpath(path, root)
        for no, line in logical_lines(path):
            for cmd in re.split(r"&&|\|\||;|\|", line):
                m = CARGO_RE.search(cmd)
                if not m:
                    continue
                n_cmds += 1
                sub = m.group(2)
                text = cmd[m.start():]
                try:
                    toks = shlex.split(text)
                except ValueError:
                    toks = text.split()
                # drop `cargo`, optional +toolchain, and the subcommand
                toks = toks[1:]
                if toks and toks[0].startswith("+"):
                    toks = toks[1:]
                toks = toks[1:]
                if "--" in toks:
                    toks = toks[:toks.index("--")]
                P, EXCL, feats = set(), set(), []
                WS = allf = manifest_flag = False
                j = 0
                while j < len(toks):
                    t = toks[j]
                    nxt = toks[j + 1] if j + 1 < len(toks) else None
                    if t in ("-p", "--package") and nxt is not None:
                        P.add(nxt); j += 2; continue
                    if t.startswith("--package="):
                        P.add(t.split("=", 1)[1])
                    elif t.startswith("-p=") :
                        P.add(t[3:])
                    elif t.startswith("-p") and not t.startswith("--") and len(t) > 2:
                        P.add(t[2:])
                    elif t in ("--workspace", "--all"):
                        WS = True
                    elif t == "--exclude" and nxt is not None:
                        EXCL.add(nxt); j += 2; continue
                    elif t.startswith("--exclude="):
                        EXCL.add(t.split("=", 1)[1])
                    elif t in ("--features", "-F") and nxt is not None:
                        feats += re.split(r"[,\s]+", nxt); j += 2; continue
                    elif t.startswith("--features="):
                        feats += re.split(r"[,\s]+", t.split("=", 1)[1])
                    elif t.startswith("-F") and not t.startswith("--") and len(t) > 2:
                        feats += re.split(r"[,\s]+", t[2:])
                    elif t == "--all-features":
                        allf = True
                    elif t == "--manifest-path" or t.startswith("--manifest-path="):
                        manifest_flag = True
                    j += 1
                where = f"{rel}:{no}:"
                if OWNER in P and (len(P) > 1 or WS):
                    violations.append(f"  - {where} (a) {OWNER} must be built/tested/linted in its own cargo invocation (invariant #26(b))")
                if WS and OWNER not in EXCL:
                    violations.append(f"  - {where} (b) --workspace without --exclude {OWNER} builds the ORT sidecar into the shared feature resolution (invariant #26(b))")
                elif not P and not WS and not manifest_flag and sub != "install" and "$" not in text:
                    violations.append(f"  - {where} (b) bare cargo command at the virtual root selects every member including {OWNER} (invariant #26(b))")
                enables = any(v == "local-embedder" or v.endswith("/local-embedder") for v in feats) \
                    or (allf and (WS or not P or HOME in P))
                if enables and not (P == {HOME} or (P == {OWNER} and not WS)):
                    violations.append(f"  - {where} (c) local-embedder / --all-features outside -p {HOME} or -p {OWNER} (invariant #26(b))")
    if n_cmds < MIN_LINES:
        fatal.append(f"tripwire: only {n_cmds} cargo commands linted in {len(files)} files (floor {MIN_LINES}); wrong --root or broken glob?")
        return
    summary.append(f"{n_cmds} cargo commands in {len(files)} files linted")


if only in (None, "reachability"):
    reachability()
if only in (None, "topology"):
    topology()

for v in violations:
    print(v)
for f in fatal:
    print(f"ort-isolated: ERROR: {f}", file=sys.stderr)
if fatal:
    sys.exit(2)
if violations:
    print(f"ort-isolated: {len(violations)} violation(s) of invariant #26", file=sys.stderr)
    sys.exit(1)
print("ort-isolated: OK (" + "; ".join(summary) + ")")
PY
