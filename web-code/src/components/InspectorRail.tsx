import { forwardRef, useImperativeHandle, type ReactNode } from "react";
import type { AnnotationIntent, Symbol } from "../api/types";
import {
  INSPECTOR_TABS,
  normalizeInspectorTab,
  useInspectorTab,
  type InspectorTab,
  type LegacyInspectorTab,
} from "../hooks/useInspectorTab";
import { pinnedHeaderText, subjectLabel, type RailSubject } from "../desk/railSubject";
import { Icon } from "./icons";
import OutlineRail from "./OutlineRail";
import AnnotationsPanel from "./annotations/AnnotationsPanel";
import BookmarksPanel from "./bookmarks/BookmarksPanel";

export type { InspectorTab };

// V70-A4 — the rail re-cut from six SOURCE tabs to the design's TASK
// tabs (docs/research/kb-code-v7-continuum-2026-09.html §P1):
//
//   All        everything stacked — tab one, hides nothing
//   Understand outline + the entity passport ("what is this?")
//   History    blame/why/story + file history ("how did it get here?")
//   Notes      annotations + bookmarks ("what did I mark?")
//   Review     threads + findings ("what's contested?") — CONDITIONAL
//
// Review is rendered only when the open file belongs to an open review.
// When it is selected without one, the body says so ("no review context
// for this file") rather than going blank: the research report's rail
// rule (panel-layout-system.md §3.3) is "**Never blank.** If a lens has
// nothing for the current subject, show the file-level fallback with a
// caption … NN/g: recognition over recall."
//
// Everything else about the component is unchanged on purpose: the same
// props, the same three ALWAYS-VISIBLE passport cards above the body,
// the same `asSheet` mobile promotion, the same imperative
// `openTab` handle (which now also accepts the legacy six-tab ids, so
// the blame gutter, the `a` key and `?shell=legacy`'s reader keep
// working without a call-site sweep). Root CLAUDE.md invariant #30's
// grammar — ONE rail, one home per action, badge on the tab that has
// something to show — is the thing being preserved, not the tab count.
const TAB_META: Record<InspectorTab, { label: string; icon: ReactNode; hint: string }> = {
  all: { label: "All", icon: <Icon.Layers />, hint: "everything about this subject, stacked" },
  understand: { label: "Understand", icon: <Icon.Entity />, hint: "what is this?" },
  history: { label: "History", icon: <Icon.History />, hint: "how did it get here?" },
  notes: { label: "Notes", icon: <Icon.Note />, hint: "what did I mark?" },
  review: { label: "Review", icon: <Icon.ClipboardCheck />, hint: "what's contested?" },
  dossier: { label: "Dossier", icon: <Icon.List />, hint: "what does this entity have?" },
};

export interface InspectorRailHandle {
  /// Force-switch the visible tab — the blame gutter's click handler
  /// and the annotation "+"/`a` keybinding both drive the rail from
  /// OUTSIDE it, mirroring `FileTree.tsx`'s own `FileTreeHandle`
  /// imperative-ref convention (the established idiom in this codebase
  /// for "an external event needs to reach into a component's own UI
  /// state"). Accepts the V70-A4 tab ids AND the pre-A4 six, so a
  /// caller that has not been re-worded still lands on the right tab.
  openTab: (tab: InspectorTab | LegacyInspectorTab) => void;
}

