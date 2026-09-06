import { useEffect, useRef, useState } from "react";

// The sandboxed drawing frame (design §10 "Drawings").
//
// SECURITY, and the reason this is an iframe at all: `sandbox="allow-scripts"`
// WITHOUT `allow-same-origin` gives the frame an OPAQUE origin — no cookies,
// no storage, no access to the parent DOM, and no way back into the SPA
// except a postMessage the parent validates. Mermaid is a large renderer
// that parses attacker-shaped text (a post body written by another session);
// running it inside the SPA document would mean adding a raw-SVG sink to the
// page that deliberately has none (CommentBody's "no rehype-raw" posture).
// So mermaid lives in its OWN Vite entry (`web/sketch.html` +
// `src/sketch/main.ts`) and the SPA never imports it.
//
// The handshake, exactly as §9's last lines pin it:
//   frame → parent  { type: "kb-sketch/1", ready: true }
//   parent → frame  { type: "kb-sketch/1", source }        (targetOrigin "*")
//   frame → parent  { type: "kb-sketch/1", height }
// `targetOrigin` MUST be "*": the frame's origin is opaque, so there is no
// literal origin string to name. That is safe here precisely because the
// frame is sandboxed — the only thing we send it is text it was going to
// render anyway. In the other direction we CANNOT check `event.origin` (it
// is "null" for every opaque frame and for a hostile one alike), so the
// parent authenticates by OBJECT IDENTITY instead:
// `event.source === iframe.contentWindow` — a window reference no other
// document can forge.

const PROTOCOL = "kb-sketch/1";
/// Height cap (§10): taller diagrams scroll inside the frame.
export const SKETCH_MAX_HEIGHT = 480;
const MIN_HEIGHT = 48;

export default function Sketch({
  source,
  seq,
}: {
  source: string;
  /// The post's sequence number — the frame's accessible title.
  seq?: number;
}) {
  const ref = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(MIN_HEIGHT);
  const sourceRef = useRef(source);
  sourceRef.current = source;

  useEffect(() => {
    function onMessage(e: MessageEvent) {
      // Object-identity check — see the module comment. An opaque origin
      // reports `event.origin === "null"`, which is unforgeable-adjacent at
      // best; the window reference is the real gate.
      const frame = ref.current;
      if (!frame || e.source !== frame.contentWindow) return;
      const d = e.data as { type?: string; ready?: boolean; height?: number } | null;
      if (!d || d.type !== PROTOCOL) return;
      if (d.ready) {
        frame.contentWindow?.postMessage(
          { type: PROTOCOL, source: sourceRef.current },
          "*",
        );
        return;
      }
      if (typeof d.height === "number" && Number.isFinite(d.height)) {
        setHeight(
          Math.max(MIN_HEIGHT, Math.min(SKETCH_MAX_HEIGHT, Math.ceil(d.height))),
        );
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  // A source change after `ready` re-sends without remounting the frame.
  useEffect(() => {
    ref.current?.contentWindow?.postMessage({ type: PROTOCOL, source }, "*");
  }, [source]);

  return (
    <div className="slate-sketch">
      <iframe
        ref={ref}
        className="slate-sketch__frame"
        // EXACTLY "allow-scripts" — asserted by the Playwright spec. Adding
        // allow-same-origin here would hand the frame the SPA's origin and
        // undo every guarantee in the module comment.
        sandbox="allow-scripts"
        src="/sketch.html"
        title={seq === undefined ? "sketch" : `sketch #${seq}`}
        style={{ height }}
        loading="lazy"
      />
    </div>
  );
}
