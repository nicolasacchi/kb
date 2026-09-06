import React from "react";
import ReactDOM from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { QueryClientProvider } from "@tanstack/react-query";
import App from "./app";
import { LiveTailPortalProvider } from "./context/LiveTailPortalContext";
import { applyPrefs, loadPrefs, watchSystemTheme } from "./api/prefs";
import { queryClient, startSseInvalidationBridge } from "./api/queryClient";
import "./styles/tokens.css";
import "./styles/app.css";
import "./styles/chrome.css";
import "./styles/gallery.css";
import "./styles/search.css";
import "./styles/history.css";
import "./styles/sessions.css";
import "./styles/versions.css";
import "./styles/notes.css";
import "./styles/lists.css";
import "./styles/boards.css";
import "./styles/attachments.css";
import "./styles/score.css";
import "./styles/resurface.css";
// SL4 — the slate board. BEFORE mobile.css, like every other view sheet, so
// the ≤860px overrides there (and the .kb-pinsp sheet pattern this view
// reuses for its composer + history drawer) still win the cascade.
import "./styles/slates.css";
// Mobile shell overrides — imported LAST so its ≤860px / pointer:coarse
// rules win the cascade over the desktop-first chrome above.
import "./styles/mobile.css";
// editor.css (CodeMirror) moved into the lazy MarkdownEditor module (P2) so it
// loads on demand with the CM chunk instead of in the critical CSS.

// X1 — own scroll restoration ourselves (useScrollRestoration). Switch the
// browser's native restoration off so it doesn't race our rAF restore on
// back/forward + reload (both target the same offset, but the browser can fire
// before the virtualizer lays out, causing a one-frame jump).
if ("scrollRestoration" in history) history.scrollRestoration = "manual";

// Apply saved prefs before first paint to avoid a theme flash on reload.
applyPrefs(loadPrefs());
// When theme=system, follow OS light/dark flips live (swaps the inline accent).
watchSystemTheme();

// One SSE → query-invalidation bridge for the app's lifetime (the
// subscriptions wire onto every daemon source, current and future).
startSseInvalidationBridge();

const root = document.getElementById("root");
if (!root) throw new Error("#root missing");

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <LiveTailPortalProvider>
      <BrowserRouter>
        <QueryClientProvider client={queryClient}>
          <App />
        </QueryClientProvider>
      </BrowserRouter>
    </LiveTailPortalProvider>
  </React.StrictMode>,
);
