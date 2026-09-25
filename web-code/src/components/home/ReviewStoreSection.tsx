// RS-U11 — the Home dashboard's per-repo "Review store" + "Fetch
// credential" sections (README §12: "Settings get 'Review store' and
// 'Fetch credential' cards"). web-code has no dedicated Settings page —
// `RepoCard`'s per-repo dashboard card is the closest existing per-repo
// surface, so these ride ITS OWN "each section is an independent React
// Query consumer, a failed fetch renders a quiet inline note rather than
// breaking the whole card" discipline (that file's module doc) rather
// than inventing a new route (new landmarks, new keyboard scope — out of
// scope for a chips-and-cards unit).
//
// Both routes (`GET /api/repos/{name}/store` / `…/credentials`) already
// exist (RS-U3) and return everything rendered here — nothing computed
// client-side beyond `formatBytes`, the doctor-level → CSS-class map, and
// (RS-U9) `lib/reviewStore.ts`'s parse of the maintenance/GC facts riding
// the opaque `state_json` blob (no route turns those into a typed field
// or a doctor finding — see that module's own doc).
import { useReviewCredentials, useReviewStoreCard } from "../../hooks/useReviewStore";
import { formatBytes, relativeTime } from "../../lib/format";
import { parseLastGcApply, parseLastGcDryRun, parseLastMaint } from "../../lib/reviewStore";
import MetaLine from "../MetaLine";

export function ReviewStoreSection({ repo }: { repo: string }) {
  const storeQ = useReviewStoreCard(repo);
  const stateJson = storeQ.data?.store?.state_json;
  const lastMaint = parseLastMaint(stateJson);
  const lastGcDryRun = parseLastGcDryRun(stateJson);
  const lastGcApply = parseLastGcApply(stateJson);

  return (
    <section className="kbc-home-card__section" data-kbc-home-store>
      <h3 className="kbc-home-card__section-title">Review store</h3>
      {storeQ.isLoading ? (
        <p className="kbc-home-card__muted">Loading…</p>
      ) : storeQ.error ? (
        <p className="kbc-home-card__error">Couldn't load the review store</p>
      ) : storeQ.data ? (
        <>
          <MetaLine
            items={[
              <span
                key="state"
                className={`kbc-home-card__store-state kbc-home-card__store-state--${storeQ.data.store?.state ?? "none"}`}
                data-kbc-home-store-state={storeQ.data.store?.state ?? "not-registered"}
              >
                {storeQ.data.store?.state ?? "not registered"}
              </span>,
              storeQ.data.store?.store_key ?? null,
              storeQ.data.members.length > 0
                ? `${storeQ.data.members.length} member${storeQ.data.members.length === 1 ? "" : "s"}`
                : null,
              storeQ.data.disk
                ? `${storeQ.data.disk.packs} pack${storeQ.data.disk.packs === 1 ? "" : "s"} · ${formatBytes(storeQ.data.disk.total_bytes)}`
                : null,
            ]}
          />
          {/* The store card's own doctor findings (`review_store/routes.rs::store_card`) — rendered
              VERBATIM, never re-derived: registration/credential/forge-verification/objects-missing
              notes. The maintenance/GC summary below is a SEPARATE block — RS-U9 never turned those
              facts into a doctor finding, so they're parsed from `state_json` instead. */}
          {storeQ.data.doctor.length > 0 && (
            <ul className="kbc-home-card__doctor" data-kbc-home-store-doctor>
              {/* `code` is NOT unique — `store_card` can push several
                  `"config"`-coded findings (one per `[review.store]`
                  warning) — so this keys on position, not `f.code`. */}
              {storeQ.data.doctor.map((f, i) => (
                <li
                  key={i}
                  className={`kbc-home-card__doctor-row kbc-home-card__doctor-row--${f.level}`}
                  data-kbc-home-doctor={f.code}
                  title={f.code}
                >
                  {f.message}
                </li>
              ))}
            </ul>
          )}
          {/* RS-U9's maintenance/GC facts (daily/weekly/monthly cadences,
              the last GC dry-run/apply) — parsed from `state_json`
              (`lib/reviewStore.ts`'s own doc on why that's a client-side
              read rather than a server-typed field). Scheduled GC is
              REPORT-ONLY in Phase 1 (operator ruling), so `lastGcApply`
              in practice only ever comes from an explicit `store gc
              --yes` — both render independently since they answer
              different questions ("last time we scanned" vs. "last time
              we actually deleted"). */}
          {(lastMaint || lastGcDryRun || lastGcApply) && (
            <div className="kbc-home-card__maint" data-kbc-home-store-maint>
              {lastMaint && (
                <MetaLine
                  items={[
                    lastMaint.daily != null ? `daily maint ${relativeTime(lastMaint.daily)}` : null,
                    lastMaint.weekly != null ? `weekly ${relativeTime(lastMaint.weekly)}` : null,
                    lastMaint.monthly != null ? `monthly ${relativeTime(lastMaint.monthly)}` : null,
                  ]}
                />
              )}
              {lastGcApply && (
                <p className="kbc-home-card__muted" data-kbc-home-store-gc-apply>
                  last GC apply: {lastGcApply.candidates} removed · {relativeTime(lastGcApply.at)}
                </p>
              )}
              {lastGcDryRun && (
                <p className="kbc-home-card__muted" data-kbc-home-store-gc-dryrun>
                  last GC dry-run: {lastGcDryRun.candidates} candidate{lastGcDryRun.candidates === 1 ? "" : "s"} ·{" "}
                  {relativeTime(lastGcDryRun.at)}
                  {lastGcDryRun.partial ? " · partial — some members unresolved" : ""}
                </p>
              )}
            </div>
          )}
        </>
      ) : (
        <p className="kbc-home-card__muted">No store yet.</p>
      )}
    </section>
  );
}

export function ReviewCredentialSection({ repo }: { repo: string }) {
  const credQ = useReviewCredentials(repo);

  return (
    <section className="kbc-home-card__section" data-kbc-home-credentials>
      <h3 className="kbc-home-card__section-title">Fetch credential</h3>
      {credQ.isLoading ? (
        <p className="kbc-home-card__muted">Loading…</p>
      ) : credQ.error ? (
        <p className="kbc-home-card__error">Couldn't load the fetch credential</p>
      ) : credQ.data ? (
        credQ.data.fetch.resolved ? (
          <>
            <MetaLine
              items={[
                credQ.data.fetch.cred_kind,
                credQ.data.fetch.account,
                credQ.data.fetch.host,
              ]}
            />
            {credQ.data.fetch.broader_than_needed && (
              <p className="kbc-home-card__cred-amber" data-kbc-home-cred-broader>
                broader than needed — used read-only (fetch + GET only, never a push)
              </p>
            )}
            {credQ.data.fetch.reason && <p className="kbc-home-card__muted">{credQ.data.fetch.reason}</p>}
          </>
        ) : (
          <p className="kbc-home-card__muted">Not resolved yet — runs on the first fetch or `store sync`.</p>
        )
      ) : null}
    </section>
  );
}
