// OL-SIBL1 - a URL that arrives over the wire (a kb link-out) is rendered as
// an `href` only when it is plainly http(s). `javascript:`, `data:`, relative
// and empty values all collapse to `null`, so a caller falls back to a
// non-link rather than emitting a navigable URL it did not vet.
export function safeHref(url: string | null | undefined): string | null {
  if (typeof url !== "string" || url.trim() === "") return null;
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return null; // relative or malformed
  }
  return parsed.protocol === "http:" || parsed.protocol === "https:" ? url : null;
}
