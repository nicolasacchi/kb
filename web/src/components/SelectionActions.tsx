import { useEffect, useRef } from "react";
import type { SelectionAnchor, SelectionRect } from "./AnnotatorBridge";
import { buildSelectionCite } from "../lib/quote";
import { toast } from "../lib/toast";
import { censusBump } from "../lib/census";
import {
  deriveMemoryTitle,
  pickMemoryKb,
  rememberMemory,
} from "../api/client";
import { useKbs } from "../hooks/useKbs";
import { useIsMobile } from "../hooks/useIsMobile";
import { useConfirm } from "./ConfirmProvider";
import AddToListButton from "./lists/AddToListButton";
import { Icon } from "./icons";

// W2.16 — the small floating chooser that appears near a text selection
// made inside the artifact iframe. Fed by AnnotatorBridge's mode-
// INDEPENDENT `cm:selection` relay (annotate.ts fires it in both view and
// annotate mode — see that file's header comment), so this works whether
// or not the comments pencil is on. Four actions:
//   comment    selection→comment — opens the SAME inline composer the
//              in-artifact click-to-compose path uses (`cp__compose` in
//              CommentsPanel, driven by detail.tsx's `composeAnchor`
//              state), keyed off THIS selection's anchor. This closes the
//              gap where authoring a comment from a selection required
//              annotate mode PLUS a click on text that was still selected
//              — nearly impossible on a phone, where the tap that would
//              open the composer collapses the selection first (WebKit
//              drops the DOM selection the instant focus moves to a
//              button). It works anyway because the anchor already
//              arrived as serialized plain data (css_path + offset +
//              snippet, via `cm:selection`) — composing needs no re-read
//              of the live `window.getSelection()`. See detail.tsx's
//              `onComposeSelection` wiring for why this deliberately does
//              NOT also arm annotate mode.
//   cite       a quiet clipboard citation (mirrors CommentsPanel's own
//              cite-comment button, minus the comment machinery — see
//              lib/quote.ts's buildSelectionCite).
//   add to list  the existing AddToListButton popover, scoped to this
//              selection's anchor. Invariant #25: a selection anchor rides
//              `review::Anchor` verbatim, so list membership, staleness,
//              and fuzzy re-resolution all apply for free — zero new
//              storage, zero new server code.
//   remember   U3 — save the highlight as a MEMORY. W2.16 shipped the
//              first two and left this one unwired pending a provenance
//              ruling (a human highlight is a new provenance class under
//              invariant #10). The ruling, implemented here:
//                · it rides the EXISTING memory ingest path —
//                  `rememberMemory` POSTs the same `/api/kb/{kb}/artifacts`
//                  body `kb remember` posts. No new store, no second write
//                  path, no third container type.
//                · provenance is EXPLICIT + STRUCTURAL: the memory records
//                  the source artifact (kb + id), this selection's
//                  `review::Anchor` verbatim (the SAME anchor the
//                  add-to-list button beside it uses — not a second anchor
//                  shape), and `author: "you"` — the human side of the
//                  `you | claude` ROLE split. A role, not an identity: kb
//                  is one daemon, one operator.
//                · provenance is SURFACED, NEVER SCORED. Recall stays
//                  `rank × salience × decay` with no term for who wrote
//                  the memory, no boost, no penalty — the same rule
//                  invariant #11's R3 applies to recollect staleness.
//                · the memory text is the SELECTION, VERBATIM. The confirm
//                  step lets the operator EDIT it; it never summarises it.
//                  kb runs no model, daemon-side or client-side.
//                · salience is the server default. "Human memories matter
//                  more" would be a score term wearing a hat.
//              The confirm/edit step is the existing `useConfirm()` host
//              (invariant #32 — the ONE prompt surface): a stray
//              triple-click must never silently write to memory.
//
// `rect` arrives already translated into the parent page's viewport
// coordinates (AnnotatorBridge owns that translation — it's the one
// holding the iframe element); this component only clamps to the
// viewport and sits in the popover z-layer — EXCEPT on mobile, where it
// skips the rect entirely.
//
// Mobile (≤860px, `useIsMobile()`): the floater becomes a FIXED BOTTOM
// ACTION BAR (`kb-selact--bar` in mobile.css) instead of tracking
// `rect.top`/`left`. Two reasons this is the right call rather than just
// shrinking the floater: the native iOS/Android selection callout already
// claims the screen real estate directly above/below a mobile selection,
// so anchoring a second floating toolbar there fights it for space and
// frequently loses (covered or covering); and a fixed bottom bar is a
// stable, thumb-reachable target regardless of where in the viewport the
// selection happened to land. The bar unmounts along with the rest of this
// component the moment a selection is dismissed or compose starts
// (`onDismiss`/`onComment`), so it never lingers as dead chrome.
export type SelectionActionsProps = {
  kb: string;
  artifactId: string;
  sourceRelative: string;
  title: string;
  anchor: SelectionAnchor;
  rect: SelectionRect;
  /// Nearest section heading id active when the selection was made (frozen
  /// at capture time by the caller), or null when unknown — folded into
  /// the cite permalink's `?sec=` when present.
  sectionId: string | null;
  /// Opens the inline comment composer on this selection's anchor. Omitted
  /// (the whole button is withheld) only if a caller genuinely has nowhere
  /// to route a compose — every real caller (ArtifactPane) always passes
  /// it, since #30's ONE inspector rail is always reachable.
  onComment?: () => void;
  onDismiss: () => void;
};

