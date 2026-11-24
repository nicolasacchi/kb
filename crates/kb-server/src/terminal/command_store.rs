//! The terminal command catalogue overlay (kbt2 CMDSTORE): ONE daemon-owned,
//! operator-edited overlay file (`kb-term-commands/1`) merged over the built-in
//! defaults at read time.
//!
//! The daemon only STORES the overlay. It never runs, pastes or sends a
//! catalogue entry: the SPA turns an item into a paste or a key chord through
//! the existing terminal routes.
//!
//! Routes (`/api/terms/commands`, GET / PUT / DELETE) are terminal-tier
//! routes: every handler runs [`gate`] (mutations) or [`gate_read`] (GET)
//! first, so token callers are refused and loopback does not bypass. The file
//! is mutated ONLY here, under ONE daemon-wide async lock held across the
//! load-modify-write; the file IO itself runs on the blocking pool, and no
//! other `.await` happens while the guard is held. The write is tmp file ->
//! fsync -> rename -> directory fsync, so a crash leaves the old or the new
//! file, never a torn one.
//!
//! The stored document is
//! `{"schema":"kb-term-commands/1","rev","harnesses":{…},"hidden":{…}}`. Only
//! `{schema, harnesses}` goes through the shared validator
//! ([`super::commands::validate`], which rejects unknown fields); `rev` and
//! `hidden` are checked here. `rev` is a counter that bumps once per durable
//! change and is the ETag. A reset writes an EMPTY overlay with `rev + 1`
//! instead of removing the file, so an ETag is never reused. A missing file is
//! rev 0. An unknown schema or corrupt JSON is refused with a typed 500 and
//! the file is left untouched, never silently reset.
//!
//! Merge rule (per harness, [`merge`]): default groups in default order, then
//! overlay-only groups. Inside a group, a default item whose id appears
//! anywhere in the overlay's harness is REPLACED in place by the overlay item;
//! overlay items with no default twin sit in the overlay group they came from
//! (a group id shared with a default merges into it, the overlay's label
//! winning). `hidden[h]` drops default items; an overlay replacement of a
//! hidden id is an overlay item and stays. A group left with no item is
//! dropped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use kb_core::paths::KbPaths;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::commands::{validate, CmdError, SCHEMA};
use crate::middleware::Identity;
use crate::state::KbHandles;
use crate::terminal::gate::{
    gate, gate_read, ERR_TERMINAL_BAD_REQUEST, ERR_TERMINAL_PAYLOAD_TOO_LARGE,
};

/// The shipped defaults: the ONE source, shared with the SPA and with the
/// validator's own tests (never copied).
const DEFAULTS_JSON: &str = include_str!("../../../../web/src/terminal/commands/defaults.json");

/// Largest PUT body (bytes).
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Most ids in one harness's `hidden` list.
pub const MAX_HIDDEN_PER_HARNESS: usize = 200;
/// Most validation errors a 400 body carries.
pub const MAX_ERRORS_REPORTED: usize = 50;

/// `urn:kb:errors:bad-catalogue` — the overlay failed validation (400).
pub(crate) const ERR_BAD_CATALOGUE: &str = "urn:kb:errors:bad-catalogue";
/// `urn:kb:errors:catalogue-too-large` — the merged result would break a
/// catalogue cap (409).
pub(crate) const ERR_CATALOGUE_TOO_LARGE: &str = "urn:kb:errors:catalogue-too-large";
/// `urn:kb:errors:commands-stale` — `If-Match` named an old revision (412).
pub(crate) const ERR_COMMANDS_STALE: &str = "urn:kb:errors:commands-stale";
/// `urn:kb:errors:precondition-required` — a PUT without `If-Match` (428).
pub(crate) const ERR_PRECONDITION_REQUIRED: &str = "urn:kb:errors:precondition-required";
/// `urn:kb:errors:commands-corrupt` — the overlay file is unreadable or has
/// an unknown schema; it is left untouched (500).
pub(crate) const ERR_COMMANDS_CORRUPT: &str = "urn:kb:errors:commands-corrupt";
/// `urn:kb:errors:commands-io` — the overlay could not be read or written (500).
pub(crate) const ERR_COMMANDS_IO: &str = "urn:kb:errors:commands-io";

// ---------------------------------------------------------------------------
// Wire + stored types
// ---------------------------------------------------------------------------
// (Plain comments, not doc comments: ts-rs would copy a doc comment into the
// generated TS file.)

// One declared argument of a command.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OvArg {
    pub name: String,
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub default: Option<String>,
}

// One catalogue entry as the overlay stores it (the CMDSCHEMA item shape).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OvItem {
    pub id: String,
    pub kind: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub args: Option<Vec<OvArg>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub keys: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub unverified: Option<bool>,
}

// One group of entries.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OvGroup {
    pub id: String,
    pub label: String,
    pub items: Vec<OvItem>,
}

// One harness's groups.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OvHarness {
    pub groups: Vec<OvGroup>,
}

// The `PUT` body, and the shape the overlay is stored in. The server stamps
// `schema` and `rev`; the client sends neither.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayPut {
    #[serde(default)]
    pub harnesses: BTreeMap<String, OvHarness>,
    #[serde(default)]
    pub hidden: BTreeMap<String, Vec<String>>,
}

// Where an effective entry came from.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EffOrigin {
    Default,
    Overlay,
}

