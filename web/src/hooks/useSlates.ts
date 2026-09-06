// kb-slate/1 server state — invariant #23 shaped: plain `useQuery` over the
// shared cache with `staleTime: Infinity`, and the SSE bridge
// (`api/queryClient.ts`'s `slate.updated` / `slate.deleted` handlers) owns
// every invalidation. These hooks carry NO fetch-or-subscribe plumbing.
//
// Three keys, exactly as the design names them:
//   ["slates"]                  — GET /api/slates (list + nav chip)
//   ["slate", slug]             — GET …/{slug}?view=board
//   ["slate", slug, "history"]  — GET …/{slug}/history (the drawer)
//
// ONE mutation, because the ledger has one: `appendSlatePost`. Mark, pin,
// drop, edit, done, answer and take are all appends of one of the twelve
// kinds, so they share `post()` and its refusal handling rather than each
// growing a bespoke path.

import { useCallback, useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  appendSlatePost,
  fetchSlateBoard,
  fetchSlateHistory,
  fetchSlates,
  SlateApiError,
} from "../api/slates";
import type {
  SlateAppendResponse,
  SlateBoardResponse,
  SlateHistoryRow,
  SlateKind,
  SlatePostBody,
  SlateProv,
  SlateSummary,
} from "../api/slateTypes";
import { totalAttention } from "../lib/slateLanes";

const SLATES_KEY = ["slates"] as const;
export const slateKey = (slug: string) => ["slate", slug] as const;
export const slateHistoryKey = (slug: string) =>
  ["slate", slug, "history"] as const;

const EMPTY_SLATES: SlateSummary[] = [];
const EMPTY_HISTORY: SlateHistoryRow[] = [];

/// Every SPA post is `origin: "human"` (rules matrix "`origin`": the pin
/// check and the `[you]` rendering read THIS field, never
/// `Identity.source`). `harness` names the surface that wrote it, so a
/// board post is legible in the CLI digest as what it is.
export const SPA_PROV: SlateProv = { harness: "spa", origin: "human" };

export type UseSlatesResult = {
  slates: SlateSummary[];
  /// hand_unack + ask_open + take_contested + take_stale, fleet-wide.
  attention: number;
  loading: boolean;
  error: string | null;
};

/// The list + the nav chip read ONE cache entry.
export function useSlates(): UseSlatesResult {
  const q = useQuery({
    queryKey: SLATES_KEY,
    queryFn: ({ signal }) => fetchSlates(signal),
    staleTime: Infinity,
  });
  const slates = q.data ?? EMPTY_SLATES;
  return {
    slates,
    attention: useMemo(() => totalAttention(slates), [slates]),
    loading: q.isPending,
    error: q.error ? String(q.error) : null,
  };
}

/// The nav badge's ONE number. Split out so `NavList`/the header don't pull
/// the whole list into their render — same cache entry, same fetch.
///
/// Returns 0 (not null) while loading; the CALLER hides at zero, which is
/// what keeps the chip from ever flashing a `0` between a load and a
/// count — see `NavList.tsx`'s `attention > 0 &&` guard and its vitest case.
export function useSlatesAttention(): number {
  return useSlates().attention;
}

export type SlateBoardResult = {
  board: SlateBoardResponse | undefined;
  loading: boolean;
  error: string | null;
};

/// The board itself (`?view=board`). Never budget-truncated — the board
/// scrolls, and truncation is a digest concern.
export function useSlateBoard(
  slug: string,
  topic?: string,
): SlateBoardResult {
  const q = useQuery({
    // The ?topic= variant gets its own SEGMENT rather than sitting directly
    // under ["slate", slug]: a bare [...slateKey(slug), topic] would collide
    // with ["slate", slug, "history"] for a slate whose topic is literally
    // named "history". Both still ride the ["slate", slug] prefix the bridge
    // invalidates.
    queryKey: topic ? [...slateKey(slug), "topic", topic] : slateKey(slug),
    queryFn: ({ signal }) => fetchSlateBoard(slug, { topic }, signal),
    staleTime: Infinity,
    enabled: !!slug,
  });
  return {
    board: q.data,
    loading: q.isPending,
    error: q.error ? String(q.error) : null,
  };
}

/// The history drawer's rows. `enabled` so the drawer's query only runs
/// when it is actually open (`?history=1`) — a permanent second fetch per
/// board view would be a read nobody asked for.
export function useSlateHistory(
  slug: string,
  enabled: boolean,
): { rows: SlateHistoryRow[]; loading: boolean; error: string | null } {
  const q = useQuery({
    queryKey: slateHistoryKey(slug),
    queryFn: ({ signal }) => fetchSlateHistory(slug, {}, signal),
    staleTime: Infinity,
    enabled: enabled && !!slug,
  });
  return {
    rows: q.data ?? EMPTY_HISTORY,
    loading: q.isPending && enabled,
    error: q.error ? String(q.error) : null,
  };
}

