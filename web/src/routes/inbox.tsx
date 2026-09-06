import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useInbox } from "../hooks/useInbox";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { useIsMobile } from "../hooks/useIsMobile";
import { artifactHref } from "../lib/artifactHref";
import { relativeAge } from "../lib/time";
import type { InboxItem } from "../api/inbox";
import {
  approveProposal,
  fetchProposals,
  rejectProposal,
  type ProposalItem,
  type ProposalsResponse,
} from "../api/client";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import ProposalRow from "../components/ProposalRow";
import {
  ArchiveArtifactButton,
  InboxActionRail,
  InboxCommentRow,
  type InboxTarget,
} from "../components/InboxTriage";

// Z4 — the fleet-wide open-comments inbox. Every OPEN kb-comments/1 comment
// across every corpus, grouped by artifact, newest activity first. One place
// to catch replies that would otherwise sit silently per-artifact. Each group
// deep-links to the reader with the comments panel pre-opened
// (`?panel=comments`, consumed once by detail.tsx — invariant #30).
//
// W1.mobile — per-item triage lives in InboxTriage.tsx: resolve/reply on
// every comment row (buttons-first, desktop + mobile), archive on the
// artifact card header, plus two mobile-only enhancements (a thumb-zone
// action rail + swipe on the row). `activeCommentId` tracks which row is
// "active" (tapped) on mobile — the ONLY thing driving the rail's target;
// it self-clears the instant the active comment disappears from `items`
// (resolved, or its artifact archived), so a successful action always
// dismisses the rail without a separate "close" wire.

type Group = {
  kb: string;
  artifactId: string;
  title: string;
  sourceRelative: string | null;
  latest: number;
  items: InboxItem[];
};

function groupByArtifact(items: InboxItem[]): Group[] {
  const map = new Map<string, Group>();
  for (const it of items) {
    const key = `${it.kb} ${it.artifact_id}`;
    let g = map.get(key);
    if (!g) {
      g = {
        kb: it.kb,
        artifactId: it.artifact_id,
        title: it.title,
        sourceRelative: it.source_relative ?? null,
        latest: it.updated_at,
        items: [],
      };
      map.set(key, g);
    }
    g.items.push(it);
    if (it.updated_at > g.latest) g.latest = it.updated_at;
  }
  // Newest-active group first; items within a group already arrive
  // updated_at-desc from the server, but re-sort defensively.
  const groups = Array.from(map.values());
  for (const g of groups) g.items.sort((a, b) => b.updated_at - a.updated_at);
  groups.sort((a, b) => b.latest - a.latest);
  return groups;
}

// W2.15b — the tribal-knowledge proposal inbox key (documented in
// queryClient.ts's key-contract comment block, bridged there on
// proposal.created/proposal.resolved). Fleet-wide (no `kb` arg from this
// route), matching the fleet-wide ["inbox"] shape above — no dedicated hook
// file per this phase's file ownership, so the query + optimistic
// approve/reject mutations live inline here.
const PROPOSALS_KEY = ["proposals"] as const;
const EMPTY_PROPOSALS: ProposalItem[] = [];

