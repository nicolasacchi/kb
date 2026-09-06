// V71-D2 — the scope chip, in the DEGRADED form the tree actually supports.
//
// Design P3 gives scoped search its own protocol, `kbc-scope/1`. **That
// protocol does not exist in this tree** (it lands with the V71-F1 tree
// unit; `commands/registry.json`'s own `scope.edit`/`scope.clear` rows still
// say `cli: "none:kbc-scope/1 lands in the P3 unit"`). So this unit ships
// the chip against what IS here — `GET /api/scopes`, the `[scopes]` config's
// named path-glob sets — and refuses, out loud, for every scope it cannot
// express.
//
// The refusal is the point. A scope is a set of globs joined by OR; kbcq/1's
// `path:` is ONE case-insensitive substring and is not multi-valued. So a
// scope maps cleanly in exactly two shapes:
//
//   - every glob is extension-shaped (`*.rb`, `**/*.erb`) => ONE `ext:a|b`
//     clause (`ext:` IS multi-valued, and alternation is an OR — the same
//     OR the scope means);
//   - there is exactly ONE distinct prefix (`app/**`, `spec/models/**`)
//     => ONE `path:app/` clause.
//
// Anything else — two different prefixes, a mixed set, a `**/*.test.*`
// middle wildcard — is REFUSED with a reason the chip renders. Applying
// "most of" a scope would return rows the scope excludes, and a scope
// filter that quietly over-matches is worse than a disabled chip: it looks
// like it worked. (Same posture as the trust vocabulary: an uncertain
// answer is an honest refusal, never a guess.)

export type ScopeClause = { ok: true; clause: string } | { ok: false; reason: string };

/// `app/**` -> `app/`; `**/dir/**` -> `dir/`; a literal directory -> itself.
/// `null` when the glob is not prefix-shaped.
function prefixOf(glob: string): string | null {
  const g = glob.trim();
  if (g === "") return null;
  if (g.endsWith("/**")) {
    let body = g.slice(0, -3);
    if (body.startsWith("**/")) body = body.slice(3);
    return body.includes("*") || body === "" ? null : `${body}/`;
  }
  if (!g.includes("*")) return g.endsWith("/") ? g : `${g}/`;
  return null;
}

/// `*.rb` / `**/*.rb` -> `rb`. `null` for anything else, including
/// `**/*.test.*` (a middle wildcard is not an extension) and `app/*.rb`
/// (which also constrains the directory and cannot ride `ext:` alone).
function extOf(glob: string): string | null {
  const g = glob.trim();
  const cut = g.lastIndexOf("/");
  const base = g.slice(cut + 1);
  const head = g.slice(0, cut + 1);
  if (head !== "" && head !== "**/") return null;
  const m = /^\*\.([A-Za-z0-9_]+)$/.exec(base);
  return m ? m[1].toLowerCase() : null;
}

/// The kbcq/1 clause that scopes a search to `globs`, or an honest refusal.
export function scopeToClause(name: string, globs: string[]): ScopeClause {
  const clean = globs.map((g) => g.trim()).filter((g) => g !== "");
  if (clean.length === 0) {
    return { ok: false, reason: `scope "${name}" has no patterns` };
  }
  const exts = clean.map(extOf);
  if (exts.every((e) => e !== null)) {
    const uniq = [...new Set(exts as string[])].sort();
    return { ok: true, clause: `ext:${uniq.join("|")}` };
  }
  const prefixes = clean.map(prefixOf).filter((p): p is string => p !== null);
  if (prefixes.length === clean.length) {
    const uniq = new Set(prefixes);
    if (uniq.size === 1) return { ok: true, clause: `path:${prefixes[0]}` };
    return {
      ok: false,
      reason: `scope "${name}" is ${uniq.size} directories and kbcq/1's \`path:\` is one substring — needs kbc-scope/1 (P3)`,
    };
  }
  return {
    ok: false,
    reason: `scope "${name}" mixes pattern shapes kbcq/1 cannot express as one clause — needs kbc-scope/1 (P3)`,
  };
}
