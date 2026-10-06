//! `kb sessions segment-plan` (v0.46 SEG-B) - the deterministic, LLM-free
//! planner for segmented harness capture.
//!
//! A long omp session is captured today by one `jq -s` slurp of the whole
//! JSONL, which is O(session) in CPU and RSS. Segmented capture instead cuts
//! the session's LEAF CHAIN into an ordered list of parts, each small enough
//! to translate on its own. This verb is the planner: it reads the source
//! once (streaming, one line at a time, never the whole file), keeps a compact
//! per-line index `{offset, len, kind, id, parentId, aux}` (about 64 bytes per
//! entry), resolves the leaf chain, and cuts it.
//!
//! The chain rules mirror the adapter's TRANSLATE jq program
//! (`plugins/kb-memory/hooks/kb-capture-omp.sh`) exactly, and a test runs that
//! very program (sliced out of the shipped script) against every fixture:
//!
//! * non-title entries only; the walk starts at the LAST such entry and goes
//!   backward over file order; the FIRST backward match of the cursor wins; a
//!   null cursor stops matching; the cursor then becomes that entry's parent;
//! * the LAST `reset_boundary` on the chain cuts everything at or before it.
//!
//! Cuts are planned over the live chain, in raw bytes of chain lines, with a
//! target of 16 MiB (`--target-bytes` overrides, for tests):
//!
//! 1. a cut is LEGAL only before a user message with text, with no tool call
//!    pending (every `toolCall` id seen so far has its `toolResult`);
//! 2. among legal cuts leaving a part of `[target/2, target]` bytes, the LAST
//!    one that follows a compaction wins, else the last legal one;
//! 3. else the first legal cut leaving `(target, 2*target]` bytes;
//! 4. else, once the part exceeds `2*target`, a HARD cut at the first entry
//!    boundary reaching `target`.
//!
//! A cut is only DECIDED once more entries than the rule can look at exist, so
//! a decided (frozen) part never changes when the file grows by appending to
//! the leaf; the open remainder is the live tail. The checkpoint (`--state`)
//! stores the index plus the frozen parts, is validated on every run (size,
//! checksum of the index body, sha of the 4 KiB window before `indexed_bytes`,
//! first-line hash, adapter version) and is rebuilt by streaming the source
//! again when any check fails. Frozen parts are then compared with the freshly
//! computed plan: the first mismatch is reported as a divergence and the stored
//! parts beyond the new part count as orphans (for the adapter to delete
//! through kb's own delete path).
//!
//! Output: one JSON document (`segment-plan/1`, see `docs/cli.md`), or with
//! `--emit <idx>` the raw source lines of one part (header + title slot +
//! the part's chain lines) for the adapter to pipe into TRANSLATE.
//!
//! The planner is harness-agnostic where free, but the leaf-chain rules are
//! omp's (`id`/`parentId` tree, `reset_boundary`, `compaction`).

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const PLAN_SCHEMA: &str = "segment-plan/1";
const STATE_MARK: &str = "segment-plan-state";
const STATE_VERSION: u32 = 1;
/// The operator-ruled raw target per part (16 MiB).
pub const DEFAULT_TARGET_BYTES: u64 = 16 * 1024 * 1024;
const WINDOW: u64 = 4096;
const HEADER_LINE_CAP: u64 = 1 << 20;

/// What a source line is, as far as planning cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Session,
    Title,
    /// A user message with non-empty text: the only legal cut point.
    User,
    /// An assistant message; `aux` is a JSON array of its `toolCall` ids.
    Assistant,
    /// A `toolResult` message; `aux` is the JSON string of its `toolCallId`.
    ToolResult,
    Compaction,
    Reset,
    /// A `model_change`; `aux` is the JSON text of its `model`.
    Model,
    Other,
}