export default function InboxRoute() {
  useDocumentTitle("Inbox");
  const { items, totalOpen, loading, error, resolveItem, archiveArtifact } =
    useInbox();
  const isMobile = useIsMobile();
  const groups = useMemo(() => groupByArtifact(items), [items]);

  const queryClient = useQueryClient();
  const proposalsQuery = useQuery({
    queryKey: PROPOSALS_KEY,
    queryFn: ({ signal }) => fetchProposals({}, signal),
  });
  const proposals = proposalsQuery.data?.items ?? EMPTY_PROPOSALS;

  const approveProposalItem = async (kb: string, id: string) => {
    queryClient.setQueryData<ProposalsResponse>(PROPOSALS_KEY, (prev) =>
      prev
        ? {
            items: prev.items.filter((p) => !(p.kb === kb && p.id === id)),
            total: Math.max(0, prev.total - 1),
          }
        : prev,
    );
    try {
      await approveProposal(kb, id);
    } catch (e) {
      void queryClient.invalidateQueries({ queryKey: PROPOSALS_KEY });
      throw e;
    }
  };

  const rejectProposalItem = async (kb: string, id: string) => {
    queryClient.setQueryData<ProposalsResponse>(PROPOSALS_KEY, (prev) =>
      prev
        ? {
            items: prev.items.filter((p) => !(p.kb === kb && p.id === id)),
            total: Math.max(0, prev.total - 1),
          }
        : prev,
    );
    try {
      await rejectProposal(kb, id);
    } catch (e) {
      void queryClient.invalidateQueries({ queryKey: PROPOSALS_KEY });
      throw e;
    }
  };

  // Which comment row is "active" (tapped, mobile-only) — the rail's sole
  // target. Self-clears the moment it stops appearing in `items` (resolved,
  // or its artifact archived) so a successful action always dismisses the
  // rail with no separate close wire.
  const [activeCommentId, setActiveCommentId] = useState<string | null>(null);
  useEffect(() => {
    if (activeCommentId && !items.some((i) => i.comment_id === activeCommentId)) {
      setActiveCommentId(null);
    }
  }, [activeCommentId, items]);

  const activeTarget: InboxTarget | null = useMemo(() => {
    if (!activeCommentId) return null;
    const it = items.find((i) => i.comment_id === activeCommentId);
    if (!it) return null;
    return {
      kb: it.kb,
      artifactId: it.artifact_id,
      commentId: it.comment_id,
      sourceRelative: it.source_relative ?? null,
      title: it.title || it.artifact_id,
    };
  }, [activeCommentId, items]);

  return (
    <div className="inbox-view">
      {proposals.length > 0 && (
        <section className="proposals-section" aria-label="Proposals">
          <h2 className="proposals-section__title">
            Proposals
            <span className="inbox-view__count">{proposals.length}</span>
          </h2>
          <p className="proposals-section__sub">
            Memory candidates awaiting review — approve writes a memory,
            reject discards it.
          </p>
          <ul className="proposal-list">
            {proposals.map((p) => (
              <ProposalRow
                key={`${p.kb} ${p.id}`}
                item={p}
                onApprove={approveProposalItem}
                onReject={rejectProposalItem}
              />
            ))}
          </ul>
        </section>
      )}

      <header className="inbox-view__head">
        <h1 className="inbox-view__title">
          Comments inbox
          {totalOpen > 0 && (
            <span className="inbox-view__count">{totalOpen}</span>
          )}
        </h1>
        <p className="inbox-view__sub">
          Every open comment across every corpus, newest activity first.
        </p>
      </header>

      {error && (
        <div className="inbox-view__error" role="alert">
          Couldn’t load the inbox: {error}
        </div>
      )}

      {!error && !loading && groups.length === 0 && (
        <EmptyState
          icon={<Icon.Comment />}
          title="No open comments"
          hint="Comments you or Claude leave on artifacts show up here until they’re resolved."
          cli="kb comments inbox"
        />
      )}

      {groups.length > 0 && (
        <ul className="inbox-list">
          {groups.map((g) => (
            <InboxGroupCard
              key={`${g.kb} ${g.artifactId}`}
              group={g}
              activeCommentId={isMobile ? activeCommentId : null}
              swipeEnabled={isMobile}
              onActivate={setActiveCommentId}
              onResolve={resolveItem}
              onArchive={archiveArtifact}
            />
          ))}
        </ul>
      )}

      {isMobile && (
        <InboxActionRail
          target={activeTarget}
          onResolve={resolveItem}
          onArchive={archiveArtifact}
          onDismiss={() => setActiveCommentId(null)}
        />
      )}
    </div>
  );
}

function InboxGroupCard({
  group,
  activeCommentId,
  swipeEnabled,
  onActivate,
  onResolve,
  onArchive,
}: {
  group: Group;
  activeCommentId: string | null;
  swipeEnabled: boolean;
  onActivate: (commentId: string | null) => void;
  onResolve: (kb: string, artifactId: string, commentId: string) => Promise<void>;
  onArchive: (
    kb: string,
    artifactId: string,
    sourceRelative: string,
  ) => Promise<void>;
}) {
  const href = group.sourceRelative
    ? artifactHref(group.kb, group.sourceRelative, { panel: "comments" })
    : null;
  const title = group.title || group.artifactId;
  return (
    <li className="inbox-card" data-testid="inbox-card">
      <div className="inbox-card__head">
        <span className="inbox-card__kb" title={`corpus: ${group.kb}`}>
          {group.kb}
        </span>
        {href ? (
          <Link className="inbox-card__title" to={href}>
            {title}
          </Link>
        ) : (
          <span className="inbox-card__title inbox-card__title--dead" title="artifact no longer indexed">
            {title}
          </span>
        )}
        <span className="inbox-card__age">{relativeAge(group.latest)}</span>
        <ArchiveArtifactButton
          kb={group.kb}
          artifactId={group.artifactId}
          sourceRelative={group.sourceRelative}
          title={title}
          onArchive={onArchive}
        />
      </div>
      <ul className="inbox-card__comments">
        {group.items.map((it) => (
          <InboxCommentRow
            key={it.comment_id}
            kb={group.kb}
            artifactId={group.artifactId}
            sourceRelative={group.sourceRelative}
            item={it}
            active={activeCommentId === it.comment_id}
            swipeEnabled={swipeEnabled}
            onActivate={onActivate}
            onResolve={onResolve}
          />
        ))}
      </ul>
    </li>
  );
}
