// U4/U5 (v0.25 quick capture) — parses the Web Share Target redirect's
// `?captured=<kb>:<source_relative>` query value (`routes/capture.rs`'s
// `share_target` handler / `percent_encode_component`). The browser already
// decodes the percent-escapes for us via `URLSearchParams.get`, so this
// just splits on the FIRST `:` — kb names are `[a-z0-9_-]+` (`types::KbName`)
// and so never contain one, while `source_relative` may contain further
// `/` (and, in principle, `:`) on the right-hand side.

export type CapturedRef = {
  kb: string;
  sourceRelative: string;
};

export function parseCapturedParam(value: string | null): CapturedRef | null {
  if (!value) return null;
  const i = value.indexOf(":");
  if (i <= 0 || i === value.length - 1) return null;
  return { kb: value.slice(0, i), sourceRelative: value.slice(i + 1) };
}