impl Kind {
    fn code(self) -> &'static str {
        match self {
            Kind::Session => "S",
            Kind::Title => "T",
            Kind::User => "U",
            Kind::Assistant => "A",
            Kind::ToolResult => "R",
            Kind::Compaction => "C",
            Kind::Reset => "B",
            Kind::Model => "M",
            Kind::Other => "O",
        }
    }

    fn from_code(s: &str) -> Option<Kind> {
        Some(match s {
            "S" => Kind::Session,
            "T" => Kind::Title,
            "U" => Kind::User,
            "A" => Kind::Assistant,
            "R" => Kind::ToolResult,
            "C" => Kind::Compaction,
            "B" => Kind::Reset,
            "M" => Kind::Model,
            "O" => Kind::Other,
            _ => return None,
        })
    }
}

/// One indexed source line (a line that parsed as a JSON object).
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub offset: u64,
    /// Byte length including the trailing newline (when there is one).
    pub len: u64,
    pub kind: Kind,
    /// JSON text of `.id` (None when null/absent/false, like jq's `// null`).
    pub id: Option<String>,
    pub parent: Option<String>,
    pub aux: String,
}

type Classified = (Kind, Option<String>, Option<String>, String);

fn canon_id(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => None,
        Some(x) => Some(x.to_string()),
    }
}

fn text_nonempty(content: &Value) -> bool {
    match content.as_array() {
        None => false,
        Some(parts) => parts.iter().any(|p| {
            p.as_object().is_some_and(|o| {
                o.get("type").and_then(Value::as_str) == Some("text")
                    && match o.get("text") {
                        None | Some(Value::Null) => false,
                        Some(Value::String(s)) => !s.is_empty(),
                        Some(_) => true,
                    }
            })
        }),
    }
}

/// Classify one parsed line. `None` = not a JSON object (jq would error or
/// drop it; the planner skips it).
fn classify(v: &Value) -> Option<Classified> {
    let obj = v.as_object()?;
    let id = canon_id(obj.get("id"));
    let parent = canon_id(obj.get("parentId"));
    let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");
    let mut aux = String::new();
    let kind = match ty {
        "session" => Kind::Session,
        "title" => Kind::Title,
        "reset_boundary" => Kind::Reset,
        "compaction" => Kind::Compaction,
        "model_change" => {
            aux = obj.get("model").cloned().unwrap_or(Value::Null).to_string();
            Kind::Model
        }
        "message" => {
            let empty = serde_json::Map::new();
            let m = obj
                .get("message")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            match m.get("role").and_then(Value::as_str) {
                Some("user") => {
                    if m.get("content").is_some_and(text_nonempty) {
                        Kind::User
                    } else {
                        Kind::Other
                    }
                }
                Some("assistant") => {
                    let mut calls: Vec<String> = Vec::new();
                    if let Some(parts) = m.get("content").and_then(Value::as_array) {
                        for p in parts {
                            let Some(o) = p.as_object() else { continue };
                            if o.get("type").and_then(Value::as_str) != Some("toolCall") {
                                continue;
                            }
                            calls.push(match o.get("id") {
                                Some(Value::String(s)) => s.clone(),
                                None | Some(Value::Null) => String::new(),
                                Some(x) => x.to_string(),
                            });
                        }
                    }
                    if !calls.is_empty() {
                        aux = serde_json::to_string(&calls).unwrap_or_default();
                    }
                    Kind::Assistant
                }
                Some("toolResult") => {
                    let id = match m.get("toolCallId") {
                        Some(Value::String(s)) => s.clone(),
                        None | Some(Value::Null) => String::new(),
                        Some(x) => x.to_string(),
                    };
                    aux = Value::String(id).to_string();
                    Kind::ToolResult
                }
                _ => Kind::Other,
            }
        }
        _ => Kind::Other,
    };
    Some((kind, id, parent, aux))
}

fn parse_line(buf: &[u8]) -> Option<Classified> {
    let v: Value = match std::str::from_utf8(buf) {
        Ok(s) => serde_json::from_str(s).ok()?,
        Err(_) => serde_json::from_str(&String::from_utf8_lossy(buf)).ok()?,
    };
    classify(&v)
}

