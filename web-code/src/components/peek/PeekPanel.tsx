import { useEffect, useRef } from "react";
import EmptyState from "../EmptyState";
import { Icon } from "../icons";
import TrustBadge from "../TrustBadge";
import {
  currentRow,
  resolveCandidateToRow,
  type HoverCard as HoverCardT,
  type PeekRow,
  type PeekState,
} from "../../lib/peekState";
import ScentCard from "../nav/ScentCard";
import { rungForKey, rungForMouse, type RampRung, type RampTarget } from "../../nav/ramp";
import { useScentIntent } from "../../hooks/useScentIntent";
import { fullSearchUrl } from "../../lib/searchLanes";
import { buildSym, langIdForPath, symbolPermalinkFor } from "../../lib/codeUrl";
import { useCommands } from "../../commands/CommandRoot";
import { copyToClipboard } from "../../editor/vimReader";
import { toast } from "../../lib/toast";

/// Fixed panel width used to keep the cursor-anchored placement on-screen
/// (`computeStyle` below) — matches the CSS `width` on `.kbc-peek`.
const PANEL_WIDTH = 420;
const VIEWPORT_MARGIN = 12;
const MIN_SPACE_BELOW = 220;

const APPROX_LABEL = "approximate — name match, not scope resolution";

const MODE_LABEL: Record<PeekState["mode"], string> = {
  defs: "Definitions",
  refs: "References",
  hover: "Quick info",
};

export interface PeekAnchor {
  top: number;
  bottom: number;
  left: number;
}

export interface PeekPanelProps {
  state: PeekState;
  /// The repo currently open in the reader — rows whose `repo` differs get
  /// a repo tag; rows that match navigate WITHOUT one (Deliverable 2's
  /// cross-repo requirement).
  currentRepo: string;
  /// Cursor-anchored placement (`CodeViewHandle.cursorCoords()`, captured
  /// by `Reader.tsx` when the panel opens). `null` docks the panel to the
  /// bottom of `.kbc-reader__main` instead (the v1 fallback the milestone
  /// brief allows when coordinates aren't available).
  anchor: PeekAnchor | null;
  onMove(delta: number): void;
  onActivate(row: PeekRow): void;
  onClose(): void;
  /// T1 (design-ui.md §9.1) — the hover card's footer action hints
  /// (`Enter go to def · u usages · h callers`). Both OPTIONAL and only
  /// rendered when the top candidate is `class: "exact"` AND carries a
  /// signature/doc — wiring existing Reader handlers (`handleFindRefs`/
  /// `handleHierarchyCallers`) through as plain onClick affordances, never
  /// new GLOBAL keybindings (the brief's own constraint: "do not invent new
  /// global keys beyond K").
  onFindRefs?(): void;
  onFindCallers?(): void;
  /// V72-G1.2 — "Open dossier", offered when the peeked identifier is a
  /// CONSTANT (a class or module). An actions/1-style ROW, never an
  /// auto-navigation: D5's rule is that a row which cannot PROVE `exact`
  /// must not jump, and this one deliberately does not even try to resolve
  /// the entity first — it hands the identifier to `entity/1`, which
  /// answers with the dossier, with `candidates` when the name is
  /// ambiguous, or with an honest `entity-unknown`. Offered at every trust
  /// class for the same reason `u` stopped gating on `exact` (V71-E2): in
  /// Ruby, where this lane lives, `exact` is structurally hard to reach,
  /// so gating would hide the affordance from the only language that has
  /// it. The host supplies the callback only for a constant-shaped target.
  onOpenDossier?(): void;
  /// V70-A6 — the Ramp (§P7). One handler for every rung on every result
  /// surface (`nav/ramp.ts`); this panel supplies the target and the host
  /// runs it. Absent ⇒ `Enter` keeps its pre-A6 behaviour (`onActivate`) and
  /// the other rungs do nothing, which is the honest degrade for a host that
  /// has not adopted the Ramp.
  onRamp?(rung: RampRung, row: PeekRow): void;
  /// Scent for a row — what the hover/focus card shows. Returning `null`
  /// suppresses the card for that row.
  scentFor?(row: PeekRow): RampTarget | null;
  /// How many times the current trail has landed on a row's file.
  visitsFor?(row: PeekRow): number;
  /// V70-A4 — "Keep in drawer": promote THIS row set into a bottom-drawer
  /// tab that survives the Esc which closes this popup (docs/research/
  /// kb-code-v7-continuum-2026-09.html §P1, the drawer's result-set
  /// ring). Absent on a host with no Desk around it (`?shell=legacy`),
  /// in which case the action simply isn't offered — never a dead
  /// button. No new fetch: the rows handed over are the rows already on
  /// screen.
  onKeepInDrawer?(): void;
}

