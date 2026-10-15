import { forwardRef, useImperativeHandle, useRef, type ReactNode } from "react";
import type { AnnotationIntent, Symbol } from "../api/types";
import {
  INSPECTOR_TABS,
  normalizeInspectorTab,
  useInspectorTab,
  type InspectorTab,
  type LegacyInspectorTab,
} from "../hooks/useInspectorTab";
import { pinnedHeaderText, subjectLabel, type RailSubject } from "../desk/railSubject";
import { Icon } from "./iconSet";
import OutlineRail from "./OutlineRail";
import AnnotationsPanel from "./annotations/AnnotationsPanel";
import BookmarksPanel from "./bookmarks/BookmarksPanel";

export type { InspectorTab };

// OL-C4 — the rail is five HOMES plus the stacked All view (it was nine
// tabs; several answered neighbouring questions):
//
//   All        everything stacked — tab one, hides nothing
//   Understand outline + entity passport + the dossier's members ("what is this?")
//   History    blame/why/story + file history ("how did it get here?")
//   Notes      annotations + bookmarks + workspace notes + the trail
//   Facts      aug-lane facts + the source's own comments
//   Review     threads + findings ("what's contested?") — CONDITIONAL
//
// `dossier`, `comments` and `trail` are no longer tabs: their bodies render
// as SECTIONS inside Understand, Facts and Notes (and in All). Their old
// ids stay ACCEPTED by `openTab` / `normalizeInspectorTab` / persisted desk
// state and land on the new home, scrolled to the section.
//
// Review is rendered only when the open file belongs to an open review.
// When it is selected without one, the body says so ("no review context
// for this file") rather than going blank: the research report's rail
// rule (panel-layout-system.md §3.3) is "**Never blank.** If a lens has
// nothing for the current subject, show the file-level fallback with a
// caption". The same props, the three-plus ALWAYS-VISIBLE passport cards
// above the body, the `asSheet` mobile promotion and the imperative
// `openTab` handle are unchanged. Root CLAUDE.md invariant #30's grammar,
// ONE rail, one home per action, a badge on the tab that has something to
// show, is the thing being preserved.
const TAB_META: Record<InspectorTab, { label: string; icon: ReactNode; hint: string }> = {
  all: { label: "All", icon: <Icon.Layers />, hint: "everything about this subject, stacked" },
  understand: { label: "Understand", icon: <Icon.Entity />, hint: "what is this?" },
  history: { label: "History", icon: <Icon.History />, hint: "how did it get here?" },
  notes: { label: "Notes", icon: <Icon.Note />, hint: "what did I mark, where have I been?" },
  // V76-R3a — aug-lane/1 Facts. A RAIL tab (Lane Budget: "gutter slot four
  // or a rail row, never a fifth slot"; slot four is comments/1).
  facts: { label: "Facts", icon: <Icon.Facts />, hint: "what do the lanes and the source say?" },
  review: { label: "Review", icon: <Icon.ClipboardCheck />, hint: "what's contested?" },
};

export type RailSectionId =
  | "workspace-notes"
  | "outline"
  | "entity"
  | "members"
  | "provenance"
  | "history"
  | "annotations"
  | "bookmarks"
  | "trail"
  | "comments"
  | "facts"
  | "review";

const SECTION_TITLE: Record<RailSectionId, string> = {
  "workspace-notes": "Workspace notes",
  outline: "Outline",
  entity: "Entity",
  members: "Members",
  provenance: "Provenance",
  history: "History",
  annotations: "Annotations",
  bookmarks: "Bookmarks",
  trail: "Trail",
  comments: "Comments",
  facts: "Facts",
  review: "Review",
};

