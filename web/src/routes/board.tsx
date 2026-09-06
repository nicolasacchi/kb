import { useParams, Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchBoardCanvas } from "../api/client";
import { useListDetail } from "../hooks/useLists";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { listStatsLine } from "../components/lists/ListCard";
import BoardCanvas from "../components/BoardCanvas";

// W2.4 — Boards v1: /board/:kb/:listId. A board is a reading list's
// entries laid out on a JSON Canvas geometry sidecar — the list ("what
// + in what order") stays the ["list", kb, id] query this route shares
// with `listDetail.tsx`; the canvas ("where") is its own
// `["board", kb, id]` entry (staleTime Infinity like every other server-
// state query, #23), bridged on `board.updated` (queryClient.ts). A
// fresh list has no sidecar yet — `GET` serves the empty default, so
// there's nothing to create until the first drag/PUT.

export default function BoardRoute() {
  const params = useParams<{ kb: string; listId: string }>();
  const { kb, listId } = params;
  const { list, entries, loading, error } = useListDetail(kb, listId);
  const canvasQuery = useQuery({
    queryKey: ["board", kb, listId],
    queryFn: ({ signal }) => fetchBoardCanvas(kb as string, listId as string, signal),
    enabled: !!kb && !!listId,
  });
  useDocumentTitle(list ? `${list.title} — board` : null);

  if (!kb || !listId) return <div className="empty">missing board address</div>;
  if (error) {
    return (
      <div className="kb-board__error" role="alert">
        {error} — <Link to="/lists">back to lists</Link>
      </div>
    );
  }
  if (loading || !list) return <div className="empty">loading…</div>;
  if (canvasQuery.isLoading) return <div className="empty">loading board…</div>;
  if (canvasQuery.isError) {
    return (
      <div className="kb-board__error" role="alert">
        {String(canvasQuery.error)}
      </div>
    );
  }

  return (
    <div className="kb-board" data-testid="board-detail">
      <header className="kb-board__head">
        <Link className="kb-board__back" to={`/lists/${kb}/${listId}`}>
          ← {list.title}
        </Link>
        <span className="kb-list-card__kb">{kb}</span>
        <span className="kb-list-card__stats">{listStatsLine(list)}</span>
      </header>
      {canvasQuery.data && (
        <BoardCanvas
          key={`${kb}/${listId}`}
          kb={kb}
          listId={listId}
          entries={entries}
          initialCanvas={canvasQuery.data}
        />
      )}
    </div>
  );
}