/// V71-K4 — the keys this panel ITSELF acts on, and therefore the only
/// ones it may stop from reaching the rest of the app. Pure and exported so
/// the filter is pinned by a unit test rather than by reading the switch
/// below and hoping.
///
/// Its `onKeyDown` used to call `stopPropagation()` unconditionally, for
/// every key, "to keep the vim keymap and the window tree handler out of
/// its business" — and the panel focuses itself on mount, so from then on
/// the panel was a hole in the keyboard. The one that mattered:
/// `drawer.keep` (`Space K`) is the command for "keep THIS result set in
/// the drawer", so the only moment it is worth pressing is a populated
/// peek — which is exactly when the panel existed to eat it. Reported by
/// V71-K3 as structurally unreachable; this is the fix.
///
/// Blanket interception was never needed for the vim keymap either: that
/// layer lives inside `.cm-editor`, and while this panel holds focus the
/// keydown's path does not go through it at all.
export function peekPanelHandlesKey(e: {
  key: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
}): boolean {
  // The list's own cursor and its dismissal — BARE only. `j`/`k` are the
  // panel's letters, `Mod-k` is the omnibox's and `Ctrl-o`/`Ctrl-i` are the
  // jump list's, and a panel that claimed those would be the same hole one
  // modifier over.
  if (!e.ctrlKey && !e.metaKey && !e.altKey) {
    switch (e.key) {
      case "ArrowDown":
      case "j":
      case "ArrowUp":
      case "k":
      case "Escape":
        return true;
    }
  }
  // Every Ramp rung this panel offers (`Enter`/`Shift-Enter`/`Ctrl-Enter`/
  // `o`/`O`/`K`), resolved by the SAME shared table the handler uses — one
  // home, so the two can never drift apart.
  return rungForKey(e) !== null;
}

function computeStyle(anchor: PeekAnchor | null): React.CSSProperties {
  if (!anchor || typeof window === "undefined") return {};
  const left = Math.min(Math.max(anchor.left, VIEWPORT_MARGIN), window.innerWidth - PANEL_WIDTH - VIEWPORT_MARGIN);
  const spaceBelow = window.innerHeight - anchor.bottom;
  const openUpward = spaceBelow < MIN_SPACE_BELOW && anchor.top > spaceBelow;
  return openUpward
    ? { left: Math.max(left, VIEWPORT_MARGIN), bottom: window.innerHeight - anchor.top + 6 }
    : { left: Math.max(left, VIEWPORT_MARGIN), top: anchor.bottom + 6 };
}

function RowKindTag({ row, currentRepo }: { row: PeekRow; currentRepo: string }) {
  return (
    <span className="kbc-peek__row-loc">
      {row.repo !== currentRepo && <span className="kbc-peek__row-repo">{row.repo}</span>}
      <span className="kbc-peek__row-path">{row.path}</span>
      <span className="kbc-peek__row-line">:{row.line}</span>
    </span>
  );
}

/// T1 (design-ui.md §9.1) — the per-candidate trust badge, wrapping the
/// shared `TrustBadge` (`lib/trustBadge.ts`'s `exact`/`likely`/`candidate`
/// tiers) in a stable `[data-kbc-peek-badge]` shell so existing e2e
/// selectors (`e2e/clickable.spec.ts`) keep finding it. `cls` is the
/// candidate's OWN `resolve::Candidate.class` — an older daemon that only
/// ever sent `precision` (no `class` at all) classifies DOWN to
/// `"candidate"` (`trustTierFrom`'s own documented policy), never guessed
/// UP to `exact`/`likely`. `precision === "lsp-live"` additionally renders a
/// small "live" label naming the provider lane, alongside the (already
/// `exact`-tier) badge — see `TrustBadge`'s own doc. Distinct from the
/// header's whole-set `.kbc-peek__badge` (the name-based `defs`/`refs`
/// fallback's own honesty flag) — resolve tells its honesty story PER
/// CANDIDATE, not per response.
function PrecisionBadge({ precision, cls, note }: { precision: string; cls?: string; note?: string }) {
  return (
    <span data-kbc-peek-badge data-kbc-peek-precision={precision}>
      <TrustBadge cls={cls} precision={precision} title={note} />
    </span>
  );
}

