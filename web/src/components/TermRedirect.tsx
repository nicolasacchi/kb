// HUBDOOR — `/term/:id` is the pre-hub address of a terminal's page. It stays
// only as a redirect to `/terminals/:id`: replace (Back never bounces), and the
// query, hash and navigation state (a just-launched terminal's `launched` hint)
// all survive. The raw id is carried so the page itself shows its not-found card
// for a malformed one.
import type { ReactElement } from "react";
import { Navigate, useLocation, useParams } from "react-router-dom";

export default function TermRedirect(): ReactElement {
  const { id } = useParams();
  const loc = useLocation();
  return (
    <Navigate
      replace
      to={`/terminals/${encodeURIComponent(id ?? "")}${loc.search}${loc.hash}`}
      state={loc.state}
    />
  );
}
