// V72-G1.2 — the entity dossier, D6's "snippet-list dossier", rendered as the
// reader shell's `dossier` CENTER MODE.
//
// This is a RENDERER, not a second entity engine: every number on screen comes
// off `entity/1`'s wire and every ordering decision is delegated to
// `lib/dossier.ts`'s pure half. The one thing this file computes is which
// lines of a definition's file to paint, and that cut is captioned.
//
// FOUR READ STATES, FOUR RENDERINGS (`lib/dossier.ts`'s `DossierReadState`):
// a request in flight, a request that failed, an honest server-side `empty`
// with its reason, and a `partial` whose budget caption names what was
// dropped. The fifth case — `ok` — is the only one that renders no banner at
// all, which is what makes a banner's presence mean something.
//
// TRUST IS LINE STYLE, NOT HUE. Every trust class renders through the shared
// `TrustBadge` (kbc-theme/1's Lane Budget: `--kbc-trust-style` solid/dashed/
// dotted), never through a colour this file picks — see `TrustBadge.tsx`'s own
// comment for why that survives every theme and every colour-vision
// deficiency.

import { useMemo, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchFile } from "../../api/client";
import type {
  DefinitionBlock,
  DossierOut,
  MemberRow,
  UsageGroup,
  UsageRow2,
} from "../../api/types";
import {
  buildLineSpans,
  paintLine,
  splitContentLines,
  type LineSpan,
} from "../../lib/diffHighlight";
import {
  cycleMemberSort,
  DOSSIER_SECTIONS,
  honestyCaption,
  nextUsagesPerKind,
  sortMembers,
  usageGroupCensus,
  type DossierReadState,
  type DossierSectionId,
  type MemberSort,
} from "../../lib/dossier";
import TrustBadge from "../TrustBadge";

/// How many lines of one definition block this page paints before it cuts.
/// A reopened class in a monolith is legitimately hundreds of lines; the
/// dossier is read to ORIENT, so the block shows its head and SAYS it cut.
const DEF_SNIPPET_MAX_LINES = 40;

/// Whole-file fetches this page will run for definition snippets. A 60-file
/// reopening is real (`MAX_DEFINITION_FILES` is 64 server-side) and 60
/// whole-file reads on an IO-bound host is not a page load — the blocks past
/// this cut render their address and their opener, with a caption, and the
/// reader row opens the real file.
const DEF_SNIPPET_MAX_BLOCKS = 8;

export interface DossierViewProps {
  repo: string;
  /// The entity address (`?ent=`), verbatim as it was asked.
  ent: string;
  /// The ONE `entity/1` read (`hooks/useDossier.ts`), owned by the shell and
  /// shared with the Dossier rail tab — this component never fetches the
  /// dossier itself, so the two renderers cannot disagree.
  data: DossierOut | undefined;
  state: DossierReadState;
  error: unknown;
  inherited: boolean;
  onInheritedChange: (next: boolean) => void;
  sort: MemberSort;
  onSortChange: (next: MemberSort) => void;
  /// The current per-kind usages cut, and the re-ask that raises it.
  usagesPerKind: number;
  onUsagesPerKind: (next: number) => void;
  /// Open a file location in the reader (the shell owns navigation).
  onOpen: (path: string, line?: number) => void;
  /// Open ANOTHER entity's dossier (hierarchy + namespace rows).
  onOpenEntity: (fqn: string) => void;
  /// The section the `]s`/`[s` motions last moved to, scrolled into view by
  /// the shell; `null` before any motion.
  activeSection: DossierSectionId | null;
}

// ── one definition block's live code ───────────────────────────────────────

