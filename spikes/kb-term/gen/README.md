# gen/ - synthetic byte-stream generators

All generators are Python 3 standard library only, content-free, and deterministic (seeded or fixed).
Run them with `python3 -I`.

| File | What it emits |
|------|---------------|
| `fake_harness.py` | Harness-like stream: ED2+ED3 inside `?2026` frames, DECAWM `?7` toggles, alt screen 1049, DECSTBM regions, SGR 16/256/truecolor + underline styles, CJK, emoji (VS16, ZWJ, skin tones, Unicode 14/15), pending-wrap edge cases, OSC 8, cursor save/restore (ESC 7/8, CSI s/u), DCS-tmux-wrapped kitty APC + U+10EEEE placeholder rows, plain line runs. Flags: `--cols --rows --seed --length -o FILE`. Same flags give identical bytes. |
| `synth_omp_kitty_stream.py` | One kitty image: tmux-wrapped APC transmit (chunked PNG), virtual placement `U=1`, placeholder rows. Flags: `--cols --rows --id --px --no-wrap --resize-replay`. Imported by `fake_harness.py`. |
| `gen.py` | Throughput payloads for pane-throughput tests: `--kind text\|apc\|lines --total BYTES [--rate B/s] --tag T --shaout FILE --go FILE`. Waits for the `--go` file to exist, writes a START marker, the payload and an END marker, records the sha256 of what it sent, then sleeps forever (intended to run inside a tmux pane). |
| `midseq.py` | Three timed writes whose boundaries fall inside a CSI sequence and inside an OSC title, to test late-join seeding mid-sequence. Flag: `--gap SECONDS`. Standalone extract of the embedded generator from the original spike. |
