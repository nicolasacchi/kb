# kb-term matrix streams (generated at test time)

No binaries are committed. `crates/kb-term/tap/test/matrix/matrix.test.ts`
generates these streams into a temp dir at test time with `python3 -I` and
the generators in this directory's parent (`crates/kb-term/fixtures/`):

| File | Generator | Cols x Rows | Notes |
|---|---|---|---|
| `harness-s1.100x30.bin` … `harness-s7.100x30.bin` | `fake_harness.py --seed N --cols C --rows R --length L` for `(1,100,30,60) (2,100,30,60) (3,100,30,120) (4,80,24,60) (5,200,50,60) (6,40,12,60) (7,100,30,300)` | per `CxR` | Deterministic: harness seed 3 is generated twice per run, asserted byte-equal, and its sha256 is printed (pinned in the test after the first CI run). |
| `omp-kitty.100x30.bin` | `synth_omp_kitty_stream.py` (defaults) | 100x30 (name) | One kitty image, virtual placement. The generator defaults are 90x19; the file name keeps the spike's `100x30` (both matrix parties use the name's size, consistently). |
| `omp-kitty-replay.100x30.bin` | `synth_omp_kitty_stream.py --cols 60 --rows 12 --resize-replay` | 100x30 (name) | Resize-replay variant; same naming convention as above. |
| `fx-omp-synthetic-12img.100x30.bin` | the committed `omp-synthetic-12img.bin` (610,240 B, synthetic content-free capture) | 100x30 (name) | Byte-identical copy of the fixture; the fixture's renders are at pane width 120/100/120, but the matrix parties consistently use the name's 100x30. |

Every stream is run through the production `stripAll` transform before the
twin/reference/viewer consume it (the twin always sees post-transform
bytes). `python3` is required: a missing generator FAILS the test, never
skips. All content is synthetic; no real session data enters these streams.
