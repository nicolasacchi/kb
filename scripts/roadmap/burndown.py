#!/usr/bin/env python3
"""Roadmap burn-down: join git-log commit PhaseIDs to the 2026-07 roadmap.

Reads docs/research/assets/kb-impl-plan-2026-07/_phases.json (the canonical
428-item, 92-phase roadmap: tracks D/F/G/L/N/O/R/S, phase ids like "G7",
"L4"...) plus `git log` subjects, and reports which roadmap items have
shipped, are in flight, or are untouched.

WHY THIS IS NOT A NAIVE "(XX9) token" REGEX MATCH
--------------------------------------------------
This repo's commit convention is `type(crate): summary (PhaseID)`, but
PhaseID grammars COLLIDE across the repo's history: the 2026-07 roadmap
introduced its OWN track+number scheme (D/F/G/L/N/O/R/S), while commits
also carry tokens from many OTHER, older, unrelated schemes -- the v0.24
plan's own R1-R4/X1-X3/T1-T3, the Z-launch program's Z1-Z7, the sessions
design's W/B/U/TS letters, several now-nameless older milestones (P, C, H,
E, A, M, K, V ...), and this very gap-closure wave's own GC-* ids. Some of
those letters are SPELLED IDENTICALLY to roadmap track letters (v0.24's R1
vs this roadmap's Track R phase R1; a decade of old "(G2)"/"(D1)" tokens vs
roadmap tracks G/D). A bare token match would silently misattribute all of
that history to the new roadmap.

Disambiguation strategy (see `classify_commit` below):
  1. Content match is authoritative, and DELIBERATELY narrow: a verbatim
     slug-substring hit, or an exact 3-consecutive-word phrase shared with
     the item's title (order-preserving; e.g. a commit subject containing
     the same "graph report verb" run as a roadmap item's title), counts
     as a content match -- independent of whatever token the commit
     carries. A looser bag-of-words/word-rarity pass was tried and
     rejected: in a single-codebase commit history the same domain words
     ("sessions", "artifact", "embedder"...) recur constantly regardless
     of which specific roadmap item is meant, and it over-matched ~2/3 of
     the roadmap as "shipped" -- see the ItemIndex.content_matches
     docstring for the concrete measurement that killed that approach.
  2. A bare token that IS a real roadmap phase id (from _phases.json,
     e.g. "G7", "L4") is accepted as a (weaker) direct phase hit ONLY
     when either (a) a content match into that same phase corroborates
     it, or (b) the commit postdates the roadmap's own synthesis date
     (2026-07-10) -- before that date the roadmap did not exist, so an
     older same-lettered commit is definitionally a different, older
     scheme, never this roadmap.
  3. Tokens known from the roadmap's own S1.5 glossary of external
     phase-letter systems (v0.24 R1-R4/X1-X2/T1-T3, Z-launch Z1-Z7,
     sessions W1-W5/B2,B3,B5,B7/U2/TS1-TS5) are always reported
     separately as "external" context. Where the glossary (or a strong
     content match) gives an equivalence to a roadmap slug -- e.g. v0.24
     "R3" mapped its own retention item, whose text was carried into this
     roadmap verbatim as `r3-retention` -- that equivalence is recorded
     too, but the shipped attribution comes from the content match, not
     the bare token.
  4. This wave's own `GC-<letter><n>` ids are recognized and reported
     separately (own-wave tracking); they are not roadmap track ids.

Usage:
    scripts/roadmap/burndown.py [--since 2026-06-01] [--repo-root PATH]

Stdlib only. Writes the same report to stdout and to
docs/research/assets/kb-impl-plan-2026-07/burndown-2026-07-10.md.
"""
from __future__ import annotations

import argparse
import collections
import datetime
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ASSETS = ROOT / "docs/research/assets/kb-impl-plan-2026-07"
PHASES_JSON = ASSETS / "_phases.json"
OUT_MD = ASSETS / "burndown-2026-07-10.md"

ROADMAP_SYNTHESIS_DATE = datetime.date(2026, 7, 10)

TRACK_ORDER = ["D", "F", "G", "L", "N", "O", "R", "S"]

TRAILING_TOKEN_RE = re.compile(r"\(([A-Za-z][A-Za-z0-9_.-]*)\)\s*$")
CONVENTIONAL_PREFIX_RE = re.compile(r"^\w+\([^)]*\):\s*|^\w+:\s*")
PHASE_ID_RE = re.compile(r"^[A-Z]\d{1,2}$")
GC_RE = re.compile(r"^GC-[A-Z]\d+$", re.IGNORECASE)