/// Stream `[from, size)` of the source, appending one entry per complete JSON
/// object line. Returns `(end of the last complete line, volatile)` where
/// `volatile` is an unterminated final fragment that parses (jq would read it;
/// it is planned over but never persisted, since its line may still grow).
fn scan(
    f: &mut File,
    from: u64,
    size: u64,
    entries: &mut Vec<Entry>,
) -> Result<(u64, Option<Entry>)> {
    if from >= size {
        return Ok((from, None));
    }
    f.seek(SeekFrom::Start(from))?;
    let mut r = BufReader::with_capacity(1 << 20, Read::take(&*f, size - from));
    let mut buf: Vec<u8> = Vec::new();
    let mut pos = from;
    let mut end = from;
    let mut volatile = None;
    loop {
        buf.clear();
        let n = r.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        let complete = buf.last() == Some(&b'\n');
        if let Some((kind, id, parent, aux)) = parse_line(&buf) {
            let e = Entry {
                offset: pos,
                len: n as u64,
                kind,
                id,
                parent,
                aux,
            };
            if complete {
                entries.push(e);
            } else {
                volatile = Some(e);
            }
        }
        pos += n as u64;
        if complete {
            end = pos;
        }
    }
    Ok((end, volatile))
}

fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_range(f: &mut File, start: u64, len: u64) -> Result<Vec<u8>> {
    f.seek(SeekFrom::Start(start))?;
    let mut v = vec![0u8; len as usize];
    f.read_exact(&mut v)?;
    Ok(v)
}

fn window_sha(f: &mut File, indexed_bytes: u64) -> Result<String> {
    let start = indexed_bytes.saturating_sub(WINDOW);
    Ok(sha_hex(&read_range(f, start, indexed_bytes - start)?))
}

fn first_line_sha(f: &mut File) -> Result<String> {
    f.seek(SeekFrom::Start(0))?;
    let mut r = BufReader::new(Read::take(&*f, HEADER_LINE_CAP));
    let mut buf = Vec::new();
    r.read_until(b'\n', &mut buf)?;
    Ok(sha_hex(&buf))
}

/// The live leaf chain, as indices into `entries` in ascending file order.
/// Exactly the TRANSLATE walk (see the module doc).
pub fn live_chain(entries: &[Entry]) -> Vec<usize> {
    let es: Vec<usize> = (0..entries.len())
        .filter(|&i| entries[i].kind != Kind::Title)
        .collect();
    let Some(&last) = es.last() else {
        return Vec::new();
    };
    let mut cur: Option<&str> = entries[last].id.as_deref();
    let mut chain: Vec<usize> = Vec::new();
    for &i in es.iter().rev() {
        let e = &entries[i];
        if cur.is_some() && e.id.as_deref() == cur {
            chain.push(i);
            cur = e.parent.as_deref();
        }
    }
    chain.reverse();
    let rb = chain
        .iter()
        .rposition(|&i| entries[i].kind == Kind::Reset)
        .map_or(0, |p| p + 1);
    chain.split_off(rb)
}

#[derive(Clone, Debug)]
struct Cut {
    start: usize,
    end: usize,
    frozen: bool,
    kind: &'static str,
}

