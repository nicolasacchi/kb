import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useParams, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  useSlateBoard,
  useSlateHistory,
  useSlatePost,
  SPA_PROV,
} from "../hooks/useSlates";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { useIsMobile } from "../hooks/useIsMobile";
import { useActiveKb } from "../hooks/useActiveKb";
import { useCodeUrlForKb } from "../hooks/useCodeUrlForKb";
import { useScrollRestoration } from "../hooks/useScrollRestoration";
import { useNowTick } from "../hooks/useNowTick";
import { useConfirm } from "../components/ConfirmProvider";
import { toast } from "../lib/toast";
import { fetchIdentity } from "../api/client";
import { isEditableTarget } from "../lib/keymap";
import {
  COLUMN_IDS,
  columnsOf,
  lanesOf,
  nowRows,
  readingOrder,
  warnRows,
  moveCursor,
} from "../lib/slateLanes";
import NowBand from "../components/slate/NowBand";
import SlateColumn from "../components/slate/SlateColumn";
import SlateComposer, { type ComposerSeed } from "../components/slate/SlateComposer";
import HistoryDrawer from "../components/slate/HistoryDrawer";
import type { SlateCardActions } from "../components/slate/SlateCard";
import type { SlateBoardCard } from "../api/slateTypes";

// SL4 — the board. A SECOND PRESENTER over the same pure projection the CLI
// digest renders (D25 / invariant #11's engine-plus-presenters rule): it
// adds every visual device the injected digest refuses (emoji, colour, two
// card sizes, age fade, columns, swimlanes, drawings) and NOT ONE FACT the
// CLI cannot print.
//
// URL is the state, always (#23's "no second home" reflex):
//   /slates/:slug            the board
//   ?topic=<t>               the topic filter
//   ?history=1               the history drawer
// `useScrollRestoration` declares ["post", "history"] ephemeral so a
// card-flash or a drawer toggle doesn't fragment the board's one scroll
// slot (the #31 W3.D/S2 carve-out, same reason /sessions declares
// ["focus"]).
//
// The keyboard grammar (j/k/Enter/m/x/e/p/c/t/h) is DOC-SCOPED in
// `lib/keymap.ts` under `scope: "slates"` and dispatched by this route's own
// window listener — NOT by HotkeyRoot. That is the two-layer rule
// `keymap.ts` spells out: a `scope: "global"` binding would put `j`/`k`/`m`
// on the window while `useRovingCursor` and HotkeyRoot's own marks handler
// are also listening, and `preventDefault()` on one listener does not stop a
// sibling on the same target.

