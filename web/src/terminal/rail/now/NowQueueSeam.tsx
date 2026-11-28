// RAILNOW — the empty seam PQUI fills with the saved prompt queue. Renders
// nothing visible and holds no store, no query and no state: the queue lives in
// its own unit, this file only reserves the named place in the Now column.

import type { ReactElement } from "react";

export function NowQueueSeam(): ReactElement {
  return <section data-testid="now-queue" data-seam="pqui" hidden aria-label="Prompt queue" />;
}