fn plan_cuts(entries: &[Entry], live: &[usize], target: u64) -> Vec<Cut> {
    let n = live.len();
    if n == 0 {
        return Vec::new();
    }
    let t = target.max(1);
    let mut cum: Vec<u64> = Vec::with_capacity(n + 1);
    cum.push(0);
    for &i in live {
        let last = cum[cum.len() - 1];
        cum.push(last + entries[i].len);
    }
    let mut legal = vec![false; n];
    let mut after_comp = vec![false; n];
    let mut pending: HashSet<String> = HashSet::new();
    let mut seen_comp = false;
    for (p, &i) in live.iter().enumerate() {
        let e = &entries[i];
        if e.kind == Kind::User && pending.is_empty() {
            legal[p] = true;
            after_comp[p] = seen_comp;
            seen_comp = false;
        }
        match e.kind {
            Kind::Compaction => seen_comp = true,
            Kind::Assistant => {
                if !e.aux.is_empty() {
                    if let Ok(calls) = serde_json::from_str::<Vec<String>>(&e.aux) {
                        for c in calls {
                            pending.insert(c);
                        }
                    }
                }
            }
            Kind::ToolResult => {
                if let Ok(id) = serde_json::from_str::<String>(&e.aux) {
                    pending.remove(&id);
                }
            }
            _ => {}
        }
    }
    let size = |s: usize, b: usize| cum[b] - cum[s];
    let t2 = t.saturating_mul(2);
    let half = t - t / 2;
    let mut cuts: Vec<Cut> = Vec::new();
    let mut s = 0usize;
    while s < n {
        let total = cum[n] - cum[s];
        let mut chosen: Option<(usize, &'static str)> = None;
        if total > t {
            let mut best_any: Option<usize> = None;
            let mut best_comp: Option<usize> = None;
            let mut b = s + 1;
            while b < n && size(s, b) <= t {
                if legal[b] && size(s, b) >= half {
                    best_any = Some(b);
                    if after_comp[b] {
                        best_comp = Some(b);
                    }
                }
                b += 1;
            }
            if let Some(b) = best_comp {
                chosen = Some((b, "compaction"));
            } else if let Some(b) = best_any {
                chosen = Some((b, "user"));
            } else {
                let mut b = s + 1;
                while b < n && size(s, b) <= t2 {
                    if legal[b] && size(s, b) > t {
                        chosen = Some((b, "user-late"));
                        break;
                    }
                    b += 1;
                }
                if chosen.is_none() && total > t2 {
                    let mut b = s + 1;
                    while b < n && size(s, b) < t {
                        b += 1;
                    }
                    if b < n {
                        chosen = Some((b, "hard"));
                    }
                }
            }
        }
        match chosen {
            Some((b, kind)) => {
                cuts.push(Cut {
                    start: s,
                    end: b,
                    frozen: true,
                    kind,
                });
                s = b;
            }
            None => break,
        }
    }
    if s < n {
        cuts.push(Cut {
            start: s,
            end: n,
            frozen: false,
            kind: "tail",
        });
    }
    cuts
}

// --- checkpoint ---------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct StoredPart {
    idx: u64,
    first_id: String,
    last_id: String,
    n_entries: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Trailer {
    state: String,
    version: u32,
    adapter_ver: String,
    target_bytes: u64,
    size: u64,
    indexed_bytes: u64,
    window_sha: String,
    header_sha: String,
    index_lines: u64,
    index_sha: String,
    part_count: u64,
    parts: Vec<StoredPart>,
}

struct Loaded {
    trailer: Trailer,
    entries: Vec<Entry>,
    body_sha: String,
}

fn tsv_line(e: &Entry) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\n",
        e.offset,
        e.len,
        e.kind.code(),
        e.id.as_deref().unwrap_or(""),
        e.parent.as_deref().unwrap_or(""),
        e.aux
    )
}

fn parse_tsv(line: &str) -> std::result::Result<Entry, String> {
    let mut it = line.splitn(6, '\t');
    let mut field = |name: &str| {
        it.next()
            .map(str::to_string)
            .ok_or_else(|| format!("state line lacks {name}"))
    };
    let offset = field("offset")?
        .parse::<u64>()
        .map_err(|_| "state offset not a number".to_string())?;
    let len = field("len")?
        .parse::<u64>()
        .map_err(|_| "state len not a number".to_string())?;
    let code = field("kind")?;
    let kind = Kind::from_code(&code).ok_or_else(|| "state kind unknown".to_string())?;
    let id = field("id")?;
    let parent = field("parent")?;
    let aux = field("aux")?;
    Ok(Entry {
        offset,
        len,
        kind,
        id: if id.is_empty() { None } else { Some(id) },
        parent: if parent.is_empty() {
            None
        } else {
            Some(parent)
        },
        aux,
    })
}