export default function SelectionActions({
  kb,
  artifactId,
  sourceRelative,
  title,
  anchor,
  rect,
  sectionId,
  onComment,
  onDismiss,
}: SelectionActionsProps) {
  const ref = useRef<HTMLDivElement | null>(null);
  const confirm = useConfirm();
  const { data: kbs } = useKbs();
  const isMobile = useIsMobile();
  // Which corpus a hand-kept memory lands in — the same resolution the CLI
  // does (`resolve_memory_kb`). `null` = this daemon has no unambiguous
  // memory corpus, and the action says so instead of guessing.
  const memoryKb = pickMemoryKb(kbs);

  useEffect(() => {
    // Esc is handled by the caller's own vim-style Esc chain (invariant
    // #30 — one home per action; detail.tsx already drops annotate-mode /
    // compose-draft layers there, and treats an open selection as the
    // topmost layer). Click-outside is self-contained here since it's a
    // purely local concern.
    function onDocDown(e: MouseEvent) {
      if (!ref.current?.contains(e.target as Node)) onDismiss();
    }
    document.addEventListener("mousedown", onDocDown);
    return () => document.removeEventListener("mousedown", onDocDown);
  }, [onDismiss]);

  function copyCite() {
    const md = buildSelectionCite({
      kb,
      sourceRelative,
      title,
      anchor,
      sec: sectionId,
    });
    void navigator.clipboard
      .writeText(md)
      .then(() => toast.ok("citation copied"))
      .catch(() => toast.err("copy failed"));
    onDismiss();
  }

  // U3 — highlight → memory. The draft lives in a ref, not state: the
  // confirm host renders `body` as a snapshot ReactNode, so the textarea
  // is uncontrolled and writes through here. Reading a ref after the
  // await is safe even though this component unmounts when the modal
  // steals the click (the click-outside handler above fires) — a ref read
  // needs no mounted component, where a `useState` value would be stale.
  const draftRef = useRef<string>("");

  async function saveAsMemory() {
    if (!memoryKb) return; // button is disabled in this case
    censusBump("selection.remember.open");
    draftRef.current = anchor.snippet;
    const ok = await confirm({
      title: "Remember this highlight?",
      danger: false,
      confirmLabel: "Remember",
      body: (
        <>
          <p>
            Saves the selection as a memory in <code>{memoryKb}</code>,
            stamped with where it came from — <em>{title}</em> and this
            exact highlight. Stored verbatim; kb never summarises it.
          </p>
          <span className="confirm__hint">memory text</span>
          <textarea
            className="confirm__input"
            data-kb-act="selection-remember-text"
            aria-label="memory text"
            rows={5}
            spellCheck={false}
            defaultValue={anchor.snippet}
            onChange={(e) => {
              draftRef.current = e.target.value;
            }}
            style={{
              display: "block",
              boxSizing: "border-box",
              width: "100%",
              marginTop: 6,
              resize: "vertical",
            }}
          />
        </>
      ),
    });
    if (!ok) {
      onDismiss();
      return;
    }
    const text = draftRef.current.trim();
    if (!text) {
      toast.err("nothing to remember — the text was empty");
      onDismiss();
      return;
    }
    try {
      await rememberMemory(memoryKb, {
        title: deriveMemoryTitle(text),
        body: text,
        // Provenance: source artifact + the selection anchor + the human
        // ROLE. No salience/decay override — the server default stands.
        author: "you",
        source_kb: kb,
        source_artifact: artifactId,
        source_anchor: anchor,
      });
      censusBump("selection.remember.save");
      toast.ok("remembered");
    } catch (e) {
      // Invariant #32 — a user-action .catch never swallows.
      toast.err(e instanceof Error ? e.message : "remember failed");
    }
    onDismiss();
  }

  // Clamp so the floater never runs off either edge. A generous fixed
  // width estimate keeps the right-edge clamp simple — no measure-after-
  // paint dance for what's just a four-button toolbar (320 covers the
  // widest case: comment + cite + list + remember; the old 220 was sized
  // for three buttons and let a right-edge selection push the comment
  // button offscreen). Mobile ignores this entirely (see the
  // `kb-selact--bar` doc comment above the props type) — a fixed bottom
  // bar has no rect to clamp.
  const left = Math.min(Math.max(8, rect.left), window.innerWidth - 320);
  const top = Math.min(rect.bottom + 6, window.innerHeight - 44);

  return (
    <div
      ref={ref}
      className={`kb-selact${isMobile ? " kb-selact--bar" : ""}`}
      role="toolbar"
      aria-label="selection actions"
      style={isMobile ? undefined : { top, left }}
    >
      {onComment && (
        <button
          type="button"
          data-kb-act="selection-comment"
          className="kb-selact__btn"
          onClick={onComment}
          title="comment on this selection"
        >
          <Icon.Comment aria-hidden="true" /> comment
        </button>
      )}
      <button
        type="button"
        data-kb-act="selection-cite"
        className="kb-selact__btn"
        onClick={copyCite}
        title="copy a markdown citation — quote + a deep link"
      >
        ❝ cite
      </button>
      <AddToListButton
        kb={kb}
        artifactId={artifactId}
        anchor={anchor}
        variant="icon"
        dataAct="selection-list"
      />
      <button
        type="button"
        data-kb-act="selection-remember"
        className="kb-selact__btn"
        onClick={() => {
          void saveAsMemory();
        }}
        disabled={!memoryKb}
        title={
          memoryKb
            ? `save this highlight as a memory in ${memoryKb} — you review the text first`
            : "no memory corpus on this daemon (a kb with memory_scope = “project” or “global”)"
        }
      >
        <Icon.Spark aria-hidden="true" /> remember
      </button>
    </div>
  );
}