export interface InspectorRailProps {
  symbols: Symbol[];
  onJumpOutline: (line: number) => void;
  /// The provenance content, already built by `Reader.tsx` (it owns
  /// the region/attribution lookups `WhyPanel` needs) — `null` before any
  /// gutter line has been clicked. Lives under History since V70-A4.
  whyPanel: ReactNode | null;
  /// The file-history content (Wave C) — built by `Reader.tsx` (it owns
  /// the `file-history` fetch + the `?ref=` current-position lookup);
  /// `null` when no file is open.
  historyPanel: ReactNode | null;
  repo: string;
  path: string;
  annotationActiveLine: number | null;
  /// Phase D — the other end of a visual-mode `a` range selection; `null`
  /// for a plain single-line invocation. See `AnnotationsPanel`'s own doc.
  annotationActiveLineEnd: number | null;
  /// V71-E2 — pass-through for the composer's opening intent (the action
  /// menu's "Ask here" opens a QUESTION, `a` still opens a note).
  annotationInitialIntent?: AnnotationIntent;
  onGotoAnnotationLine: (line: number, lineEnd?: number) => void;
  unresolvedAnnotations: number;
  /// V3.N2 — badge for the Bookmarks content (count for the current repo).
  bookmarkCount?: number;
  /// V3.N2 — jump from a bookmark row (path + line; may leave the current file).
  onJumpBookmark?: (loc: { path: string; line: number }) => void;
  /// F5 — when raised as the mobile bottom sheet (phone only), the rail is a
  /// modal dialog: `asSheet` adds `role="dialog"`/`aria-modal`/
  /// `id="kbc-reader-sheet"` (the mobile entry button's `aria-controls`
  /// target) plus a phone-only grab handle + close header. `false` on
  /// desktop, so the docked `<aside>` semantics + e2e stay byte-identical
  /// there — mirrors kb's own `PreviewInspector`'s `asSheet` prop (root
  /// CLAUDE.md invariant #30).
  asSheet?: boolean;
  /// Mobile-only — renders the sheet's phone-only dismiss header (CSS-hidden
  /// on desktop). The mobile entry button is the other way to close it.
  onMobileClose?: () => void;
  /// V3.1-H3b — the entity passport (built by Reader; needs cursor + lenses).
  entityPanel?: ReactNode | null;
  /// V72-G1.2 — the Dossier tab's body (the member jump list). Supplied ONLY
  /// while the dossier center is mounted; its presence is what OFFERS the tab
  /// (`hasDossierContext` below), the exact shape `reviewPanel`/
  /// `hasReviewContext` already take.
  dossierPanel?: ReactNode | null;
  hasDossierContext?: boolean;
  /// DCB W3.B — the "Cited by" strip, ALWAYS visible above the tab body
  /// regardless of which tab is selected (does NOT extend `InspectorTab`'s
  /// closed union — root CLAUDE.md invariant #30). Built by `Reader.tsx`
  /// (same ownership pattern as `whyPanel`/`historyPanel`): `null` while no
  /// file is open. `CitedBy` itself renders nothing (not even this
  /// wrapper's chrome) when its own fetch has zero claims, so a file with
  /// no citations shows no slot at all.
  citedBy?: ReactNode | null;
  /// T1 (design-ui.md §9.4a) — the rails-lens Framework card, mounted the
  /// SAME always-visible way `citedBy` is.
  frameworkCard?: ReactNode | null;
  /// PRR-U9 (design-addendum-2.md §D) — the Diagnostics card, mounted the
  /// SAME always-visible way `frameworkCard`/`citedBy` are.
  diagnosticsCard?: ReactNode | null;

  // --- V70-A4 additions --------------------------------------------------
  /// The Review tab's body. `null` + `hasReviewContext: false` renders
  /// the honest empty state instead of a blank column.
  reviewPanel?: ReactNode | null;
  /// Whether the open file belongs to an open review — gates whether the
  /// Review TAB is offered at all (§P1: "a Review tab that appears only
  /// when the open file belongs to an open review").
  hasReviewContext?: boolean;
  /// What the rail is about. `null` when no file is open.
  subject?: RailSubject | null;
  /// Where the caret actually is, which differs from `subject` exactly
  /// when the rail is pinned — the header names both.
  caretSubject?: RailSubject | null;
  pinned?: boolean;
  onTogglePin?: () => void;
  /// Controlled tab, when the Desk owns it (the shell's rail stripe and
  /// the presets both set it). Uncontrolled — falling back to the
  /// per-surface `useInspectorTab` pref — when absent, which is what
  /// `?shell=legacy`'s reader uses.
  tab?: InspectorTab;
  onTabChange?: (tab: InspectorTab) => void;

  // --- V70-A10 addition ----------------------------------------------------
  /// `WorkspaceNotesPanel`, built by `Reader.tsx` (same ownership pattern
  /// as `whyPanel`/`historyPanel`: it owns the `?workspace=`/strip lookup).
  /// `null`/`undefined` when no workspace is currently open — the "Notes"
  /// and "All" tab bodies then render exactly as they did before this
  /// unit, byte-identical. When present, it renders ABOVE Annotations/
  /// Bookmarks (D26: "a Workspace notes section at the top").
  workspaceNotesPanel?: ReactNode | null;
}

