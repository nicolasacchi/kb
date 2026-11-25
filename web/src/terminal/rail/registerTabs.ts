// CONVRAIL — the ONE registration site for rail tabs. A side-effect module:
// it imports only the registry and the tab components, never `@xterm/*` and
// never `TerminalPanel`, so the shell can import it without pulling xterm
// into the main chunk (see no-static-xterm.test.ts). Later rail units append
// their own `registerRailTab` line here.

import { createElement } from "react";
import { registerRailTab } from "./registry";
import { ConversationRail } from "./ConversationRail";
import { ChangesRail } from "./changes/ChangesRail";
import { KnowledgeRail } from "./knowledge/KnowledgeRail";

registerRailTab({
  id: "conversation",
  label: "Conversation",
  badge: undefined,
  node: createElement(ConversationRail),
});

registerRailTab({
  id: "changes",
  label: "Changes",
  badge: undefined,
  node: createElement(ChangesRail),
});

registerRailTab({
  id: "knowledge",
  label: "Knowledge",
  badge: undefined,
  node: createElement(KnowledgeRail),
});