function PaintedLine({ text, spans }: { text: string; spans: LineSpan[] | undefined }) {
  const segs = paintLine(text, spans);
  if (segs.length === 1 && !segs[0].cls) return <>{text}</>;
  return (
    <>
      {segs.map((s, i) =>
        s.cls ? (
          <span key={i} className={s.cls} data-kbc-hl>
            {s.text}
          </span>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </>
  );
}

function DefinitionSnippet({
  repo,
  block,
  onOpen,
}: {
  repo: string;
  block: DefinitionBlock;
  onOpen: (path: string, line?: number) => void;
}) {
  // A `missing` block has no live bytes by definition — never fetch, and say
  // why rather than showing an empty frame.
  const enabled = !block.missing;
  const q = useQuery({
    // The EXACT key `useDiffHighlights`/`useFile` use, so a definition
    // snippet and the reader share one cached read of the file.
    queryKey: ["file", repo, block.path, null],
    queryFn: () => fetchFile(repo, block.path),
    enabled,
    staleTime: 30_000,
  });

  const painted = useMemo(() => {
    const f = q.data;
    if (!f || f.encoding !== "utf8") return null;
    const lines = splitContentLines(f.content);
    const spans = f.highlights ? buildLineSpans(f.content, f.highlights) : new Map<number, LineSpan[]>();
    const start = Math.max(1, block.line_start);
    const wanted = Math.max(0, block.line_end - start + 1);
    const end = Math.min(lines.length, start + Math.min(wanted, DEF_SNIPPET_MAX_LINES) - 1);
    const rows: Array<{ n: number; text: string; spans: LineSpan[] | undefined }> = [];
    for (let n = start; n <= end; n++) {
      rows.push({ n, text: lines[n - 1] ?? "", spans: spans.get(n) });
    }
    return { rows, cut: wanted > DEF_SNIPPET_MAX_LINES ? wanted : 0 };
  }, [q.data, block.line_start, block.line_end]);

  return (
    <li className="kbc-dossier__def" data-kbc-dossier-def={block.reopening_index}>
      <div className="kbc-dossier__def-head">
        <button
          type="button"
          className="kbc-dossier__def-addr"
          onClick={() => onOpen(block.path, block.line_start)}
          data-kbc-dossier-def-open
          title={`open ${block.path}:${block.line_start} in the reader`}
        >
          {block.path}:{block.line_start}-{block.line_end}
        </button>
        <span className="kbc-dossier__def-kind" data-kbc-dossier-def-kind={block.kind}>
          {block.kind}
        </span>
        <TrustBadge cls={block.trust} title={`matched via ${block.matched_via}; nesting ${block.nesting}`} />
      </div>
      {/* The opener chain is read off the SOURCE, never reconstructed from the
          FQN — rendered verbatim beside the form the server named. */}
      <p className="kbc-dossier__def-opener" data-kbc-dossier-opener>
        <code>{block.opener}</code>
        <span className="kbc-dossier__def-form" data-kbc-dossier-opener-form={block.opener_form}>
          {block.opener_form}
        </span>
      </p>
      {block.stale && (
        <p className="kbc-dossier__note kbc-dossier__note--warn" data-kbc-dossier-note>
          stale — these lines were indexed from a blob that is no longer the live one
        </p>
      )}
      {block.missing ? (
        <p className="kbc-dossier__note kbc-dossier__note--warn" data-kbc-dossier-note>
          the file carries no live bytes any more — the block could not be read
        </p>
      ) : q.isLoading ? (
        <p className="kbc-dossier__note" data-kbc-dossier-note>
          loading {block.path}…
        </p>
      ) : q.error ? (
        <p className="kbc-dossier__note kbc-dossier__note--warn" data-kbc-dossier-note>
          could not read {block.path}
        </p>
      ) : !painted ? (
        <p className="kbc-dossier__note" data-kbc-dossier-note>
          not a UTF-8 text file — nothing to paint
        </p>
      ) : (
        <>
          <pre className="kbc-dossier__code" data-kbc-dossier-code>
            {painted.rows.map((r) => (
              <div key={r.n} className="kbc-dossier__code-line">
                <span className="kbc-dossier__code-num">{r.n}</span>
                <span className="kbc-dossier__code-text">
                  <PaintedLine text={r.text} spans={r.spans} />
                </span>
              </div>
            ))}
          </pre>
          {painted.cut > 0 && (
            <p className="kbc-dossier__note" data-kbc-dossier-note>
              showing the first {DEF_SNIPPET_MAX_LINES} of {painted.cut} lines — open the file for
              the rest
            </p>
          )}
        </>
      )}
    </li>
  );
}

// ── sections ───────────────────────────────────────────────────────────────

function Section({
  id,
  label,
  active,
  count,
  children,
}: {
  id: DossierSectionId;
  label: string;
  active: boolean;
  /// The wire's own count for this section's lane, when it has one.
  count?: number;
  children: ReactNode;
}) {
  const dom = DOSSIER_SECTIONS.find((s) => s.id === id)!.domId;
  return (
    <section
      id={dom}
      className={"kbc-dossier__section" + (active ? " is-active" : "")}
      data-kbc-dossier-section={id}
      aria-label={label}
    >
      <h2 className="kbc-dossier__h">
        {label}
        {count !== undefined && (
          <span className="kbc-dossier__h-count" data-kbc-dossier-count={id}>
            {count}
          </span>
        )}
      </h2>
      {children}
    </section>
  );
}

function MemberTable({
  members,
  sort,
  onOpen,
  onOpenEntity,
}: {
  members: MemberRow[];
  sort: MemberSort;
  onOpen: (path: string, line?: number) => void;
  onOpenEntity: (fqn: string) => void;
}) {
  const rows = useMemo(() => sortMembers(members, sort), [members, sort]);
  if (rows.length === 0) {
    return (
      <p className="kbc-dossier__note" data-kbc-dossier-note>
        no members this scanner can see — check Unknown members below
      </p>
    );
  }
  return (
    <table className="kbc-dossier__members" data-kbc-dossier-members>
      <thead>
        <tr>
          <th scope="col">name</th>
          <th scope="col">kind</th>
          <th scope="col">visibility</th>
          <th scope="col">defining type</th>
          <th scope="col">via</th>
          <th scope="col">trust</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((m) => (
          <tr
            key={`${m.defining_type}#${m.name}@${m.path}:${m.line}`}
            data-kbc-dossier-member={m.name}
            data-kbc-dossier-member-inherited={m.inherited ? "1" : "0"}
          >
            <td>
              <button
                type="button"
                className="kbc-dossier__member-name"
                onClick={() => onOpen(m.path, m.line)}
                data-kbc-dossier-member-open
                title={`open ${m.path}:${m.line} in the reader`}
              >
                {m.name}
              </button>
            </td>
            <td>{m.kind}</td>
            <td data-kbc-dossier-member-vis={m.visibility}>{m.visibility}</td>
            <td>
              <button
                type="button"
                className="kbc-dossier__link"
                onClick={() => onOpenEntity(m.defining_type)}
                title={`open ${m.defining_type}'s dossier`}
              >
                {m.defining_type}
              </button>
              {m.inherited && (
                <span className="kbc-dossier__pill" data-kbc-dossier-inherited-pill>
                  inherited
                </span>
              )}
            </td>
            <td>{m.via}</td>
            <td>
              <TrustBadge cls={m.trust} title={`found via ${m.via}`} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function UsageGroupBlock({
  group,
  onOpen,
}: {
  group: UsageGroup;
  onOpen: (path: string, line?: number) => void;
}) {
  const census = usageGroupCensus(group);
  return (
    <div className="kbc-dossier__ugroup" data-kbc-dossier-ugroup={group.kind}>
      <h3 className="kbc-dossier__ugroup-h">
        {group.kind}
        {/* The TRUE total, straight off the wire — never `rows.length`. */}
        <span className="kbc-dossier__h-count" data-kbc-dossier-ugroup-total={group.kind}>
          {group.total}
        </span>
      </h3>
      <p className="kbc-dossier__note" data-kbc-dossier-note data-kbc-dossier-ucensus={group.kind}>
        {census.caption}
      </p>
      <ul className="kbc-dossier__urows">
        {group.rows.map((r: UsageRow2, i) => (
          <li key={`${r.path}:${r.line}:${r.col}:${i}`} className="kbc-dossier__urow">
            <button
              type="button"
              className="kbc-dossier__link"
              onClick={() => onOpen(r.path, r.line)}
              data-kbc-dossier-usage-open
            >
              {r.path}:{r.line}
            </button>
            <TrustBadge cls={r.trust} precision={r.precision} />
            {r.context && <code className="kbc-dossier__ucontext">{r.context}</code>}
          </li>
        ))}
      </ul>
    </div>
  );
}

// ── the view ───────────────────────────────────────────────────────────────

export default function DossierView(props: DossierViewProps) {
  const { repo, ent, inherited, sort, onOpen, onOpenEntity, activeSection } = props;
  const { data, state, error, usagesPerKind } = props;

  const header = (
    <header className="kbc-dossier__head" data-kbc-dossier-head>
      <div className="kbc-dossier__title-row">
        <h1 className="kbc-dossier__title" data-kbc-dossier-fqn>
          {data?.entity.fqn ?? ent}
        </h1>
        {data && (
          <>
            <span className="kbc-dossier__kind" data-kbc-dossier-kind={data.entity.kind}>
              {data.entity.kind}
            </span>
            {/* A census, never an aggregate verdict (`entities/1`'s posture). */}
            <span className="kbc-dossier__census" data-kbc-dossier-census>
              {data.entity.trust_counts.exact} exact · {data.entity.trust_counts.likely} likely ·{" "}
              {data.entity.trust_counts.candidate} candidate
            </span>
          </>
        )}
      </div>
      <div className="kbc-dossier__controls">
        <button
          type="button"
          className={"kbc-dossier__toggle" + (inherited ? " is-on" : "")}
          onClick={() => props.onInheritedChange(!inherited)}
          aria-pressed={inherited}
          data-kbc-dossier-inherited-toggle
          title="include members from ancestors and mixins (re-fetches with ?inherited=1)"
        >
          inherited
        </button>
        <button
          type="button"
          className="kbc-dossier__sort"
          onClick={() => props.onSortChange(cycleMemberSort(sort))}
          data-kbc-dossier-sort={sort}
          title="cycle the member table's sort"
        >
          sort: {sort}
        </button>
      </div>
    </header>
  );

  if (state === "loading") {
    return (
      <div className="kbc-dossier" data-kbc-dossier data-kbc-dossier-state="loading">
        {header}
        <p className="kbc-dossier__note" data-kbc-dossier-note>
          reading {ent}…
        </p>
      </div>
    );
  }

  if (state === "error" || !data) {
    const msg = error instanceof Error ? error.message : "the dossier request failed";
    return (
      <div className="kbc-dossier" data-kbc-dossier data-kbc-dossier-state="error">
        {header}
        <p className="kbc-dossier__note kbc-dossier__note--warn" data-kbc-dossier-note data-kbc-dossier-error>
          {msg}
        </p>
      </div>
    );
  }

  const caption = honestyCaption(data.honesty);
  const defBlocks = data.definitions.slice(0, DEF_SNIPPET_MAX_BLOCKS);
  const defOverflow = data.definitions.length - defBlocks.length;
  const nextPerKind = nextUsagesPerKind(usagesPerKind);

  return (
    <div className="kbc-dossier" data-kbc-dossier data-kbc-dossier-state={state}>
      {header}

      {/* The honesty block — a header caption, never a footnote. */}
      {caption && (
        <p
          className={
            "kbc-dossier__honesty" +
            (data.honesty.state === "empty" ? " kbc-dossier__honesty--empty" : "")
          }
          data-kbc-dossier-honesty={data.honesty.state}
        >
          {caption}
        </p>
      )}
      {/* Every caption the response owes its reader, verbatim — the
          kbc-tree/1 honesty-strip rule (`web-code/CLAUDE.md`): rendered one
          per note, never summarised. */}
      {data.honesty.notes.map((n, i) => (
        <p key={i} className="kbc-dossier__note" data-kbc-dossier-note>
          {n}
        </p>
      ))}
      {data.candidates.length > 0 && (
        <p className="kbc-dossier__note kbc-dossier__note--warn" data-kbc-dossier-note>
          ambiguous address — {data.candidates.length} entities answer to {ent}:{" "}
          {data.candidates.map((c) => (
            <button
              key={c}
              type="button"
              className="kbc-dossier__link"
              onClick={() => onOpenEntity(c)}
            >
              {c}
            </button>
          ))}
        </p>
      )}

      <Section
        id="definitions"
        label="Definitions"
        active={activeSection === "definitions"}
        count={data.definitions.length}
      >
        <ul className="kbc-dossier__defs">
          {defBlocks.map((b) => (
            <DefinitionSnippet key={`${b.path}:${b.line_start}`} repo={repo} block={b} onOpen={onOpen} />
          ))}
        </ul>
        {defOverflow > 0 && (
          <p className="kbc-dossier__note" data-kbc-dossier-note>
            {defOverflow} more reopening{defOverflow === 1 ? "" : "s"} not painted — this page reads
            at most {DEF_SNIPPET_MAX_BLOCKS} definition files
          </p>
        )}
      </Section>

      <Section id="members" label="Members" active={activeSection === "members"} count={data.members.length}>
        <MemberTable members={data.members} sort={sort} onOpen={onOpen} onOpenEntity={onOpenEntity} />
      </Section>

      <Section id="hierarchy" label="Hierarchy" active={activeSection === "hierarchy"}>
        {data.hierarchy.notes.map((n, i) => (
          <p key={i} className="kbc-dossier__note" data-kbc-dossier-note>
            {n}
          </p>
        ))}
        <h3 className="kbc-dossier__ugroup-h">Ancestors</h3>
        <ul className="kbc-dossier__hlist" data-kbc-dossier-ancestors>
          {data.hierarchy.ancestors.length === 0 && (
            <li className="kbc-dossier__note">none</li>
          )}
          {data.hierarchy.ancestors.map((a) => (
            <li key={`${a.written}@${a.depth}`}>
              <button
                type="button"
                className="kbc-dossier__link"
                onClick={() => (a.fqn ? onOpenEntity(a.fqn) : onOpen(a.from_path, a.from_line))}
              >
                {a.written}
              </button>
              {a.fqn && a.fqn !== a.written && <code className="kbc-dossier__fqn">{a.fqn}</code>}
              <span className="kbc-dossier__pill">depth {a.depth}</span>
              <TrustBadge cls={a.resolved} title={`resolved: ${a.resolved}`} />
            </li>
          ))}
        </ul>
        <h3 className="kbc-dossier__ugroup-h">Mixins</h3>
        <ul className="kbc-dossier__hlist" data-kbc-dossier-mixins>
          {data.hierarchy.mixins.length === 0 && <li className="kbc-dossier__note">none</li>}
          {data.hierarchy.mixins.map((m, i) => (
            <li key={`${m.kind}:${m.written}:${i}`}>
              <span className="kbc-dossier__pill" data-kbc-dossier-mixin-kind={m.kind}>
                {m.kind}
              </span>
              <button
                type="button"
                className="kbc-dossier__link"
                onClick={() => (m.fqn ? onOpenEntity(m.fqn) : onOpen(m.from_path, m.from_line))}
              >
                {m.written}
              </button>
              {m.fqn && m.fqn !== m.written && <code className="kbc-dossier__fqn">{m.fqn}</code>}
              <TrustBadge cls={m.resolved} title={`resolved: ${m.resolved}`} />
            </li>
          ))}
        </ul>
        <h3 className="kbc-dossier__ugroup-h">Descendants</h3>
        <ul className="kbc-dossier__hlist" data-kbc-dossier-descendants>
          {data.hierarchy.descendants.length === 0 && <li className="kbc-dossier__note">none</li>}
          {data.hierarchy.descendants.map((d, i) => (
            <li key={`${d.fqn ?? d.path}:${i}`}>
              <button
                type="button"
                className="kbc-dossier__link"
                onClick={() => (d.fqn ? onOpenEntity(d.fqn) : onOpen(d.path, d.line))}
              >
                {d.fqn ?? d.path}
              </button>
              <span className="kbc-dossier__pill">{d.via}</span>
              <TrustBadge cls={d.trust} />
            </li>
          ))}
        </ul>
        <h3 className="kbc-dossier__ugroup-h">Implementors</h3>
        <ul className="kbc-dossier__hlist" data-kbc-dossier-implementors>
          {data.hierarchy.implementors.length === 0 && <li className="kbc-dossier__note">none</li>}
          {data.hierarchy.implementors.map((d, i) => (
            <li key={`${d.fqn ?? d.path}:${i}`}>
              <button
                type="button"
                className="kbc-dossier__link"
                onClick={() => (d.fqn ? onOpenEntity(d.fqn) : onOpen(d.path, d.line))}
              >
                {d.fqn ?? d.path}
              </button>
              <span className="kbc-dossier__pill">{d.via}</span>
              <TrustBadge cls={d.trust} />
            </li>
          ))}
        </ul>
      </Section>

      <Section id="usages" label="Usages" active={activeSection === "usages"} count={data.usages.total}>
        {data.usages.state !== "ok" && (
          <p className="kbc-dossier__note kbc-dossier__note--warn" data-kbc-dossier-note>
            usages {data.usages.state}
            {data.usages.reason ? ` — ${data.usages.reason}` : ""}
          </p>
        )}
        {data.usages.anchor && (
          <p className="kbc-dossier__note" data-kbc-dossier-note>
            run at {data.usages.anchor.path}:{data.usages.anchor.line}:{data.usages.anchor.col}
          </p>
        )}
        {data.usages.groups.map((g) => (
          <UsageGroupBlock key={g.kind} group={g} onOpen={onOpen} />
        ))}
        {data.usages.truncated && (
          <>
            <p className="kbc-dossier__note" data-kbc-dossier-note>
              truncated at {usagesPerKind} rows per kind — {data.usages.total} usages in total
            </p>
            {/* Never a client-side slice: "show more" RE-ASKS with a higher
                `usages_per_kind`. Absent (not disabled) at the cap. */}
            {nextPerKind !== null && (
              <button
                type="button"
                className="kbc-dossier__more"
                onClick={() => props.onUsagesPerKind(nextPerKind)}
                data-kbc-dossier-usages-more
              >
                show more ({nextPerKind} per kind)
              </button>
            )}
          </>
        )}
      </Section>

      <Section
        id="unknown-members"
        label="Unknown members"
        active={activeSection === "unknown-members"}
        count={data.unknown_members.length}
      >
        <p className="kbc-dossier__note" data-kbc-dossier-note data-kbc-dossier-holes-caption>
          metaprogramming holes: these members exist at runtime but the index cannot see them
        </p>
        <ul className="kbc-dossier__holes" data-kbc-dossier-holes>
          {data.unknown_members.length === 0 && <li className="kbc-dossier__note">none found</li>}
          {data.unknown_members.map((u, i) => (
            <li key={`${u.path}:${u.line}:${i}`}>
              <span className="kbc-dossier__pill" data-kbc-dossier-mech={u.mechanism}>
                {u.mechanism}
              </span>
              {u.name_hint && <code className="kbc-dossier__fqn">{u.name_hint}</code>}
              <button
                type="button"
                className="kbc-dossier__link"
                onClick={() => onOpen(u.path, u.line)}
              >
                {u.path}:{u.line}
              </button>
              <code className="kbc-dossier__ucontext">{u.context}</code>
            </li>
          ))}
        </ul>
      </Section>

      <Section
        id="namespace"
        label="Namespace tree"
        active={activeSection === "namespace"}
        count={data.namespace_tree.length}
      >
        <ul className="kbc-dossier__ns" data-kbc-dossier-ns>
          {data.namespace_tree.length === 0 && (
            <li className="kbc-dossier__note">nothing nested under {data.entity.fqn}</li>
          )}
          {data.namespace_tree.map((c) => (
            <li key={c.fqn}>
              <button
                type="button"
                className="kbc-dossier__link"
                onClick={() => onOpenEntity(c.fqn)}
                data-kbc-dossier-ns-open={c.segment}
              >
                {c.segment}
              </button>
              <span className="kbc-dossier__pill">{c.kind}</span>
              <span className="kbc-dossier__note">
                {c.definitions} definition{c.definitions === 1 ? "" : "s"} · {c.descendants} below
              </span>
            </li>
          ))}
        </ul>
      </Section>
    </div>
  );
}