/// T1 (design-ui.md §9.2) — copies a `?sym=` deep link for `row` (a
/// resolve-candidate-backed row only — see the call sites: `DefRow` gates on
/// `row.symbolKind`, `HoverCardView` always has one since a card only
/// exists once resolve found a real candidate). `word` is the identifier
/// name resolve reported for the WHOLE panel/card (`state.word`/
/// `card.ident`) — not carried per-row, since every row in a `defs`-mode
/// result set shares it by construction. `langIdForPath` degrades to the
/// generic `"code"` namespace for an unrecognized extension — see that
/// function's own doc; the namespace is cosmetic for anything but the
/// `rails:` construct dispatch (`symbol_addr.rs`'s module doc), so a miss
/// here never breaks resolution, only the label.
function copySymbolLink(currentRepo: string, word: string, row: PeekRow) {
  const sym = buildSym({
    namespace: langIdForPath(row.path) ?? "code",
    name: word,
    container: row.container,
    kind: row.symbolKind,
  });
  copyToClipboard(
    symbolPermalinkFor(window.location.origin, row.repo || currentRepo, sym, {
      fallbackPath: row.path,
      fallbackLine: row.line,
    }),
  );
  toast.ok("symbol link copied");
}

function CopySymbolLinkButton({
  currentRepo,
  word,
  row,
}: {
  currentRepo: string;
  word: string;
  row: PeekRow;
}) {
  return (
    <button
      type="button"
      className="kbc-peek__row-copy-sym"
      title="copy symbol link"
      aria-label="copy symbol link"
      data-kbc-peek-copy-sym
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        copySymbolLink(currentRepo, word, row);
      }}
    >
      <Icon.Copy width={12} height={12} aria-hidden />
    </button>
  );
}

function DefRow({
  row,
  currentRepo,
  note,
  word,
}: {
  row: PeekRow;
  currentRepo: string;
  note?: string;
  word?: string;
}) {
  return (
    <>
      {row.symbolKind && <span className="kbc-peek__row-kind">{row.symbolKind}</span>}
      <span className="kbc-peek__row-main">
        <RowKindTag row={row} currentRepo={currentRepo} />
        {row.container && <span className="kbc-peek__row-container"> — in {row.container}</span>}
      </span>
      {row.precision && <PrecisionBadge precision={row.precision} cls={row.trustClass} note={note} />}
      {/* T1 §9.2 — copy-symbol-link: "every peek CANDIDATE row" (a real
          symbol, not a bare text ref match) gets this affordance. */}
      {row.symbolKind && word && <CopySymbolLinkButton currentRepo={currentRepo} word={word} row={row} />}
    </>
  );
}

function RefRow({ row, currentRepo }: { row: PeekRow; currentRepo: string; note?: string; word?: string }) {
  return (
    <span className="kbc-peek__row-main">
      <RowKindTag row={row} currentRepo={currentRepo} />
      {row.text !== undefined && <code className="kbc-peek__row-text">{row.text.trim()}</code>}
    </span>
  );
}

/** Cap the doc excerpt to ~12 lines for the hover card (V3.1-H3a). */
function docExcerpt(doc: string, maxLines = 12): { text: string; clipped: boolean } {
  const lines = doc.split(/\r?\n/);
  if (lines.length <= maxLines) return { text: doc, clipped: false };
  return { text: lines.slice(0, maxLines).join("\n"), clipped: true };
}