fn read_state(path: &Path) -> std::result::Result<Option<Loaded>, String> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("state unreadable: {e}")),
    };
    let mut r = BufReader::with_capacity(1 << 20, f);
    let mut hasher = Sha256::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut trailer: Option<Trailer> = None;
    loop {
        buf.clear();
        let n = r
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("state unreadable: {e}"))?;
        if n == 0 {
            break;
        }
        if trailer.is_some() {
            return Err("state has data after its trailer".to_string());
        }
        if buf[0] == b'{' {
            let t: Trailer =
                serde_json::from_slice(&buf).map_err(|e| format!("state trailer: {e}"))?;
            trailer = Some(t);
            continue;
        }
        if buf.last() != Some(&b'\n') {
            return Err("state truncated".to_string());
        }
        hasher.update(&buf);
        let line = std::str::from_utf8(&buf[..buf.len() - 1])
            .map_err(|_| "state line not utf-8".to_string())?;
        entries.push(parse_tsv(line)?);
    }
    let trailer = trailer.ok_or_else(|| "state has no trailer".to_string())?;
    if trailer.state != STATE_MARK {
        return Err("state is not a segment-plan checkpoint".to_string());
    }
    Ok(Some(Loaded {
        trailer,
        entries,
        body_sha: hex::encode(hasher.finalize()),
    }))
}

fn validate(
    f: &mut File,
    size: u64,
    loaded: &Loaded,
    adapter_ver: &str,
) -> std::result::Result<(), String> {
    let t = &loaded.trailer;
    if t.version != STATE_VERSION {
        return Err("state version changed".to_string());
    }
    if t.adapter_ver != adapter_ver {
        return Err("adapter_ver changed".to_string());
    }
    if size < t.indexed_bytes {
        return Err("source is smaller than indexed_bytes".to_string());
    }
    if t.index_lines != loaded.entries.len() as u64 || t.index_sha != loaded.body_sha {
        return Err("index body does not match its checksum".to_string());
    }
    if let Some(last) = loaded.entries.last() {
        if last.offset + last.len > t.indexed_bytes {
            return Err("index runs past indexed_bytes".to_string());
        }
    }
    if t.indexed_bytes > 0 {
        let w = window_sha(f, t.indexed_bytes).map_err(|e| format!("window read: {e}"))?;
        if w != t.window_sha {
            return Err("window hash mismatch (source rewritten before indexed_bytes)".to_string());
        }
        let h = first_line_sha(f).map_err(|e| format!("header read: {e}"))?;
        if h != t.header_sha {
            return Err("header line changed".to_string());
        }
    }
    Ok(())
}

fn write_state(path: &Path, entries: &[Entry], mut trailer: Trailer) -> Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".to_string());
    let tmp = path.with_file_name(format!("{name}.tmp{}", std::process::id()));
    {
        let mut w = BufWriter::new(
            File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?,
        );
        let mut hasher = Sha256::new();
        for e in entries {
            let line = tsv_line(e);
            hasher.update(line.as_bytes());
            w.write_all(line.as_bytes())?;
        }
        trailer.index_lines = entries.len() as u64;
        trailer.index_sha = hex::encode(hasher.finalize());
        w.write_all(serde_json::to_string(&trailer)?.as_bytes())?;
        w.write_all(b"\n")?;
        w.flush()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("rename {}", tmp.display()))?;
    Ok(())
}

// --- the plan -----------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Opts {
    pub target_bytes: u64,
    pub adapter_ver: String,
    pub print_chain: bool,
    pub write_state: bool,
}

#[derive(Clone, Debug)]
pub struct Part {
    pub idx: u64,
    pub frozen: bool,
    pub first_id: String,
    pub last_id: String,
    pub n_entries: u64,
    pub start_offset: u64,
    pub end_offset: u64,
    pub bytes: u64,
    pub cut: &'static str,
    pub reused: bool,
    /// Positions in the live chain: `[lo, hi)`.
    pub lo: usize,
    pub hi: usize,
}

pub struct PlanResult {
    pub json: Value,
    pub entries: Vec<Entry>,
    pub live: Vec<usize>,
    pub parts: Vec<Part>,
}

fn id_value(id: &str) -> Value {
    serde_json::from_str(id).unwrap_or_else(|_| Value::String(id.to_string()))
}

