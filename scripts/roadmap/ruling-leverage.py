#!/usr/bin/env python3
"""Ruling-leverage report (GC-A5).

Orders the R-track (operator-ruling) work items by how much live work their
resolution unblocks, so the operator can answer the highest-leverage
questions first.

Inputs (stdlib json only, no third-party deps):
  docs/research/assets/kb-impl-plan-2026-07/_all_workitems.json  -- every
      work item ever extracted from the roadmap research corpus.
  docs/research/assets/kb-impl-plan-2026-07/_phases.json          -- the
      phase/track grouping (id, track letter, label, items[]).
  docs/research/assets/kb-impl-plan-2026-07/_live_items.json      -- optional
      pre-filtered {slug: item} dict already excluding shipped + absorbed
      slugs. Used when present (matches _all_workitems minus shipped minus
      absorbed exactly, verified by hand 2026-07-10); otherwise this script
      derives the same filter itself from _all_shipped.json /
      _absorbed_slugs.json so it degrades gracefully if that cache is
      missing.

Method:
  1. Build the "live" item set: every work item slug that is neither in
     _all_shipped.json (shipped == done) nor _absorbed_slugs.json (merged
     into another item, so the slug itself no longer independently exists).
  2. Build the reverse dependsOn graph restricted to live items: for a live
     item Y listing dep D in its `dependsOn`, add an edge D -> Y ("D unblocks
     Y") when D resolves to a live slug. `dependsOn` entries are sometimes
     annotated free text ("slug (external -- ...)") rather than a bare slug;
     these are normalized (strip a trailing parenthetical) before matching,
     and left unresolved (skipped) if they still don't match a known slug --
     the graph only asserts edges it can verify against real slugs.
  3. For every work item belonging to an R-track phase (R1..R9 in
     _phases.json -- the operator-ruling tracks), compute:
       - direct dependents: items with a direct edge from the ruling slug.
       - transitive dependents: full reachable set via the reverse graph
         (BFS/DFS, excluding the ruling itself), i.e. everything that is
         blocked on this ruling either directly or through a chain of other
         live items.
       - max blocked priority: the highest-urgency `priority` (P0 best) among
         the transitive dependents; "-" if none.
  4. Emit a markdown table sorted by transitive-dependent count descending
     (ties broken by direct count desc, then slug asc).

Usage:
  python3 scripts/roadmap/ruling-leverage.py [--data-dir DIR] [--out FILE]

With no arguments, reads from and writes to the canonical
docs/research/assets/kb-impl-plan-2026-07/ location relative to the repo
root (this file's grandparent's parent -- scripts/roadmap/../.. ).
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from collections import deque
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_DATA_DIR = REPO_ROOT / "docs" / "research" / "assets" / "kb-impl-plan-2026-07"

PRIORITY_RANK = {"P0": 0, "P1": 1, "P2": 2, "P3": 3}

# Strip a trailing parenthetical annotation some dependsOn entries carry,
# e.g. "release-provenance-attestation (share the attest-build-provenance
# step ...)" -> "release-provenance-attestation".
_PAREN_SUFFIX_RE = re.compile(r"\s*\([^)]*\)\s*$")


def normalize_dep(raw: str) -> str:
    return _PAREN_SUFFIX_RE.sub("", raw).strip()


def load_json(path: Path):
    with path.open(encoding="utf-8") as fh:
        return json.load(fh)


def build_live_items(data_dir: Path) -> dict:
    """Return {slug: item} for every item that is neither shipped nor absorbed.

    Prefers the pre-computed _live_items.json cache when present (verified
    2026-07-10 to equal all_workitems - shipped - absorbed exactly); falls
    back to deriving it from the three source files so the script still
    works if that cache is ever removed.
    """
    live_cache = data_dir / "_live_items.json"
    if live_cache.exists():
        return load_json(live_cache)

    all_items = load_json(data_dir / "_all_workitems.json")
    shipped_path = data_dir / "_all_shipped.json"
    absorbed_path = data_dir / "_absorbed_slugs.json"

    shipped_slugs = set()
    if shipped_path.exists():
        shipped_slugs = {it["slug"] for it in load_json(shipped_path)}

    absorbed_slugs = set()
    if absorbed_path.exists():
        absorbed_slugs = set(load_json(absorbed_path))

    return {
        it["slug"]: it
        for it in all_items
        if it["slug"] not in shipped_slugs and it["slug"] not in absorbed_slugs
    }


def build_reverse_graph(live: dict) -> dict:
    """reverse[D] = [Y, ...] for every live Y whose dependsOn resolves to D."""
    reverse: dict[str, list[str]] = {}
    for slug, item in live.items():
        for raw_dep in item.get("dependsOn") or []:
            dep = normalize_dep(raw_dep)
            if dep in live:
                reverse.setdefault(dep, []).append(slug)
    return reverse


def transitive_dependents(start: str, reverse: dict) -> set:
    seen: set = set()
    q = deque(reverse.get(start, []))
    seen.update(reverse.get(start, []))
    while q:
        cur = q.popleft()
        for nxt in reverse.get(cur, []):
            if nxt not in seen:
                seen.add(nxt)
                q.append(nxt)
    seen.discard(start)
    return seen


def max_priority(slugs: set, live: dict) -> str:
    best = None
    for s in slugs:
        p = live.get(s, {}).get("priority")
        rank = PRIORITY_RANK.get(p)
        if rank is None:
            continue
        if best is None or rank < best[0]:
            best = (rank, p)
    return best[1] if best else "-"


def r_track_phases(data_dir: Path):
    phases = load_json(data_dir / "_phases.json")
    return [p for p in phases if p.get("track") == "R"]


def one_liner(title: str, limit: int = 100) -> str:
    title = " ".join(title.split())
    if len(title) > limit:
        title = title[: limit - 1].rstrip() + "…"
    # markdown table cells can't contain a literal pipe
    return title.replace("|", "\\|")


def build_report(data_dir: Path) -> str:
    live = build_live_items(data_dir)
    reverse = build_reverse_graph(live)
    phases = r_track_phases(data_dir)

    rows = []
    for phase in phases:
        for it in phase["items"]:
            slug = it["slug"]
            item = live.get(slug)
            if item is None:
                # Ruling itself shipped/absorbed since the roadmap snapshot;
                # still report it (0 dependents) rather than silently drop it.
                item = it
            direct = sorted(reverse.get(slug, []))
            transitive = transitive_dependents(slug, reverse)
            rows.append(
                {
                    "slug": slug,
                    "phase": phase["id"],
                    "title": item.get("title", ""),
                    "direct": len(direct),
                    "transitive": len(transitive),
                    "max_priority": max_priority(transitive, live),
                }
            )

    rows.sort(key=lambda r: (-r["transitive"], -r["direct"], r["slug"]))

    lines = []
    lines.append("# Ruling-leverage report (GC-A5)")
    lines.append("")
    lines.append(
        "R-track (operator-ruling) work items ordered by how much live work "
        "their resolution unblocks. Generated by "
        "`scripts/roadmap/ruling-leverage.py` from "
        "`docs/research/assets/kb-impl-plan-2026-07/{_all_workitems,_phases,"
        "_live_items,_all_shipped,_absorbed_slugs}.json`. \"Direct\" = live "
        "items whose `dependsOn` names this ruling; \"transitive\" = the full "
        "reachable set through the live-item dependsOn graph (BFS, "
        "excluding the ruling itself); \"max blocked priority\" = the "
        "highest-urgency `priority` (P0 best) among the transitive set, "
        "`-` if none."
    )
    lines.append("")
    lines.append(f"Total R-track rulings: {len(rows)}. Total live work items: {len(live)}.")
    lines.append("")
    lines.append("| Ruling slug | Phase | Question | Direct | Transitive | Max blocked priority |")
    lines.append("|---|---|---|---:|---:|---|")
    for r in rows:
        lines.append(
            f"| `{r['slug']}` | {r['phase']} | {one_liner(r['title'])} "
            f"| {r['direct']} | {r['transitive']} | {r['max_priority']} |"
        )
    lines.append("")
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--data-dir", type=Path, default=DEFAULT_DATA_DIR)
    ap.add_argument(
        "--out",
        type=Path,
        default=DEFAULT_DATA_DIR / "ruling-leverage-2026-07-10.md",
    )
    args = ap.parse_args(argv)

    report = build_report(args.data_dir)
    args.out.write_text(report, encoding="utf-8")
    print(f"wrote {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
