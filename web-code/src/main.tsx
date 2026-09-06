import React from "react";
import ReactDOM from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { QueryClientProvider } from "@tanstack/react-query";
import App from "./app";
import { applyTheme, loadTheme } from "./lib/prefs";
import { queryClient, startSseInvalidationBridge } from "./api/queryClient";
import "./styles/tokens.css";
// V70-A7 — the generated `[data-kbc-theme]` blocks. Imported here, right
// after tokens.css, so the palette layer is complete before the first
// component stylesheet; the blocks also carry a higher-specificity
// selector (`html:root[…]`) so route-lazy CSS cannot out-cascade them.
import "./themes/themes.gen.css";
import "./styles/reader.css";
import "./styles/topbar.css";
import "./styles/diff.css";
import "./styles/search.css";
import "./styles/provenance.css";
import "./styles/history.css";
import "./styles/story.css";
import "./styles/home.css";
import "./styles/sets.css";
// V70-A4 — the Desk shell. After the route stylesheets (it overrides the
// pre-Desk fixed-width `.kbc-reader__tree`/`.kbc-reader__outline` columns),
// before `mobile.css` (which must still win at ≤860px).
import "./styles/desk.css";
// V70-A5 — cmd/1's chrome (which-key, the rehearsal overlay, the palette's
// command rows, the `?` sheet's preset select, day-one teaching bits). After
// desk.css so the two overlays sit above the shell it defines.
import "./styles/commands.css";
// V70-A6 — the navigation layer's chrome (the scent card, the pane history
// arrows, the trail chip, the inline peek widget, the hover tooltip). After
// commands.css so its overlays sit above the shell those two define.
import "./styles/nav.css";
// V71-E2 — the Usages dock (inside the drawer) and the ONE action menu.
// After nav.css so the menu's top-layer overlay sits above the navigation
// chrome, before mobile.css so the ≤860px sheet promotion still wins.
import "./styles/usages.css";
// V72-G1.2 — the dossier center mode + its rail jump list. Before mobile.css
// for the same reason every other surface is: its own ≤860px block must stay
// overridable by the shared mobile-shell rules.
import "./styles/dossier.css";
// F5 — media-queried mobile-shell overrides; imported LAST so they win over
// every desktop rule above (mirrors kb's own `main.tsx` import order).
import "./styles/mobile.css";

// Apply saved theme before first paint to avoid a theme flash on reload.
// V70-A7 keeps this the ONLY pre-paint hook — deliberately NOT an inline
// <script> in index.html, because a sibling unit lands a strict CSP with
// `script-src 'self'` and an inline theme shim would have to be nonced.
applyTheme(loadTheme());

// V70-A6 — root CLAUDE.md #31's floor. The browser's own scroll restoration
// lands "unpredictably and often at the wrong times" for an async-rendered
// SPA (WICG navigation-api#187), and it FIGHTS `hooks/useScrollRestoration.ts`
// — which restores per history ENTRY where the Navigation API exists, and per
// normalised URL where it does not. One line, and it has to be here rather
// than in a route: by the time a lazy route mounts, the browser has already
// tried.
if (typeof history !== "undefined" && "scrollRestoration" in history) {
  history.scrollRestoration = "manual";
}

startSseInvalidationBridge();

const root = document.getElementById("root");
if (!root) throw new Error("#root missing");

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <BrowserRouter>
      <QueryClientProvider client={queryClient}>
        <App />
      </QueryClientProvider>
    </BrowserRouter>
  </React.StrictMode>,
);
