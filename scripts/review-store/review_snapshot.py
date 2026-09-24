#!/usr/bin/env python3
"""review_snapshot.py — RS-U0 golden harness, part (a).

Dumps every review on a running kb-code-server daemon (all states, every
configured repo) into ONE canonical, sorted, deterministic JSON document —
review id/repo/state/base_ref, patchsets (tip/base sha, commit count), per
patchset the changed files with old/new blob ids and +/- stats, review
comments with their anchors (file, line/carry-forward result, patchset),
findings with anchors + dispositions, and the verdict + verdict ps. A `diff`
mode compares two such snapshots and prints a readable report, exiting
non-zero on any unexplained difference.

Stdlib only (no third-party deps — BUILDER-RULES / README §9).

    USAGE

    Snapshot every review across every configured repo:
        python3 review_snapshot.py snapshot --base http://127.0.0.1:PORT \\
            -o before.json

    Snapshot one repo only:
        python3 review_snapshot.py snapshot --base http://127.0.0.1:PORT \\
            --repo acme-widgets -o before.json

    Against a non-loopback daemon (bearer token from a FILE or the
    KB_CODE_TOKEN env var — never argv, so it never lands in `ps`/shell
    history/logs):
        python3 review_snapshot.py snapshot --base https://kbc.example \\
            --token-file ~/.config/kb-code/review-store-token -o before.json

    Compare two snapshots (e.g. before/after the V0045 store migration —
    BUILD-BRIEF §3 gate 1):
        python3 review_snapshot.py diff before.json after.json
        # exit 0  = identical (modulo the additive-field allowlist)
        # exit 1  = real differences found (see BUILD-LOG.md — "explained,
        #           or a bug")
        # exit 2  = usage / load error

    A migration is expected to ADD envelope fields (base_mode, base_status,
    objects_state, patchset kind/base_tip_sha, ...) and never remove or
    change an existing one (BUILD-BRIEF §3 gate 1: "the only expected
    differences are new envelope fields"). Pass --allow-new-keys to make
    `diff` ignore keys present only in `after` (still fails on any key
    REMOVED or CHANGED):
        python3 review_snapshot.py diff before.json after.json --allow-new-keys

    Regenerating the committed fixture (see
    crates/kb-code-server/tests/review/rs_u0_golden.rs's own module doc —
    that test drives this exact command against a fresh temp daemon):
        python3 review_snapshot.py snapshot --base http://127.0.0.1:PORT \\
            --repo acme-widgets --normalize-fixture -o fixtures/golden-review-snapshot.json

    NORMALIZATION (--normalize-fixture, OFF by default)

    Off by default: a `snapshot` taken for the real BUILD-BRIEF gates reads
    the SAME rows before and after a migration that touches no timestamp or
    id column, so an unexplained timestamp/id change IS real signal and
    must not be hidden. --normalize-fixture exists ONLY for the committed
    fixture, which is regenerated from scratch against a fresh temp daemon
    (new random ids, wall-clock timestamps) every time golden_snapshot.rs's
    UPDATE_GOLDEN=1 path runs. When set, it rewrites, in this order:

      1. timestamps — any object value under a key that is exactly "at" or
         ends in "_at" (created_at/updated_at/captured_at/published_at/
         content_updated_at/disposition.at/...), plus the literal key
         "last_fetch" (Phase-1 base_status field) -> the literal "<TS>".
      2. the fixture's own review id — this tool's fixture caller always
         creates exactly ONE review, so its integer "id" (and every
         "review_id" reference to it) is rewritten to the literal
         "<REVIEW_ID>".
      3. annotation ids — every string matching ^ann_[0-9a-f]{12}$
         (annotations.rs::new_annotation_id's own shape), WHEREVER it
         appears (an "id" field, a "parent_id"/"annotation_id" reference),
         is rewritten to a stable "<ID:0001>"-style token minted by first
         occurrence order over the whole canonical document — so identity
         (a reply's parent_id still points at its parent's token) survives
         normalization even though the literal random suffix does not.
      4. local filesystem paths — any string starting with "/home/" or
         "/tmp/" -> "<PATH>" (defense in depth; none are expected in this
         document's fields, but the public-repo grep gate treats a leaked
         /home path as a hard failure, so this tool never risks it).

    Every one of the four is applied consistently in `snapshot` (so the
    committed fixture is deterministic across regenerations) — `diff` never
    normalizes on its own; feed it two already-normalized files if that's
    what you want compared.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from typing import Any

SCHEMA = "kbrs-golden-review-snapshot/1"

ANNOTATION_ID_RE = re.compile(r"^ann_[0-9a-f]{12}$")
LOCAL_PATH_RE = re.compile(r"^/(?:home|tmp)/")

# Statuses (the FIRST character of git's name-status code, per
# numstat.rs::FileChange::status) that mean "no blob on that side" — never
# spend an HTTP round trip proving what the status already says.
STATUS_NO_OLD_BLOB = {"A"}
STATUS_NO_NEW_BLOB = {"D"}


# --------------------------------------------------------------------------
# HTTP
# --------------------------------------------------------------------------


class DaemonError(RuntimeError):
    """A non-2xx response the caller must treat as fatal (not a 404 we
    specifically expect and handle, e.g. an old-blob lookup on an added
    file)."""


def _read_token(args: argparse.Namespace) -> str | None:
    if args.token_file:
        with open(os.path.expanduser(args.token_file), "r", encoding="utf-8") as fh:
            return fh.read().strip() or None
    env_name = args.token_env or "KB_CODE_TOKEN"
    val = os.environ.get(env_name)
    return val.strip() if val else None


def _get_json(
    base: str,
    path: str,
    token: str | None,
    timeout: float,
    *,
    ok_404: bool = False,
) -> Any | None:
    url = base.rstrip("/") + path
    req = urllib.request.Request(url, method="GET")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310 (fixed http/https base, operator-supplied)
            body = resp.read()
    except urllib.error.HTTPError as e:
        if ok_404 and e.code == 404:
            return None
        detail = e.read().decode("utf-8", "replace")[:500]
        raise DaemonError(f"GET {path} -> HTTP {e.code}: {detail}") from None
    except urllib.error.URLError as e:
        raise DaemonError(f"GET {path} -> {e.reason}") from None
    if not body:
        return None
    return json.loads(body)


# --------------------------------------------------------------------------
# Snapshot assembly
# --------------------------------------------------------------------------


def _blob_field(v: str | None) -> str | None:
    """review_files's `blob_sha` (and compare/file's `blob_hash`) use `""`
    as the sentinel for "no blob on this side" (a deleted/added file) — fold
    that into a real `null` so the canonical document says what it means."""
    if v is None or v == "":
        return None
    return v


def _old_blob_sha(
    base: str, token: str, timeout: float, repo: str, path: str, base_sha: str
) -> str | None:
    """The base-tip blob id for `path` — NOT exposed by `GET
    /reviews/{id}/files` (it only resolves the tip-side blob via
    `blob_sha_at`). Derived via a self-compare on `GET /api/compare/file`
    (`a=b=base_sha`): that route independently content-hashes each side
    from the bytes it reads (`ingest::git_blob_hash`), so comparing a
    revision to itself is a legitimate way to resolve ONE side's blob id
    through an existing GET route without a daemon change. A 404 means the
    path did not exist at `base_sha` (an added file on some status codes
    other than the plain "A" this tool already skips, e.g. a copy) — treated
    as "no old blob", not a fatal error.
    """
    q = urllib.parse.urlencode(
        {"repo": repo, "path": path, "a": base_sha, "b": base_sha}
    )
    body = _get_json(base, f"/api/compare/file?{q}", token, timeout, ok_404=True)
    if body is None:
        return None
    return _blob_field(body.get("a", {}).get("blob_hash"))


def _snapshot_files(
    base: str, token: str, timeout: float, repo: str, review_id: int, ps_number: int
) -> list[dict]:
    q = urllib.parse.urlencode({"ps": str(ps_number)})
    body = _get_json(base, f"/api/reviews/{review_id}/files?{q}", token, timeout)
    out = []
    for f in body.get("files", []):
        status = f.get("status") or ""
        path = f.get("path") or ""
        old_path = f.get("old_path")
        new_blob = None if status in STATUS_NO_NEW_BLOB else _blob_field(f.get("blob_sha"))
        if status in STATUS_NO_OLD_BLOB:
            old_blob = None
        else:
            lookup_path = old_path or path
            old_blob = _old_blob_sha(
                base, token, timeout, repo, lookup_path, body["base_sha"]
            )
        out.append(
            {
                "path": path,
                "old_path": old_path,
                "status": status,
                "additions": f.get("additions"),
                "deletions": f.get("deletions"),
                "old_blob_sha": old_blob,
                "new_blob_sha": new_blob,
                "open_annotations": f.get("open_annotations"),
            }
        )
    out.sort(key=lambda x: (x["path"], x["old_path"] or ""))
    return out


def _comment_sort_key(c: dict) -> tuple:
    # `(created_at, id)` — the SAME two-column order
    # `store::list_review_annotations`'s SQL uses. Explicit here rather
    # than trusted implicitly: `created_at` is second-granularity
    # (`now_unix()`), so two comments minted within the same second tie on
    # a RANDOM `ann_<hex>` id — reproducible for one fixed set of rows
    # (this is exactly what the server itself does), but not something a
    # caller should have to re-derive by reading the server's SQL. Sorting
    # explicitly here is a no-op against real (already-ordered) server
    # data and just documents the contract this tool relies on.
    return (c.get("created_at") or 0, c.get("id") or "")


def _snapshot_comments(
    base: str, token: str, timeout: float, review_id: int, ps_number: int
) -> list[dict]:
    q = urllib.parse.urlencode({"ps": str(ps_number)})
    body = _get_json(base, f"/api/reviews/{review_id}/comments?{q}", token, timeout)
    groups = body.get("groups", [])
    for g in groups:
        g["comments"] = sorted(g.get("comments", []), key=_comment_sort_key)
        for c in g["comments"]:
            c["replies"] = sorted(c.get("replies", []), key=_comment_sort_key)
    groups.sort(key=lambda g: g.get("path") or "")
    return groups


def _snapshot_findings(
    base: str, token: str, timeout: float, review_id: int, ps_number: int
) -> list[dict]:
    q = urllib.parse.urlencode({"ps": str(ps_number), "include_superseded": "true"})
    body = _get_json(base, f"/api/reviews/{review_id}/findings?{q}", token, timeout)
    findings = body.get("findings", [])
    findings.sort(key=lambda x: x.get("slug") or "")
    return findings


def _snapshot_review(
    base: str, token: str, timeout: float, repo: str, review_id: int
) -> dict:
    review = _get_json(base, f"/api/reviews/{review_id}", token, timeout)
    patchsets_out = []
    for ps in review.get("patchsets", []):
        n = ps["ps_number"]
        patchsets_out.append(
            {
                "ps_number": n,
                "tip_sha_full": ps.get("tip_sha_full"),
                "base_sha_full": ps.get("base_sha_full"),
                "captured_at": ps.get("captured_at"),
                "commit_count": ps.get("commit_count"),
                "files": _snapshot_files(base, token, timeout, repo, review_id, n),
                "comments": _snapshot_comments(base, token, timeout, review_id, n),
                "findings": _snapshot_findings(base, token, timeout, review_id, n),
            }
        )
    patchsets_out.sort(key=lambda p: p["ps_number"])
    return {
        "id": review.get("id"),
        "repo": review.get("repo"),
        "title": review.get("title"),
        "state": review.get("state"),
        "base_ref": review.get("base_ref"),
        "head_ref": review.get("head_ref"),
        "session_id": review.get("session_id"),
        "verdict": review.get("verdict"),
        "verdict_stale": review.get("verdict_stale"),
        "patchsets": patchsets_out,
    }


def _discover_repos(base: str, token: str, timeout: float) -> list[str]:
    body = _get_json(base, "/api/repos", token, timeout)
    return sorted(r["name"] for r in body.get("repos", []))


def build_snapshot(
    base: str, token: str | None, timeout: float, repos: list[str] | None
) -> dict:
    repo_names = repos if repos else _discover_repos(base, token, timeout)
    all_reviews = []
    for repo in repo_names:
        q = urllib.parse.urlencode({"repo": repo})
        body = _get_json(base, f"/api/reviews?{q}", token, timeout)
        for r in body.get("reviews", []):
            all_reviews.append(
                _snapshot_review(base, token, timeout, repo, r["id"])
            )
    all_reviews.sort(key=lambda r: (r["repo"] or "", r["id"] or 0))
    return {
        "schema": SCHEMA,
        "repos": repo_names,
        "review_count": len(all_reviews),
        "reviews": all_reviews,
    }


# --------------------------------------------------------------------------
# Normalization (fixture regeneration only — see module docstring)
# --------------------------------------------------------------------------


def _is_timestamp_key(key: str) -> bool:
    return key == "at" or key.endswith("_at") or key == "last_fetch"


def normalize_for_fixture(doc: dict) -> dict:
    id_tokens: dict[str, str] = {}

    def mint(raw: str) -> str:
        if raw not in id_tokens:
            id_tokens[raw] = f"<ID:{len(id_tokens) + 1:04d}>"
        return id_tokens[raw]

    def walk(node: Any, key: str | None) -> Any:
        if isinstance(node, dict):
            return {k: walk(v, k) for k, v in node.items()}
        if isinstance(node, list):
            return [walk(v, key) for v in node]
        if isinstance(node, str):
            if ANNOTATION_ID_RE.match(node):
                return mint(node)
            if LOCAL_PATH_RE.match(node):
                return "<PATH>"
            if key is not None and _is_timestamp_key(key):
                return "<TS>"
            return node
        if isinstance(node, (int, float)) and key is not None and _is_timestamp_key(key):
            return "<TS>"
        return node

    normalized = walk(doc, None)

    # The fixture creates exactly ONE review; its numeric id (and every
    # review_id back-reference) is rewritten LAST, once we know what it is,
    # rather than guessed from field-name heuristics like the timestamp
    # pass above.
    review_ids = {r["id"] for r in normalized.get("reviews", [])}
    if len(review_ids) == 1:
        (rid,) = review_ids

        def rewrite_review_id(node: Any) -> Any:
            if isinstance(node, dict):
                out = {}
                for k, v in node.items():
                    if k == "id" and v == rid:
                        out[k] = "<REVIEW_ID>"
                    elif k == "review_id" and v == rid:
                        out[k] = "<REVIEW_ID>"
                    else:
                        out[k] = rewrite_review_id(v)
                return out
            if isinstance(node, list):
                return [rewrite_review_id(v) for v in node]
            return node

        normalized = rewrite_review_id(normalized)

    return normalized


# --------------------------------------------------------------------------
# diff mode
# --------------------------------------------------------------------------


class DiffEntry:
    """One structural difference. `kind` is one of:
      - "removed_key"   a dict key present in `before`, absent in `after`
      - "added_key"     a dict key present in `after`, absent in `before`
                         (the ONLY kind --allow-new-keys ever excuses)
      - "list_len"      a list changed length (cardinality — an added or
                         removed file/comment/finding/patchset row) —
                         NEVER excusable, regardless of --allow-new-keys
      - "changed"       a scalar value changed, or the two sides are
                         different JSON types at the same path — NEVER
                         excusable
    """

    __slots__ = ("path", "kind", "before", "after")

    def __init__(self, path: str, kind: str, before: Any, after: Any) -> None:
        self.path = path
        self.kind = kind
        self.before = before
        self.after = after

    def render(self) -> str:
        if self.kind == "removed_key":
            return f"- removed:   {self.path} = {self.before!r}"
        if self.kind == "added_key":
            return f"+ added:     {self.path} = {self.after!r}"
        if self.kind == "list_len":
            return f"# list size: {self.path} len {self.before} -> {self.after}"
        return f"~ changed:   {self.path} = {self.before!r} -> {self.after!r}"


def _diff_node(before: Any, after: Any, path: str) -> list[DiffEntry]:
    """Recursive structural diff. Dicts diff by key (missing/extra/common,
    recursing into common keys); lists diff POSITIONALLY (index i of
    before vs index i of after) and refuse to recurse past a length
    mismatch — the review/comment/finding rows in this document have no
    stable cross-run key to align on other than position within an
    already-sorted list (see the `snapshot` assembly's explicit sort keys),
    so a length change is reported once, precisely, rather than as a
    confusing run of every trailing index looking "added"."""
    if isinstance(before, dict) and isinstance(after, dict):
        entries: list[DiffEntry] = []
        bkeys, akeys = set(before), set(after)
        for k in sorted(bkeys - akeys):
            entries.append(DiffEntry(f"{path}.{k}" if path else k, "removed_key", before[k], None))
        for k in sorted(akeys - bkeys):
            entries.append(DiffEntry(f"{path}.{k}" if path else k, "added_key", None, after[k]))
        for k in sorted(bkeys & akeys):
            entries.extend(_diff_node(before[k], after[k], f"{path}.{k}" if path else k))
        return entries

    if isinstance(before, list) and isinstance(after, list):
        if len(before) != len(after):
            return [DiffEntry(path, "list_len", len(before), len(after))]
        entries = []
        for i, (b, a) in enumerate(zip(before, after)):
            entries.extend(_diff_node(b, a, f"{path}[{i}]"))
        return entries

    if before != after:
        return [DiffEntry(path, "changed", before, after)]
    return []


def diff_snapshots(before: dict, after: dict, allow_new_keys: bool) -> list[str]:
    """Returns a list of human-readable difference lines; empty = no
    (unexplained) differences."""
    entries = _diff_node(before, after, "")
    lines = []
    for e in entries:
        if allow_new_keys and e.kind == "added_key":
            continue
        lines.append(e.render())
    return lines


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def cmd_snapshot(args: argparse.Namespace) -> int:
    token = _read_token(args)
    doc = build_snapshot(args.base, token, args.timeout, args.repo)
    if args.normalize_fixture:
        doc = normalize_for_fixture(doc)
    text = json.dumps(doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    if args.out:
        with open(args.out, "w", encoding="utf-8") as fh:
            fh.write(text)
    else:
        sys.stdout.write(text)
    return 0


def cmd_diff(args: argparse.Namespace) -> int:
    with open(args.before, "r", encoding="utf-8") as fh:
        before = json.load(fh)
    with open(args.after, "r", encoding="utf-8") as fh:
        after = json.load(fh)
    lines = diff_snapshots(before, after, args.allow_new_keys)
    if not lines:
        print("no differences" + (" (modulo new envelope fields)" if args.allow_new_keys else ""))
        return 0
    print(f"{len(lines)} difference(s):")
    for line in lines:
        print(line)
    return 1


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(
        prog="review_snapshot.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    ps = sub.add_parser("snapshot", help="dump every review as one canonical JSON document")
    ps.add_argument("--base", required=True, help="daemon base URL, e.g. http://127.0.0.1:PORT")
    ps.add_argument("--repo", action="append", help="repo name (repeatable); default: every configured repo")
    ps.add_argument("--token-file", help="path to a file holding the bearer token (never via argv)")
    ps.add_argument("--token-env", help="env var holding the bearer token (default KB_CODE_TOKEN)")
    ps.add_argument("--timeout", type=float, default=30.0)
    ps.add_argument("-o", "--out", help="output path (default: stdout)")
    ps.add_argument(
        "--normalize-fixture",
        action="store_true",
        help="rewrite timestamps/ids/paths for a reproducible fixture (see module docstring) — do NOT use for a real before/after gate",
    )
    ps.set_defaults(func=cmd_snapshot)

    pd = sub.add_parser("diff", help="compare two snapshots, exit non-zero on unexplained differences")
    pd.add_argument("before")
    pd.add_argument("after")
    pd.add_argument(
        "--allow-new-keys",
        action="store_true",
        help="ignore dict keys present only in `after` (new envelope fields); never excuses a removed/changed key or a new list element",
    )
    pd.set_defaults(func=cmd_diff)

    args = p.parse_args(argv)
    try:
        return args.func(args)
    except DaemonError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    except (OSError, json.JSONDecodeError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