function SubjectChip({
  subject,
  caretSubject,
  pinned,
  onTogglePin,
}: {
  subject: RailSubject | null;
  caretSubject: RailSubject | null;
  pinned: boolean;
  onTogglePin?: () => void;
}) {
  if (!subject) {
    return (
      <div className="kbc-inspector__subject" data-kbc-rail-subject="none">
        <span className="kbc-inspector__subject-seg is-on">no file open</span>
      </div>
    );
  }
  return (
    <div className="kbc-inspector__subject" data-kbc-rail-subject={subject.kind}>
      <div className="kbc-inspector__subject-row">
        {/* The subject is ALWAYS named, and always says WHICH of the
            three grains it is — a rail that just shows content without
            saying what it is about is the Xcode failure mode. */}
        <span className="kbc-inspector__subject-segs" role="group" aria-label="rail subject">
          {(["symbol", "line", "file"] as const).map((k) => (
            <span
              key={k}
              className={"kbc-inspector__subject-seg" + (subject.kind === k ? " is-on" : "")}
              data-kbc-rail-seg={k}
            >
              {k}
            </span>
          ))}
        </span>
        {onTogglePin && (
          <button
            type="button"
            className={"kbc-inspector__pin" + (pinned ? " is-on" : "")}
            aria-pressed={pinned}
            title={pinned ? "unpin — follow the caret again" : "pin the rail to this subject"}
            aria-label={pinned ? "unpin the rail" : "pin the rail"}
            data-cmd="rail.pin"
            data-kbc-rail-pin={pinned ? "1" : "0"}
            onClick={onTogglePin}
          >
            <Icon.Pin />
          </button>
        )}
      </div>
      <div className="kbc-inspector__subject-name" data-kbc-rail-subject-name>
        {pinned ? pinnedHeaderText(subject, caretSubject) : subjectLabel(subject)}
      </div>
      {subject.caption && (
        <div className="kbc-inspector__subject-caption" data-kbc-rail-subject-caption>
          {subject.caption}
        </div>
      )}
    </div>
  );
}

/// A stacked section inside the All tab. Each carries its own heading so
/// the merged view reads as a document rather than an undifferentiated
/// pile — kb's own "all = every section stacked" idiom (invariant #30).
function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="kbc-inspector__section" data-kbc-rail-section={title.toLowerCase()}>
      <h3 className="kbc-inspector__section-head">{title}</h3>
      {children}
    </section>
  );
}

