// RAILCHANGES — the session page's own ChecksSection behind a React.lazy
// boundary, so the verdict phrase, per-check lines and captions are the page's
// byte for byte and the routes module stays out of the terminal chunk.

import { lazy, Suspense, type ReactElement } from "react";
import type { SessionDetail } from "../../../api/generated/SessionDetail";

const Checks = lazy(async () => {
  const m = await import("../../../routes/sessions");
  return {
    default: function ChecksBridge({ detail }: { detail: SessionDetail }): ReactElement {
      return <m.ChecksSection row={detail} detail={detail} />;
    },
  };
});

export function ChecksLazy({ detail }: { detail: SessionDetail }): ReactElement {
  return (
    <Suspense fallback={<p className="kb-chgrail__hint" data-testid="changes-checks-loading">Loading checks</p>}>
      <Checks detail={detail} />
    </Suspense>
  );
}