/// Which sections each home renders, in order. Pure so the placement is
/// pinned by a unit test rather than by reading JSX. `workspaceNotes`,
/// `dossier` and `review` are the conditional sections (a workspace is open,
/// the dossier center is mounted, the open file belongs to a review).
export function railSections(
  tab: InspectorTab,
  has: { workspaceNotes: boolean; dossier: boolean; review: boolean },
): RailSectionId[] {
  const ws: RailSectionId[] = has.workspaceNotes ? ["workspace-notes"] : [];
  const members: RailSectionId[] = has.dossier ? ["members"] : [];
  switch (tab) {
    case "all":
      return [
        ...ws,
        "outline",
        "entity",
        ...members,
        "provenance",
        "history",
        "annotations",
        "bookmarks",
        "trail",
        "comments",
        "facts",
        ...(has.review ? (["review"] as const) : []),
      ];
    case "understand":
      return ["outline", "entity", ...members];
    case "history":
      return ["provenance", "history"];
    case "notes":
      return [...ws, "annotations", "bookmarks", "trail"];
    case "facts":
      return ["facts", "comments"];
    case "review":
      return ["review"];
  }
}

/// The section a retired tab id pointed at, so `openTab("comments")` lands on
/// its new home AND brings the section into view.
const RETIRED_TAB_SECTION: Partial<Record<LegacyInspectorTab, string>> = {
  dossier: "members",
  comments: "comments",
  trail: "trail",
};

export interface InspectorRailHandle {
  /// Force-switch the visible tab — the blame gutter's click handler
  /// and the annotation "+"/`a` keybinding both drive the rail from
  /// OUTSIDE it, mirroring `FileTree.tsx`'s own `FileTreeHandle`
  /// imperative-ref convention (the established idiom in this codebase
  /// for "an external event needs to reach into a component's own UI
  /// state"). Also accepts the retired ids (the pre-A4 six, plus
  /// `dossier`/`comments`/`trail` since OL-C4), so a caller that has not been
  /// re-worded still lands on the right tab.
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
  /// V72-G1.2 — the dossier's member jump list. Since OL-C4 it is the
  /// "Members" SECTION of Understand (and All), rendered only while
  /// `hasDossierContext` says the dossier center is mounted.
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
  /// V72-I2 — the annotaterb Schema card. A FOURTH always-visible passport
  /// slot beside `citedBy`/`frameworkCard`/`diagnosticsCard`, not a sixth
  /// tab: it is a per-FILE derived fact, and it renders nothing at all for a
  /// file with no `# == Schema Information` banner (`DiagnosticsCard`'s own
  /// "absent" idiom, not `FrameworkCard`'s named-empty one).
  schemaCard?: ReactNode | null;
  /// V73-K2c (kbc-claim/1, design D18) — the Claims card: a RAIL ROW (never
  /// a gutter slot, the Lane Budget's own rule), mounted the SAME always-
  /// visible way `diagnosticsCard`/`frameworkCard`/`citedBy` are.
  claimsCard?: ReactNode | null;

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
  /// per-surface `useInspectorTab` pref — when absent.
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

  // --- V72-J2 (D8) addition ------------------------------------------------
  /// The Comments tab's body (`components/comments/CommentsPanel.tsx`),
  /// built by `Reader.tsx` (same ownership pattern as `annotationsBody`
  /// below — it owns the `useCommentsFile`/bridge fetches). UNCONDITIONAL,
  /// unlike Review/Dossier: every file the scanner covers has an honest
  /// list, possibly empty, so there is no "no context" gate to render.
  commentsPanel?: ReactNode | null;
  /// Badge count — the number of ACTIONABLE (drifted/aged/unreasoned)
  /// comments/1 rows in the open file, off the wire (never re-derived here).
  commentsBadgeCount?: number;

  /// V74-L3b — the Trail tab's body (`components/trail/TrailRail.tsx`).
  /// Supplied by whoever mounts the rail; absent renders the honest "nothing
  /// is reading your trail here" hint rather than an empty panel.
  trailPanel?: ReactNode | null;

