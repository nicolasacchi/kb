# Keys that moved in V70-A5 (cmd/1)

Every binding in kb-code is now declared once, in
`crates/kb-code-server/commands/registry.json` (kbc-cmd/1). The migration was
behaviour-preserving **except** for the rows below, each of which is a
documented-surface change under design decision **D2**:

> Space is only the leader, globally. K peek everywhere. `.` action panel
> (vim's repeat has no meaning in a read-only instrument), `f` hint-jump with
> `F` act-on-target, `s`/`S` scope, `m{a-z}` stays marks. Three departures
> from vim convention (`.`, `f`, `s`) and every intra-app collision the
> generated conflicts report finds are on the ratification list; v7.0 exits
> with zero unratified rows. **Review-diff verbs that collide with reader
> verbs move under the leader.**

`kb-code commands conflicts` prints the generated report these moves resolve;
`kb-code commands doctor` fails the build on any unratified row.

---

## 1. Review-diff verbs that collided with reader verbs

| Command | Was | Now | Why |
|---|---|---|---|
| `diff.toggle-viewed` | `v` | `Space v` | `v` is visual select in the reader. Two meanings, no mode indicator (recon R5). |
| `diff.viewed-advance` | `V` | `Space V` | Same, for visual-line. |
| `diff.split-toggle` | `s` | `Space s` | D2 ratifies bare `s` as the **scope** verb, globally. |
| `diff.permalink-copy` | `y` | `Y` | *Harmonised, not moved*: `y` is yank in the reader, but `Y` is already the reader's copy-permalink verb. Same verb, different object — so it keeps a bare key and is a **ratified conflict** with `permalink.copy` rather than a move. |
| `diff.file-next` / `diff.file-prev` | `n` / `p` (and `]f` / `[f`) | `]f` / `[f` only | `n` / `N` is search-step in the reader. The diff already had the `[f`/`]f` pair, which is *also* the reader's working-set pair — so retiring `n`/`p` removed a collision and left the two surfaces agreeing. |
| `diff.tour-next` / `diff.tour-prev` | `n` / `p` **when a tour was running** | `] t` / `[ t` | The worst row in the recon (R5): `n`/`p` silently changed meaning on `L.tourOn`, with nothing on screen to say so. The tour now has its own keys and is `when: diff.tour` in the registry, so the `?` sheet and the palette both show that it is conditional. |

Unchanged in the diff, because the reader does not bind them: `c` / `C`
(compose), `x` (collapse), `t` / `T` (thread step), `o` (overlay cycle), `d`
(disposition menu), `j` / `k`, `gg` / `G`.

## 2. Space is the leader, everywhere

| Command | Was | Now | Why |
|---|---|---|---|
| `board.pan` (canvas) | hold `Space` | hold `Alt` | The canvas was the one surface where the leader — and therefore the whole `Space …` family, the drawer tabs and the rehearsal overlay — did not exist. `Alt` is free (the reader's vim layer bails on Alt) and is the pan modifier Figma and Excalidraw already train. |
| `player.autoplay` (story) | `Space` | `p` | Same reason; `p` is play/pause. |

## 3. Esc never navigates

§P2: *"Esc rows are a first-class key class with a dismiss order (innermost
first); Esc never navigates."*

| Surface | Was | Now |
|---|---|---|
| Review diff | `Esc` → help → tour → **back to the cockpit** | `Esc` dismisses help (`dismiss.help`, order 2), then the disposition menu (`dismiss.menu`, 3), then leaves the tour (`dismiss.mode`, 7). Leaving the page is `u` (`nav.back`) or browser Back. |
| Tour (`~sets/:id/~tour`) | `Esc` → **back to the set** | Not bound. `u` / browser Back — which restore where you came from, instead of jumping to a fixed destination. |
| Story player | `Esc` → exit the story | Unchanged in effect, reclassified as `dismiss.mode`: it leaves a MODE and returns you to the file you were already reading. |

The full dismiss ladder, innermost first:

| Order | Row | When |
|---|---|---|
| 1 | `dismiss.popover` | a row popover inside the palette |
| 2 | `dismiss.help` | the `?` sheet |
| 3 | `dismiss.menu` | the disposition menu |
| 4 | `dismiss.overlay` | a result panel (peek / hierarchy / impact / ego-graph / DAG) |
| 5 | `dismiss.palette` | the palette itself |
| 6 | `dismiss.sheet` | a mobile sheet |
| 7 | `dismiss.mode` | a tour / resize submode / canvas selection |
| 8 | `mode.normal` | the buffer's visual mode → normal |

A pending chord is a special case handled before the ladder: `Escape` with a
prefix in flight **cancels the chord and stops**. One keystroke, one job — it
does not also dismiss the panel underneath.

## 4. Two keys that grew, rather than moved

* **`?`** was route-local in two of twenty-seven routes and dead in the rest.
  It is now `scope: global`, so it works everywhere, and the sheet it opens is
  scoped to wherever you pressed it.
* **`⌘K` / `Ctrl-K`** kept its meaning (search) and gained `:` and `Space :`
  as the command-mode door, plus the `>` prefix inside the box.

## 5. Nothing else moved

Every other binding in the recon's ~90-row table resolves to the same command
it did before, in the same scope. `web-code/src/commands/dispatch.test.ts`
pins the full key→id table per scope; if a future edit changes what an
existing key does, that golden is what says so.

---

## 6. The 43 ratified conflicts

`kb-code commands conflicts` lists every key bound to two different commands
in two coactive scopes at the same modal depth. After the moves above, 43
pairs remain and all 43 are ratified, in three families:

* **Same verb, different unit** (the bulk). `j`/`k` = "next/previous thing"
  in six scopes — a line, a hunk, a review, a branch, a symbol row, a
  reference row. `gg`/`G` = first/last. `[f`/`]f` = previous/next file, which
  the reader and the diff now agree on exactly. `Enter` = open the focused
  row. These are not collisions to be resolved; they are the vocabulary
  working.
* **Same verb, different object.** `Y` copies a permalink — to a line in the
  reader, to a finding in the diff.
* **Modal, mutually exclusive in practice.** `a` (annotate / agree / ask),
  `w` (word-forward / waive), `f` (hint-jump / fix-later): the diff's are all
  `when: diff.menu` rows, reachable only while the disposition menu is up.
  The resolver prefers the scope-specific row, so the menu always wins while
  it is open — and the pair is written down so a future reader can see that
  was a decision.

The list is a SNAPSHOT of decisions taken at this commit, not a computed
identity: a collision introduced later will not be in it, and
`kb-code commands doctor` will fail until a human either moves the key or
writes down why it stays.