/// Everything a friction handler needs to write an honest sentence, from
/// the card the user acted on. Optional: a composer post has no target.
export type SlateActionContext = {
  targetKind?: SlateKind;
  /// The target's author tag (`Who::tag`) — "codex/8f2a".
  holder?: string;
};

export type SlatePostFn = (
  body: SlatePostBody,
  ctx?: SlateActionContext,
) => Promise<SlateAppendResponse | null>;

/// Invalidate the three keys one append can move. The SSE `slate.updated`
/// handler does this too; doing it here as well means the board is right
/// the instant the POST resolves even if the stream is degraded, and a
/// double invalidation of the same key is a no-op refetch collapse.
export function useInvalidateSlate(slug: string): () => void {
  const qc = useQueryClient();
  return useCallback(() => {
    void qc.invalidateQueries({ queryKey: slateKey(slug) });
    void qc.invalidateQueries({ queryKey: SLATES_KEY });
  }, [qc, slug]);
}

/// The ONE write path, with the design's friction attached.
///
/// Refusals (rules matrix "Drop and edit friction (the one rule)"):
///   * `slate-taken` (409) — a live take on the same subject. Toasts with a
///     "post anyway" action that resends with `anyway: true`.
///   * `slate-live-author` (409) — a drop/edit of a live other session's
///     now/warn/take/unacknowledged hand. Opens `useConfirm` naming the
///     holder, and resends with `anyway: true` on yes.
///     ONE exception, and it is NOT an `anyway` case: a `supersedes` on
///     another live session's TAKE is refused outright with no escape — the
///     remedy is `take --over`. We say so instead of offering a button that
///     would 409 again.
///   * anything else — the problem title, verbatim.
///
/// On success, `displaced` (what this post pushed off the default digest)
/// and `nudge` (nine or more undropped found/idea posts by this session)
/// are surfaced as toasts. Neither ever blocked the write, so neither is
/// rendered as a failure.
export function useSlatePost(
  slug: string,
  deps: {
    confirm: (opts: { title: string; body: string; confirmLabel?: string }) => Promise<boolean>;
    toastErr: (msg: string, action?: { label: string; onClick: () => void }) => void;
    toastInfo: (msg: string, action?: { label: string; onClick: () => void }) => void;
    onOpenHistory?: () => void;
  },
): SlatePostFn {
  const invalidate = useInvalidateSlate(slug);
  const { confirm, toastErr, toastInfo, onOpenHistory } = deps;

  return useCallback(
    async (body: SlatePostBody, ctx?: SlateActionContext) => {
      const send = async (b: SlatePostBody) => {
        const res = await appendSlatePost(slug, b);
        invalidate();
        if (res.displaced.length > 0) {
          const first = res.displaced[0];
          const more =
            res.displaced_total > 1 ? ` (+${res.displaced_total - 1} more)` : "";
          toastInfo(
            `Pushed off the board: #${first.seq} ${first.kind} "${first.line}"${more}`,
            onOpenHistory ? { label: "open history", onClick: onOpenHistory } : undefined,
          );
        }
        if (res.nudge) toastInfo(res.nudge);
        return res;
      };

      try {
        return await send(body);
      } catch (e) {
        if (!(e instanceof SlateApiError)) {
          toastErr(String(e));
          return null;
        }
        const code = e.code;
        const holder = e.holder
          ? `${e.holder.harness}/${e.holder.session_short}`
          : (ctx?.holder ?? "another session");

        if (code === "slate-live-author") {
          // The one no-escape case, named honestly rather than retried.
          if (body.supersedes != null && ctx?.targetKind === "take") {
            toastErr(
              `${holder} is live and holds this take — edit your own claim, or take it over.`,
            );
            return null;
          }
          const kindWord = ctx?.targetKind ?? "post";
          const verb = body.kind === "drop" ? "Drop" : "Edit";
          const ok = await confirm({
            title: `${verb} anyway?`,
            body: `${holder} is live and holds this ${kindWord}. ${verb} anyway?`,
            confirmLabel: `${verb} anyway`,
          });
          if (!ok) return null;
          try {
            return await send({ ...body, anyway: true });
          } catch (e2) {
            toastErr(e2 instanceof SlateApiError ? e2.title : String(e2));
            return null;
          }
        }

        if (code === "slate-taken") {
          toastErr(e.title, {
            label: "post anyway",
            onClick: () => {
              void send({ ...body, anyway: true }).catch((e2) =>
                toastErr(e2 instanceof SlateApiError ? e2.title : String(e2)),
              );
            },
          });
          return null;
        }

        toastErr(e.title);
        return null;
      }
    },
    [slug, invalidate, confirm, toastErr, toastInfo, onOpenHistory],
  );
}
