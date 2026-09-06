import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchKbs, type ListSummary } from "../api/client";
import { useLists } from "../hooks/useLists";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import ListCard from "../components/lists/ListCard";
import EmptyState from "../components/EmptyState";
import Loading from "../components/Loading";
import { Icon } from "../components/icons";

// RL-track (v0.18) — /lists index: every reading list across the fleet's
// kbs, pinned first, archived collapsed. Lives entirely on the ["lists"]
// cache entry; the SSE bridge keeps it fresh (list.*, history.recorded).

function NewListForm({
  onCreate,
}: {
  onCreate: (kb: string, title: string) => Promise<void>;
}) {
  const [title, setTitle] = useState("");
  const [kb, setKb] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The kb selector only renders for multi-kb daemons; single-kb picks
  // it automatically.
  const kbsQuery = useQuery({
    queryKey: ["kbs"] as const,
    queryFn: ({ signal }) => fetchKbs(signal),
  });
  const kbs = kbsQuery.data ?? [];
  const effectiveKb = kb || kbs[0]?.name || "";

  const submit = async () => {
    const t = title.trim();
    if (!t || !effectiveKb || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onCreate(effectiveKb, t);
      setTitle("");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="kb-lists__new">
      <input
        type="text"
        className="kb-lists__new-title"
        placeholder="New reading list…"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void submit();
        }}
        aria-label="new list title"
      />
      {kbs.length > 1 && (
        <select
          className="kb-lists__new-kb"
          value={effectiveKb}
          onChange={(e) => setKb(e.target.value)}
          aria-label="kb for the new list"
        >
          {kbs.map((k) => (
            <option key={k.name} value={k.name}>
              {k.name}
            </option>
          ))}
        </select>
      )}
      <button
        type="button"
        className="kb-lists__new-btn"
        onClick={() => void submit()}
        disabled={!title.trim() || !effectiveKb || busy}
      >
        Create
      </button>
      {error && <span className="kb-lists__new-err">{error}</span>}
    </div>
  );
}

function Section({
  title,
  lists,
  onPin,
  onArchive,
}: {
  title: string;
  lists: ListSummary[];
  onPin: (l: ListSummary, pinned: boolean) => void;
  onArchive: (l: ListSummary, archived: boolean) => void;
}) {
  if (lists.length === 0) return null;
  return (
    <section className="kb-lists__section">
      {title && <h2 className="kb-lists__section-title">{title}</h2>}
      <div className="kb-lists__grid">
        {lists.map((l) => (
          <ListCard
            key={`${l.kb}/${l.id}`}
            list={l}
            onPin={onPin}
            onArchive={onArchive}
          />
        ))}
      </div>
    </section>
  );
}

export default function ListsRoute() {
  const { pinned, active, archived, loading, error, create, setPinned, setArchived } =
    useLists();
  useDocumentTitle("Reading lists");

  const onPin = (l: ListSummary, v: boolean) => void setPinned(l, v);
  const onArchive = (l: ListSummary, v: boolean) => void setArchived(l, v);

  return (
    <div className="kb-lists" data-testid="lists-view">
      <header className="kb-lists__head">
        <h1 className="kb-lists__title">Reading lists</h1>
        <NewListForm
          onCreate={async (kb, title) => {
            await create(kb, title);
          }}
        />
      </header>
      {error && (
        <div className="kb-lists__error" role="alert">
          {error}
        </div>
      )}
      {loading && <Loading icon={<Icon.Tasks />} />}
      {!loading && pinned.length + active.length + archived.length === 0 && (
        <EmptyState
          icon={<Icon.Tasks />}
          title="no reading lists yet"
          hint="Build an ordered, section-aware reading queue with derived read-state — create one above, or from the CLI."
          cli={'kb list create "Title"'}
        />
      )}
      <Section title="Pinned" lists={pinned} onPin={onPin} onArchive={onArchive} />
      <Section
        title={pinned.length > 0 ? "Lists" : ""}
        lists={active}
        onPin={onPin}
        onArchive={onArchive}
      />
      {archived.length > 0 && (
        <details className="kb-lists__archived">
          <summary className="kb-lists__section-title">
            Archived ({archived.length})
          </summary>
          <div className="kb-lists__grid">
            {archived.map((l) => (
              <ListCard
                key={`${l.kb}/${l.id}`}
                list={l}
                onPin={onPin}
                onArchive={onArchive}
              />
            ))}
          </div>
        </details>
      )}
    </div>
  );
}
