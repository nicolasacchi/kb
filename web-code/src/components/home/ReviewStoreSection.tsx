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
// client-side beyond `formatBytes` and the doctor-level → CSS-class map.
import { useReviewCredentials, useReviewStoreCard } from "../../hooks/useReviewStore";
import { formatBytes } from "../../lib/format";
import MetaLine from "../MetaLine";

export function ReviewStoreSection({ repo }: { repo: string }) {
  const storeQ = useReviewStoreCard(repo);

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
              VERBATIM, never re-derived; this IS the store's maintenance/GC/objects-missing summary
              when one exists (kb-code-server's own U9 maintenance unit hasn't landed yet, so today
              this is mostly registration/credential/forge-verification notes). */}
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
