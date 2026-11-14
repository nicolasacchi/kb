# T4 mirror stand-in: operator re-check kit

Operator check for the kb-term local mirror claim. CI only proves the script
runs and its byte stream is self-consistent (the `mirror stand-in unit tests` step of the `term-tap` job). Whether
the terminal multiplexer behaves is what the operator finds out here.

`crates/kb-term/tools/mirror_standin.py` (python3 stdlib, no install) is a
synthetic-content, host-local operator tool, not a kb verb. It imitates what
the kb-term local mirror client will write into a pane:

1. `ESC[3J` (ED3) then a SEED of N numbered history lines (default 3,000; some
   SGR colour, a few wide characters), at the pane's current width.
2. A live stream of numbered lines at a configurable rate.
3. Every K live lines an omp-like rebuild: `ESC[?2026h ESC[H ESC[2J ESC[3J`, a
   full reprint of every line so far, `ESC[?2026l`.
4. Every Nth line is an image row, rewritten from placeholder cells to a
   width-safe dim label such as `L000150 [image 1024x768, on web]`.
5. On exit a summary of what to check, including the exact `--check-dump`
   command with the run's total.

Every line starts with `L` + 6 digits (`L000001` ...), so counting is
mechanical.

## Current result: partial PASS

The last T4 run (201x48 pane over mosh, 3,000 seed + 300 live lines) reported
`total numbered lines : 3300 (L000001 .. L003300)`, 3 rebuilds, 4 ED3. The
saved dump held 2,714 numbered lines ending at L002714, with 0 duplicates,
0 missing in `1..highest`, in order. The seed was reachable from L000001,
but the last 586 lines (L002715..L003300, about 12 screens of 48 rows)
were absent. The cause of the missing tail is unknown today.

The checker now fails on that shape: `--check-dump` requires `--expect-total`
taken from the summary's total, and reports `truncated` when the dump's
highest number is below it. A dump without `--expect-total` is refused.

## Steps

Copy `mirror_standin.py` onto the operator's machine (that file, nothing else
needed).

1. From a laptop: `mosh <host>`, then start zellij (`zellij`) and use a plain
   pane. (`<host>` is a placeholder for the host the operator moshes into.)
2. Run, in that pane:
   ```
   python3 mirror_standin.py
   ```
   Defaults: `--seed 3000 --live 300 --rate 20 --rebuild-every 100 --image-every 50`.
   Useful options: `--seed 20000`, `--rate 100`, `--rebuild-every 50`, `--live 1000`, `--image-every 0`.
3. Wait for the summary (about 15 s with defaults; Ctrl-C also prints it).
   Note the summary's `total numbered lines` and `last line written` numbers.
4. Scroll back in zellij (mouse wheel, or `Ctrl-s` then PageUp / `k`).
5. Dump the scrollback and check it mechanically. Always pass `--expect-total`
   with the total from the summary; without it the checker refuses to run.

## Dump routes

**Route A (preferred):** from the zellij pane, first run
`zellij action dump-screen --help`, then dump the full scrollback with
`--full` to `/tmp/sb.txt`, for example:

```
zellij action dump-screen --full /tmp/sb.txt
python3 mirror_standin.py --check-dump /tmp/sb.txt --expect-total 3300
```

`--full` (whole scrollback, not just the viewport) is expected from zellij's
documentation, but was NOT verified by the author. The operator must confirm
with `--help`, report the zellij version, and note the exact flags used.

**Route B:** the editor route, kept for comparison. From the pane, `Ctrl-s`
then `e` opens the scrollback in `$EDITOR`; save it as `/tmp/sb.txt`
(vim: `:w /tmp/sb.txt`, then quit). Then:

```
python3 mirror_standin.py --check-dump /tmp/sb.txt --expect-total 3300
```

It prints how many numbered lines exist, duplicates, missing numbers,
`expected total`, `tail`, and PASS/FAIL.

## Triage table for a truncated result

Run these in this order, each in a fresh pane. The point is to separate the
three open hypotheses: (1) the editor route drops or cuts the tail, (2) the
multiplexer loses the end of the last rebuild's reprint written inside one
synchronized-output frame, (3) mosh or the checker/summary pairing is at
fault. The cause is unknown today.

| # | Run | Points at |
|---|---|---|
| 1 | Same run, dump via route A and route B, compare `highest`. | A complete and B truncated points at the editor route (hypothesis 1). |
| 2 | `--rebuild-every 0` (no rebuild), same sizes, `--expect-total 3300`. | Complete here but truncated with rebuilds points at the multiplexer handling of the last reprint frame (hypothesis 2). |
| 3 | `--live 350 --rebuild-every 100` (the last rebuild is followed by 50 plain lines; `--expect-total 3350`). | Complete here but truncated with the default `--live 300` points at the final-frame case (hypothesis 2). |
| 4 | Run on the host in a zellij pane without mosh vs over mosh. | A difference between the two points at mosh (hypothesis 3). |
| 5 | Look at the live pane: is the summary's `last line written` number visible directly above the summary block? | If the pane itself lacks it, the dump route is not the culprit. |

For each run, report `highest`, the expected total, the zellij version, and
the `scroll_buffer_size` in force.

## Checklist

Write down yes/no and, when no, the numbers.

- [ ] **Seed reachable.** After the run, scrolling to the top reaches `L000001`
      (not just the last ~10,000 lines).
- [ ] **No duplicates after a rebuild.** Scrolling through, every `Lnnnnnn`
      appears once. `--check-dump` says `duplicates : 0`.
- [ ] **Nothing lost.** `--check-dump` says `missing : 0` and `highest` equals
      the summary's total (`tail : OK`). If the multiplexer ignores ED3 you
      will see duplicates (the pre-rebuild copy stays in history); that is a
      valid FINDING, report the counts. If `tail` says `truncated`, follow
      the triage table above.
- [ ] **Labels do not break the layout.** The dim `[image WxH, on web]` rows
      are one row each, not wrapped, no shifted or clipped rows next to them.
      Try narrowing the pane (zellij resize) before a run and rerun.
- [ ] **Larger buffer.** zellij keeps 10,000 lines by default, so with
      `--seed 20000` the top is cut off by default. Raise it in the
      operator's zellij `config.kdl`:
      ```
      scroll_buffer_size 100000
      ```
      Restart zellij (a NEW session; a running session keeps the old value),
      repeat the run with `python3 mirror_standin.py --seed 20000` and check
      with `--expect-total <N>` from that run's summary. Expected: `L000001`
      reachable now.
- [ ] **Over mosh.** Note whether the run looks smooth and whether scrolling
      after it differs from a plain local zellij pane.

## Separate check: Shift+Enter through real omp (no script involved)

In a plain zellij pane (also over mosh), start a real `omp` and, in its
prompt, press Shift+Enter. Expected: a newline in the prompt, the message is
NOT sent. Report: works / sends the message / nothing. Repeat once with the
pane inside tmux if you like (not required).

## What to report back

For each checklist line: yes/no, the `--check-dump` output, the zellij version
(`zellij --version`), and whether `scroll_buffer_size` was default or raised.
For every triage run: `highest`, expected total, zellij version, and
`scroll_buffer_size`.