// An effective entry: the item fields plus its origin.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffItem {
    pub id: String,
    pub kind: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub args: Option<Vec<OvArg>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub keys: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub unverified: Option<bool>,
    pub origin: EffOrigin,
}

// An effective group.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffGroup {
    pub id: String,
    pub label: String,
    pub origin: EffOrigin,
    pub items: Vec<EffItem>,
}

// An effective harness.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffHarness {
    pub groups: Vec<EffGroup>,
}

// The effective catalogue (defaults + overlay) and its revision.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveCatalogue {
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub rev: u64,
    pub harnesses: BTreeMap<String, EffHarness>,
}

// The `term.commands` SSE payload: the new revision, never any content. A
// client refetches the catalogue.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct TermCommandsEvent {
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub rev: u64,
}

// ---------------------------------------------------------------------------
// Defaults + merge (pure)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DefaultsDoc {
    schema: String,
    harnesses: BTreeMap<String, OvHarness>,
}

/// The shipped defaults, parsed once. `None` only if the bundled file is
/// broken, which a unit test rules out.
fn defaults() -> Option<&'static BTreeMap<String, OvHarness>> {
    static CELL: OnceLock<Option<BTreeMap<String, OvHarness>>> = OnceLock::new();
    CELL.get_or_init(|| {
        serde_json::from_str::<DefaultsDoc>(DEFAULTS_JSON)
            .ok()
            .filter(|d| d.schema == SCHEMA)
            .map(|d| d.harnesses)
    })
    .as_ref()
}

/// One merged group: the group plus a per-item origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergedGroup {
    pub id: String,
    pub label: String,
    pub origin: EffOrigin,
    pub items: Vec<(OvItem, EffOrigin)>,
}

/// The effective catalogue of every harness named by the defaults or the
/// overlay (sorted by harness name).
pub(crate) fn merge(
    defaults: &BTreeMap<String, OvHarness>,
    overlay: &OverlayPut,
) -> BTreeMap<String, Vec<MergedGroup>> {
    let mut names: Vec<&String> = defaults.keys().chain(overlay.harnesses.keys()).collect();
    names.sort();
    names.dedup();
    let empty: Vec<OvGroup> = Vec::new();
    let no_hidden: Vec<String> = Vec::new();
    let mut out = BTreeMap::new();
    for name in names {
        let dgroups = defaults.get(name).map_or(&empty, |h| &h.groups);
        let ogroups = overlay.harnesses.get(name).map_or(&empty, |h| &h.groups);
        let hidden: HashSet<&str> = overlay
            .hidden
            .get(name)
            .unwrap_or(&no_hidden)
            .iter()
            .map(String::as_str)
            .collect();
        let mut oitems: HashMap<&str, &OvItem> = HashMap::new();
        for g in ogroups {
            for it in &g.items {
                oitems.entry(it.id.as_str()).or_insert(it);
            }
        }
        let default_ids: HashSet<&str> = dgroups
            .iter()
            .flat_map(|g| g.items.iter().map(|i| i.id.as_str()))
            .collect();
        let dgroup_ids: HashSet<&str> = dgroups.iter().map(|g| g.id.as_str()).collect();

        let mut groups: Vec<MergedGroup> = Vec::new();
        for dg in dgroups {
            let og = ogroups.iter().find(|g| g.id == dg.id);
            let mut items: Vec<(OvItem, EffOrigin)> = Vec::new();
            for it in &dg.items {
                if let Some(o) = oitems.get(it.id.as_str()) {
                    items.push(((*o).clone(), EffOrigin::Overlay));
                } else if !hidden.contains(it.id.as_str()) {
                    items.push((it.clone(), EffOrigin::Default));
                }
            }
            if let Some(og) = og {
                for it in &og.items {
                    if !default_ids.contains(it.id.as_str()) {
                        items.push((it.clone(), EffOrigin::Overlay));
                    }
                }
            }
            if !items.is_empty() {
                groups.push(MergedGroup {
                    id: dg.id.clone(),
                    label: og.map_or_else(|| dg.label.clone(), |g| g.label.clone()),
                    origin: if og.is_some() {
                        EffOrigin::Overlay
                    } else {
                        EffOrigin::Default
                    },
                    items,
                });
            }
        }
        for og in ogroups {
            if dgroup_ids.contains(og.id.as_str()) {
                continue;
            }
            let items: Vec<(OvItem, EffOrigin)> = og
                .items
                .iter()
                .filter(|i| !default_ids.contains(i.id.as_str()))
                .map(|i| (i.clone(), EffOrigin::Overlay))
                .collect();
            if !items.is_empty() {
                groups.push(MergedGroup {
                    id: og.id.clone(),
                    label: og.label.clone(),
                    origin: EffOrigin::Overlay,
                    items,
                });
            }
        }
        out.insert(name.clone(), groups);
    }
    out
}

/// The merged catalogue as a plain `{schema, harnesses}` value (origins
/// stripped), exactly what [`validate`] takes.
fn merged_value(merged: &BTreeMap<String, Vec<MergedGroup>>) -> Value {
    let mut hs = Map::new();
    for (name, groups) in merged {
        let h = OvHarness {
            groups: groups
                .iter()
                .map(|g| OvGroup {
                    id: g.id.clone(),
                    label: g.label.clone(),
                    items: g.items.iter().map(|(i, _)| i.clone()).collect(),
                })
                .collect(),
        };
        hs.insert(
            name.clone(),
            serde_json::to_value(&h).unwrap_or(Value::Null),
        );
    }
    let mut root = Map::new();
    root.insert("schema".into(), Value::String(SCHEMA.into()));
    root.insert("harnesses".into(), Value::Object(hs));
    Value::Object(root)
}

