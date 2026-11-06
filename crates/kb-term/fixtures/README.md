# kb-term fixtures

Synthetic, content-free generators for the kb-term lanes. No real session
content, provider output, or machine paths ever appear here. Run them with
`python3 -I` (isolated mode: no site packages, stdlib only).

| File | What it emits |
|---|---|
| `fake_harness.py` | Harness-like stream: ED2+ED3 inside `?2026` frames, alt screen, DECSTBM regions, SGR, CJK, emoji, pending-wrap, OSC 8, DCS-tmux-wrapped kitty APC + U+10EEEE placeholder rows. Flags: `--cols --rows --seed --length -o FILE`. |
| `synth_omp_kitty_stream.py` | One kitty image: tmux-wrapped APC transmit (chunked PNG), virtual placement, placeholder rows. Flags: `--cols --rows --id --px --no-wrap --resize-replay`. Imported by `fake_harness.py`. |
| `test_fake_harness.py` | `unittest` cases over both generators; runs in the `term-tap` lane. |

Ported from the spike branch `origin/spike/kb-term-ci` (never merged);
`gen.py` and `midseq.py` were intentionally not ported.

The compiled tap binary produced by `bun build --compile` is
glibc/arch-specific and must be rebuilt for the target host.