# Roadmap section 1.5's own glossary of external phase-letter systems.
# token -> (scheme label, roadmap slug this token is known to map to (or
# None), short note). See docs/research/kb-research-implementation-roadmap-
# 2026-07.html#glossary for the source text this table transcribes.
EXTERNAL_SCHEMES: dict[str, tuple[str, str | None, str]] = {
    "R1": ("v0.24 milestone", None,
           "storage-actor panic supervision + typed arrow decode (shipped 2ff019da)"),
    "R2": ("v0.24 milestone", None,
           "delete-cascade + reconcile orphan sweep (shipped 4e6357fd)"),
    "R3": ("v0.24 milestone", "r3-retention", "retention/pruning"),
    "R4": ("v0.24 milestone", "r4-zip-extract-caps-and-lance-escape-helper",
           "security-fix bundle"),
    "X1": ("v0.24 milestone", None, "configurable indexable-extension map"),
    "X2": ("v0.24 milestone", None, "per-file exclusion store + ingest gate"),
    "T1": ("v0.24 milestone", None, "kb-tui parity gap-closers"),
    "T2": ("v0.24 milestone", "t2-remove-kb-tui", "remove kb-tui crate entirely"),
    "T3": ("v0.24 milestone", None, "books-closing, runs last"),
    "Z1": ("Z-launch program", None, "repo-truth pass (shipped d4f3130f)"),
    "Z2": ("Z-launch program", None,
           "non-goals constitution + invariant cap (shipped 130728e5)"),
    "Z3": ("Z-launch program", None, "README repositioning (shipped d28ffbf9)"),
    "Z4": ("Z-launch program", None, "/inbox route (shipped)"),
    "Z5": ("Z-launch program", None, "kb import claude-history (shipped dc3c970e)"),
    "Z6": ("Z-launch program", "z6-release-zero-remaining-steps",
           "Release Zero packaging, mostly shipped"),
    "Z7": ("Z-launch program", "z7-launch-execution-ruling",
           "launch execution itself (needs operator go/no-go)"),
    "W1": ("sessions/workflow design", None, "capture-layer prerequisite"),
    "W2": ("sessions/workflow design", None, "capture-layer prerequisite"),
    "W3": ("sessions/workflow design", None, "capture-layer prerequisite"),
    "W4": ("sessions/workflow design", None, "capture-layer prerequisite"),
    "W5": ("sessions/workflow design", None, "capture-layer prerequisite"),
    "B2": ("session-memory deep-review defect", None, "numbered defect"),
    "B3": ("session-memory deep-review defect", None, "numbered defect"),
    "B5": ("session-memory deep-review defect", None, "numbered defect"),
    "B7": ("session-memory deep-review defect", None, "numbered defect"),
    "U2": ("session-memory deep-review", None,
           "pre-shipped cascade/retained-state policy-conflict flag"),
    "TS1": ("truthful-sessions milestone", None, "~ roadmap S1 (collision, per glossary)"),
    "TS2": ("truthful-sessions milestone", None, "~ roadmap S2 (collision, per glossary)"),
    "TS3": ("truthful-sessions milestone", None, "~ roadmap S3 (collision, per glossary)"),
    "TS4": ("truthful-sessions milestone", None, "truthful-sessions milestone"),
    "TS5": ("truthful-sessions milestone", None, "truthful-sessions milestone"),
}

# Pure grammatical glue -- dropped even from PHRASE (n-gram) construction,
# since it never carries topical signal and its adjacency is coincidental.
# Also includes this repo's own REST/CLI scaffolding words ("kb", "api",
# HTTP verbs, "add"/"new"): these recur across dozens of unrelated route/
# verb titles ("GET /api/kb/{kb}/...", "add kb ...", "add `kb ...` verb"),
# so leaving them in would let phrase-matching key off boilerplate rather
# than the topic -- see the module docstring's worked false-positive
# example (an "add kb config"-shaped phrase collision).
FUNCTION_WORDS = {
    "a", "an", "the", "of", "for", "with", "and", "or", "per", "is", "as",
    "at", "by", "from", "over", "to", "in", "on", "into", "this", "that",
    "its", "so", "not", "no", "be", "are", "was", "were", "it",
    "kb", "api", "get", "post", "put", "patch", "delete", "add", "new",
}