/// `K`'s provenance hover card (B3 + H3a quick-doc upgrade) — replaces the
/// row list ENTIRELY once `state.card` is populated. Signature renders mono
/// with the first line emphasized; doc (when present) is capped ~12 lines;
/// meta + trust badge + provenance (when the why fetch lands) follow. Absent
/// signature/doc never leave empty boxes — graceful degradation only.
///
/// T1 (design-ui.md §9.1) — when `sig`/`doc` is present, the footer renders
/// action hints instead of the old bare "open definition" text link: `Enter
/// go to def` always (mirrors the old behavior, now styled as the first
/// hint); `u usages`/`h callers` ADDITIONALLY, but ONLY when the top
/// candidate is `class: "exact"` AND the corresponding callback was given
/// (an older/lower-tier candidate keeps the def-only footer — this app
/// never claims a confidence it doesn't have).
function HoverCardView({
  card,
  currentRepo,
  note,
  onOpenDefinition,
  onFindRefs,
  onFindCallers,
  onOpenDossier,
}: {
  card: HoverCardT;
  currentRepo: string;
  note?: string;
  onOpenDefinition?: () => void;
  onFindRefs?: () => void;
  onFindCallers?: () => void;
  onOpenDossier?: () => void;
}) {
  const { candidate } = card;
  const sig = candidate.signature ?? null;
  const sigLines = sig ? sig.split(/\r?\n/) : [];
  const sigFirst = sigLines[0] ?? card.ident;
  const sigRest = sigLines.length > 1 ? sigLines.slice(1).join("\n") : "";
  const doc = candidate.doc ? docExcerpt(candidate.doc) : null;
  const isExact = candidate.class === "exact";
  return (
    <div className="kbc-peek__card" data-kbc-peek-card>
      {/* Doc section ABOVE provenance — signature + doc excerpt first. */}
      <code
        className={"kbc-peek__card-sig" + (sig ? "" : " kbc-peek__card-sig--bare")}
        data-kbc-peek-sig
      >
        <span className="kbc-peek__card-sig-first">{sigFirst}</span>
        {sigRest && <span className="kbc-peek__card-sig-rest">{"\n" + sigRest}</span>}
      </code>
      {doc && (
        <p className="kbc-peek__card-doc" data-kbc-peek-doc>
          {doc.text}
          {doc.clipped ? "…" : ""}
        </p>
      )}
      {(sig || doc) && onOpenDefinition && (
        <div className="kbc-peek__card-actions" data-kbc-peek-card-actions>
          <button
            type="button"
            className="kbc-peek__card-action"
            data-kbc-peek-action="def"
            onClick={(e) => {
              e.preventDefault();
              e.stopPropagation();
              onOpenDefinition();
            }}
          >
            <kbd>Enter</kbd> go to def
          </button>
          {/* V71-E2 — no longer gated on `class === "exact"`. The gate was
              correct while `u` fired a grep (recon §6.8: for Ruby and Go,
              where `exact` is structurally unreachable, the affordance never
              rendered — so the languages that most need a usages list were
              the ones that could not reach it). `u` now opens the CLASSIFIED
              dock, which groups BY trust and is meaningful at every class;
              `h` (callers) keeps its gate, because the call hierarchy really
              does only cover four proof languages. */}
          {onFindRefs && (
            <button
              type="button"
              className="kbc-peek__card-action"
              data-kbc-peek-action="usages"
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                onFindRefs();
              }}
            >
              <kbd>u</kbd> usages
            </button>
          )}
          {isExact && onFindCallers && (
            <button
              type="button"
              className="kbc-peek__card-action"
              data-kbc-peek-action="callers"
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                onFindCallers();
              }}
            >
              <kbd>h</kbd> callers
            </button>
          )}
          {/* V72-G1.2 — see `onOpenDossier`'s own doc: a ROW, never a jump. */}
          {onOpenDossier && (
            <button
              type="button"
              className="kbc-peek__card-action"
              data-kbc-peek-action="dossier"
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                onOpenDossier();
              }}
            >
              <kbd>Space e d</kbd> dossier
            </button>
          )}
        </div>
      )}
      <div className="kbc-peek__card-meta">
        {candidate.kind && <span className="kbc-peek__row-kind">{candidate.kind}</span>}
        <RowKindTag row={resolveCandidateToRow(candidate)} currentRepo={currentRepo} />
        {candidate.container && <span className="kbc-peek__row-container"> — in {candidate.container}</span>}
        <PrecisionBadge precision={candidate.precision} cls={candidate.class} note={note} />
        <CopySymbolLinkButton currentRepo={currentRepo} word={card.ident} row={resolveCandidateToRow(candidate)} />
      </div>
      {card.provenance && (
        <div className="kbc-peek__card-provenance" data-kbc-peek-provenance>
          {card.provenance.none
            ? "no recorded session"
            : [card.provenance.displayName, card.provenance.commitSubject].filter(Boolean).join(" — ")}
        </div>
      )}
    </div>
  );
}