/// The reader's right rail. Mirrors kb's own merged-inspector-rail idiom
/// (root CLAUDE.md invariant #30: "ONE merged inspector rail… icon tab
/// bar, badge on the tab that has something to show"), re-cut in V70-A4
/// from data-source tabs to task tabs. The active tab persists per
/// surface (`useInspectorTab`) unless the Desk controls it.
const InspectorRail = forwardRef<InspectorRailHandle, InspectorRailProps>(function InspectorRail(
  {
    symbols,
    onJumpOutline,
    whyPanel,
    historyPanel,
    repo,
    path,
    annotationActiveLine,
    annotationActiveLineEnd,
    annotationInitialIntent,
    onGotoAnnotationLine,
    unresolvedAnnotations,
    bookmarkCount = 0,
    onJumpBookmark,
    asSheet = false,
    onMobileClose,
    entityPanel = null,
    dossierPanel = null,
    hasDossierContext = false,
    citedBy = null,
    frameworkCard = null,
    diagnosticsCard = null,
    reviewPanel = null,
    hasReviewContext = false,
    subject,
    caretSubject = null,
    pinned = false,
    onTogglePin,
    tab: tabProp,
    onTabChange,
    workspaceNotesPanel = null,
  },
  handleRef,
) {
  const own = useInspectorTab("reader");
  const tab = tabProp ?? own.tab;
  const setTab = (next: InspectorTab) => {
    own.setTab(next);
    onTabChange?.(next);
  };

  useImperativeHandle(handleRef, () => ({ openTab: (t) => setTab(normalizeInspectorTab(t)) }));

  const badgeFor = (t: InspectorTab): number => {
    if (t === "notes") return unresolvedAnnotations + bookmarkCount;
    // Understand/History/All/Review have no unread concept — never invent one.
    return 0;
  };

  // Review is offered only when there IS one. A selected-but-absent
  // Review tab still renders (with its honest empty body) so a persisted
  // choice never silently becomes a different tab.
  // Review is offered only when there IS one; Dossier only while the dossier
  // center is mounted. Both keep the same carve-out: a selected-but-absent tab
  // still renders (with its own honest empty body) so a persisted choice never
  // silently becomes a different tab.
  const visibleTabs = INSPECTOR_TABS.filter(
    (t) =>
      (t !== "review" || hasReviewContext || tab === "review") &&
      (t !== "dossier" || hasDossierContext || tab === "dossier"),
  );

  const outlineBody = <OutlineRail symbols={symbols} onJump={onJumpOutline} />;
  const entityBody = entityPanel ?? (
    <div className="kbc-inspector__hint">Place the cursor on a symbol.</div>
  );
  const provenanceBody = whyPanel ?? (
    <div className="kbc-inspector__hint">
      Turn on "Provenance" above the editor, then click a gutter line.
    </div>
  );
  const historyBody = historyPanel ?? <div className="kbc-inspector__hint">No file open.</div>;
  const annotationsBody = (
    <AnnotationsPanel
      repo={repo}
      path={path}
      activeLine={annotationActiveLine}
      activeLineEnd={annotationActiveLineEnd}
      initialIntent={annotationInitialIntent}
      onGotoLine={onGotoAnnotationLine}
    />
  );
  const bookmarksBody = onJumpBookmark ? (
    <BookmarksPanel repo={repo} onJump={onJumpBookmark} />
  ) : (
    <div className="kbc-inspector__hint">Bookmarks unavailable.</div>
  );
  const dossierBody = hasDossierContext ? (
    (dossierPanel ?? <div className="kbc-inspector__hint">No members in this dossier.</div>)
  ) : (
    <div className="kbc-inspector__hint" data-kbc-rail-no-dossier>
      Open an entity dossier to jump through its members.
    </div>
  );
  const reviewBody = hasReviewContext ? (
    (reviewPanel ?? <div className="kbc-inspector__hint">No threads on this file yet.</div>)
  ) : (
    <div className="kbc-inspector__hint" data-kbc-rail-no-review>
      No review context for this file.
    </div>
  );

  return (
    <div
      className="kbc-inspector"
      {...(asSheet
        ? { role: "dialog" as const, "aria-modal": true, id: "kbc-reader-sheet" }
        : {})}
    >
      {onMobileClose && (
        <header className="kbc-inspector__sheet-head">
          {/* Grab pill — reads as a native bottom sheet; centred at the top
              via CSS. Decorative (dismiss is the ✕ / scrim / Esc). */}
          <span className="kbc-inspector__grab" aria-hidden />
          <span className="kbc-inspector__sheet-lab">reader tools</span>
          <button
            type="button"
            className="kbc-inspector__sheet-x"
            onClick={onMobileClose}
            title="close reader tools"
            aria-label="close reader tools"
            data-kbc-inspector-sheet-close
          >
            <Icon.X />
          </button>
        </header>
      )}
      <nav className="kbc-inspector__icons" role="tablist" aria-label="reader inspector">
        {visibleTabs.map((t) => {
          const sel = tab === t;
          const badge = badgeFor(t);
          return (
            <button
              key={t}
              type="button"
              role="tab"
              aria-selected={sel}
              className={"kbc-inspector__itab" + (sel ? " is-on" : "")}
              onClick={() => setTab(t)}
              data-kbc-itab={t}
              data-tab={t}
              title={`${TAB_META[t].label} — ${TAB_META[t].hint}`}
              aria-label={TAB_META[t].label}
            >
              {TAB_META[t].icon}
              {badge > 0 && (
                <span className="kbc-inspector__itab-badge" data-kbc-itab-badge={t}>
                  {badge > 99 ? "99+" : badge}
                </span>
              )}
            </button>
          );
        })}
      </nav>
      {/* Only when the host actually tracks a subject. `?shell=legacy`'s
          pre-Desk reader passes none, so its rail markup keeps the
          pre-V70-A4 shape (nav → cards → body) exactly. */}
      {subject !== undefined && (
        <SubjectChip
          subject={subject}
          caretSubject={caretSubject}
          pinned={pinned}
          onTogglePin={onTogglePin}
        />
      )}
      {citedBy}
      {frameworkCard}
      {diagnosticsCard}
      <div className="kbc-inspector__body" data-kbc-rail-body={tab}>
        {tab === "all" && (
          <>
            {workspaceNotesPanel && <Section title="Workspace notes">{workspaceNotesPanel}</Section>}
            <Section title="Outline">{outlineBody}</Section>
            <Section title="Entity">{entityBody}</Section>
            <Section title="Provenance">{provenanceBody}</Section>
            <Section title="History">{historyBody}</Section>
            <Section title="Annotations">{annotationsBody}</Section>
            <Section title="Bookmarks">{bookmarksBody}</Section>
            {hasReviewContext && <Section title="Review">{reviewBody}</Section>}
          </>
        )}
        {tab === "understand" && (
          <>
            <Section title="Outline">{outlineBody}</Section>
            <Section title="Entity">{entityBody}</Section>
          </>
        )}
        {tab === "history" && (
          <>
            <Section title="Provenance">{provenanceBody}</Section>
            <Section title="History">{historyBody}</Section>
          </>
        )}
        {tab === "notes" && (
          <>
            {workspaceNotesPanel && <Section title="Workspace notes">{workspaceNotesPanel}</Section>}
            <Section title="Annotations">{annotationsBody}</Section>
            <Section title="Bookmarks">{bookmarksBody}</Section>
          </>
        )}
        {tab === "review" && <Section title="Review">{reviewBody}</Section>}
        {tab === "dossier" && <Section title="Members">{dossierBody}</Section>}
      </div>
    </div>
  );
});

export default InspectorRail;
