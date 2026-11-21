# kb-term — a live terminal in kb

kb-term is kb's operator-intent terminal tier: the operator starts an agent
session (claude, codex, opencode, grok or omp; kimi support was removed) on the
kb host with `kb-term new` (or from the SPA: the New terminal control on
`/sessions`, the session page's Resume in a kb terminal and its Handoff
panel), then watches and types into it from the SPA's
session page (desktop or phone) and kills it from there or from a local shell,
where `kb-term attach` mirrors the same session into a native terminal. It is built for one machine in a controlled environment, it is off
by default, and it is deliberately not a general remote shell. This page is the
reference for how it works, what it protects, how to install it, and where it
is known to be imperfect. The step-by-step install guide is
[kb-term-install.md](kb-term-install.md); the route shapes live in the
`/api/terms` block of [http-api.md](http-api.md); the `[terminal]` keys live in
[configuration.md](configuration.md#terminal).

Only what is on `main` is described here. Where a claim has no test or CI lane
behind it, it is listed under [Known limits and unverified](#known-limits-and-unverified)
and not stated as fact.

## What it is, and what it is not

- **An operator-intent tier behind `[terminal] enabled` (default `false`).**
  With the key off, every `/api/terms` route answers a bodiless 404, so a
  disabled daemon does not reveal that the tier exists.
- **SPA-only.** The browser is the only caller of `/api/terms`. The `kb` CLI and
  agents never attach to, type into or launch terminals over kb's HTTP surface;
  there is no `kb` verb for it and `kb tools` does not list one.
- **`kb-term attach|new|ls|kill|doctor` is a host-local tool, not a kb verb.** It
  talks to the runner socket and the per-session tap socket on the same machine
  and never goes through the kb daemon. It is not in the verb list of
  [cli.md](cli.md).
- **State still comes only from beats.** Whether a session is working, waiting or
  blocked is derived by the daemon from the harness hooks' beats (see
  [live-sessions.md](live-sessions.md)). The terminal supplies terminal facts
  only: the pane's modes, a quiet clock, an exit code. Nothing a terminal emits
  feeds the derived session state.
- **The kb daemon persists nothing about terminals.** The runner owns every
  terminal; kb-server holds an in-memory ticket table and an in-memory
  terminal-to-session directory, both rebuilt empty on restart.
- **Light security, stated plainly.** See [Security model](#security-model): the
  gate is a convention for a single-machine environment, not a hard boundary.

The ruling text lives in the numbered invariants of
[architecture-invariants.md](architecture-invariants.md): the KT amendments to
#4 (the in-handler gate), #11 (the operator-intent terminal tier), #13 (relays
close 1012 on an in-process restart), #23 (terminal bytes never enter the
TanStack cache) and #24 (the per-panel terminal WebSocket sits outside the
SharedWorker). [live-sessions.md](live-sessions.md) carries the scoped "no
control" exception.

## Architecture

```
 browser (SPA)                         host
 ──────────────      /api/terms        ┌──────────────────────────────────────┐
  xterm.js panel  ── WS + REST ──────▶ │ kb-server  (gate + relay, in memory)  │
  phone Drive/Read                     │    │ one JSON line per RPC, unix sock │
                                       │    ▼                                  │
                                       │ kb-termd   runner.sock (mode 0600)    │
                                       │    │ launch / kill / paste / keys     │
                                       │    ▼                                  │
                                       │ tmux 3.7c: one PRIVATE server per     │
                                       │ session, /run/kb-term/s/<id>.sock     │
                                       │    ▲ control mode     ▲ hidden clients│
                                       │ kb-tap@<id> ──────────┘ (anchor,      │
                                       │    │                     viewers)     │
                                       │    └─ tap.sock ─▶ kb-term attach,     │
                                       │                   kb-termd stream     │
                                       └──────────────────────────────────────┘
```

Each session is a private tmux server: one tmux server per session, listening
on its own socket under the run directory (`/run/kb-term/s/<id>.sock`), started
by a `systemd --user` unit `kb-term@<id>` with `tmux -D` and `exit-empty off`.
The server never draws for a viewer. Three consequences drive the design:

- The server owns the pty, so a session survives a restart of the runner, of the
  tap, and of kb itself.
- The server answers terminal queries (colour, cell size, device attributes)
  with nobody watching, because the runner always keeps one hidden "anchor"
  client attached that carries a responder.
- An agent running `tmux kill-server` in its pane ends only its own session.

Output reaches viewers only through the tap: `kb-tap@<id>` (Bun/TypeScript, a
compiled single binary; `PartOf=kb-term@<id>`) attaches to the holder in tmux
control mode, sanitises and numbers the byte stream, keeps a twin terminal and
a ring, and serves viewers over a per-session unix socket. Viewer keystrokes do
not travel through the tap: they go through a per-viewer hidden tmux client
whose output is drained and discarded. One session accepts at most
`max_viewers_per_session` (default 8) viewers, counted across the host CLI,
the REST routes and browser streams alike; a stream opened at the cap is
refused up front with `too-many-viewers` (a 409 `terminal-conflict` at the
WebSocket upgrade), instead of being admitted and ended at its first
keystroke. A stream reserves its slot when it opens, whether or not it ever
types, and holds it until the stream ends, so two simultaneous opens cannot
both take the last slot.

`kb-termd` (Rust) is the runner: it fronts launch, kill, paste, keys, read and
history over `runner.sock` (mode 0600, connections from another uid are
rejected) and relays the tap stream for kb-server. `runner.sock` lives in its own
directory, `/run/kb-term/runner`, which is the only path bind-mounted into the
kb container; the per-session sockets never are. Ids are minted by the runner
as 8 to 12 characters of `[a-z0-9]`; every kb-side surface accepts the wider
`^[a-z0-9]{1,32}$` and an id the runner does not know is a 404, never a 502.

## Start order