# Additionally dropped from bag-of-words matching (but these DO stay in the
# phrase/n-gram pass above -- e.g. "kb" matters for the phrase "kb graph
# report", even though alone it's meaningless): scaffolding + generic verbs
# that appear in a large fraction of this repo's commit subjects/titles.
STOPWORDS = FUNCTION_WORDS | {
    "kb", "kb-core", "kb-cli", "kb-server", "kb-tui", "kb-embedder", "plugins",
    "feat", "fix", "fixes", "fixed", "docs", "chore", "ci", "test", "tests",
    "add", "adds", "added", "adding", "update", "updates", "updated",
    "implement", "implements", "implemented", "land", "lands", "landed",
    "wire", "wires", "wired", "ship", "ships", "shipped", "introduce",
    "introduced", "support", "supports", "new", "now", "own",
}


def raw_words(text: str) -> list[str]:
    """Lowercase word-tokenize, keeping order, dropping only function words."""
    words = re.findall(r"[a-z0-9_]+", text.lower())
    return [w for w in words if w not in FUNCTION_WORDS]


def normalize_words(text: str) -> set[str]:
    """Bag-of-words form for overlap scoring: also drops stopwords/scaffolding."""
    words = re.findall(r"[a-z0-9_]+", text.lower())
    return {w for w in words if len(w) >= 4 and w not in STOPWORDS}


def ngrams(words: list[str], n: int) -> set[tuple[str, ...]]:
    return {tuple(words[i : i + n]) for i in range(len(words) - n + 1)}


def strip_commit_subject(subject: str) -> tuple[str, str | None]:
    """Return (core subject with prefix+trailing token removed, token or None)."""
    token = None
    m = TRAILING_TOKEN_RE.search(subject)
    core = subject
    if m:
        token = m.group(1)
        core = subject[: m.start()].rstrip()
    core = CONVENTIONAL_PREFIX_RE.sub("", core, count=1)
    return core, token


def load_phases() -> list[dict]:
    return json.loads(PHASES_JSON.read_text())


def load_git_log(since: str | None, repo_root: pathlib.Path) -> list[dict]:
    cmd = ["git", "-C", str(repo_root), "log", "--date=format:%Y-%m-%d %H:%M",
           "--pretty=format:%H\x1f%ad\x1f%s"]
    if since:
        cmd.append(f"--since={since}")
    out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    commits = []
    for line in out.splitlines():
        if not line.strip():
            continue
        h, d, s = line.split("\x1f", 2)
        commits.append({"hash": h[:8], "date": d, "subject": s})
    return commits


class ItemIndex:
    """Flattened roadmap items with precomputed content-match token sets."""

    def __init__(self, phases: list[dict]):
        self.phase_by_id: dict[str, dict] = {p["id"]: p for p in phases}
        self.phase_id_set = set(self.phase_by_id)
        self.items: list[dict] = []  # each: slug, title, status, phase_id, track, words, trigrams
        self.item_by_slug: dict[str, dict] = {}
        for p in phases:
            for it in p["items"]:
                title_words = raw_words(it["title"])
                rec = {
                    "slug": it["slug"],
                    "title": it["title"],
                    "status": it["status"],
                    "phase_id": p["id"],
                    "track": p["track"],
                    "words": normalize_words(it["title"]) | normalize_words(
                        it["slug"].replace("-", " ")
                    ),
                    "trigrams": ngrams(title_words, 3),
                }
                self.items.append(rec)
                self.item_by_slug[it["slug"]] = rec
        # Trigram item-document-frequency: how many roadmap items contain
        # each 3-word run. Boilerplate runs (route-path scaffolding like
        # "api kb kb" from repeated "/api/kb/{kb}/..." title text, or
        # recurring doc-filename mentions like "self host md") show up
        # across several unrelated items and must NOT be treated as a
        # content match; a genuinely distinctive run is item-specific.
        self.trigram_df: collections.Counter = collections.Counter()
        for it in self.items:
            for t in it["trigrams"]:
                self.trigram_df[t] += 1

    def phase_items(self, phase_id: str) -> list[dict]:
        return [it for it in self.items if it["phase_id"] == phase_id]

    def content_matches(self, commit_words: set[str], commit_raw_words: list[str],
                         subject_raw: str) -> list[tuple[dict, str]]:
        """Return [(item, reason)] for items this commit's content matches.

        Deliberately narrow (see module docstring point 1): a bag-of-words /
        semantic-similarity pass over 886 commits x 428 items over-matches
        badly in a single-codebase corpus that reuses the same domain words
        ("sessions", "artifact", "embedder"...) constantly -- an early
        version of this script using word-overlap + "long/underscored word"
        rarity flagged 266/428 items as shipped, which contradicts the
        roadmap's own authored status counts (only 8 implemented-unmerged +
        33 partial). So content matching here is restricted to two signals
        that are effectively verbatim, not similarity-scored:
          - the item's slug (kebab-case) appears as a literal substring of
            the commit subject; or
          - an exact 3-consecutive-word run (order-preserving, after
            dropping pure function words) is shared with the item's title
            -- e.g. commit "...kb graph report verb" contains the same
            "graph report verb" run as item L5's title; or
          - a shared word is a coined underscored identifier (e.g.
            `graph_boost`, `spawn_blocking`) -- these read like designed
            names/flags rather than prose, so one verbatim hit is as
            reliable as a slug match, without needing 3-in-a-row.
        Coincidental collisions on either signal are vanishingly unlikely.
        """
        subject_slug_form = subject_raw.lower()
        commit_trigrams = ngrams(commit_raw_words, 3)
        commit_word_set = set(commit_raw_words)
        hits = []
        for it in self.items:
            if len(it["slug"]) >= 10 and it["slug"] in subject_slug_form.replace(" ", "-"):
                hits.append((it, "verbatim-slug"))
                continue
            shared_tri = [
                t for t in (commit_trigrams & it["trigrams"])
                if self.trigram_df[t] <= 2 and len(set(t)) == 3  # no self-repeating words (URL-path noise)
            ]
            if shared_tri:
                phrase = " ".join(shared_tri[0])
                hits.append((it, f"fuzzy-phrase({phrase})"))
                continue
            shared_ids = {w for w in (commit_word_set & it["words"]) if "_" in w}
            if shared_ids:
                hits.append((it, f"verbatim-identifier({','.join(sorted(shared_ids))})"))
        return hits


