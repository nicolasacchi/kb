# T4: mosh into zellij, with a stand-in for the kb-term local mirror

Operator check. CI only proves the script runs and its byte stream is self-consistent
(`ci.sh`, artifact `t4-out`). Whether zellij behaves is what YOU find out here.

`mirror_standin.py` (python3 stdlib, no install) imitates what the kb-term local mirror
client will write into a pane:

1. `ESC[3J` (ED3) then a SEED of N numbered history lines (default 3,000; some SGR colour,
   a few wide characters), at the pane's current width.
2. A live stream of numbered lines at a configurable rate.
3. Every K live lines an omp-like rebuild: `ESC[?2026h ESC[H ESC[2J ESC[3J`, a full reprint of
   every line so far, `ESC[?2026l`.
4. Every Nth line is an image row, rewritten from placeholder cells to a width-safe dim label
   such as `L000150 [image 1024x768, on web]`.
5. On exit a summary of what to check.

Every line starts with `L` + 6 digits (`L000001` ...), so counting is mechanical.

## Steps

Get the file onto the machine you mosh into (copy `spikes/kb-term/t4/mirror_standin.py`, nothing else needed).

1. From your laptop: `mosh <host>`, then start zellij (`zellij`) and use a plain pane.
2. Run, in that pane:
   ```
   python3 mirror_standin.py
   ```
   Defaults: `--seed 3000 --live 300 --rate 20 --rebuild-every 100 --image-every 50`.
   Useful options: `--seed 20000`, `--rate 100`, `--rebuild-every 50`, `--live 1000`, `--image-every 0`.
3. Wait for the summary (about 15 s with defaults; Ctrl-C also prints it).
4. Scroll back in zellij (mouse wheel, or `Ctrl-s` then PageUp / `k`).
5. Dump the scrollback and check it mechanically: `Ctrl-s` then `e` opens the scrollback in
   `$EDITOR`; save it as `/tmp/sb.txt` (vim: `:w /tmp/sb.txt`, then quit), then
   ```
   python3 mirror_standin.py --check-dump /tmp/sb.txt
   ```
   It prints how many numbered lines exist, duplicates and missing numbers, and PASS/FAIL.

## Checklist (write down yes/no and, when no, the numbers)

- [ ] **Seed reachable.** After the run, scrolling to the top reaches `L000001` (not just the last ~10,000 lines).
- [ ] **No duplicates after a rebuild.** Scrolling through, every `Lnnnnnn` appears once. `--check-dump` says `duplicates : 0`.
- [ ] **Nothing lost.** `--check-dump` says `missing : 0` and highest number equals the summary's total.
      If zellij ignores ED3 you will see duplicates (the pre-rebuild copy stays in history); that is a valid FINDING, report the counts.
- [ ] **Labels do not break the layout.** The dim `[image WxH, on web]` rows are one row each, not wrapped, no shifted or clipped rows next to them. Try narrowing the pane (zellij resize) before a run and rerun.
- [ ] **Larger buffer.** zellij keeps 10,000 lines by default, so with `--seed 20000` the top is cut off by default. Raise it in the zellij config (`~/.config/zellij/config.kdl`):
      ```
      scroll_buffer_size 100000
      ```
      Restart zellij (a new session; a running session keeps the old value), repeat the run with
      `python3 mirror_standin.py --seed 20000` and re-do the three checks above. Expected: `L000001` reachable now.
- [ ] **Over mosh.** Note whether the run looks smooth and whether scrolling after it differs from a plain local zellij pane.

## Separate check: Shift+Enter through omp (no script involved)

In a plain zellij pane (also over mosh), start a real `omp` and, in its prompt, press Shift+Enter.
Expected: a newline in the prompt, the message is NOT sent. Report: works / sends the message / nothing.
Repeat once with the pane inside tmux if you like (not required).

## What to report back

For each checklist line: yes/no, the `--check-dump` output, your zellij version (`zellij --version`)
and whether scroll_buffer_size was default or raised.