fn truthy_str(v: &Value) -> Option<String> {
    match v {
        Value::Null | Value::Bool(false) => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn read_entry_json(f: &mut File, e: &Entry) -> Option<Value> {
    let bytes = read_range(f, e.offset, e.len).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn plan_file(source: &Path, state_path: &Path, opts: &Opts) -> Result<PlanResult> {
    let mut f = File::open(source).with_context(|| format!("open source {}", source.display()))?;
    let size = f.metadata()?.len();

    // 1. checkpoint: load, validate, or rebuild.
    let mut status = "absent";
    let mut reason: Option<String> = None;
    let mut entries: Vec<Entry> = Vec::new();
    let mut from: u64 = 0;
    let mut stored_parts: Vec<StoredPart> = Vec::new();
    let mut prev_part_count: u64 = 0;
    let mut prev_target: Option<u64> = None;
    match read_state(state_path) {
        Ok(None) => {}
        Err(why) => {
            status = "rebuilt";
            reason = Some(why);
        }
        Ok(Some(loaded)) => {
            stored_parts = loaded.trailer.parts.clone();
            prev_part_count = loaded.trailer.part_count;
            prev_target = Some(loaded.trailer.target_bytes);
            match validate(&mut f, size, &loaded, &opts.adapter_ver) {
                Ok(()) => {
                    status = "valid";
                    from = loaded.trailer.indexed_bytes;
                    entries = loaded.entries;
                }
                Err(why) => {
                    status = "rebuilt";
                    reason = Some(why);
                }
            }
        }
    }
    let lines_before = entries.len();
    let (indexed_end, volatile) = scan(&mut f, from, size, &mut entries)?;
    let scanned_bytes = size - from;
    let persisted = entries.len();
    let new_lines = persisted - lines_before;
    if let Some(v) = volatile {
        entries.push(v);
    }

    // 2. chain + cuts.
    let live = live_chain(&entries);
    let cuts = plan_cuts(&entries, &live, opts.target_bytes);
    let mut cum_bytes: Vec<u64> = Vec::with_capacity(live.len() + 1);
    cum_bytes.push(0);
    for &i in &live {
        let last = cum_bytes[cum_bytes.len() - 1];
        cum_bytes.push(last + entries[i].len);
    }
    let mut parts: Vec<Part> = Vec::with_capacity(cuts.len());
    for (k, c) in cuts.iter().enumerate() {
        let first = &entries[live[c.start]];
        let last = &entries[live[c.end - 1]];
        parts.push(Part {
            idx: k as u64 + 1,
            frozen: c.frozen,
            first_id: first.id.clone().unwrap_or_default(),
            last_id: last.id.clone().unwrap_or_default(),
            n_entries: (c.end - c.start) as u64,
            start_offset: first.offset,
            end_offset: last.offset + last.len,
            bytes: cum_bytes[c.end] - cum_bytes[c.start],
            cut: c.kind,
            reused: false,
            lo: c.start,
            hi: c.end,
        });
    }

    // 3. divergence: stored frozen parts vs the fresh plan.
    let mut divergence = Value::Null;
    let mut mismatch: Option<usize> = None;
    for (k, sp) in stored_parts.iter().enumerate() {
        let same = parts.get(k).is_some_and(|p| {
            p.frozen
                && p.first_id == sp.first_id
                && p.last_id == sp.last_id
                && p.n_entries == sp.n_entries
        });
        if same {
            parts[k].reused = true;
        } else if mismatch.is_none() {
            mismatch = Some(k);
        }
    }
    if let Some(k) = mismatch {
        // Everything from the first mismatch on is re-planned: a later part
        // that happens to match was cut from a different prefix.
        for p in parts.iter_mut().skip(k) {
            p.reused = false;
        }
        let why = if prev_target.is_some_and(|t| t != opts.target_bytes) {
            "target_bytes changed".to_string()
        } else if parts.len() <= k {
            "part no longer exists on the chain".to_string()
        } else {
            "frozen boundary is no longer on the chain".to_string()
        };
        divergence = json!({
            "first_mismatch_part": k as u64 + 1,
            "reason": why,
            "stored_last_id": id_value(&stored_parts[k].last_id),
        });
    }
    let orphans: Vec<u64> = ((parts.len() as u64 + 1)..=prev_part_count).collect();

    // 4. header facts.
    let hdr_entry = entries.iter().find(|e| e.kind == Kind::Session);
    let hdr_json = hdr_entry.and_then(|e| read_entry_json(&mut f, e));
    let hdr_id = hdr_json
        .as_ref()
        .and_then(|v| v.get("id"))
        .and_then(truthy_str);
    let es0_id = entries
        .iter()
        .find(|e| e.kind != Kind::Title)
        .and_then(|e| e.id.as_deref())
        .map(|s| match id_value(s) {
            Value::String(x) => x,
            other => other.to_string(),
        });
    let session_id = hdr_id.or(es0_id).unwrap_or_else(|| "unknown".to_string());
    let cwd = hdr_json
        .as_ref()
        .and_then(|v| v.get("cwd"))
        .and_then(truthy_str)
        .unwrap_or_else(|| "unknown".to_string());
    let title = entries
        .iter()
        .find(|e| e.kind == Kind::Title)
        .and_then(|e| read_entry_json(&mut f, e))
        .and_then(|v| v.get("title").and_then(Value::as_str).map(str::to_string))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let dmodel = live
        .iter()
        .rev()
        .map(|&i| &entries[i])
        .find(|e| e.kind == Kind::Model)
        .and_then(|e| serde_json::from_str::<Value>(&e.aux).ok())
        .filter(|v| !matches!(v, Value::Null | Value::Bool(false)))
        .unwrap_or_else(|| Value::String("omp".to_string()));

    // 5. persist the checkpoint (derived data: safe to lose).
    let new_stored: Vec<StoredPart> = parts
        .iter()
        .filter(|p| p.frozen)
        .map(|p| StoredPart {
            idx: p.idx,
            first_id: p.first_id.clone(),
            last_id: p.last_id.clone(),
            n_entries: p.n_entries,
        })
        .collect();
    let dirty = status != "valid"
        || new_lines > 0
        || indexed_end != from
        || new_stored != stored_parts
        || parts.len() as u64 != prev_part_count
        || prev_target != Some(opts.target_bytes);
    if opts.write_state && dirty {
        let trailer = Trailer {
            state: STATE_MARK.to_string(),
            version: STATE_VERSION,
            adapter_ver: opts.adapter_ver.clone(),
            target_bytes: opts.target_bytes,
            size,
            indexed_bytes: indexed_end,
            window_sha: if indexed_end > 0 {
                window_sha(&mut f, indexed_end)?
            } else {
                String::new()
            },
            header_sha: if indexed_end > 0 {
                first_line_sha(&mut f)?
            } else {
                String::new()
            },
            index_lines: 0,
            index_sha: String::new(),
            part_count: parts.len() as u64,
            parts: new_stored,
        };
        write_state(state_path, &entries[..persisted], trailer)?;
    }

    // 6. the document.
    let parts_json: Vec<Value> = parts
        .iter()
        .map(|p| {
            let state = if p.frozen { "frozen" } else { "live" };
            json!({
                "idx": p.idx,
                "state": state,
                "reused": p.reused,
                "first_id": id_value(&p.first_id),
                "last_id": id_value(&p.last_id),
                "n_entries": p.n_entries,
                "start_offset": p.start_offset,
                "end_offset": p.end_offset,
                "bytes": p.bytes,
                "cut": p.cut,
            })
        })
        .collect();
    let mut doc = json!({
        "schema": PLAN_SCHEMA,
        "session_id": session_id,
        "cwd": cwd,
        "title": title,
        "dmodel": dmodel,
        "target_bytes": opts.target_bytes,
        "adapter_ver": opts.adapter_ver,
        "source": {
            "size": size,
            "indexed_bytes": indexed_end,
            "lines_indexed": persisted,
        },
        "checkpoint": {
            "status": status,
            "reason": reason,
            "scanned_bytes": scanned_bytes,
        },
        "chain": {
            "entries": live.len(),
            "bytes": cum_bytes[cum_bytes.len() - 1],
        },
        "parts": parts_json,
        "divergence": divergence,
        "orphans": orphans,
    });
    if opts.print_chain {
        let ids: Vec<Value> = live
            .iter()
            .map(|&i| entries[i].id.as_deref().map_or(Value::Null, id_value))
            .collect();
        doc["chain_ids"] = Value::Array(ids);
    }
    Ok(PlanResult {
        json: doc,
        entries,
        live,
        parts,
    })
}

fn write_line(out: &mut impl Write, bytes: &[u8]) -> Result<()> {
    out.write_all(bytes)?;
    if bytes.last() != Some(&b'\n') {
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// Stream the raw source lines of part `idx` (1-based): the session header and
/// the title slot first (unless they are part of the chain), then the part's
/// chain lines in file order. Feeding this to TRANSLATE yields the part's
/// translation.
pub fn emit_part(source: &Path, res: &PlanResult, idx: usize, out: &mut impl Write) -> Result<()> {
    if idx == 0 || idx > res.parts.len() {
        bail!("no part {idx}: the plan has {} part(s)", res.parts.len());
    }
    let part = &res.parts[idx - 1];
    let wanted: Vec<u64> = res.live[part.lo..part.hi]
        .iter()
        .map(|&i| res.entries[i].offset)
        .collect();
    let mut f = File::open(source).with_context(|| format!("open {}", source.display()))?;
    let mut lead: Vec<&Entry> = Vec::new();
    for kind in [Kind::Session, Kind::Title] {
        if let Some(e) = res.entries.iter().find(|e| e.kind == kind) {
            if wanted.binary_search(&e.offset).is_err() {
                lead.push(e);
            }
        }
    }
    lead.sort_by_key(|e| e.offset);
    for e in lead {
        let bytes = read_range(&mut f, e.offset, e.len)?;
        write_line(out, &bytes)?;
    }
    f.seek(SeekFrom::Start(part.start_offset))?;
    let mut r =
        BufReader::with_capacity(1 << 20, Read::take(&f, part.end_offset - part.start_offset));
    let mut buf: Vec<u8> = Vec::new();
    let mut pos = part.start_offset;
    let mut wi = 0usize;
    while wi < wanted.len() {
        buf.clear();
        let n = r.read_until(b'\n', &mut buf)?;
        if n == 0 {
            bail!("source ended before part {idx}'s last chain line (file rewritten during emit?)");
        }
        if wanted[wi] == pos {
            write_line(out, &buf)?;
            wi += 1;
        }
        pos += n as u64;
    }
    out.flush()?;
    Ok(())
}

/// CLI entry. Without `--emit` prints the plan JSON; with it, one part's lines.
pub fn run(
    source: PathBuf,
    state: PathBuf,
    target_bytes: Option<u64>,
    adapter_ver: String,
    emit: Option<usize>,
    print_chain: bool,
    no_write: bool,
) -> Result<()> {
    let target = match target_bytes {
        Some(t) => t,
        None => match std::env::var("KB_CAPTURE_SEGMENT_BYTES") {
            Ok(s) if !s.trim().is_empty() => s
                .trim()
                .parse::<u64>()
                .map_err(|_| anyhow!("KB_CAPTURE_SEGMENT_BYTES is not a number: {s}"))?,
            _ => DEFAULT_TARGET_BYTES,
        },
    };
    if target == 0 {
        bail!("--target-bytes must be at least 1");
    }
    let opts = Opts {
        target_bytes: target,
        adapter_ver,
        print_chain,
        write_state: !no_write,
    };
    let res = plan_file(&source, &state, &opts)?;
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    match emit {
        Some(idx) => emit_part(&source, &res, idx, &mut out)?,
        None => {
            serde_json::to_writer(&mut out, &res.json)?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "sessions_segment_plan_tests.rs"]
mod tests;
