// The `kb-term-commands/1` catalogue validator (CMDSCHEMA), the TS twin of
// crates/kb-server/src/terminal/commands.rs. Both read ONE shared golden
// (commands.golden.json) and must agree on every case, code AND path.
//
// Error order is deterministic and identical in both validators: an object's
// known fields are visited in the fixed schema order, then its unknown fields
// in sorted order; the keys of the `harnesses` map are visited sorted; arrays
// by index; depth first. A path is an RFC 6901 JSON pointer. Lengths count
// Unicode code points. React-free and pure: it never throws on any input.

import { KEY_NAMES, type KeyName } from "../phone/keys";

export const CMD_SCHEMA = "kb-term-commands/1";

/** The closed harness set (kb_core::sessions::HARNESSES). */
export const HARNESS_NAMES = ["claude", "codex", "opencode", "grok", "omp"] as const;

export type ItemKind = "command" | "chord" | "reply";
export interface CmdArg {
  name: string;
  hint: string;
  default?: string;
}
export interface CmdItem {
  id: string;
  kind: ItemKind;
  label: string;
  description?: string;
  text?: string;
  mode?: "paste" | "run";
  args?: CmdArg[];
  keys?: KeyName[];
  unverified?: boolean;
}
export interface CmdGroup {
  id: string;
  label: string;
  items: CmdItem[];
}
export interface Catalogue {
  schema: typeof CMD_SCHEMA;
  harnesses: Record<string, { groups: CmdGroup[] }>;
}
export interface CmdError {
  code: string;
  path: string;
}

const MAX_GROUPS = 20;
const MAX_ITEMS = 200;
const MAX_ARGS = 4;
const MAX_KEYS = 8;
const MAX_LABEL = 40;
const MAX_DESCRIPTION = 200;
const MAX_TEXT = 1000;
const MAX_HINT = 60;
const MAX_DEFAULT = 200;

type Obj = Record<string, unknown>;
type Out = CmdError[];

