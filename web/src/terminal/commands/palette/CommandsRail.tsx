// CMDPALETTE: the rail's `commands` tab body: the same list as the palette,
// inline (no dialog), reading the terminal it sits beside from the rail
// context. --touch-min rows (see palette.css) so it is usable in the phone sheet.

import type { ReactElement } from "react";
import { usePhonePanel } from "../../../hooks/useIsMobile";
import { useTermRail } from "../../rail/context";
import { CommandList } from "./CommandList";
import { focusTerminalPane } from "./sendEntry";

export function CommandsRail(): ReactElement {
  const { termId, harness } = useTermRail();
  const phone = usePhonePanel();
  if (termId === "") return <p className="kb-cmdp__empty">No terminal</p>;
  return (
    <CommandList
      termId={termId}
      harness={harness}
      variant="rail"
      onSent={() => {
        if (!phone) focusTerminalPane();
      }}
    />
  );
}