/// A floating quick-navigation panel over `/api/defs` + `/api/xrefs` (B1) +
/// `/api/resolve` (B3) — `gd` (definitions), `gr` (references), `K`
/// (provenance hover). Keyboard-first: while open, it holds real DOM focus
/// and stops the keydowns it ACTS on from bubbling (mirrors `KeyboardHelp`'s
/// Esc-capture, but for a key set rather than one key, since this panel —
/// unlike that static cheatsheet — has rows to navigate) so neither the vim
/// keymap nor the window-level tree-nav handler double-handles Up/Down/j/k/
/// Enter/Esc while it's up. V71-K4 narrowed that from "EVERY keydown" to
/// `peekPanelHandlesKey`'s set: a panel that swallows the whole keyboard
/// takes the app's global commands down with it — including `Space K`, the
/// one command whose entire purpose is to act on the peek that is open. See
/// that predicate's doc.
///
/// Two body shapes, chosen by `state.card`'s presence (not `state.mode`):
/// the row list (every mode, `gd`'s resolve/fallback candidates or `gr`'s
/// refs) or `K`'s single provenance card (`HoverCardView`, B3) once
/// `/api/resolve` has answered. Honesty is never hidden: the row list's
/// whole-set `approximate` badge (name-based `defs`/`refs` fallback) and
/// resolve's PER-ROW `precision` badge (`PrecisionBadge`) both carry the
/// server's own `note` as their tooltip.
export default function PeekPanel({
  state,
  currentRepo,
  anchor,
  onMove,
  onActivate,
  onClose,
  onFindRefs,
  onFindCallers,
  onOpenDossier,
  onKeepInDrawer,
  onRamp,
  scentFor,
  visitsFor,
}: PeekPanelProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const bus = useCommands();
  // The Ramp's hover rung. Suppressed for 3 s after any keyboard move in this
  // list, never fired on touch — see `hooks/useScentIntent.ts`.
  const scent = useScentIntent<PeekRow>();

  useEffect(() => {
    containerRef.current?.focus();
  }, []);

  if (!state.open) return null;

  function onKeyDown(e: React.KeyboardEvent) {
    // V71-K4 — a central chord already in flight owns its continuation key,
    // even one this panel would otherwise claim: `K` is the Ramp's peek
    // rung here AND `drawer.keep`'s final token (`Space K`). Asked first,
    // so the chord always wins the tie.
    if (bus.chordWillConsume(e)) return;
    // Stop only the keys this panel actually acts on from reaching the vim
    // keymap / the window-level dispatcher; everything else keeps
    // bubbling, which is what makes any global command (and any chord)
    // reachable with a peek open. `preventDefault` stays narrower still —
    // only the keys handled below — so e.g. Tab still behaves natively.
    if (!peekPanelHandlesKey(e)) return;
    e.stopPropagation();
    switch (e.key) {
      case "ArrowDown":
      case "j":
        e.preventDefault();
        scent.noteKeyboard();
        onMove(1);
        return;
      case "ArrowUp":
      case "k":
        e.preventDefault();
        scent.noteKeyboard();
        onMove(-1);
        return;
      case "Escape":
        e.preventDefault();
        onClose();
        return;
    }
    // V70-A6 — every other Ramp rung, resolved by the ONE shared table
    // (`nav/ramp.ts`'s `rungForKey`, which mirrors the registry rows). `Enter`
    // resolves to `"here"`, so the pre-A6 behaviour survives a host that has
    // not passed `onRamp`.
    const rung = rungForKey(e);
    if (!rung) return;
    const row = state.card ? resolveCandidateToRow(state.card.candidate) : currentRow(state);
    if (!row) return;
    e.preventDefault();
    if (onRamp) onRamp(rung, row);
    else if (rung === "here") onActivate(row);
  }

  const style = anchor ? computeStyle(anchor) : undefined;
  const className = "kbc-peek" + (anchor ? "" : " kbc-peek--dock");
  const Row = state.mode === "refs" ? RefRow : DefRow;

  return (
    <div
      ref={containerRef}
      className={className}
      style={style}
      role="dialog"
      aria-modal="true"
      aria-label={`${MODE_LABEL[state.mode]}: ${state.word}`}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      data-kbc-peek
      data-kbc-peek-mode={state.mode}
    >
      <header className="kbc-peek__head">
        <span className="kbc-peek__mode">{MODE_LABEL[state.mode]}</span>
        <span className="kbc-peek__title">{state.word}</span>
        {!state.card && state.approximate && (
          <span className="kbc-peek__badge" title={state.note ?? APPROX_LABEL} data-kbc-peek-badge>
            {APPROX_LABEL}
          </span>
        )}
        {onKeepInDrawer && !state.card && !state.loading && state.rows.length > 0 && (
          <button
            type="button"
            className="kbc-peek__keep"
            onClick={onKeepInDrawer}
            title="keep this result set in the drawer, beside the code"
            data-cmd="drawer.keep"
            data-kbc-peek-keep
          >
            Keep in drawer
          </button>
        )}
        <button type="button" className="kbc-peek__close" onClick={onClose} aria-label="close">
          <Icon.X />
        </button>
      </header>
      {state.card ? (
        <HoverCardView
          card={state.card}
          currentRepo={currentRepo}
          note={state.note}
          onOpenDefinition={() => onActivate(resolveCandidateToRow(state.card!.candidate))}
          onFindRefs={onFindRefs}
          onFindCallers={onFindCallers}
          onOpenDossier={onOpenDossier}
        />
      ) : (
        <div className="kbc-peek__body" role="listbox" aria-label={`${MODE_LABEL[state.mode]} results`}>
          {state.loading && <div className="kbc-peek__hint kbc-peek__loading">Loading…</div>}
          {!state.loading && state.error && <div className="kbc-peek__hint kbc-peek__error">{state.error}</div>}
          {!state.loading && !state.error && state.rows.length === 0 && (
            <EmptyState
              variant="inline"
              title={`No ${MODE_LABEL[state.mode].toLowerCase()} found`}
              hint={state.note}
              action={{ label: `Search "${state.word}" everywhere`, to: fullSearchUrl(state.word, currentRepo) }}
            />
          )}
          {!state.loading &&
            !state.error &&
            state.rows.map((row, i) => (
              <div
                key={`${row.repo}/${row.path}:${row.line}`}
                className={"kbc-peek__row" + (i === state.cursor ? " kbc-peek__row--active" : "")}
                // V70-A6 — middle-click and Ctrl/Cmd-click now mean what they
                // mean everywhere else in a browser (recon: a peek row was one
                // of six surfaces where they silently did nothing).
                onMouseDown={(e) => {
                  const rung = rungForMouse(e);
                  if (!rung || rung === "here") return;
                  e.preventDefault();
                  if (onRamp) onRamp(rung, row);
                }}
                onClick={() => (onRamp ? onRamp("here", row) : onActivate(row))}
                onPointerEnter={(e) => scent.enter(row, e)}
                onPointerLeave={scent.leave}
                role="option"
                aria-selected={i === state.cursor}
              >
                <Row row={row} currentRepo={currentRepo} note={state.note} word={state.word} />
                {scentFor && i === state.cursor && (() => {
                  const t = scentFor(row);
                  return t ? <ScentCard target={t} visits={visitsFor?.(row) ?? 0} inline /> : null;
                })()}
              </div>
            ))}
          {scentFor && scent.active && scent.at && (() => {
            const t = scentFor(scent.active);
            return t ? (
              <ScentCard
                target={t}
                visits={visitsFor?.(scent.active) ?? 0}
                style={{ position: "fixed", left: scent.at.x + 12, top: scent.at.y + 12 }}
              />
            ) : null;
          })()}
          {state.mode === "hover" && state.counts && (
            <div className="kbc-peek__counts">
              {state.counts.defs} definition{state.counts.defs === 1 ? "" : "s"} · {state.counts.refs} reference
              {state.counts.refs === 1 ? "" : "s"}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