A launch builds one session in a fixed order (see the module header of the
runner's launch code). After the holder starts, any failure tears the session
down, leaving no stray process and no `s/<id>*` files.

1. The runner takes the id lock, starts the holder unit, and polls until the
   tmux server answers.
2. It creates the session `kb` with a pane command that is a gate:
   `/bin/sh -c 'T=$1; S=$2; shift 2; "$T" -S "$S" wait-for kbterm-go; exec "$@"'`
   followed by the harness argv. The pane is a non-login shell, so no user
   profile is sourced; the final `exec` keeps `#{pane_pid}` equal to the
   harness pid. The pane's environment is built from an allowlist (see
   [Launch profiles](#launch-profiles)).
3. It sets the `@kb_*` user options (id, harness, exit keys, start time).
4. It attaches the **anchor**: a hidden client on a pty (`TERM=xterm-256color`,
   pixel size from the profile's cell size) with a responder, attached without
   `-f ignore-size`.
5. It starts the tap and waits for its control-mode client. The tap attaches
   with `-f ignore-size`, then sends `refresh-client -f pause-after=20`.
6. It sends `tmux wait-for -S kbterm-go`, which releases the harness, and reads
   `pane_pid`.

The reason for the order: the harness emits no byte until the tap is listening,
so the twin sees the harness's first byte; and tmux answers the colour and
cell-size queries from the client it recorded first, so the anchor (a client
that can answer) is attached before the idle control client.

## kb-term.conf

Every holder loads this file (shipped at `crates/kb-term/share/kb-term.conf`,
installed to `~/.local/share/kb-term/kb-term.conf`):

```
# kb-term.conf v2 -- loaded by every per-session holder; the harness is pane 0.
set -g @kb_conf_version 2
set -g default-terminal "tmux-256color"
set -g prefix None
set -g prefix2 None
unbind-key -a -T root
unbind-key -a -T prefix
unbind-key -a -T copy-mode
unbind-key -a -T copy-mode-vi
set -g mouse off
set -g window-size manual
set -g default-size 120x40
set -s extended-keys always
set -g extended-keys-format csi-u
set -g history-limit 50000
set -g status off
set -g remain-on-exit on
set -g remain-on-exit-format ""
set -s exit-empty off
set -g allow-passthrough off
```

- **Prefix and key tables.** `prefix None`/`prefix2 None` and four `unbind-key -a`
  lines leave no tmux key binding: every key a client sends goes to the harness.
  `mouse off` keeps tmux out of the mouse.
- **Grid.** `window-size manual` plus `default-size`: the window never follows a
  client. The runner changes the size only with an explicit `resize-window`
  (see [Grid policy](#grid-policy)).
- **Keys.** `extended-keys always` with `csi-u` format lets modified keys reach
  the harness unambiguously.
- **History and exit.** `history-limit 50000` is the pane scrollback the reseed
  capture reads; `remain-on-exit on` (with an empty format) keeps a dead pane so
  its exit status can be read; `exit-empty off` keeps the server alive when no
  client is attached.
- **Passthrough.** `allow-passthrough off`: the harness cannot tunnel sequences
  to the outer terminal through tmux.

## Tap pipeline

The stages run in this order on every `%output`/`%extended-output` record. The
order matters; several of the rules exist because an earlier order leaked.

1. **Decode** the control-mode lines into pane bytes.
2. **Unwrap DCS-tmux envelopes** (`ESC P tmux; ... ST`, with the inner ESCs
   un-doubled), up to 4 levels. A fifth level is dropped entirely (fail
   closed). Unwrapping happens before stripping so a query hidden in a wrapper
   cannot get past it.
3. **C1 filter.** The C1 controls (U+0080..U+009F, and lone raw 0x80..0x9f
   bytes) are deleted and abort whatever sequence was in progress, so a
   deletion can never splice the bytes before it onto the bytes after it.
4. **Query strip, by shape** (see [Strip list](#strip-list)). Matching is by the
   sequence's shape, never by harness name. It is byte-at-a-time with no timer
   flush. Hold-back caps: CSI 64 bytes, OSC 256, DCS 4096, an APC `G` sniff
   window of 512. A sequence that grows past its cap passes verbatim, so a
   padded query escapes the stripper and the anchor's responder is the
   backstop. An aborted or partial sequence is dropped, never emitted.
   Stripping is **best effort**.
5. **Graphics extraction.** Kitty APC `G` sequences leave the viewer stream and
   their pictures go to the image store (see [Pictures](#pictures)); IIP and
   SIXEL payloads are replaced by a small stub.
6. **Withhold/route.** On the main screen the tap withholds the mouse-enable
   modes (9, 1000, 1001, 1002, 1003, 1005, 1006, 1015, 1016) and the kitty
   keyboard push/pop/set sequences, and routes OSC 52 clipboard writes to the
   viewer that typed most recently (a 10 s window) instead of the shared
   stream. On the alternate screen everything passes. This stage also tracks
   alt-screen and synchronized-output state, which the join rule uses.
7. **Number** the surviving bytes by absolute byte offset, then feed the twin,
   the ring and the viewers.

`reply` lines of the control channel never enter a pane stream.

## Strip list

The stripper removes terminal **queries** (sequences that make a terminal
answer) so a viewer never answers on the pane's behalf. Everything it does not
match passes through byte for byte. The labels below are the stripper's own.

**Stripped (by the shape of the query):**

- CSI: `DA1` (`CSI c`, `CSI 0 c`), `DA2` (`CSI > c`), `DA3` (`CSI = c`),
  `XTVERSION` (`CSI > 0 q`), `DSR` (`CSI 5 n`, `CSI 6 n`), `DECDSR` (`CSI ? Ps n`),
  `KITTYKBD` (`CSI ? u`), `DECREQTPARM` (`CSI 0 x`, `CSI 1 x`), `XTWINOPS` report
  requests (`CSI Ps t` for 11, 13, 14, 15, 16, 18, 19, 20, 21), `XTSMGRAPHICS`
  (`CSI ? ... S`), `XTQMODKEYS` (`CSI ? ... m`), `DECRQM` (`CSI Ps $ p` and
  `CSI ? Ps $ p`), `DECRQTSR` (`CSI Ps $ u`), `DECRQPSR` (`CSI Ps $ w`), `DECRQCRA`
  (`CSI ... * y`).
- ESC: `DECID` (`ESC Z`).
- OSC with a `?` argument for 4, 5 and 10 through 19; OSC 21 with `=?`; OSC 52
  clipboard reads (`?`); `OSC 7501;?`; OSC 1337 `Capabilities`, `ReportCellSize`
  and `ReportVariable=`; OSC 99 with `p=?`; OSC 5522 with `type=read`.
- DCS: `+q` (XTGETTCAP) and `$q` (DECRQSS); a nested DCS-tmux beyond the depth
  limit.
- APC: kitty graphics `G` with `a=q`, and APC `25a1`.

**Handled by a later stage (not the stripper):** kitty graphics transmit, place
and delete (extracted, stored, never forwarded); IIP and SIXEL (stubbed);
main-screen mouse enables; kitty keyboard push/pop/set; OSC 52 writes (rerouted).

**Kept byte for byte (examples from the stripper goldens):** every mode
set/reset including `?2026`, `?7`, `?25`, `?2004`, `?1049`; ED2/ED3, cursor,
SGR and REP; DECSCUSR (`CSI Ps SP q`); `CSI > 4;2 m` (modifyOtherKeys set);
OSC colour and cursor-colour sets, OSC 8 hyperlinks (a `?` inside the URL does
not make it a query), OSC 66 and `9;4`; OSC 1337 `File=`; XTWINOPS 22/23 (title
push/pop); DA with `Ps=1` (not a query). The shipped stripper is the authority;
the golden table in the tap tests is the readable form of it.

## Pictures

omp draws pictures as kitty Unicode placeholders: rows of U+10EEEE cells whose
foreground colour carries the image id, with row and column diacritics. kb-term
launches omp with `PI_FORCE_IMAGE_PROTOCOL=kitty` to get that form (automatic
detection inside tmux yields no protocol).

- **The tap stores each distinct PNG once, by sha256**, in the session's store.
  Caps: 256 MiB per session and 1 GiB across the host, evicted
  least-recently-seen first. An image over 32 MiB, or one that is not a PNG, is
  refused and counted.
- **An id map** (`{id, gen, sha256, mime, w, h, cols, rows}`, an append-only
  file in the session store) survives deletes and tap restarts. A kitty delete
  (`a=d`, any form) becomes a `GfxReset` marker and never frees the store. A new
  generation appears only when an id returns with different bytes.
- **The APC bytes leave the viewer stream.** Viewers get `ImageDef` metadata
  frames (see [Seed, late join, resume](#seed-late-join-resume)); the placeholder
  text itself passes verbatim.
- **The SPA draws the stored picture over the placeholder cells**: it finds the
  visible placeholder runs, groups them per picture, lays one cropped image per
  group in an overlay inside `.xterm-screen` (so it scrolls with the cells), and
  recolours the placeholder glyphs to the background so no glyph or diacritic
  spills past the picture. Bytes come from the media route (see
  [Transcript image lane](#transcript-image-lane)) through an in-memory cache;
  a lightbox opens a picture full size.
- **Local mirror:** `kb-term attach` shows each picture as a one-row dim label
  such as `[image 1024x768, on web]` and keeps row counts exact.
- **Other harnesses draw no terminal pictures** that kb-term shows; their
  pictures are references in the transcript (next sections).

Limits, stated: a same-id retransmit with different bytes rewrites older
scrollback rows that still carry that id; IIP and SIXEL are not drawn (the tap
replaces the payload with a stub and keeps the original in the session store,
and nothing draws it); kitty direct placement from other programs is not drawn;
pictures sent while the tap was down are lost until the program redraws. See
[Known limits and unverified](#known-limits-and-unverified).

## Seed, late join, resume

A viewer that connects late receives, in order: `hello` (protocol `kb-term/1`,
session, stream epoch, harness, bound session id, and the viewer id the socket
joined under), `profile` (xterm, serialize and width-table versions, grid size,
scrollback, cell pixels), one `imagedef` per image id still in the twin buffer,
a `seed` marker, the seed bytes, then live bytes from exactly the offset the
seed was taken at.

- **The seed is `serialize()` of the headless twin plus a state-restore tail**
  (scroll region, saved cursor, modes, pen), because `serialize()` alone is not a
  faithful reproduction of terminal state.
- **Joins happen only at quiescent points:** the twin has parsed through the join
  offset, synchronized output is off, the parser is at ground state and no
  strip bytes are held. An ED2/ED3 and its reprint always travel inside one
  `?2026` frame, so a join never falls inside a rebuild. If a join cannot reach
  a quiescent point within the tap's own wait (3 s by default) the tap answers
  `error{busy}` and the client retries; kb-server's relay is that client for the
  browser (up to 4 opens, 150 ms doubling backoff) and the browser never sees
  the busy frame unless the retries run out.
- **Every binary frame starts with an 8-byte big-endian absolute offset.** Text
  frames are JSON with a `t` tag.
- **Resume:** the browser reconnects with `?from=<offset>&epoch=<epoch>` (and
  `mseq`, the newest marker sequence it applied). The tap keeps a 32 MiB ring of
  post-transform bytes (picture payloads are references, not bytes). It resumes
  from the ring when the epoch is its own and the offset is inside the ring and
  not more than 8 MiB behind the head; an unknown epoch, an aged-out offset or a
  viewer too far behind gets a full join (a seed) instead. An offset ahead of the
  stream is an `offset-ahead` error followed by a full join. The client dedupes
  by offset, treats an offset above what it applied as a gap and rejoins in
  full.
- **Client timers:** the browser's reconnect delay is `min(10 s, 250 ms * 2^n)`
  with jitter, and the attempt count resets only after a connection stayed up for
  5 s. A first-frame watchdog of 6 s covers the time between socket open and the
  first frame. The relay pings the browser every 25 s.
- **Stall policy:** the tap asks tmux for `pause-after=20`. When tmux pauses the
  client (`%pause`), the tap resumes it with `refresh-client -A '%P:continue'`
  and **reseeds** the twin: it waits for a quiescent point (50 ms quiet, bounded
  at 2 s), captures the pane twice around a `capture-pane -p -e -J` in one write,
  retries if output interleaved (up to 5 attempts), feeds the capture to the
  twin, restores modes, then replays the output that arrived after the second
  capture.
- **Watchdogs** warn on an event-loop lag over 3 s or a `%extended-output` age
  over 3 s, and re-tap if the tmux server's resident size passes 512 MiB.

The server-to-client text frames are `hello`, `profile`, `imagedef`, `seed`,
`resize`, `sizechanged`, `gfxreset`, `exit`, `clipboard` and `error`; the
client-to-server frames are `resumefrom`, `takesize`, `input` and `ping` (unknown
fields are rejected). The golden JSON files of the kb-term crate are the
contract.

## Grid policy

There is one grid per session (`window-size manual`). The runner records the
owner in a tmux user option, so it survives a runner restart; sizes are bounded
to 20..=500 columns and 5..=200 rows.

- **`kb-term attach`** takes the grid on attach and again on each window-size
  change (debounced); `--watch` never takes it and reads no stdin.
- **The phone** takes it only through the explicit "Fit to phone" action. A
  take that finds a local attach as owner needs no force and is never refused.
  Watching, the composer and the soft keyboard never move it.
- Taking the grid makes omp rebuild its transcript (ED2, ED3 and a full reprint
  inside one `?2026` frame) in every viewer.
- **Precedence: the web outranks a local attach** (operator ruling, 2026-11-20).
  One pure function in the runner decides every take:
  - no owner, or the same owner retaking: allowed;
  - the web owner (`kb-term`, shared by every web client) takes over a local
    attach owner (`local:<pid>`) **without force**: the "web panel" is the
    place where the operator sizes the session;
  - a local attach can **never** take the grid from the web, even forced: the
    take is refused `grid-owned` with a message naming the web panel;
  - between web viewers the owner string is the same, so the latest explicit
    Fit wins; a passive resize never takes the grid;
  - any other owner pair keeps the old rule: refused `grid-owned` unless forced
    (HTTP 409 over the relay).
- **A stale local owner no longer blocks the web.** A crashed or killed attach
  leaves `local:<pid>` in the owner option; the web takes over it like any
  other local owner. A local attach started later takes the grid only when the
  owner is none or a local one.
- **The local attach does not fight the web.** An attach refused with
  `grid-owned` prints a banner and keeps running as a watch inside the mirror
  (it does not `exec tmux`: the empty key tables mean tmux's own attach could
  not implement "any key takes the size back"). Under a **local or other**
  owner the first keypress takes the size back, as before. Under the **web**
  owner a keypress takes nothing and forwards nothing: the attach prints one
  short line (at most once per 10 s) saying the web panel owns the grid and
  that the operator can release it there or detach with Ctrl-], and keeps
  watching. When the web releases the grid (`release-size`, owner none) the
  next keypress takes it and the attach resumes as usual.
- **An attach that loses the grid** notices it: a `resize` marker whose size
  differs from the size it last took makes it ask the runner who owns the grid.
  If the owner is now someone else it drops into the same watch with the banner
  (never retaking on its own, so no resize loop) and the session keeps running.
  The ruling also allows the web to detach the local attach; that is **not
  built**, because watching behind a banner already satisfies the ruling.
- **Release.** `release-size` clears the owner only when it matches the web
  owner; a local owner is not cleared by it.
- **The tap follows the grid.** A take-size is a tmux `resize-window`, and tmux
  announces every window resize with a `%layout-change` notification, in order
  with the pane's output. The tap resizes its headless twin at that point of the
  stream (after the bytes already queued for it, never while earlier writes are
  unparsed), stamps a `resize` marker at the current offset so every live viewer
  resizes at the same point (a caught-up live viewer gets the marker at once, even when the
  pane prints nothing afterwards; an idle pane used to hold it until its next byte), and reports the new size in later `profile` frames;
  a joiner waits for a pending twin resize, so its seed and its profile agree. A
  repeat of the current size does nothing. At start the tap also asks tmux for
  the window's current size, so a tap restarted after a take-size does not keep
  the launch grid. Before the FUATTACH fix nothing resized the twin: `profile`,
  the web panel's header and terminal, and the seed of every viewer that joined
  after a take-size kept the launch grid (120x40) while the pane was larger, so
  a wider stream was drawn at the wrong width (rules wrapped onto a second row,
  rows doubled).
- **Why a single grid:** zellij moves a scrolled row into its scrollback only
  when the scroll region is the whole pane, so a local mirror's scrollback is
  complete only while the local pane equals the grid.

## Launch profiles

The runner builds the harness argv from a closed table; a caller never supplies
it. The closed set is the six harnesses of the session model. The table at the
time of writing (the profiles module is the authority):

| harness | fresh-launch flags | profile env | resume tail | exit sequence | status |
|---|---|---|---|---|---|
| `omp` | none | `PI_FORCE_IMAGE_PROTOCOL=kitty`, `PI_CONFIG_FILES=<private overlay>` | `-r <sid>` | Ctrl-C, 300 ms, Ctrl-C, 300 ms, Ctrl-D | marked verified in the table's source comment |
| `claude` | none | `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1` | `-r <sid>` | text `/exit`, Enter | exit sequence not verified; the cgroup kill is the backstop |
| `codex` | `--no-alt-screen` | none | `resume <sid>` | Ctrl-C, 300 ms, Ctrl-C | not verified; the cgroup kill is the backstop |
| `grok` | `--minimal --no-alt-screen` | none | `-r <sid>` plus `--cwd <cwd>` for an absolute clean cwd | Ctrl-C, 300 ms, Ctrl-C | **experimental** |
| `opencode` | `--mini` | none | `--session <sid>` | Ctrl-C, 300 ms, Ctrl-C | **experimental** |
| handoff (`omp` or `claude` target) | none | the target harness's profile env, plus `KB_TERM_ID` | `kb sessions handoff <sid> --to <harness> --transplant --launch --cwd <cwd>` replaces the harness binary | the target harness's | not verified; the cgroup kill is the backstop |

Fixed rules:

- **Resume** always passes the session id through the table above (the same
  tokens as the resume command the rest of kb prints). A bare continue is never
  emitted: `--kb-off`, `-c`, `--continue` and `--resume-last` are forbidden argv
  elements and a launch containing one is refused.
- **omp overlay** (written 0600 into the run directory for omp only; the user's
  global omp config is never read or written): `tui.maxInlineImages: 0` (this
  means *keep every image*, not "off"), `tui.resizeScrollback: rebuild`,
  `display.collapseCompacted: false`. `kb-term doctor` fails if the on-disk
  overlay does not carry these.
- **Pane environment is an allowlist.** Copied from the runner's own environment:
  `HOME`, `USER`, `LOGNAME`, `SHELL`, `PATH`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ`,
  five `XDG_*` names, and the kb names `KB_DAEMON_URL`, `KB_CACHE_DIR`,
  `KB_BIN_DIR`, `KB_SESSIONS_DIR`, `KB_CAPTURE_SPOOL`, `KB_CAPTURE_LOCKS`. Never
  forwarded: outer-terminal identity (`KITTY_*`, `GHOSTTY_*`, `WEZTERM_*`,
  `TERM_PROGRAM`, `LC_TERMINAL`, `ZELLIJ*`, `ITERM_*`, `KONSOLE_*`, `VTE_*`,
  `WT_*`, `STY`), because harnesses derive terminal identity from the
  environment first. The wrapper also unsets any such name a server inherited.
  The runner adds `KB_TERM_ID`.
- **Handoff launch.** A launch carrying `handoff_from` (the source session id of a
  cross-harness handoff, see `kb sessions handoff` in [cli.md](cli.md)) runs
  `kb sessions handoff <sid> --to <harness> --transplant --launch --cwd <cwd>`
  in the pane instead of the harness binary, with the bare `kb` resolved from the
  pane `PATH`. The transplant file must be written on the host, which the daemon
  (possibly containerised) never sees, so the pane itself runs the kb CLI; that
  writes a new native session file and `exec`s `claude -r <new>` / `omp -r <new>`
  in the cwd. The pane env is the target harness's normal profile env (above) plus
  `KB_TERM_ID`, and the exit sequence is the target's, so the resumed harness runs
  as in any kb-term launch and its beats bind the terminal to the new session.
  Rules, enforced by the server, the runner and the plan alike: `handoff_from` is
  a plain token (non-empty, at most 128 bytes, first character ASCII
  alphanumeric, the rest alphanumeric or `.` `_` `:` `-`); `harness` is `claude`
  or `omp`; it is exclusive with `resume_sid`; `cwd` is required and absolute.
- **Beats stay on.** kb-term never launches a harness with `--kb-off`, because
  the beats are what tell kb the session's state.

## Binding a terminal to a session

The runner exports `KB_TERM_ID` into the pane, and the `kb-beat.sh` hook adds it
to every beat as an optional top-level `term` (the beat wire stays `v:1`; see
`POST /api/sessions/beat` in [http-api.md](http-api.md)). The daemon binds the
terminal to the session when `term` is a valid id, the runner's list knows that
terminal, and the beat's pid equals that pane's `#{pane_pid}`. A `start` beat is
the rebind signal; a stale holder (waiting or cold) keeps a terminal against a
late bind of a different session until it ends.

The binding is in memory only. It is used to gate the composer (the server
refuses `enter` with 409 while the bound session is blocked; an unbound terminal
is treated as not blocked and its reply carries `x-kb-term-state: unbound`) and
to link the terminal and the session page. It never changes the derived session
state.

Known limit: the binding code's own header notes that a sub-agent that beats
`start` with the same term and pid could steal the binding. The omp extension no
longer lets subagents beat or take the process identity, so the known source is
closed for omp; other harnesses were not checked. An agent that sets no blocked
beat (for example a prompt that does not raise one) shows no choice bar.

## Open it on the web

1. Start a session on the kb host: `kb-term new <harness> --cwd <dir>` (add
   `--no-attach` to leave it running unattended), or launch one from the SPA
   (see [Start or resume from the web](#start-or-resume-from-the-web)).
2. The harness's first beat makes the session known to the daemon at once, so
   it appears in the Now band on `/sessions` (it needs no transcript first).
   Its row title links to the session page; when `[terminal]` is admitted for
   you and the session is bound to a terminal, the row also carries an "Open
   terminal" link to the page's Terminal section.
3. The session page shows a live-only card until the first capture exists:
   the header and live state, a caption saying the transcript lands at the end
   of the session's first turn, the Terminal section and the resume command.
   The terminal panel itself still mounts only after the explicit "Open
   terminal" click on that page.
4. When the first capture lands, the same page becomes the full card (files,
   commits, outcome and the rest) without a reload.

A session the daemon has never heard a beat from, and that has no capture, is
still a 404: the live-only card is derived per request from the in-memory live
registry and persists nothing.

## Start or resume from the web

For a caller the terminal tier admits (`terminal_admitted` on
`GET /api/identity`; with the tier off there is no control at all):

- **New terminal** sits under the page head on `/sessions`. Pick a harness
  (claude, codex, omp, opencode, grok; the last two are experimental) and type
  an absolute working directory on the daemon's host. The directory is checked
  in the browser the way the server checks it (absolute, at most 512 bytes, no
  control character) and a path that fails sends no request. The harness and
  the last few directories are remembered in the browser only. Suggestions come
  from the sessions the page already lists and from those remembered
  directories; there is no directory-browsing route.
- **Resume in a kb terminal** is in a session page's Resume section, beside the
  copyable resume command and the Handoff panel. It appears only when the
  session's working directory is known, its harness is one of the five above and
  it has no live bound terminal. It launches the harness's own resume for that
  session (`resume_sid` is the session id; a bare continue is never sent).

Both send nothing until the click, then open the terminal inline at once. A
harness in a directory it has never trusted shows its trust prompt before any
hook runs, so the new session may stay unbound until you answer that prompt in
the terminal itself. While it is unbound the panel says the session appears in
the list once the harness starts. The page re-reads the existing terminals list
every two seconds until a session id appears, the pane ends or two minutes have
passed (it then stops polling and says so); there is no extra event stream. When
the row carries a session id the panel shows a link to that session's Terminal
section. The link is never followed for you, and a harness may resume into a
different session id than the one you resumed; the link names whichever id the
row carries. A refused or failed launch (400, 404, 5xx) and a pane that ended
(with its exit code) are shown as they are.

## Security model

- **Disabled is invisible.** `[terminal] enabled = false` (the default) makes every
  `/api/terms` route, including a malformed request, a bodiless 404.
- **The gate runs in the handler**, not in middleware, because a WebSocket
  upgrade is a GET that the origin allowlist does not police and the loopback
  bypass would admit every agent on the machine. In order: enabled; id shape
  (`^[a-z0-9]{1,32}$`); exact `Origin` (on a POST and the WebSocket upgrade an absent Origin
  fails closed; only the configured parent origin or an origin whose scheme and
  host equal the request's are accepted; artifact-subdomain origins never are,
  even on GETs); the `X-Kb-Intent: terminal` header on every POST; then the
  caller class. Every GET (list, read, history, grid, media) takes one shared
  rule instead, because a same-origin `fetch()` GET sends no `Origin`: the
  intent header is required, an `Origin` must be exact when present, a present
  `Sec-Fetch-Site` must be `same-origin` or `none`, and token callers are
  refused.
- **Loopback does not bypass.** Admission by the bearer gate is necessary, never
  sufficient.
- **Token callers are refused by default.** `[terminal] refuse_token_callers`
  (default `true`) refuses bearer, registry and legacy-token identities on these
  routes; the SPA's own identity passes. See the proxy caveat in
  [configuration.md](configuration.md#terminal).
- **A WebSocket needs a single-use ticket**: minted by an authenticated POST,
  returned only in the response body, bound to the terminal and the caller,
  valid for `ticket_ttl_secs` (30 by default), consumed at the upgrade, never
  logged, and gone after a restart. At most 16 terminal sockets are open at once.
- **Honest limit:** the `Origin` header and the intent header are forgeable by a
  non-browser local client. This is a convention gate that stops a cross-origin
  page and an agent that was never told to type; it is not a hard boundary
  against a local process that sets headers. The deployment assumption is one
  machine, one operator.
- **Terminal bytes are not scrubbed**, so they and the pictures never leave this
  gate. `/api/sessions/presence` and `/api/sessions/{id}/live` stay loopback-only.
- **Restarts:** a config edit restarts the daemon in process and closes every
  terminal relay with WebSocket code 1012; the holders, tap and runner are
  untouched and the panel reconnects with a fresh ticket.
- **Sockets:** `runner.sock` is mode 0600 and rejects a peer with another uid;
  the tap socket is created 0600; the per-session holder sockets sit under a
  0700 directory and are invisible to the container.
- **No unvalidated client string reaches an argv:** key names are a closed
  vocabulary, media keys are 64 lowercase hex characters, ids are plain tokens,
  and the harness argv comes only from the profile table.
  A handoff launch keeps this property: the only caller-supplied values in its
  argv are the validated plain-token session id, the closed harness name and the
  validated absolute cwd; the argv is otherwise fixed tokens, `FORBIDDEN_ARGV`
  still applies, and the launch is still SPA-only behind the operator-intent
  gate (no kb verb launches terminals).
- **Not exposed publicly:** there is no `/api/terms` rule in the public-mirror
  allowlist (`docs/public-mirror/allowlist.txt`) and none may be added.

## Desktop panel

The session page's Terminal section opens the panel from an "Open terminal"
button; the panel's own Close disconnects (it never kills the session). While
the panel is open the page column widens from the 860 px reading column to up to
1400 px, and every box on the way down may shrink, so a wide grid cannot widen
the page.

- **Sizing.** The panel never sends a resize on its own. It fits the **font**,
  not the grid: the largest whole font size (9 to 20 px inline) at which the whole
  grid fits the host on both axes, using the cell size measured from the loaded
  webfont (a hidden probe, not a constant ratio), then verifies against the
  rendered screen and steps down. The host is a bounded box (about the smaller of
  80% of the viewport height and 60 rem) painted in the terminal's own background.
  Below the font floor the cropped axis scrolls and a "Cropped" hint appears. The
  page itself is checked not to scroll sideways at 1024 px and wider. A window resize only refits the font
  (100 ms debounce); a closed `<details>` (zero width) is never fitted, and
  opening it refits.
- **Fonts.** The stream opens after the webfont loads (bounded at 1.5 s); a face
  that arrives later makes the panel re-measure and refit. The stack ends in
  system symbol faces before the generic family; no font files are added.
- **Fit to window.** A bar button. It measures the host at a 14 px reference font,
  clamps to the daemon's bounds (20 to 500 columns, 5 to 200 rows) and posts the
  existing `take-size` with the viewer id from the `hello` frame. It is only ever
  an explicit click: opening the panel, resizing the window and focusing it never
  send `take-size`.
- **Release grid.** Shown while the web owns the grid (owner `kb-term`, the one
  name every web client shares). It posts `release-size` with an empty body.
- **The shared take rule.** The desktop button and the phone's "Fit to phone"
  use one helper. The web outranks a local `kb-term attach`, and between two web
  clients the latest explicit Fit wins, so there is no confirmation when the grid
  is free, owned by the web or owned by a local attach (`local:<pid>`). A
  confirmation is kept only for an owner of any other class. A refusal (409
  `grid-owned`) shows the runner's own sentence.
- **Status line.** Outcomes (took the grid, released it, a refusal, a network
  error) go to an `aria-live="polite"` line inside the panel, so they survive a
  full-screen panel; failures also raise a toast.
- **Chip.** `201x48 · web`, `· local attach`, `· free` or `· <other owner>`, then
  `font Npx` and the "Cropped" hint. It never says "this window": the relay is
  shared by every web client and cannot tell windows apart. The chip is dimmed
  while the panel is not live.
  The owner comes from the terminal list row; the runner's own answer to this
  panel's take or release names the owner immediately, and any later change of
  the row's owner replaces that answer.
- **Theme.** Colors come from the Profile palette when it names one and from the
  app's tokens otherwise; selection, inactive selection and the cursor accent
  always come from tokens, and the theme is re-read when the SPA theme changes.
- **Full screen.** The bar's "Full screen" button (and "Exit full screen" in
  its place while on) makes the panel fill the screen; the bar stays as a slim
  header with the state, chip, Fit, Release, Find and Exit, and the status line
  stays inside the panel, so outcomes remain visible. Two paths, one rule set:
  - **Native** only where Esc can stay with the terminal: a browser with the
    Keyboard Lock API (Chromium) gets element fullscreen with navigation UI
    hidden, then `navigator.keyboard.lock(["Escape"])` (best effort). A short Esc
    reaches the agent (Claude's "esc to interrupt" keeps working); holding Esc
    exits natively.
  - **Maximised overlay** everywhere else (Firefox, desktop Safari, a missing
    API, a refused request): the panel is fixed to the whole dynamic viewport,
    above the page, with safe-area padding. The overlay binds no Escape key and
    uses no platform close request, so Esc always reaches the agent; the exit is
    the visible button.
  - The state is derived, never stored: the panel is full screen when it is the
    document's fullscreen element or carries the overlay class. An exit that the
    browser makes (holding Esc, a gesture) clears it.
  - Entering focuses the terminal and refits the font (8 to 28 px; both axes). Full
    screen **never takes the grid**: it sends no `take-size` and no resize. A grid
    larger than the screen shows the "Cropped" hint, and Fit to window is the
    explicit way to change the grid.
  - Closing the panel, unmounting it or switching terminal exits native
    fullscreen, releases the keyboard lock and drops the overlay class, so a
    reload or a navigation leaves no stuck state.
- **Who is typing.** A chip says "typing" while the terminal has keyboard focus
  (the view also gets an accent ring) and "click to type" otherwise, with xterm's
  inactive cursor style. Nothing focuses the terminal on mount; a click does.
- **Copy and paste.** Ctrl/Cmd+C copies the selection only when there is one;
  with no selection the key is still ^C. Ctrl/Cmd+Shift+C always copies and
  Ctrl/Cmd+Shift+V is the browser's paste.
- **Links.** `http://` and `https://` text is a link: wrapped rows are joined,
  trailing punctuation and an unbalanced closing bracket are trimmed, and
  Ctrl/Cmd-click opens it in a new tab with `noopener,noreferrer`. A double-width
  character before a link on the same row shifts its highlighted range.
- **Scrollback.** "Jump to latest" appears while the view is scrolled up. Find
  restarts from the top when the query changes, Enter steps through the hits, and
  closing Find returns the keyboard to the terminal.

## Phone

The phone view has three parts.

- **Watch:** the terminal at the session grid, default font 12 px (steps 8 to 24),
  with a pointer-event scroller (xterm.js 6.0.0 has no touch scrolling),
  two-finger pinch for font size, and horizontal pan over the real grid. The DOM
  renderer is used on a phone-sized viewport.
- **Read:** a snapshot of the panel's own buffer in a native scrolling, natively
  selectable view (up to 20 000 lines), refreshed on demand; it never follows the
  stream by itself.
- **Drive:** a native `<textarea>` composer (xterm's hidden textarea is unreliable
  for phone input), a key bar, a choice bar and the explicit **Fit to phone**.
  - **Fit to phone** and **Release grid** share the desktop's take rule (see
    [Desktop panel](#desktop-panel)): no confirmation for a free, web or local-attach
    owner, a confirmation for any other owner class. Release shows while the web
    owns the grid, next to Fit.
  - The composer sends by bracketed paste. A multi-line message is refused when
    the pane has not enabled bracketed paste. Text that begins with `/` or `@` is
    pasted and **not** sent (the menu or file picker would take an Enter blindly);
    the operator presses Enter on the key bar.
  - Enter is never implied. The runner presses it only after the pane has been
    quiet for 400 ms (it gives up after 5 s with `busy`), and the server refuses
    it with 409 while the bound session is blocked.
  - The key bar sends key **names** from a closed vocabulary; Ctrl-C must be
    held (600 ms); a held `/` opens the harness's slash menu, whose entries are
    pasted and never entered.
  - The choice bar appears only while a bound session is blocked on the operator,
    which is known from beats alone.
  - Guards for Claude: every `Left` and every message that is exactly `/bg` asks
    for confirmation first. This is a misfire guard, not a security boundary.
  - The keyboard opening or closing never resizes the pty. Back closes the panel.
  - **Full screen on a phone.** The sheet's full detent is the whole dynamic
    viewport (`100dvh`, with the notch and home-indicator insets kept clear), and its
    title is merged into the terminal bar. A "Full screen" button in the bar
    appears only where the browser offers element fullscreen (Android Chrome); it
    fullscreens the terminal so the browser's own bars go away, and the first
    Back leaves it. Where it is not offered (iPhone Safari) there is no button:
    the `100dvh` sheet is the most the browser allows. There is no overlay
    fallback and no Escape lock on a phone.
  - **Image upload** accepts `image/png`, `image/jpeg`, `image/gif` or
    `image/webp` up to 8 MiB, stores the file in the session's own 0700 upload
    directory and pastes its absolute path.

Verification: CI runs the phone specs in Chromium (Pixel 7 and iPhone 15
emulation) and WebKit (iPhone 15 emulation) with synthetic pointer events and
touch contexts. **Not verified:** real iOS or Android devices, bfcache, a real
soft keyboard, real-GPU WebGL, desktop Firefox or Safari for the panel.

## Transcript image lane

Separately from the terminal's own pictures, session transcripts that contain
pictures are shown as references. kb stores **references, never pixels**.

- `session-view/1` has an `Item::Image` carrying a MIME type and, when the
  transcript named one, a sha256 or an inline byte count. Every presenter prints
  one label: `[image <mime> blob:<first 12 hex>]`, `[image <mime> inline <N> b64
  chars]` or `[image <mime>]`.
- The **live tail** replaces inline base64 pictures (source blocks and long base64
  leaves) with a reference (sha256 of the decoded bytes plus the length) below
  every consumer, so a screenshot no longer ships its pixels through the live
  lane.
- **`GET /api/terms/media/{sha256}`** serves a stored picture to the SPA. Gate:
  the tier enabled, `X-Kb-Intent: terminal`, an exact `Origin` when one is
  present (a same-origin `fetch` GET sends none), a `Sec-Fetch-Site` of
  `same-origin` or `none` when present, a non-token caller, no loopback bypass.
  The key is 64 lowercase hex characters. The runner serves the tap's own store
  first, then omp's content-addressed blob directory (the file must hash to the
  requested key, be a regular file of at most 20 MiB, and carry PNG, JPEG, GIF
  or WebP magic). The content type comes from the magic bytes, never from a
  stored or client-supplied name; SVG, text and PDF are 415. Responses carry
  `nosniff`, `Cache-Control: private, max-age=31536000, immutable`, a sandboxing
  `Content-Security-Policy` and an `ETag` that makes a matching `If-None-Match` a
  304.
- The session page's **Images strip** lists the transcript's pictures beside the
  terminal's, ordered by time and never deduplicated by hash (the sha256 of omp's
  kitty PNG is not the original blob's sha256). An unadmitted browser makes no
  media request and shows caption tiles with turn links.

## Local tools

`kb-term` is a separate host-local binary (installed next to `kb-termd`):

```
kb-term doctor [--json] [--strict]
kb-term attach <ID> [--watch] [--runner-sock PATH] [--no-labels-dim]
kb-term new <HARNESS> [--resume SID] [--cwd DIR] [--cols N] [--rows N]
                      [--attach | --no-attach] [--runner-sock PATH]
kb-term ls [--json] [--runner-sock PATH]
kb-term kill <ID> [--grace SECS] [--runner-sock PATH]
```

`--runner-sock` defaults to `$KB_TERM_RUNNER_SOCK`, then
`/run/kb-term/runner/runner.sock`. `HARNESS` is one of
`claude|codex|opencode|grok|omp`. `new` attaches after launch unless
`--no-attach`, which prints the new id.

`kill` ends a session through the runner's own kill op (the one the SPA's kill
uses): the exit keys, then a whole-holder stop, then the holder, tap and session
files are removed. It never stops systemd units itself. If no holder answers for
the id (for example the `kb-term@<id>` and `kb-tap@<id>` units were stopped by
hand and `ls` shows the id as `stale`), the runner removes exactly that
session's `s/<id>.sock` and `s/<id>/` and says so; discovery itself never
deletes. The runner owns the run directory, so the cleanup lives in its kill op,
not in the CLI: the CLI cannot be tricked into deleting paths, and the REST kill
gets the same cleanup. The id must match the runner's id grammar; a malformed or
path-traversing id, a symlinked entry, or an entry that is not a socket or a
plain directory is refused with nothing removed, and a probe that fails to
complete (as opposed to a tmux that answers "no server") removes nothing.
Exit codes: 0 done, 2 usage (bad id), 3 runner unreachable, 4 nothing by that
id, 1 anything else.

`attach` mirrors a session into the terminal's **native scrollback**: it writes
`ESC[3J`, then the seed, then the live stream from the exact offset the seed was
taken at, so the whole session is scrollable in the outer terminal. Keystrokes
go to the session's hidden client. **Detach with Ctrl-] then `.`** (Ctrl-]
followed by anything else sends both bytes; Ctrl-C goes to the harness). Exit
codes: 0 normal, 2 usage, 3 runner unreachable (the `kb-termd` user unit may not
be running), 4 session not found. The mirror reconnects with exponential
backoff (100 ms to 5 s). Pictures are dim one-row labels; see
[Grid policy](#grid-policy) for how it takes and gives back the grid.

**Main-screen differential renderers (Claude Code).** A harness that redraws in
place with relative cursor moves (CR, CUU, CHA, CUF, EL) and a colour SGR on
nearly every line writes `ESC[4A` and similar moves right BEFORE the colour
SGR of the line it is about to rewrite. The label rewriter holds a row from its
first 256-colour or truecolor foreground SGR (picture evidence), and until the
FUATTACH fix it reduced everything before that SGR on the same line to its
visible text. That deleted the cursor moves, so every in-place redraw landed on
a new line: spinner frames and counters stacked vertically, the input box's
rules repeated, and status lines came out as fragments. The tap's live stream
was correct (this garble existed only in what `attach` wrote; the stale grid
described under [Grid policy](#grid-policy) is a second, independent defect). The
bytes before a row's first trigger now pass through verbatim, with only the
underline-colour (`58`) parameters removed, so a stream that contains no
placeholder is byte-identical through the rewriter.

Claude Code positions words with cursor-forward and column moves instead of
writing spaces (its own output, in the pane too, and in the seed that the tap
serializes), so those cells are blank in the outer terminal. Copying such a line
out of some terminals yields no spaces between the words. That is how the
program draws, not a mirror defect, and it remains a limit.

For zellij over mosh: mosh keeps no scrollback and carries no images, so local
scrollback is the zellij pane's `scroll_buffer_size` (default 10 000 lines). A
long mirrored session wants a larger value in the user's own zellij config (50 000
to 100 000 is a reasonable range; larger values have not been checked).

**Optional `--graphics` (Ghostty or kitty over plain ssh):** not available. The
pure core of that mode (mode decision, startup probe, chunked transmit and
virtual placement, an insert-only stream filter that never forwards a delete) is
merged and unit-tested, but the attach loop does not call it, there is no
`--graphics` flag, and it has never run against a real Ghostty or kitty.

## Install and operate

The step-by-step guide is [kb-term-install.md](kb-term-install.md). In short:

- **Release archive** `kb-term-<version>-x86_64-unknown-linux-gnu.tar.gz` (with a
  `.sha256` sidecar and, on tags, build provenance) holds `kb-termd`, `kb-term`,
  `kb-tap` (compiled, no bun or npm on the host), the pinned tmux 3.7c and
  `share/kb-term/{systemd,tmpfiles.d,omp,kb-term.conf}`. It needs x86_64 Linux,
  glibc 2.39 or newer and a systemd user manager.
- **Installer** `scripts/install-kb-term.sh` fails closed on a missing checksum or
  a failed provenance check (`KB_INSECURE_SKIP_VERIFY=1` overrides). `PREFIX`
  (default `~/.local`), `KB_VERSION` and `KB_BASE_URL` select where and what to
  install. The pinned tmux goes to `~/.local/libexec/kb-term/tmux` and is never
  put on `PATH`, so a distribution tmux upgrade cannot mismatch a long-lived
  holder. A distribution tmux is not supported.
- **Three operator steps, in order:** one `sudo` line to install the tmpfiles
  snippet (it creates `/run/kb-term`, `/run/kb-term/s` and `/run/kb-term/runner`);
  `systemctl --user enable --now kb-termd.service`; and a compose bind
  `/run/kb-term/runner:/run/kb-term/runner` (the runner directory only, never the
  session sockets) plus `[terminal] enabled = true` in the host copy of
  `kb.toml`. The order is tmpfiles, then `kb-termd`, then the kb container; the
  bind source must exist before `docker compose up`. The second step also covers
  the runner's environment: a user-unit drop-in (`env.conf`) giving `kb-termd`
  a `PATH` that holds the directory of `kb` and of every harness, and
  `KB_SESSIONS_DIR` (the installer prints it pre-filled and quoted; see
  [kb-term-install.md](kb-term-install.md)). `loginctl enable-linger` keeps the
  user manager alive across logout.
- **Do not restart `kb-term@*` holder units to pick up an upgrade**: that ends
  every live session. Restart `kb-termd` only.
- **Alternative:** run kb itself as a systemd user service. It removes the bind
  mount and the uid question and gives up the container's resource caps.
- **uid:** the process that connects to `runner.sock` must run as the same
  numeric uid as `kb-termd`; match the container's user to the host account.

**`kb-term doctor`** reads the host and prints one line per check (status, id,
detail, optional fix); `--json` prints the same report. The exit code is 1 if
any check FAILs, and with `--strict` also on any WARN; a SKIP never fails. Checks:

| id | what it checks |
|---|---|
| `tmux_version` | the pinned tmux reports `tmux 3.7c` (WARN on another version, FAIL if missing) |
| `tmux_sha256` | the tmux binary matches its recorded sidecar digest |
| `tap_binary` | `kb-tap` exists and is executable; its `--version` line is shown |
| `tap_version` | `kb-tap` reports the same version as this `kb-term` (WARN on a mixed install; SKIP when the tap reports none) |
| `xterm_pins` | the tap reports the pinned xterm package set (SKIP if it reports none) |
| `harness_claude`, `harness_codex`, `harness_opencode`, `harness_grok`, `harness_omp` | the harness binary is found and its version is shown; WARN when it differs from the known-good version doctor was written against (omp, claude, codex and opencode have one); WARN when it is found but `--version` exits non-zero (the exit code and first stderr line are shown) or does not finish in 20 s (so a slow cold start is not reported as a missing binary); SKIP only when it is not installed |
| `omp_overlay` | the on-disk omp overlay carries the approved pins |
| `run_layout` | `/run/kb-term` is 0755, `s/` is 0700, `runner/` is 0750 or 0700, `runner.sock` is 0600, all owned by the current uid |
| `login_env` | the login shell does not set outer-terminal identity names that a pane would inherit |
| `termd_enabled` | `kb-termd.service` is enabled so it starts after a reboot (WARN when `static`, `disabled` or masked; SKIP when `systemctl --user` cannot be reached) |
| `runner_env_sessions_dir` | `KB_SESSIONS_DIR` is set in the RUNNER's environment (read from the running `kb-termd` process); FAIL when unset or empty, because the capture and beat hooks silently do nothing in SPA-launched sessions |
| `runner_env_kb`, `runner_env_harness_<bin>` | `kb` and each harness binary resolve on the RUNNER's `PATH`, not your shell's; a harness your shell finds but the runner's `PATH` lacks WARNs (launching it would exit 127; a WARN rather than a FAIL because doctor cannot know which harnesses you launch and the runner itself works), `kb` missing WARNs; SKIP for all when `kb-termd` is not running. The runner is the process the unit's `MainPID` names; only when `systemctl --user` cannot be asked does doctor scan `/proc` for your own `kb-termd`, and it reads nothing if that is ambiguous. Limit: that fallback matches by process name and uid only, so a single `kb-termd` you started by hand with a different runtime directory is read as if it were the unit's runner |

**Panes inherit the runner's environment.** Panes get an allowlisted copy of
the environment of `kb-termd`, which under systemd is the user manager's: a
minimal `PATH` and no `KB_SESSIONS_DIR`. Put both in a user-unit drop-in (see
`docs/kb-term-install.md`); the `runner_env_*` checks verify it took effect.

## Known limits and unverified

- **Seed fidelity.** The state-restore tail is verified at the level of the
  compatibility matrix in the tap tests, not for every program. Residual
  wide-character and emoji width drift between xterm and tmux exists for
  post-Unicode-11 emoji. Links and extended underline styles present at join
  time are lost for rows already in the buffer; new output renders them
  normally.
- **Query stripping is best effort.** A query padded past a hold-back cap passes
  verbatim; the anchor's responder is the backstop.
- **Pictures** are lost while the tap is down until the program redraws; a same-id
  retransmit with different bytes rewrites older rows; IIP and SIXEL are stubbed
  and not drawn; kitty direct placement from other programs is dropped; kb-term
  draws omp's kitty placeholders only.
- **Local mirror, T4 result (as measured).** On a 201x48 zellij pane over mosh,
  the mirror stand-in (a synthetic stream imitating the mirror, not the shipped
  client) showed the seed reachable from L000001, 0 duplicates and 0 missing in
  order, after 3 framed rebuilds, **but the dump ended at L002714 of 3300 total**,
  so the tail was **not** verified; the cause of the missing 586 lines is
  unknown. The stand-in's checker now fails when the highest line is below the
  expected total. The re-check on the operator's machine is **pending**: no
  result is recorded. A larger `scroll_buffer_size` and Shift+Enter through omp
  are **not yet checked**.
- **Long sessions.** Reprint cost with `display.collapseCompacted: false` grows
  with the transcript; not measured at long-session sizes.
- **Phone:** see [Phone](#phone). Real devices are not verified.
- **Full screen.** CI drives the overlay path (Keyboard Lock removed) and a
  best-effort native path in Chromium. **Not verified:** a real Esc key (short
  press reaches the agent, hold exits) in native fullscreen, desktop Firefox and
  Safari, Android Back leaving fullscreen first, and any real phone. The
  Keyboard Lock call is best effort and its outcome is not observable in CI.
- **WebGL.** The pixel probes run on a software GL implementation in CI; real GPU
  output is not verified. At a device pixel ratio above 1, CI needs a real
  `--force-device-scale-factor` rather than Playwright's emulated factor.
- **Pins.** tmux 3.7c is pinned; the tap's xterm packages are pinned
  (`@xterm/headless` 6.0.0, serialize add-on 0.14.0, graphemes add-on 0.4.0,
  unicode11 add-on 0.9.0 in the tap; `@xterm/xterm` 6.0.0, image add-on 0.9.0 and
  WebGL add-on 0.19.0 in the SPA). `kb-term doctor` stamps harness versions.
- **Harnesses.** grok and opencode are experimental. Claude and codex exit
  sequences are unverified (the cgroup kill is the backstop). Claude's
  classic-renderer reprint behaviour is not characterised.
- **Binding.** See [Binding a terminal to a session](#binding-a-terminal-to-a-session).
- **Multi-viewer grid contention.** Every web client shares the one owner
  `kb-term`, so two browsers cannot be told apart: the latest explicit Fit wins
  and the other keeps a grid it did not choose until it fits again. Two local
  attaches still contend as before (refusal and fallback). Sustained contention
  is not characterised.
- **No local verification.** Every claim on this page that says "verified" rests
  on a GitHub-hosted CI lane named in the next section.

## Test inventory

All lanes run on GitHub-hosted runners. The witness column names the lane in
`ci/test-floors.toml` whose executed-test floor the lane must meet (floors only
ratchet upward); a dash means the lane has no kb-term-specific floor.

| lane | job in `.github/workflows/ci.yml` | what it proves | witness lane |
|---|---|---|---|
| Tap pipeline | `term-tap` | the stripper goldens, differential and fuzz tests against a reference implementation, the stripper mutation check, graphics extraction and image store, ring and resume, viewers and quiescent joins, the seed and the twin compatibility matrix, reseed, withhold; the synthetic fixture generators and the mirror stand-in unit tests; the compiled tap binary runs | `term-tap` |
| tmux integration | `term-int` | the pinned tmux 3.7c builds and passes a smoke test; real-tmux reseed equality (`term_int_reseed.rs`), the local mirror against a fake runner and tap (`term_int_attach.rs`), the runner against real tmux with a fake harness (the kb-term `integration` test), and the kb-server WebSocket round trip through the compiled tap (`terms_ws_int.rs`); packaging and a real install of the archive with the real installer | `term-int` |
| Real-stack end to end | `term-e2e` | real tmux, the compiled tap, `kb-termd`, three kb-server processes and the real SPA driven by synthetic harnesses (`spa-terminal-stack.spec.ts`), at device pixel ratios 1 and 2 | `term-e2e` |
| Terminal specs (fake transport) | `e2e` | the desktop panel (`spa-terminal.spec.ts`), phone Drive (`spa-terminal-drive.spec.ts`), pictures drawn over placeholders at DPR 1, 2 and 3 (`spa-terminal-gfx.spec.ts`), the phone view on Pixel 7, iPhone 15 and WebKit emulation (`spa-terminal-phone.spec.ts`) | `e2e` |
| Rust workspace | `workspace-test` | the gate matrix (`terms_gate.rs`), the WebSocket relay and ticket rules (`terms_ws.rs`), media (`terms_media.rs`), upload (`terminal_upload.rs`), the runner wire goldens (`wire_golden.rs`), profiles, doctor decisions and the docs tests; tmux-dependent kb-term tests return early here and only run with `KB_REQUIRE_TMUX=1`, which `term-int` sets | `nextest` |
| Generated bindings | `drift` | the TypeScript wire bindings equal what the Rust types generate | - |
| SPA unit | `web-unit` | the terminal panel, session machine, phone scroller, composer, key and choice bars, gfx renderer and media cache, Images strip (vitest, including `renderToString` and jsdom cases) | `web-vitest` |
| Hook | `hook-omp` | the omp extension's beat and subagent guards (bun) | - |
| Docs | `supply-chain` | doc line anchors, "pinned by" citations and the KT amendment text; this page is scanned | - |
| Route table | `workspace-test` | `docs/api-routes.md` equals the router | `nextest` |
| Public gate | `Public gate (whole tree)` in `repo-hygiene.yml` | no private pattern in the tree | - |

Specific pins cited by this page:

- Disabled is a bodiless 404 on every route:
  `terms_gate.rs::disabled_by_default_returns_bodiless_404_on_every_terms_route`.
- Exact Origin and the intent header on every route:
  `terms_gate.rs::every_terms_route_enforces_intent_and_origin`; loopback does not
  bypass: `terms_gate.rs::loopback_caller_without_origin_is_refused`.
- Ticket single use: `terms_ws.rs::ws_ticket_single_use`; close 1012 on restart:
  `terms_ws.rs::ws_shutdown_sends_close_1012_and_releases_the_connection`; the busy
  join retry: `terms_ws.rs::ws_busy_join_is_retried_and_the_browser_never_sees_it`.
- Wire frames match the golden JSON: `wire_golden.rs::server_frames_match_golden`
  and `wire_golden.rs::client_frames_match_golden`.
- The `[terminal]` keys are documented:
  `config.rs::every_terminal_key_is_documented`.
- The local mirror flags a truncated capture:
  `term_int_attach.rs::attach_t4_truncated_capture_is_flagged`.