def classify_commit(commit: dict, idx: ItemIndex) -> dict:
    """Attribute one commit to roadmap items / external-scheme context / wave ids."""
    subject = commit["subject"]
    core, token = strip_commit_subject(subject)
    words = normalize_words(core)
    core_raw_words = raw_words(core)

    is_gc = bool(token and GC_RE.match(token))
    external = EXTERNAL_SCHEMES.get(token.upper()) if token and not is_gc else None

    # Content matching runs on every commit (it is what disambiguates the
    # colliding schemes), but its acceptance rules (phrase/rare-word/broad
    # ratio, see ItemIndex.content_matches) are what keep the false-positive
    # rate down -- not a restriction on which commits get scanned.
    matched_items = idx.content_matches(words, core_raw_words, subject)

    commit_date = datetime.datetime.strptime(commit["date"], "%Y-%m-%d %H:%M").date()
    postdates_roadmap = commit_date >= ROADMAP_SYNTHESIS_DATE

    direct_phase_hit = None
    if token and not is_gc and token.upper() not in EXTERNAL_SCHEMES:
        if token in idx.phase_id_set:
            same_phase_content = [it for it, _ in matched_items if it["phase_id"] == token]
            if same_phase_content or postdates_roadmap:
                direct_phase_hit = token
                if not same_phase_content:
                    # weak: token-only, no content corroboration, but dated
                    # on/after the roadmap's own synthesis -- accept every
                    # item in that phase as a candidate (rare in practice;
                    # _phases.json items are 1-21 per phase).
                    for it in idx.phase_items(token):
                        matched_items.append((it, "phase-id-token(date-gated)"))

    return {
        "commit": commit,
        "token": token,
        "is_gc": is_gc,
        "external": external,
        "direct_phase_hit": direct_phase_hit,
        "matched_items": matched_items,
    }


def build_report(commits: list[dict], idx: ItemIndex) -> dict:
    classifications = [classify_commit(c, idx) for c in commits]

    # item slug -> list of (commit, reason)
    shipped_by_slug: dict[str, list[tuple[dict, str]]] = collections.defaultdict(list)
    for cl in classifications:
        for it, reason in cl["matched_items"]:
            shipped_by_slug[it["slug"]].append((cl["commit"], reason))

    external_hits = [cl for cl in classifications if cl["external"]]
    gc_hits = [cl for cl in classifications if cl["is_gc"]]

    per_track = {}
    for track in TRACK_ORDER:
        items = [it for it in idx.items if it["track"] == track]
        shipped = [it for it in items if it["slug"] in shipped_by_slug]
        in_flight = [
            it for it in items
            if it["slug"] not in shipped_by_slug and it["status"] in ("partial", "implemented-unmerged")
        ]
        untouched = [it for it in items if it not in shipped and it not in in_flight]
        per_track[track] = {
            "total": len(items),
            "shipped": shipped,
            "in_flight": in_flight,
            "untouched": untouched,
        }

    return {
        "per_track": per_track,
        "shipped_by_slug": shipped_by_slug,
        "external_hits": external_hits,
        "gc_hits": gc_hits,
    }