export default function SlateBoardRoute() {
  const { slug = "" } = useParams();
  const [params, setParams] = useSearchParams();
  const topic = params.get("topic");
  const historyOpen = params.get("history") === "1";
  const isMobile = useIsMobile();
  const confirm = useConfirm();
  // D29 — a slate is per-PROJECT, not per-kb, so "the current kb" is the
  // SPA's own active-kb rule (#33: a pure function of the URL — `?kb=`, else
  // the cold-seeded first corpus), and its `code_url` is the validated one
  // every other kb-code link-out already uses. No `code_url` ⇒ `null` ⇒
  // nothing is fetched and no caption renders anywhere on the board.
  const activeKb = useActiveKb();
  const codeUrl = useCodeUrlForKb(activeKb ?? undefined);

  useDocumentTitle(slug ? `${slug} · slate` : "Slate");
  useScrollRestoration(`/slates/${slug}${params.toString() ? `?${params}` : ""}`, [
    "post",
    "history",
  ]);
  // Ages are `age_secs` from the daemon (D5 — no client clock invents one);
  // the tick is what re-renders them as the wall clock moves past a bucket
  // boundary, matching every other age surface in the SPA.
  useNowTick(30_000);

  const { board, loading, error } = useSlateBoard(slug, topic ?? undefined);
  const history = useSlateHistory(slug, historyOpen);

  // `pin` is a UI convenience shown when the identity IS the operator; the
  // daemon checks only `origin: human` (§10 "Actions"), so a wrong answer
  // here hides a button, never grants one.
  const { data: identity } = useQuery({
    queryKey: ["identity"],
    queryFn: ({ signal }) => fetchIdentity(signal),
    staleTime: Infinity,
  });
  const isOperator = !!identity && identity.user === identity.operator;

  const [composerOpen, setComposerOpen] = useState(false);
  const [seed, setSeed] = useState<ComposerSeed | undefined>(undefined);
  const [swimlanes, setSwimlanes] = useState(false);
  const [cursor, setCursor] = useState<number | null>(null);
  const [flashSeq, setFlashSeq] = useState<number | null>(null);
  const [openCols, setOpenCols] = useState<Record<string, boolean>>(() =>
    Object.fromEntries(COLUMN_IDS.map((id) => [id, true])),
  );
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const setParam = useCallback(
    (k: string, v: string | null) => {
      const next = new URLSearchParams(params);
      if (v === null) next.delete(k);
      else next.set(k, v);
      setParams(next, { replace: true });
    },
    [params, setParams],
  );

  const openHistory = useCallback(() => setParam("history", "1"), [setParam]);
  const closeHistory = useCallback(() => setParam("history", null), [setParam]);

  const post = useSlatePost(slug, {
    confirm: (o) => confirm({ ...o, danger: true }),
    toastErr: toast.err,
    toastInfo: toast.info,
    onOpenHistory: openHistory,
  });

  const sections = board?.sections;
  const topics = board?.topics ?? [];
  const rows = useMemo(() => nowRows(sections, topics), [sections, topics]);
  const warns = useMemo(() => warnRows(sections), [sections]);
  const cols = useMemo(() => columnsOf(sections), [sections]);
  const lanes = useMemo(() => lanesOf(sections, topics), [sections, topics]);
  const order = useMemo(
    () => readingOrder(sections, topics, swimlanes),
    [sections, topics, swimlanes],
  );

  /// Scroll a seq into view and flash it — the `post:#n` chip, the
  /// `(was #n)` link and the j/k cursor all land here, so "go to a card"
  /// has exactly one implementation. `prefers-reduced-motion` disables the
  /// flash (it is decoration; the scroll is the actual affordance).
  const jumpToSeq = useCallback((seq: number) => {
    const el = document.getElementById(`slate-post-${seq}`);
    el?.scrollIntoView({ block: "center", behavior: "smooth" });
    const reduce =
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (reduce) return;
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlashSeq(seq);
    flashTimer.current = setTimeout(() => setFlashSeq(null), 1200);
  }, []);

  useEffect(
    () => () => {
      if (flashTimer.current) clearTimeout(flashTimer.current);
    },
    [],
  );

  const openComposer = useCallback((s?: ComposerSeed) => {
    setSeed(s);
    setComposerOpen(true);
  }, []);

  // ── card actions ───────────────────────────────────────────────────────
  // Every one of them is an APPEND of one of the twelve kinds. There is no
  // second mutation route and no in-place rewrite (§4 "Surface mutation").

  const actions: SlateCardActions = useMemo(
    () => ({
      onMark: (c) =>
        void post(
          { kind: "mark", line: `mark #${c.seq}`, re: c.seq, prov: SPA_PROV },
          { targetKind: c.kind, holder: c.who.tag },
        ),
      onDrop: async (c) => {
        // A LOCAL courtesy, not the daemon's rule. The daemon only refuses a
        // drop when the acting origin is NOT human (rules matrix "Drop and
        // edit friction"), and every SPA post is human — so nothing here
        // would ever 409. We still ask before stepping on a live agent's
        // coordination post, because "you can" is not "you meant to". The
        // 409 handler in `useSlatePost` covers the daemon's own refusal for
        // the cases this prompt cannot predict.
        const coordination = ["now", "warn", "take"].includes(c.kind) ||
          (c.kind === "hand" && c.acknowledged === false);
        const live = c.liveness === "live" || c.liveness === "stale";
        if (coordination && live && c.who.origin !== "human") {
          const ok = await confirm({
            title: "Drop anyway?",
            body: `${c.who.tag} is live and holds this ${c.kind}. Drop anyway?`,
            confirmLabel: "Drop anyway",
            danger: true,
          });
          if (!ok) return;
        }
        await post(
          { kind: "drop", line: `drop #${c.seq}`, re: c.seq, prov: SPA_PROV },
          { targetKind: c.kind, holder: c.who.tag },
        );
      },
      onEdit: (c) =>
        openComposer({
          kind: c.kind,
          line: c.line,
          body: c.body ?? "",
          topic: c.topic,
          subject: c.subject,
          refs: (c.refs ?? []).map((r) => r.raw),
          supersedes: c.seq,
        }),
      onPin: (c, pin) =>
        void post(
          {
            kind: "mark",
            line: `${pin ? "pin" : "unpin"} #${c.seq}`,
            re: c.seq,
            pin,
            prov: SPA_PROV,
          },
          { targetKind: c.kind, holder: c.who.tag },
        ),
      onDone: (c) =>
        void post(
          { kind: "done", line: `done #${c.seq}`, re: c.seq, prov: SPA_PROV },
          { targetKind: c.kind, holder: c.who.tag },
        ),
      onAnswer: (c) => openComposer({ kind: "answer", re: c.seq, topic: c.topic }),
      // `take #n` on an open hand copies the hand's subject and sets `re`;
      // the hand then renders acknowledged (rules matrix "`take #n` on a
      // hand"). The subject is prefilled from the hand, not invented here.
      onTake: (c) =>
        openComposer({
          kind: "take",
          line: c.line,
          subject: c.subject ?? c.line,
          topic: c.topic,
          re: c.seq,
        }),
      onJumpToSeq: jumpToSeq,
      onOpenHistoryAt: (seq) => {
        openHistory();
        setFlashSeq(seq);
      },
    }),
    [post, openComposer, jumpToSeq, openHistory, confirm],
  );

  // ── keyboard (doc-scoped, this route's own listener) ───────────────────
  const cardAt = useCallback(
    (seq: number | null): SlateBoardCard | null =>
      seq === null ? null : (order.find((c) => c.seq === seq) ?? null),
    [order],
  );

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isEditableTarget(e.target)) return;
      if (composerOpen || historyOpen) return;
      const cur = cardAt(cursor);
      switch (e.key) {
        case "j":
        case "k": {
          const next = moveCursor(order, cursor, e.key === "j" ? 1 : -1);
          setCursor(next);
          if (next !== null) {
            document
              .getElementById(`slate-post-${next}`)
              ?.scrollIntoView({ block: "nearest" });
          }
          e.preventDefault();
          return;
        }
        case "Enter":
          if (cur) {
            document
              .querySelector<HTMLButtonElement>(
                `#slate-post-${cur.seq} .slate-card__bodytoggle`,
              )
              ?.click();
            e.preventDefault();
          }
          return;
        case "m":
          if (cur) {
            actions.onMark?.(cur);
            e.preventDefault();
          }
          return;
        case "x":
          if (cur) {
            void actions.onDrop?.(cur);
            e.preventDefault();
          }
          return;
        case "e":
          if (cur && !isMobile) {
            actions.onEdit?.(cur);
            e.preventDefault();
          }
          return;
        case "p":
          if (cur && !isMobile && isOperator) {
            actions.onPin?.(cur, !cur.pinned);
            e.preventDefault();
          }
          return;
        case "c":
          openComposer(undefined);
          e.preventDefault();
          return;
        case "t":
          document
            .querySelector<HTMLButtonElement>(".slate-now__topics .slate-topic")
            ?.focus();
          e.preventDefault();
          return;
        case "h":
          openHistory();
          e.preventDefault();
          return;
        default:
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    order,
    cursor,
    cardAt,
    actions,
    openComposer,
    openHistory,
    composerOpen,
    historyOpen,
    isMobile,
    isOperator,
  ]);

  const columnProps = {
    codeUrl,
    kb: activeKb,
    // SL7f (v0.42 amendment) — a slate slug IS the kb-code repo name by
    // design (D29/SL7e), so the board sends it as the caption query's
    // `?repo=` unconditionally: the ONLY way a multi-repo kb-code can
    // answer at all, and never retried without it on a 400.
    repo: slug,
    mobile: isMobile,
    isOperator,
    focusedSeq: cursor,
    flashSeq,
    actions,
  };

  return (
    <div className={`slate-board${isMobile ? " slate-board--mobile" : ""}`}>
      <NowBand
        rows={rows}
        warns={warns}
        topics={topics}
        activeTopic={topic}
        onPickTopic={(t) => setParam("topic", t)}
        onCompose={() => openComposer(undefined)}
        onOpenHistory={openHistory}
        swimlanes={isMobile ? undefined : swimlanes}
        onToggleSwimlanes={isMobile ? undefined : () => setSwimlanes((v) => !v)}
      />

      {error && (
        <div className="slate-board__error" role="alert">
          Couldn’t load {slug}: {error}
        </div>
      )}
      {!error && loading && <p className="slate-board__loading">loading…</p>}

      {!error && !loading && !swimlanes && (
        <div className="slate-board__cols">
          {cols.map((c) => (
            <SlateColumn
              key={c.id}
              id={c.id}
              name={c.name}
              cards={c.cards}
              open={openCols[c.id] !== false}
              onToggle={() =>
                setOpenCols((o) => ({ ...o, [c.id]: o[c.id] === false }))
              }
              {...columnProps}
            />
          ))}
        </div>
      )}

      {!error && !loading && swimlanes && (
        <div className="slate-board__lanes">
          {lanes.map((lane) => (
            <section
              key={lane.topic ?? "__general"}
              className="slate-lane"
              aria-label={`Lane ${lane.label}`}
            >
              <h2 className="slate-lane__head">
                <span className="slate-lane__name">{lane.label}</span>
                <span className="slate-lane__count">{lane.total}</span>
              </h2>
              <div className="slate-board__cols">
                {lane.columns.map((c) => (
                  <SlateColumn
                    key={c.id}
                    id={`${lane.topic ?? "general"}-${c.id}`}
                    name={`${c.name} — ${lane.label}`}
                    cards={c.cards}
                    {...columnProps}
                  />
                ))}
              </div>
            </section>
          ))}
        </div>
      )}

      <SlateComposer
        open={composerOpen}
        seed={seed}
        topics={topics}
        mobile={isMobile}
        onClose={() => setComposerOpen(false)}
        onSubmit={(b) => post(b)}
      />

      <HistoryDrawer
        open={historyOpen}
        rows={history.rows}
        loading={history.loading}
        error={history.error}
        onClose={closeHistory}
        mobile={isMobile}
        flashSeq={flashSeq}
      />
    </div>
  );
}
