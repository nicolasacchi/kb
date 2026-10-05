#!/usr/bin/env bash
# Print how many search hits a captured `kb search ... --json` output holds.
#
# The shape comes from crates/kb-cli/src/commands/search.rs (`struct Hit`,
# `print_hits_json`): {"hits":[{"id","kb_category","path","title"},...],"ms","source"},
# pretty-printed. A hit is an entry with a string "id"; there is NO
# `source_relative` field. Only grep: not every leg's container has jq. Error
# text, an empty `"hits": []`, or a missing file all count 0.
f="${1:?usage: first-run-hits.sh <captured-output-file>}"
[ -f "$f" ] || { echo 0; exit 0; }
grep -Eo '"id": *"[0-9A-Za-z_-]+"' "$f" | wc -l | tr -d ' '
