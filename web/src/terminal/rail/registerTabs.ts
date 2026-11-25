// CONVRAIL — the ONE registration site for rail tabs. A side-effect module:
// it imports only the registry and the tab components, never `@xterm/*` and
// never `TerminalPanel`, so the shell can import it without pulling xterm
// into the main chunk (see no-static-xterm.test.ts). Later rail units append
// their own `registerRailTab` line here.

import { createElement } from "react";
import { registerRailTab } from "./registry";
import { ConversationRail } from "./ConversationRail";

registerRailTab({
  id: "conversation",
  label: "Conversation",
  badge: undefined,
  node: createElement(ConversationRail),
});

import { CommandsRail } from "../commands/palette/CommandsRail";

registerRailTab({
  id: "commands",
  label: "Commands",
  node: createElement(CommandsRail),
});
