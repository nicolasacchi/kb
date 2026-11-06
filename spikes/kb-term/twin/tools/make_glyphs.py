#!/usr/bin/env python3
"""Generate glyphs.json, the synthetic glyph set for the width comparison. Stdlib only."""
import json, sys, os

def cp(*a):
    return "".join(chr(x) if isinstance(x, int) else x for x in a)

G = []
def add(cls, name, text):
    G.append({"id": f"{cls}/{name}", "cls": cls, "text": text,
              "cps": " ".join("U+%04X" % ord(c) for c in text)})

for n, t in [("a", "a"), ("tilde", "~"), ("space", " "), ("digit", "7"), ("box", "─"), ("block", "█"),
             ("pm-ambiguous", "±"), ("greek", "Ω")]:
    add("narrow", n, t)
for n, t in [("han", "漢"), ("hiragana", "ひ"), ("hangul", "한"), ("fullwidth-A", "Ａ"),
             ("halfwidth-kata", "ｱ"), ("ideographic-space", "　"), ("cjk-ext-b", "\U00020000")]:
    add("wide", n, t)
for n, t in [("e-acute-comb", "é"), ("a-two-marks", "ạ̈"), ("lone-combining", "́"),
             ("lone-combining-enclosing", "⃝"), ("devanagari-ksi", "क्षि"),
             ("thai-mai-taikhu", "ก็"), ("arabic-fatha", "بَ"), ("hangul-jamo", "한")]:
    add("combining", n, t)
for n, t in [("heart-vs15", "❤︎"), ("smile-vs15", "☺︎"), ("watch-vs15", "⌚︎"),
             ("sun-vs15", "☀︎"), ("one-vs15", "1︎"), ("grin-vs15", "\U0001f600︎")]:
    add("vs15", n, t)
for n, t in [("heart-vs16", "❤️"), ("smile-vs16", "☺️"), ("check-vs16", "✔️"),
             ("copyright-vs16", "©️"), ("keycap-1", "1️⃣"), ("heart-bare", "❤"),
             ("smile-bare", "☺"), ("arrow-vs16", "↔️"), ("digit-vs16", "7️")]:
    add("vs16", n, t)
for n, t in [("family-mfg", "\U0001f468‍\U0001f469‍\U0001f467"),
             ("family-4", "\U0001f468‍\U0001f469‍\U0001f467‍\U0001f466"),
             ("technologist", "\U0001f469‍\U0001f4bb"),
             ("couple-heart", "\U0001f469‍❤️‍\U0001f468"),
             ("rainbow-flag", "\U0001f3f3️‍\U0001f308"),
             ("black-cat", "\U0001f408‍⬛"), ("lone-zwj", "‍"),
             ("zwj-between-latin", "a‍b")]:
    add("zwj", n, t)
for n, t in [("thumb-medium", "\U0001f44d\U0001f3fd"), ("wave-dark", "\U0001f44b\U0001f3ff"),
             ("handshake-2tone", "\U0001fac1\U0001f3fb"), ("lone-modifier", "\U0001f3fd"),
             ("people-holding-hands", "\U0001f9d1\U0001f3fb‍\U0001f91d‍\U0001f9d1\U0001f3ff"),
             ("raised-hand-vs16-tone", "✋\U0001f3fc")]:
    add("skin", n, t)
for n, t in [("IT", "\U0001f1ee\U0001f1f9"), ("JP", "\U0001f1ef\U0001f1f5"), ("single-RI", "\U0001f1ee"),
             ("three-RI", "\U0001f1ee\U0001f1f9\U0001f1fa"), ("four-RI", "\U0001f1ee\U0001f1f9\U0001f1ef\U0001f1f5"),
             ("england-tag", "\U0001f3f4\U000e0067\U000e0062\U000e0065\U000e006e\U000e0067\U000e007f"),
             ("pirate-zwj", "\U0001f3f4‍☠️")]:
    add("flag", n, t)
for n, t, u in [("grin", "\U0001f600", "6.1"), ("rocket", "\U0001f680", "6.0"),
                ("partying", "\U0001f973", "11"), ("ballet-shoes", "\U0001fa70", "12"),
                ("smiling-tear", "\U0001f972", "13"), ("melting", "\U0001fae0", "14"),
                ("hand-heart", "\U0001faf6", "14"), ("wheel", "\U0001f6de", "14"),
                ("hamsa", "\U0001faac", "14"), ("disco-ball", "\U0001faa9", "14"),
                ("shaking-face", "\U0001fae8", "15"), ("pink-heart", "\U0001fa77", "15"),
                ("wireless", "\U0001f6dc", "15"), ("goose", "\U0001fabf", "15"),
                ("face-bags", "\U0001fae9", "16"), ("fingerprint", "\U0001fac6", "16"),
                ("trombone", "\U0001fa8a", "16"), ("harp", "\U0001fa89", "16")]:
    add("emoji-post11" if u not in ("6.1", "6.0") else "emoji-pre11", f"{n}-u{u}", t)
PH = "\U0010eeee"
for n, t in [("bare", PH), ("row0", PH + "̅"), ("row0col0", PH + "̅̅"),
             ("row1col2", PH + "̍̎"), ("three-in-a-row", (PH + "̅̅") * 3),
             ("lone-diacritic", "̅")]:
    add("placeholder", n, t)
for n, t in [("zwsp", "​"), ("soft-hyphen", "­"), ("nbsp", " "), ("bom", "﻿"),
             ("wj", "⁠"), ("bel-free-del", "\x7f"), ("tag-letter", "\U000e0067"),
             ("private-use", ""), ("noncharacter", "﷐"), ("replacement", "�"),
             ("circled-digit-ambig", "①"), ("black-star-ambig", "★")]:
    add("misc", n, t)

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "glyphs.json")
json.dump({"schema": "kbt-glyphs/1", "glyphs": G}, open(out, "w", encoding="utf-8"), ensure_ascii=True, indent=1)
print(len(G), "glyphs", file=sys.stderr)
