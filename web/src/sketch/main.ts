// The sketch frame's entry — the SECOND Vite build input, and the ONLY
// module in this repo that imports mermaid.
//
// It runs inside `<iframe sandbox="allow-scripts">` with NO
// allow-same-origin, so:
//   * there is no cookie, no localStorage/sessionStorage and no IndexedDB
//     to reach for (touching any of them throws in an opaque origin), and
//     this module deliberately touches none;
//   * `window.parent` is cross-origin, so the only channel is postMessage
//     with `targetOrigin: "*"` — there is no origin string to name from an
//     opaque origin, and the parent authenticates us by window identity
//     (`event.source === iframe.contentWindow`) rather than by origin.
//
// mermaid runs with `securityLevel: "strict"` (HTML labels off, click
// handlers and `%%{init}%%` directives refused) and `htmlLabels: false`, so
// a post body cannot smuggle markup through a node label. `startOnLoad` is
// false — nothing renders until the parent hands us a source.

import mermaid from "mermaid";

const PROTOCOL = "kb-sketch/1";

mermaid.initialize({
  startOnLoad: false,
  securityLevel: "strict",
  htmlLabels: false,
  theme: "dark",
  flowchart: { htmlLabels: false },
  fontFamily: 'ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif',
});

const root = document.getElementById("root")!;
let renderSeq = 0;

function postHeight(): void {
  // scrollHeight, not getBoundingClientRect: the parent caps at 480px and
  // scrolls beyond, so it wants the CONTENT height, not the current box.
  const h = Math.ceil(root.scrollHeight) + 2;
  window.parent.postMessage({ type: PROTOCOL, height: h }, "*");
}

function showError(message: string): void {
  const pre = document.createElement("pre");
  pre.className = "sketch-err";
  // textContent, never innerHTML — the message can quote the source.
  pre.textContent = message;
  root.replaceChildren(pre);
  postHeight();
}

async function render(source: string): Promise<void> {
  const id = `sketch-${++renderSeq}`;
  try {
    const { svg } = await mermaid.render(id, source);
    // mermaid.render returns SVG it produced itself under securityLevel
    // "strict" (DOMPurify-sanitised inside mermaid). Parse it as XML and
    // adopt the ELEMENT rather than assigning innerHTML, so nothing here
    // is a string-to-markup sink even inside the sandbox.
    const doc = new DOMParser().parseFromString(svg, "image/svg+xml");
    const el = doc.documentElement;
    if (!el || el.nodeName === "parsererror") {
      showError("could not parse the rendered diagram");
      return;
    }
    root.replaceChildren(document.importNode(el, true));
    postHeight();
  } catch (e) {
    showError(e instanceof Error ? e.message : String(e));
  }
}

window.addEventListener("message", (e: MessageEvent) => {
  // Only the embedding parent can reach us; still, ignore anything that is
  // not our protocol envelope.
  if (e.source !== window.parent) return;
  const d = e.data as { type?: string; source?: unknown } | null;
  if (!d || d.type !== PROTOCOL || typeof d.source !== "string") return;
  void render(d.source);
});

// Announce readiness LAST, so a source that arrives in the same tick as the
// reply already has its listener attached.
window.parent.postMessage({ type: PROTOCOL, ready: true }, "*");