function isObj(v: unknown): v is Obj {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
function has(o: Obj, k: string): boolean {
  return Object.prototype.hasOwnProperty.call(o, k);
}
function get(o: Obj, k: string): unknown {
  return has(o, k) ? o[k] : undefined;
}
function push(out: Out, code: string, path: string): void {
  out.push({ code, path });
}
function seg(base: string, s: string): string {
  return `${base}/${s.replace(/~/g, "~0").replace(/\//g, "~1")}`;
}
function cpLen(s: string): number {
  return [...s].length;
}
/** Sorted by code point (the Rust side sorts by UTF-8 bytes, which orders the same). */
function cpCompare(a: string, b: string): number {
  const x = [...a];
  const y = [...b];
  const n = Math.min(x.length, y.length);
  for (let i = 0; i < n; i++) {
    const d = (x[i].codePointAt(0) as number) - (y[i].codePointAt(0) as number);
    if (d !== 0) return d;
  }
  return x.length - y.length;
}
function unknownFields(o: Obj, known: readonly string[], base: string, out: Out): void {
  const extra = Object.keys(o)
    .filter((k) => !known.includes(k))
    .sort(cpCompare);
  for (const k of extra) push(out, "unknown-field", seg(base, k));
}

const ID_RE = /^[a-z0-9][a-z0-9-]{0,47}$/;
const ARG_NAME_RE = /^[a-z][a-z0-9_]{0,23}$/;
const CONTROL_RE = /\p{Cc}/u;

function checkStr(
  v: unknown,
  path: string,
  required: boolean,
  allowEmpty: boolean,
  max: number,
  out: Out,
): string | undefined {
  if (v === undefined) {
    if (required) push(out, "missing-field", path);
    return undefined;
  }
  if (typeof v !== "string") {
    push(out, "bad-type", path);
    return undefined;
  }
  const n = cpLen(v);
  if (n === 0 && !allowEmpty) push(out, "empty", path);
  else if (n > max) push(out, "too-long", path);
  return v;
}

function checkId(v: unknown, path: string, seen: Set<string>, out: Out): void {
  if (v === undefined) push(out, "missing-field", path);
  else if (typeof v !== "string") push(out, "bad-type", path);
  else if (!ID_RE.test(v)) push(out, "bad-id", path);
  else if (seen.has(v)) push(out, "duplicate-id", path);
  else seen.add(v);
}

/** Validate a parsed catalogue. Never throws; any input yields a (possibly empty) error list. */
export function validateCatalogue(v: unknown): CmdError[] {
  const out: Out = [];
  if (!isObj(v)) {
    push(out, "bad-type", "");
    return out;
  }
  const schema = get(v, "schema");
  if (schema === undefined) push(out, "missing-field", "/schema");
  else if (typeof schema !== "string") push(out, "bad-type", "/schema");
  else if (schema !== CMD_SCHEMA) push(out, "bad-schema", "/schema");
  const hs = get(v, "harnesses");
  if (hs === undefined) push(out, "missing-field", "/harnesses");
  else if (!isObj(hs)) push(out, "bad-type", "/harnesses");
  else harnesses(hs, out);
  unknownFields(v, ["schema", "harnesses"], "", out);
  return out;
}

function harnesses(m: Obj, out: Out): void {
  for (const k of Object.keys(m).sort(cpCompare)) {
    const path = seg("/harnesses", k);
    if (!(HARNESS_NAMES as readonly string[]).includes(k)) {
      push(out, "unknown-harness", path);
      continue;
    }
    harness(m[k], path, out);
  }
}

function harness(v: unknown, path: string, out: Out): void {
  if (!isObj(v)) {
    push(out, "bad-type", path);
    return;
  }
  const gpath = `${path}/groups`;
  const groups = get(v, "groups");
  if (groups === undefined) push(out, "missing-field", gpath);
  else if (!Array.isArray(groups)) push(out, "bad-type", gpath);
  else {
    if (groups.length > MAX_GROUPS) push(out, "too-many", gpath);
    const seenGroups = new Set<string>();
    const seenItems = new Set<string>();
    const total = { n: 0 };
    groups.forEach((g, i) => group(g, `${gpath}/${i}`, seenGroups, seenItems, total, out));
    if (total.n > MAX_ITEMS) push(out, "too-many", gpath);
  }
  unknownFields(v, ["groups"], path, out);
}

function group(
  v: unknown,
  path: string,
  seenGroups: Set<string>,
  seenItems: Set<string>,
  total: { n: number },
  out: Out,
): void {
  if (!isObj(v)) {
    push(out, "bad-type", path);
    return;
  }
  checkId(get(v, "id"), seg(path, "id"), seenGroups, out);
  checkStr(get(v, "label"), seg(path, "label"), true, false, MAX_LABEL, out);
  const ipath = seg(path, "items");
  const items = get(v, "items");
  if (items === undefined) push(out, "missing-field", ipath);
  else if (!Array.isArray(items)) push(out, "bad-type", ipath);
  else {
    total.n += items.length;
    items.forEach((it, i) => item(it, `${ipath}/${i}`, seenItems, out));
  }
  unknownFields(v, ["id", "label", "items"], path, out);
}

/** Scan `text` for `{name}` placeholders (`{{`/`}}` are literal braces). */
function scanPlaceholders(text: string, declared: Set<string>): { errs: string[]; used: Set<string> } {
  const chars = [...text];
  const n = chars.length;
  const errs: string[] = [];
  const used = new Set<string>();
  let i = 0;
  while (i < n) {
    const c = chars[i];
    if (c === "{") {
      if (chars[i + 1] === "{") {
        i += 2;
        continue;
      }
      let end = -1;
      for (let j = i + 1; j < n; j++) {
        if (chars[j] === "}") {
          end = j;
          break;
        }
      }
      if (end < 0) {
        errs.push("bad-placeholder");
        break;
      }
      const name = chars.slice(i + 1, end).join("");
      if (ARG_NAME_RE.test(name)) {
        if (!declared.has(name)) errs.push("undeclared-arg");
        used.add(name);
      } else {
        errs.push("bad-placeholder");
      }
      i = end + 1;
    } else if (c === "}") {
      if (chars[i + 1] === "}") i += 2;
      else {
        errs.push("bad-placeholder");
        i += 1;
      }
    } else i += 1;
  }
  return { errs, used };
}

function item(v: unknown, path: string, seenItems: Set<string>, out: Out): void {
  if (!isObj(v)) {
    push(out, "bad-type", path);
    return;
  }
  const kindV = get(v, "kind");
  const kind: ItemKind | undefined =
    kindV === "command" || kindV === "chord" || kindV === "reply" ? kindV : undefined;
  const isChord = kind === "chord";
  const needsText = kind === "command" || kind === "reply";
  const runMode = get(v, "mode") === "run";

  const declared = new Set<string>();
  const argsV = get(v, "args");
  if (Array.isArray(argsV)) {
    for (const a of argsV) {
      const n = isObj(a) ? get(a, "name") : undefined;
      if (typeof n === "string" && ARG_NAME_RE.test(n)) declared.add(n);
    }
  }
  const textV = get(v, "text");
  let scanErrs: string[] = [];
  let used = new Set<string>();
  if (typeof textV === "string" && !isChord) {
    const r = scanPlaceholders(textV, declared);
    scanErrs = r.errs;
    used = r.used;
  }

  checkId(get(v, "id"), seg(path, "id"), seenItems, out);

  const kpath = seg(path, "kind");
  if (kindV === undefined) push(out, "missing-field", kpath);
  else if (typeof kindV !== "string") push(out, "bad-type", kpath);
  else if (kind === undefined) push(out, "kind-shape", kpath);

  checkStr(get(v, "label"), seg(path, "label"), true, false, MAX_LABEL, out);
  checkStr(get(v, "description"), seg(path, "description"), false, true, MAX_DESCRIPTION, out);

  // text
  const tpath = seg(path, "text");
  if (textV === undefined) {
    if (needsText) push(out, "kind-shape", tpath);
  } else if (isChord) {
    push(out, "kind-shape", tpath);
  } else {
    const t = checkStr(textV, tpath, false, false, MAX_TEXT, out);
    if (t !== undefined) {
      if (runMode) {
        if (t.includes("\n") || t.includes("\r")) push(out, "run-multiline", tpath);
        else if (CONTROL_RE.test(t)) push(out, "control-char", tpath);
      } else if ([...t].some((c) => CONTROL_RE.test(c) && c !== "\n" && c !== "\t")) {
        push(out, "control-char", tpath);
      }
      for (const code of scanErrs) push(out, code, tpath);
    }
  }

  // mode
  const mpath = seg(path, "mode");
  const modeV = get(v, "mode");
  if (modeV !== undefined) {
    if (isChord) push(out, "kind-shape", mpath);
    else if (typeof modeV !== "string") push(out, "bad-type", mpath);
    else if (modeV !== "paste" && modeV !== "run") push(out, "bad-mode", mpath);
  }

  // args
  const apath = seg(path, "args");
  if (argsV !== undefined) {
    if (isChord) push(out, "kind-shape", apath);
    else if (!Array.isArray(argsV)) push(out, "bad-type", apath);
    else {
      if (argsV.length > MAX_ARGS) push(out, "too-many", apath);
      const seenNames = new Set<string>();
      argsV.forEach((a, i) => arg(a, `${apath}/${i}`, seenNames, used, out));
    }
  }

  // keys
  const keyPath = seg(path, "keys");
  const keysV = get(v, "keys");
  if (keysV === undefined) {
    if (isChord) push(out, "kind-shape", keyPath);
  } else if (needsText) {
    push(out, "kind-shape", keyPath);
  } else if (!Array.isArray(keysV)) {
    push(out, "bad-type", keyPath);
  } else {
    if (keysV.length === 0) push(out, "empty", keyPath);
    else if (keysV.length > MAX_KEYS) push(out, "too-many", keyPath);
    keysV.forEach((k, i) => {
      if (typeof k !== "string") push(out, "bad-type", `${keyPath}/${i}`);
      else if (!(KEY_NAMES as readonly string[]).includes(k)) push(out, "bad-key", `${keyPath}/${i}`);
    });
  }

  const uv = get(v, "unverified");
  if (uv !== undefined && typeof uv !== "boolean") push(out, "bad-type", seg(path, "unverified"));

  unknownFields(
    v,
    ["id", "kind", "label", "description", "text", "mode", "args", "keys", "unverified"],
    path,
    out,
  );
}

function arg(v: unknown, path: string, seenNames: Set<string>, used: Set<string>, out: Out): void {
  if (!isObj(v)) {
    push(out, "bad-type", path);
    return;
  }
  const npath = seg(path, "name");
  const nameV = get(v, "name");
  let validName: string | undefined;
  if (nameV === undefined) push(out, "missing-field", npath);
  else if (typeof nameV !== "string") push(out, "bad-type", npath);
  else if (!ARG_NAME_RE.test(nameV)) push(out, "bad-arg", npath);
  else {
    validName = nameV;
    if (seenNames.has(nameV)) push(out, "bad-arg", npath);
    else seenNames.add(nameV);
  }
  checkStr(get(v, "hint"), seg(path, "hint"), true, false, MAX_HINT, out);
  checkStr(get(v, "default"), seg(path, "default"), false, true, MAX_DEFAULT, out);
  unknownFields(v, ["name", "hint", "default"], path, out);
  if (validName !== undefined && !used.has(validName)) push(out, "unused-arg", path);
}
