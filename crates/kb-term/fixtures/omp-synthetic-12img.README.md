# omp-synthetic-12img.bin

A decoded tmux control-mode tap stream (the concatenated, unescaped `%output`
payloads of one pane) captured from a real `omp` 18.17.0 resuming a fully
synthetic session, under the kb-term start order (tap first, anchor client,
then release via `wait-for`). 610240 bytes.

## Content

- The session has 14 tool-result images (11 PNG + 3 WebP, lossless; solid
  colour backgrounds with a circle, square or diamond), numbers-only text and a
  dummy provider. No model call is made.
- The stream holds three consecutive full renders at pane width 120, 100, 120:
  the initial resume render, then two `resize-window` rebuilds
  (`tui.resizeScrollback=rebuild`, `tui.maxInlineImages=0`).
- Pictures travel as kitty graphics (`PI_FORCE_IMAGE_PROTOCOL=kitty`): each
  rebuild is one `a=d,d=A`, 14 x `a=d,d=I`, 14 x `a=t,f=100,q=2` transmissions
  (every WebP is sent as PNG), 14 x `a=p,U=1,c=64,r=20` placements and the
  U+10EEEE placeholder cells.
- The only text outside escape sequences is the synthetic session content, the
  omp UI chrome and a truncated working directory, rewritten to `synthetic/wk`
  (same length).

## How it was produced

1. Python stdlib wrote the PNGs (zlib/struct); WebP came from converting a
   generated PNG losslessly with ImageMagick.
2. A session JSONL (header version 3, user / assistant / toolResult message
   entries, `read` tool results with inline base64 image blocks) was written by
   a generator script; `omp -r <file>` resumed it with a scratch agent dir
   (dummy provider entry, key `dummy`, unreachable base URL), an empty HOME and
   a settings overlay.
3. A private tmux socket with the kb-term config (manual window size, no keys)
   was driven by a Python script: control-mode tap, pty anchor client, release,
   settle, resize narrower, settle, resize back, settle, clean exit.
4. The `%output` lines were unescaped (octal escapes) and concatenated.

Checked before commit: the bytes contain none of `/home`, `/tmp`, the user
name or the host name.