  /// V76-R3a — the Facts tab's body (`components/lanes/FactsPanel.tsx`).
  /// UNCONDITIONAL like Comments/Trail: every file has an honest (possibly
  /// empty-with-reason) lane list. Built by `Reader.tsx`.
  factsPanel?: ReactNode | null;
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
    schemaCard = null,
    claimsCard = null,
    reviewPanel = null,
    hasReviewContext = false,
    subject = null,
    caretSubject = null,
    pinned = false,
    onTogglePin,
    tab: tabProp,
    onTabChange,
    workspaceNotesPanel = null,
    commentsPanel = null,
    commentsBadgeCount = 0,
    trailPanel = null,
    factsPanel = null,
  },
  handleRef,
) {
  const own = useInspectorTab("reader");
  const rootRef = useRef<HTMLDivElement | null>(null);
  const tab = tabProp ?? own.tab;
  const setTab = (next: InspectorTab) => {
    own.setTab(next);
    onTabChange?.(next);
  };

  useImperativeHandle(handleRef, () => ({
    openTab: (t) => {
      setTab(normalizeInspectorTab(t));
      const section = RETIRED_TAB_SECTION[t as LegacyInspectorTab];
      if (section) {
        // The home renders on the next frame; bring the moved section into view.
        requestAnimationFrame(() =>
          rootRef.current
            ?.querySelector(`[data-kbc-rail-section="${section}"]`)
            ?.scrollIntoView?.({ block: "nearest" }),
        );
      }
    },
  }));

  const badgeFor = (t: InspectorTab): number => {
    if (t === "notes") return unresolvedAnnotations + bookmarkCount;
    // Facts carries the source-comments section, so it carries that badge.
    if (t === "facts") return commentsBadgeCount;
    // Understand/History/All/Review have no unread concept — never invent one.
    return 0;
  };

  // Review is offered only when there IS one. A selected-but-absent Review
  // tab still renders (with its honest empty body) so a persisted choice never
  // silently becomes a different tab.
  const visibleTabs = INSPECTOR_TABS.filter(
    (t) => t !== "review" || hasReviewContext || tab === "review",
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
  const commentsBody = commentsPanel ?? (
    <div className="kbc-inspector__hint">No file open.</div>
  );
  const trailBody = trailPanel ?? (
    <div className="kbc-inspector__hint" data-kbc-rail-no-trail>
      Your trail is not readable here.
    </div>
  );
  const factsBody = factsPanel ?? (
    <div className="kbc-inspector__hint" data-kbc-rail-no-facts>
      No file open.
    </div>
  );
  const dossierBody = hasDossierContext
    ? (dossierPanel ?? <div className="kbc-inspector__hint">No members in this dossier.</div>)
    : null;
  const reviewBody = hasReviewContext ? (
    (reviewPanel ?? <div className="kbc-inspector__hint">No threads on this file yet.</div>)
  ) : (
    <div className="kbc-inspector__hint" data-kbc-rail-no-review>
      No review context for this file.
    </div>
  );

  const sectionBodies: Record<RailSectionId, ReactNode> = {
    "workspace-notes": workspaceNotesPanel,
    outline: outlineBody,
    entity: entityBody,
    members: dossierBody,
    provenance: provenanceBody,
    history: historyBody,
    annotations: annotationsBody,
    bookmarks: bookmarksBody,
    trail: trailBody,
    comments: commentsBody,
    facts: factsBody,
    review: reviewBody,
  };

  return (
    <div
      ref={rootRef}
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
      <SubjectChip
        subject={subject}
        caretSubject={caretSubject}
        pinned={pinned}
        onTogglePin={onTogglePin}
      />
      {citedBy}
      {frameworkCard}
      {diagnosticsCard}
      {schemaCard}
      {claimsCard}
      <div className="kbc-inspector__body" data-kbc-rail-body={tab}>
        {railSections(tab, {
          workspaceNotes: workspaceNotesPanel != null,
          dossier: dossierBody != null,
          review: hasReviewContext,
        }).map((id) => (
          <Section key={id} title={SECTION_TITLE[id]}>
            {sectionBodies[id]}
          </Section>
        ))}
      </div>
    </div>
  );
});

export default InspectorRail;