fn to_eff(item: OvItem, origin: EffOrigin) -> EffItem {
    EffItem {
        id: item.id,
        kind: item.kind,
        label: item.label,
        description: item.description,
        text: item.text,
        mode: item.mode,
        args: item.args,
        keys: item.keys,
        unverified: item.unverified,
        origin,
    }
}

fn effective(rev: u64, merged: BTreeMap<String, Vec<MergedGroup>>) -> EffectiveCatalogue {
    EffectiveCatalogue {
        rev,
        harnesses: merged
            .into_iter()
            .map(|(name, groups)| {
                (
                    name,
                    EffHarness {
                        groups: groups
                            .into_iter()
                            .map(|g| EffGroup {
                                id: g.id,
                                label: g.label,
                                origin: g.origin,
                                items: g.items.into_iter().map(|(i, o)| to_eff(i, o)).collect(),
                            })
                            .collect(),
                    },
                )
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Request validation (pure)
// ---------------------------------------------------------------------------

/// Why a PUT body was refused before any lock or disk.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PutError {
    /// Not JSON at all.
    Malformed,
    /// Parsed but failed validation; the ordered defect list.
    Invalid(Vec<CmdError>),
    /// Valid alone, but the merged catalogue would break a cap.
    TooLarge(CmdError),
}

fn pointer_seg(base: &str, s: &str) -> String {
    format!("{base}/{}", s.replace('~', "~0").replace('/', "~1"))
}

fn push_err(out: &mut Vec<CmdError>, code: &'static str, path: &str) {
    out.push(CmdError {
        code,
        path: path.to_string(),
    });
}

/// Check the `hidden` value: an object keyed by the closed harness set whose
/// values are arrays of at most [`MAX_HIDDEN_PER_HARNESS`] valid item ids.
fn check_hidden(v: &Value, out: &mut Vec<CmdError>) {
    let Some(m) = v.as_object() else {
        push_err(out, "bad-type", "/hidden");
        return;
    };
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort();
    for k in keys {
        let path = pointer_seg("/hidden", k);
        if !kb_core::sessions::HARNESSES.contains(&k.as_str()) {
            push_err(out, "unknown-harness", &path);
            continue;
        }
        let Some(arr) = m[k].as_array() else {
            push_err(out, "bad-type", &path);
            continue;
        };
        if arr.len() > MAX_HIDDEN_PER_HARNESS {
            push_err(out, "too-many", &path);
        }
        for (i, id) in arr.iter().enumerate() {
            let ipath = format!("{path}/{i}");
            match id.as_str() {
                Some(s) if super::commands::valid_id(s) => {}
                Some(_) => push_err(out, "bad-id", &ipath),
                None => push_err(out, "bad-type", &ipath),
            }
        }
    }
}

/// Sort + dedupe `hidden`, drop empty lists and groupless harness rows, so
/// two equal overlays compare equal and a no-op PUT is detectable.
fn normalize(mut o: OverlayPut) -> OverlayPut {
    for ids in o.hidden.values_mut() {
        ids.sort();
        ids.dedup();
    }
    o.hidden.retain(|_, ids| !ids.is_empty());
    o.harnesses.retain(|_, h| !h.groups.is_empty());
    o
}

/// Parse and validate a PUT body into a normalized overlay.
pub(crate) fn parse_put(
    body: &[u8],
    defaults: &BTreeMap<String, OvHarness>,
) -> Result<OverlayPut, PutError> {
    let v: Value = serde_json::from_slice(body).map_err(|_| PutError::Malformed)?;
    let Some(root) = v.as_object() else {
        return Err(PutError::Invalid(vec![CmdError {
            code: "bad-type",
            path: String::new(),
        }]));
    };
    let mut errs: Vec<CmdError> = Vec::new();
    let harnesses = root
        .get("harnesses")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut doc = Map::new();
    doc.insert("schema".into(), Value::String(SCHEMA.into()));
    doc.insert("harnesses".into(), harnesses.clone());
    errs.extend(validate(&Value::Object(doc)));
    let hidden = root
        .get("hidden")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    check_hidden(&hidden, &mut errs);
    let mut extra: Vec<&String> = root
        .keys()
        .filter(|k| k.as_str() != "harnesses" && k.as_str() != "hidden")
        .collect();
    extra.sort();
    for k in extra {
        push_err(&mut errs, "unknown-field", &pointer_seg("", k));
    }
    if !errs.is_empty() {
        return Err(PutError::Invalid(errs));
    }
    let mut typed = Map::new();
    typed.insert("harnesses".into(), harnesses);
    typed.insert("hidden".into(), hidden);
    let overlay: OverlayPut = serde_json::from_value(Value::Object(typed)).map_err(|_| {
        PutError::Invalid(vec![CmdError {
            code: "bad-type",
            path: String::new(),
        }])
    })?;
    let overlay = normalize(overlay);
    let errs = validate(&merged_value(&merge(defaults, &overlay)));
    if let Some(first) = errs.into_iter().next() {
        return Err(PutError::TooLarge(first));
    }
    Ok(overlay)
}

// ---------------------------------------------------------------------------
// The store (sync; the handlers hold the daemon-wide lock around it)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    schema: String,
    rev: u64,
    #[serde(default)]
    harnesses: BTreeMap<String, OvHarness>,
    #[serde(default)]
    hidden: BTreeMap<String, Vec<String>>,
}

impl Doc {
    fn empty(rev: u64) -> Self {
        Doc {
            schema: SCHEMA.to_string(),
            rev,
            harnesses: BTreeMap::new(),
            hidden: BTreeMap::new(),
        }
    }

    fn is_empty_overlay(&self) -> bool {
        self.harnesses.is_empty() && self.hidden.is_empty()
    }

    fn overlay(&self) -> OverlayPut {
        OverlayPut {
            harnesses: self.harnesses.clone(),
            hidden: self.hidden.clone(),
        }
    }
}

/// Why a store operation refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StoreError {
    Stale { current: u64 },
    Corrupt,
    Io,
}

/// One mutation.
pub(crate) enum Op {
    Put(OverlayPut),
    Reset,
}

/// What [`apply`] did. `changed` is false for a no-op (no rev bump, no
/// write, no event).
#[derive(Debug)]
pub(crate) struct Applied {
    pub rev: u64,
    pub overlay: OverlayPut,
    pub changed: bool,
}

fn load_doc(paths: &KbPaths) -> Result<Doc, StoreError> {
    let path = paths.term_commands_file();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Doc::empty(0)),
        Err(_) => {
            tracing::warn!("term-commands: overlay unreadable");
            return Err(StoreError::Io);
        }
    };
    match serde_json::from_slice::<Doc>(&bytes) {
        Ok(doc) if doc.schema == SCHEMA => Ok(doc),
        _ => {
            tracing::warn!(
                "term-commands: overlay refused (corrupt or unknown schema), left untouched"
            );
            Err(StoreError::Corrupt)
        }
    }
}

