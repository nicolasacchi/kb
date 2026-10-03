#!/usr/bin/env python3
"""edit_once.py FILE OLD NEW -- replace OLD with NEW in FILE, but ONLY if OLD
occurs EXACTLY once. Zero or several matches exit 3: a canary whose anchor no
longer matches (the code moved) must fail loudly, never pass quietly -- a
canary that applies to nothing "catches" nothing and proves nothing."""
import sys

path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path, encoding="utf-8").read()
n = text.count(old)
if n != 1:
    sys.stderr.write(f"CANARY ANCHOR ERROR: {path}: anchor matched {n} time(s), want exactly 1:\n{old}\n")
    sys.exit(3)
open(path, "w", encoding="utf-8").write(text.replace(old, new, 1))