def render_report(report: dict, since: str | None, n_commits: int) -> str:
    lines = []
    w = lines.append
    w("# Roadmap burn-down — 2026-07-10")
    w("")
    w(f"Generated by `scripts/roadmap/burndown.py`. Commits scanned: {n_commits}"
      + (f" (since {since})" if since else " (full history)") + ".")
    w("Source roadmap: `docs/research/assets/kb-impl-plan-2026-07/_phases.json` "
      "(428 items / 92 phases / 8 tracks).")
    w("")
    w("## Per-track counts")
    w("")
    w("| Track | Total | Shipped | In-flight | Untouched |")
    w("|---|---:|---:|---:|---:|")
    grand = {"total": 0, "shipped": 0, "in_flight": 0, "untouched": 0}
    for track in TRACK_ORDER:
        pt = report["per_track"][track]
        w(f"| {track} | {pt['total']} | {len(pt['shipped'])} | {len(pt['in_flight'])} | {len(pt['untouched'])} |")
        grand["total"] += pt["total"]
        grand["shipped"] += len(pt["shipped"])
        grand["in_flight"] += len(pt["in_flight"])
        grand["untouched"] += len(pt["untouched"])
    w(f"| **all** | **{grand['total']}** | **{grand['shipped']}** | "
      f"**{grand['in_flight']}** | **{grand['untouched']}** |")
    w("")

    w("## Shipped items (commit-attributed)")
    w("")
    if not report["shipped_by_slug"]:
        w("_None matched._")
    else:
        w("| Track/Phase | Slug | Commit(s) | Match reason |")
        w("|---|---|---|---|")
        # stable order: by track, phase, slug
        slug_to_item = {}
        for track in TRACK_ORDER:
            for it in report["per_track"][track]["shipped"]:
                slug_to_item[it["slug"]] = it
        for slug in sorted(report["shipped_by_slug"], key=lambda s: (slug_to_item[s]["phase_id"], s)):
            it = slug_to_item[slug]
            hits = report["shipped_by_slug"][slug]
            commit_str = ", ".join(sorted({f"`{c['hash']}`" for c, _ in hits}))
            reason_str = "; ".join(sorted({r for _, r in hits}))
            w(f"| {it['phase_id']} | `{slug}` | {commit_str} | {reason_str} |")
    w("")

    w("## External-scheme tokens (context only — NOT roadmap track hits)")
    w("")
    w("Per the roadmap's own §1.5 glossary: these tokens belong to schemes that "
      "predate/are external to the 2026-07 roadmap, even where the letter collides "
      "with a roadmap track.")
    w("")
    if not report["external_hits"]:
        w("_None found in the scanned range._")
    else:
        w("| Commit | Token | Scheme | Roadmap-slug equivalence | Note |")
        w("|---|---|---|---|---|")
        for cl in sorted(report["external_hits"], key=lambda c: c["commit"]["date"]):
            scheme, slug, note = cl["external"]
            slug_str = f"`{slug}`" if slug else "—"
            w(f"| `{cl['commit']['hash']}` | {cl['token']} | {scheme} | {slug_str} | {note} |")
    w("")

    w("## This wave's own ids (GC-*, gap-closure-2026-07 — not roadmap track ids)")
    w("")
    if not report["gc_hits"]:
        w("_None found in the scanned range._")
    else:
        w("| Commit | Token | Subject |")
        w("|---|---|---|")
        for cl in sorted(report["gc_hits"], key=lambda c: c["commit"]["date"]):
            w(f"| `{cl['commit']['hash']}` | {cl['token']} | {cl['commit']['subject']} |")
    w("")
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--since", default=None, help="git log --since= filter (default: full history)")
    ap.add_argument("--repo-root", default=str(ROOT), help="repo root (default: this script's repo)")
    args = ap.parse_args()

    repo_root = pathlib.Path(args.repo_root).resolve()
    phases = load_phases()
    idx = ItemIndex(phases)
    commits = load_git_log(args.since, repo_root)

    report = build_report(commits, idx)
    text = render_report(report, args.since, len(commits))

    sys.stdout.write(text)
    OUT_MD.write_text(text)
    print(f"\n[written to {OUT_MD}]", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