/// tmp file -> fsync -> rename (-> best-effort directory fsync).
fn save_doc(paths: &KbPaths, doc: &Doc) -> Result<(), StoreError> {
    let final_path = paths.term_commands_file();
    let Some(dir) = final_path.parent().map(std::path::Path::to_path_buf) else {
        return Err(StoreError::Io);
    };
    std::fs::create_dir_all(&dir).map_err(|_| StoreError::Io)?;
    let mut tmp_name = final_path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp_name);
    let bytes = serde_json::to_vec(doc).map_err(|_| StoreError::Io)?;
    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp_path)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp_path, &final_path)?;
        if let Ok(d) = std::fs::File::open(&dir) {
            let _ = d.sync_all();
        }
        Ok(())
    };
    if write().is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        tracing::warn!("term-commands: overlay write failed");
        return Err(StoreError::Io);
    }
    Ok(())
}

/// Read the overlay (rev 0 and empty when there is no file).
pub(crate) fn read_overlay(paths: &KbPaths) -> Result<(u64, OverlayPut), StoreError> {
    let doc = load_doc(paths)?;
    Ok((doc.rev, doc.overlay()))
}

/// Apply one mutation. The CALLER holds the daemon-wide lock. `if_match` is
/// the revision the client saw (`None` = unconditional).
pub(crate) fn apply(paths: &KbPaths, if_match: Option<u64>, op: Op) -> Result<Applied, StoreError> {
    let doc = load_doc(paths)?;
    if let Some(seen) = if_match {
        if seen != doc.rev {
            return Err(StoreError::Stale { current: doc.rev });
        }
    }
    let next = match op {
        Op::Put(o) => {
            if o == doc.overlay() {
                None
            } else {
                Some(Doc {
                    schema: SCHEMA.to_string(),
                    rev: doc.rev + 1,
                    harnesses: o.harnesses,
                    hidden: o.hidden,
                })
            }
        }
        Op::Reset => {
            if doc.is_empty_overlay() {
                None
            } else {
                Some(Doc::empty(doc.rev + 1))
            }
        }
    };
    match next {
        None => Ok(Applied {
            rev: doc.rev,
            overlay: doc.overlay(),
            changed: false,
        }),
        Some(n) => {
            save_doc(paths, &n)?;
            tracing::debug!(rev = n.rev, "term-commands: overlay mutated");
            Ok(Applied {
                rev: n.rev,
                overlay: n.overlay(),
                changed: true,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn no_store(mut resp: Response<Body>) -> Response<Body> {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn with_etag(mut resp: Response<Body>, rev: u64) -> Response<Body> {
    if let Ok(v) = HeaderValue::from_str(&format!("\"{rev}\"")) {
        resp.headers_mut().insert(header::ETAG, v);
    }
    resp
}

// One `{code, path}` defect on the wire.
#[derive(Serialize)]
struct ErrorEntry {
    code: &'static str,
    path: String,
}

// A problem body that may carry the defect list or the current revision.
#[derive(Serialize)]
struct ProblemBody<'a> {
    r#type: &'static str,
    title: &'static str,
    status: u16,
    detail: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<Vec<ErrorEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rev: Option<u64>,
}

fn problem_ext(
    urn: &'static str,
    status: StatusCode,
    title: &'static str,
    detail: &str,
    errors: Option<Vec<ErrorEntry>>,
    rev: Option<u64>,
) -> Response<Body> {
    let body = ProblemBody {
        r#type: urn,
        title,
        status: status.as_u16(),
        detail,
        errors,
        rev,
    };
    let payload = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    no_store(
        (
            status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            payload,
        )
            .into_response(),
    )
}

fn problem(
    urn: &'static str,
    status: StatusCode,
    title: &'static str,
    detail: &str,
) -> Response<Body> {
    problem_ext(urn, status, title, detail, None, None)
}

fn bad_request(detail: &str) -> Response<Body> {
    problem(
        ERR_TERMINAL_BAD_REQUEST,
        StatusCode::BAD_REQUEST,
        "Bad Request",
        detail,
    )
}

fn entries(errs: Vec<CmdError>) -> Vec<ErrorEntry> {
    errs.into_iter()
        .take(MAX_ERRORS_REPORTED)
        .map(|e| ErrorEntry {
            code: e.code,
            path: e.path,
        })
        .collect()
}

fn store_error_response(e: StoreError) -> Response<Body> {
    match e {
        StoreError::Stale { current } => with_etag(
            problem_ext(
                ERR_COMMANDS_STALE,
                StatusCode::PRECONDITION_FAILED,
                "Precondition Failed",
                &format!("the catalogue changed; the current revision is {current}"),
                None,
                Some(current),
            ),
            current,
        ),
        StoreError::Corrupt => problem(
            ERR_COMMANDS_CORRUPT,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "the command overlay is unreadable or has an unknown schema; it was left untouched",
        ),
        StoreError::Io => problem(
            ERR_COMMANDS_IO,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "the command overlay could not be read or written",
        ),
    }
}

fn put_error_response(e: PutError) -> Response<Body> {
    match e {
        PutError::Malformed => bad_request("malformed request body"),
        PutError::Invalid(errs) => {
            let n = errs.len();
            problem_ext(
                ERR_BAD_CATALOGUE,
                StatusCode::BAD_REQUEST,
                "Bad Request",
                &format!("the overlay has {n} defect(s); see `errors`"),
                Some(entries(errs)),
                None,
            )
        }
        PutError::TooLarge(first) => problem_ext(
            ERR_CATALOGUE_TOO_LARGE,
            StatusCode::CONFLICT,
            "Conflict",
            &format!(
                "the merged catalogue would exceed a cap ({} at {})",
                first.code, first.path
            ),
            Some(entries(vec![first])),
            None,
        ),
    }
}

fn catalogue_response(status: StatusCode, rev: u64, overlay: &OverlayPut) -> Response<Body> {
    let Some(defaults) = defaults() else {
        return store_error_response(StoreError::Corrupt);
    };
    let eff = effective(rev, merge(defaults, overlay));
    match serde_json::to_vec(&eff) {
        Ok(bytes) => {
            let resp =
                (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response();
            with_etag(no_store(resp), rev)
        }
        Err(_) => store_error_response(StoreError::Io),
    }
}

enum IfMatch {
    Absent,
    Rev(u64),
    Malformed,
}

/// `If-Match: "<rev>"` (a bare number and a `W/` prefix are tolerated).
fn parse_if_match(headers: &HeaderMap) -> IfMatch {
    let Some(raw) = headers.get(header::IF_MATCH) else {
        return IfMatch::Absent;
    };
    let Ok(s) = raw.to_str() else {
        return IfMatch::Malformed;
    };
    let s = s.trim();
    let s = s.strip_prefix("W/").unwrap_or(s);
    let s = s.trim_matches('"');
    match s.parse::<u64>() {
        Ok(rev) => IfMatch::Rev(rev),
        Err(_) => IfMatch::Malformed,
    }
}

/// Run one mutation under the daemon-wide lock and finish the response. The
/// file IO runs on the blocking pool; the guard is taken before and dropped
/// after the emit, with the blocking join the only `.await` in between.
async fn mutate(state: &KbHandles, if_match: Option<u64>, op: Op) -> Response<Body> {
    let guard = state.terminals.commands_lock.lock().await;
    let paths = Arc::clone(&state.paths);
    let joined = tokio::task::spawn_blocking(move || apply(&paths, if_match, op)).await;
    let out = joined.unwrap_or(Err(StoreError::Io));
    if let Ok(applied) = &out {
        if applied.changed {
            let ev = TermCommandsEvent { rev: applied.rev };
            state.bus.emit(
                "term.commands",
                serde_json::to_value(&ev).unwrap_or(Value::Null),
            );
        }
    }
    drop(guard);
    match out {
        Ok(applied) => catalogue_response(StatusCode::OK, applied.rev, &applied.overlay),
        Err(e) => store_error_response(e),
    }
}

/// `GET /api/terms/commands` — the effective catalogue, with its `ETag`.
pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
) -> Response<Body> {
    if let Err(e) = gate_read(&state, &headers, &identity).await {
        return e;
    }
    // A rename is atomic, so a reader never sees a torn file; no lock.
    let paths = Arc::clone(&state.paths);
    match tokio::task::spawn_blocking(move || read_overlay(&paths)).await {
        Ok(Ok((rev, overlay))) => catalogue_response(StatusCode::OK, rev, &overlay),
        Ok(Err(e)) => store_error_response(e),
        Err(_) => store_error_response(StoreError::Io),
    }
}

/// `PUT /api/terms/commands` — replace the overlay. `If-Match` REQUIRED (428
/// without, 412 when stale). The body is read AFTER the gate, with a hard cap
/// applied while buffering.
pub async fn put(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    body: Body,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    let rev = match parse_if_match(&headers) {
        IfMatch::Rev(r) => r,
        IfMatch::Absent => {
            return problem(
                ERR_PRECONDITION_REQUIRED,
                StatusCode::PRECONDITION_REQUIRED,
                "Precondition Required",
                "PUT requires an `If-Match` header carrying the catalogue's ETag",
            )
        }
        IfMatch::Malformed => return bad_request("malformed If-Match header"),
    };
    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => {
            return problem(
                ERR_TERMINAL_PAYLOAD_TOO_LARGE,
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload Too Large",
                "request body over 64 KiB",
            )
        }
    };
    let Some(defaults) = defaults() else {
        return store_error_response(StoreError::Corrupt);
    };
    let overlay = match parse_put(&bytes, defaults) {
        Ok(o) => o,
        Err(e) => return put_error_response(e),
    };
    mutate(&state, Some(rev), Op::Put(overlay)).await
}

/// `DELETE /api/terms/commands` — reset to the defaults (an empty overlay,
/// `rev + 1`). `If-Match` is honoured when sent.
pub async fn delete(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    let rev = match parse_if_match(&headers) {
        IfMatch::Rev(r) => Some(r),
        IfMatch::Absent => None,
        IfMatch::Malformed => return bad_request("malformed If-Match header"),
    };
    mutate(&state, rev, Op::Reset).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn defs() -> &'static BTreeMap<String, OvHarness> {
        defaults().expect("bundled defaults parse")
    }

    fn fixture() -> (tempfile::TempDir, KbPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "t");
        (tmp, paths)
    }

    fn item(id: &str, label: &str, text: &str) -> Value {
        json!({"id": id, "kind": "reply", "label": label, "text": text})
    }

    fn put(body: Value) -> Result<OverlayPut, PutError> {
        parse_put(body.to_string().as_bytes(), defs())
    }

    fn codes(e: PutError) -> Vec<(String, String)> {
        match e {
            PutError::Invalid(v) => v
                .into_iter()
                .map(|e| (e.code.to_string(), e.path))
                .collect(),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    fn find<'a>(
        m: &'a BTreeMap<String, Vec<MergedGroup>>,
        h: &str,
        id: &str,
    ) -> Option<&'a (OvItem, EffOrigin)> {
        m.get(h)?
            .iter()
            .flat_map(|g| g.items.iter())
            .find(|(i, _)| i.id == id)
    }

    #[test]
    fn bundled_defaults_parse_and_validate_clean() {
        let d = defs();
        assert!(d.contains_key("claude") && d.contains_key("omp"));
        let merged = merge(d, &OverlayPut::default());
        assert!(validate(&merged_value(&merged)).is_empty());
    }

    #[test]
    fn empty_overlay_merges_to_exactly_the_defaults_all_origin_default() {
        let d = defs();
        let merged = merge(d, &OverlayPut::default());
        for (h, groups) in &merged {
            let want = &d[h].groups;
            assert_eq!(groups.len(), want.len());
            for (g, w) in groups.iter().zip(want) {
                assert_eq!(g.id, w.id);
                assert_eq!(g.origin, EffOrigin::Default);
                let items: Vec<&OvItem> = g.items.iter().map(|(i, _)| i).collect();
                assert_eq!(items, w.items.iter().collect::<Vec<_>>());
                assert!(g.items.iter().all(|(_, o)| *o == EffOrigin::Default));
            }
        }
    }

    #[test]
    fn same_id_replaces_in_place_and_untouched_defaults_stay_default() {
        let o = put(json!({"harnesses": {"claude": {"groups": [
            {"id": "elsewhere", "label": "X", "items": [item("help", "Mine", "my help")]}
        ]}}}))
        .unwrap();
        let d = defs();
        let merged = merge(d, &o);
        let (it, origin) = find(&merged, "claude", "help").unwrap();
        assert_eq!(*origin, EffOrigin::Overlay);
        assert_eq!(it.label, "Mine");
        // In place: it stays in its default group, at its default index.
        let slash = merged["claude"].iter().find(|g| g.id == "slash").unwrap();
        let pos_default = d["claude"]
            .groups
            .iter()
            .find(|g| g.id == "slash")
            .unwrap()
            .items
            .iter()
            .position(|i| i.id == "help")
            .unwrap();
        assert_eq!(slash.items[pos_default].0.id, "help");
        // The group the replacement sat in is NOT created (it has no own item).
        assert!(!merged["claude"].iter().any(|g| g.id == "elsewhere"));
        let (_, other) = find(&merged, "claude", "compact").unwrap();
        assert_eq!(*other, EffOrigin::Default);
    }

    #[test]
    fn overlay_only_item_and_group_are_overlay_and_ordered_after_defaults() {
        let o = put(json!({"harnesses": {"claude": {"groups": [
            {"id": "slash", "label": "Slash commands", "items": [item("zz", "zz", "zz")]},
            {"id": "mine", "label": "Mine", "items": [item("yy", "yy", "yy")]}
        ]}}}))
        .unwrap();
        let merged = merge(defs(), &o);
        let groups = &merged["claude"];
        let slash = groups.iter().find(|g| g.id == "slash").unwrap();
        assert_eq!(slash.items.last().unwrap().0.id, "zz");
        assert_eq!(slash.items.last().unwrap().1, EffOrigin::Overlay);
        assert_eq!(
            slash.origin,
            EffOrigin::Overlay,
            "a group the overlay carries"
        );
        assert_eq!(groups.last().unwrap().id, "mine");
        assert_eq!(groups.last().unwrap().origin, EffOrigin::Overlay);
        let replies = groups.iter().find(|g| g.id == "replies").unwrap();
        assert_eq!(replies.origin, EffOrigin::Default);
    }

    #[test]
    fn hidden_drops_default_items_unknown_ids_are_stored_valid_and_ignored() {
        let o = put(json!({"hidden": {"claude": ["help", "no-such-item", "help"]}})).unwrap();
        assert_eq!(
            o.hidden["claude"],
            vec!["help", "no-such-item"],
            "sorted + unique"
        );
        let merged = merge(defs(), &o);
        assert!(find(&merged, "claude", "help").is_none());
        assert!(find(&merged, "claude", "compact").is_some());
    }

    #[test]
    fn a_replacement_of_a_hidden_id_is_an_overlay_item_and_stays() {
        let o = put(json!({
            "harnesses": {"claude": {"groups": [
                {"id": "slash", "label": "Slash commands", "items": [item("help", "Mine", "mine")]}]}},
            "hidden": {"claude": ["help"]}
        }))
        .unwrap();
        let merged = merge(defs(), &o);
        let (it, origin) = find(&merged, "claude", "help").unwrap();
        assert_eq!((it.label.as_str(), *origin), ("Mine", EffOrigin::Overlay));
    }

    #[test]
    fn a_group_with_every_item_hidden_is_dropped_and_the_result_validates() {
        let ids: Vec<String> = defs()["claude"]
            .groups
            .iter()
            .find(|g| g.id == "keys")
            .unwrap()
            .items
            .iter()
            .map(|i| i.id.clone())
            .collect();
        let o = put(json!({"hidden": {"claude": ids}})).unwrap();
        let merged = merge(defs(), &o);
        assert!(!merged["claude"].iter().any(|g| g.id == "keys"));
        assert!(validate(&merged_value(&merged)).is_empty());
    }

    #[test]
    fn validation_failures_surface_the_shared_validator_codes_in_order() {
        let e = put(json!({"harnesses": {"claude": {"groups": [
        {"id": "g", "label": "G", "items": [
            {"id": "a", "kind": "command", "label": "A", "text": "x\ny", "mode": "run"},
            {"id": "a", "kind": "reply", "label": "B", "text": "ok"}
        ]}]}}}))
        .unwrap_err();
        assert_eq!(
            codes(e),
            vec![
                (
                    "run-multiline".to_string(),
                    "/harnesses/claude/groups/0/items/0/text".to_string()
                ),
                (
                    "duplicate-id".to_string(),
                    "/harnesses/claude/groups/0/items/1/id".to_string()
                ),
            ]
        );
    }

    #[test]
    fn unknown_top_level_schema_rev_and_hidden_shape_errors_are_reported() {
        let e = put(json!({"schema": "kb-term-commands/1", "rev": 3,
                           "hidden": {"nope": [], "claude": "x", "omp": ["BAD", 7]}}))
        .unwrap_err();
        assert_eq!(
            codes(e),
            vec![
                ("bad-type".to_string(), "/hidden/claude".to_string()),
                ("unknown-harness".to_string(), "/hidden/nope".to_string()),
                ("bad-id".to_string(), "/hidden/omp/0".to_string()),
                ("bad-type".to_string(), "/hidden/omp/1".to_string()),
                ("unknown-field".to_string(), "/rev".to_string()),
                ("unknown-field".to_string(), "/schema".to_string()),
            ]
        );
        assert_eq!(parse_put(b"not json", defs()), Err(PutError::Malformed));
        assert_eq!(
            codes(parse_put(b"[]", defs()).unwrap_err()),
            vec![("bad-type".to_string(), String::new())]
        );
    }

    #[test]
    fn hidden_cap_is_exact_at_200_and_refused_at_201() {
        let ids = |n: usize| (0..n).map(|i| format!("h{i}")).collect::<Vec<_>>();
        assert!(put(json!({"hidden": {"claude": ids(200)}})).is_ok());
        let e = put(json!({"hidden": {"claude": ids(201)}})).unwrap_err();
        assert_eq!(
            codes(e),
            vec![("too-many".to_string(), "/hidden/claude".to_string())]
        );
    }

    #[test]
    fn a_merge_that_breaks_the_item_cap_is_catalogue_too_large() {
        // 200 overlay-only items on top of the shipped defaults.
        let items: Vec<Value> = (0..200).map(|i| item(&format!("n{i}"), "n", "n")).collect();
        let e = put(json!({"harnesses": {"claude": {"groups": [
            {"id": "big", "label": "Big", "items": items}]}}}))
        .unwrap_err();
        assert!(
            matches!(e, PutError::TooLarge(ref c) if c.code == "too-many"),
            "{e:?}"
        );
        // Exactly at the cap after the merge is fine: fill what the defaults leave.
        let used: usize = defs()["claude"].groups.iter().map(|g| g.items.len()).sum();
        let items: Vec<Value> = (0..(200 - used))
            .map(|i| item(&format!("n{i}"), "n", "n"))
            .collect();
        assert!(put(json!({"harnesses": {"claude": {"groups": [
            {"id": "big", "label": "Big", "items": items}]}}}))
        .is_ok());
    }

    #[test]
    fn roundtrip_rev_counter_noops_and_no_tmp_file() {
        let (_t, paths) = fixture();
        assert_eq!(read_overlay(&paths).unwrap().0, 0);
        assert!(!paths.term_commands_file().exists());
        // An empty PUT and a reset on a fresh daemon are no-ops: no file.
        let a = apply(&paths, Some(0), Op::Put(OverlayPut::default())).unwrap();
        assert!(!a.changed && a.rev == 0);
        let a = apply(&paths, None, Op::Reset).unwrap();
        assert!(!a.changed && a.rev == 0);
        assert!(!paths.term_commands_file().exists());

        let o = put(json!({"hidden": {"claude": ["help"]}})).unwrap();
        let a = apply(&paths, Some(0), Op::Put(o.clone())).unwrap();
        assert!(a.changed && a.rev == 1);
        let (rev, back) = read_overlay(&paths).unwrap();
        assert_eq!((rev, &back), (1, &o));
        // The same overlay again: no bump, file byte-identical.
        let before = std::fs::read(paths.term_commands_file()).unwrap();
        let a = apply(&paths, Some(1), Op::Put(o)).unwrap();
        assert!(!a.changed && a.rev == 1);
        assert_eq!(std::fs::read(paths.term_commands_file()).unwrap(), before);
        // Reset keeps counting: an empty overlay at rev 2, never rev 0.
        let a = apply(&paths, Some(1), Op::Reset).unwrap();
        assert!(a.changed && a.rev == 2);
        assert_eq!(read_overlay(&paths).unwrap(), (2, OverlayPut::default()));
        assert!(
            paths.term_commands_file().exists(),
            "reset writes, never removes"
        );
        let a = apply(&paths, None, Op::Reset).unwrap();
        assert!(!a.changed && a.rev == 2, "already empty");
        let stray: Vec<_> = std::fs::read_dir(paths.term_commands_file().parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(stray.is_empty(), "no .tmp left behind");
    }

    #[test]
    fn stale_if_match_is_refused_and_the_file_is_untouched() {
        let (_t, paths) = fixture();
        let o = put(json!({"hidden": {"claude": ["help"]}})).unwrap();
        apply(&paths, Some(0), Op::Put(o)).unwrap();
        let before = std::fs::read(paths.term_commands_file()).unwrap();
        let other = put(json!({"hidden": {"claude": ["compact"]}})).unwrap();
        assert_eq!(
            apply(&paths, Some(0), Op::Put(other)).unwrap_err(),
            StoreError::Stale { current: 1 }
        );
        assert_eq!(
            apply(&paths, Some(0), Op::Reset).unwrap_err(),
            StoreError::Stale { current: 1 }
        );
        assert_eq!(std::fs::read(paths.term_commands_file()).unwrap(), before);
    }

    #[test]
    fn unknown_schema_and_corrupt_json_refuse_and_leave_the_file_byte_identical() {
        for bytes in [
            &b"{\"schema\":\"kb-term-commands/9\",\"rev\":4,\"harnesses\":{},\"hidden\":{}}"[..],
            &b"not json at all"[..],
            &b"{\"schema\":\"kb-term-commands/1\",\"rev\":4,\"surprise\":1}"[..],
        ] {
            let (_t, paths) = fixture();
            std::fs::create_dir_all(paths.term_commands_file().parent().unwrap()).unwrap();
            std::fs::write(paths.term_commands_file(), bytes).unwrap();
            assert_eq!(read_overlay(&paths).unwrap_err(), StoreError::Corrupt);
            let o = put(json!({"hidden": {"claude": ["help"]}})).unwrap();
            assert_eq!(
                apply(&paths, None, Op::Put(o)).unwrap_err(),
                StoreError::Corrupt
            );
            assert_eq!(
                apply(&paths, None, Op::Reset).unwrap_err(),
                StoreError::Corrupt
            );
            assert_eq!(std::fs::read(paths.term_commands_file()).unwrap(), bytes);
        }
    }

    #[test]
    fn effective_wire_shape_carries_origin_and_skips_absent_fields() {
        let eff = effective(0, merge(defs(), &OverlayPut::default()));
        let v = serde_json::to_value(&eff).unwrap();
        let first = &v["harnesses"]["claude"]["groups"][0];
        assert_eq!(first["origin"], "default");
        assert_eq!(first["items"][0]["origin"], "default");
        assert!(first["items"][0].get("args").is_none());
    }

    #[test]
    fn if_match_forms_parse() {
        let mk = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::IF_MATCH, HeaderValue::from_str(v).unwrap());
            parse_if_match(&h)
        };
        assert!(matches!(mk("\"3\""), IfMatch::Rev(3)));
        assert!(matches!(mk("W/\"3\""), IfMatch::Rev(3)));
        assert!(matches!(mk("3"), IfMatch::Rev(3)));
        assert!(matches!(mk("nope"), IfMatch::Malformed));
        assert!(matches!(parse_if_match(&HeaderMap::new()), IfMatch::Absent));
    }
}
