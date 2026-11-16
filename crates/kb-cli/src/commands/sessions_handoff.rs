//! `kb sessions handoff` — move a session between omp and Claude Code.
//!
//! Filesystem-only and offline, like `kb sessions export` / `rehydrate`: the
//! source is the harness's own session file (omp tree or Claude projects dir)
//! with a fallback to the newest kb capture in `KB_SESSIONS_DIR` / `--from`.
//! Two products, both built by [`kb_core::sessions::handoff`]:
//!
//! * **brief** (default) — deterministic, LLM-free text for the NEW session
//!   of the target harness. Written to a 0600 file under the XDG cache (or
//!   `--out`), printed with `--print`, or handed straight to the harness
//!   with `--launch` (argv, never a shell).
//! * **transplant** (`--transplant`) — a native session file placed where the
//!   target harness looks for it (`claude -r <id>` / `omp -r <id>`). Never
//!   overwrites an existing file; `--dry-run` writes nothing.
//!
//! Every brief and transplant preamble opens with the
//! `[kb-handoff from <harness> session <id>]` marker, which the kb session
//! parser reads back as `continued_from` lineage.

use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use kb_core::session_bundle::claude_project_slug;
use kb_core::sessions::handoff::brief::{render_brief, BriefOptions};
use kb_core::sessions::handoff::transplant::{new_session_id, plan_transplant, TransplantOptions};
use kb_core::sessions::handoff::{claude, omp, Conversation, Harness, SourceKind, HANDOFF_SCHEMA};
use kb_core::sessions::recover_jsonl_from_capture;

use super::import::{expand_tilde, sanitize_sid};
use super::session_bundle::{claude_projects_base, find_newest_capture, resolve_from, resume_hint};

/// Fallback for the `version` field of a transplanted Claude transcript when
/// `claude --version` is unavailable.
const FALLBACK_CLAUDE_VERSION: &str = "2.1.318";

/// Parsed `kb sessions handoff` flags.
pub struct HandoffArgs {
    pub session_id: String,
    /// `claude` | `omp`; `None` = the other harness than the source.
    pub to: Option<String>,
    pub transplant: bool,
    pub budget: Option<usize>,
    pub tail: Option<usize>,
    pub cwd: Option<PathBuf>,
    pub from_capture: bool,
    pub from: Option<PathBuf>,
    pub claude_config_dir: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub print: bool,
    pub launch: bool,
    pub dry_run: bool,
    pub json: bool,
}

/// What a run produced, for the human / `--json` renderers.
struct Report {
    mode: &'static str,
    chars: Option<usize>,
    truncated: Vec<String>,
    brief_path: Option<PathBuf>,
    transcript_path: Option<PathBuf>,
    new_session_id: Option<String>,
    omitted_turns: Option<usize>,
    command: String,
}

pub fn run(args: HandoffArgs) -> Result<()> {
    validate_flags(&args)?;
    let home = home_dir()?;
    let invoke_dir = std::env::current_dir().context("resolve the current directory")?;
    // An explicitly passed dir (absolute: the printed command runs after a
    // `cd`) must also reach the launched / printed `claude`; one inherited from
    // the environment reaches the child on its own.
    let explicit_cfg_dir = args
        .claude_config_dir
        .clone()
        .map(|p| absolutize(expand_tilde(p), &invoke_dir));
    let cfg_dir = explicit_cfg_dir.clone().or_else(|| {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    let lookup = SourceLookup {
        home: &home,
        claude_config_dir: cfg_dir.as_deref(),
        from_capture: args.from_capture,
        from: args.from.clone(),
    };
    let conv = load_conversation(&args.session_id, &lookup)?;
    let target = match args.to.as_deref() {
        Some(t) => {
            Harness::parse(t).ok_or_else(|| anyhow!("--to must be `claude` or `omp`, got `{t}`"))?
        }
        None => conv.harness.other(),
    };
    if args.transplant {
        run_transplant(
            &args,
            conv,
            target,
            &home,
            cfg_dir.as_deref(),
            explicit_cfg_dir.as_deref(),
        )
    } else {
        run_brief(
            &args,
            &conv,
            target,
            &home,
            explicit_cfg_dir.as_deref(),
            &invoke_dir,
        )
    }
}

fn validate_flags(a: &HandoffArgs) -> Result<()> {
    if a.print && a.launch {
        bail!("--print and --launch are mutually exclusive");
    }
    if a.json && (a.print || a.launch) {
        bail!("--json cannot be combined with --print or --launch (stdout is the one JSON object)");
    }
    if a.launch && a.dry_run {
        bail!("--launch and --dry-run are mutually exclusive");
    }
    if a.transplant {
        if a.tail.is_some() {
            bail!("--tail applies to the brief only; a transplant carries the tail by --budget");
        }
        if a.print {
            bail!("--print renders the brief; it cannot be combined with --transplant");
        }
        if a.out.is_some() {
            bail!("--out names the brief file; it cannot be combined with --transplant");
        }
    } else if a.print && a.out.is_some() {
        bail!("--print writes the brief to stdout; it cannot be combined with --out");
    }
    Ok(())
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set; cannot locate the harness session directories"))
}

// --- source resolution -------------------------------------------------------

struct SourceLookup<'a> {
    home: &'a Path,
    claude_config_dir: Option<&'a Path>,
    from_capture: bool,
    from: Option<PathBuf>,
}

/// A raw harness session file that matched the requested id.
#[derive(Debug, PartialEq)]
enum RawHit {
    Omp(PathBuf),
    Claude(PathBuf),
}

impl RawHit {
    fn path(&self) -> &Path {
        match self {
            RawHit::Omp(p) | RawHit::Claude(p) => p,
        }
    }
    fn harness(&self) -> Harness {
        match self {
            RawHit::Omp(_) => Harness::Omp,
            RawHit::Claude(_) => Harness::Claude,
        }
    }
}

fn raw_hits(id: &str, lk: &SourceLookup<'_>) -> Result<Vec<RawHit>> {
    let omp_root = omp::omp_sessions_root(lk.home);
    let mut hits: Vec<RawHit> = omp::locate_omp_session(&omp_root, id)
        .with_context(|| format!("scan omp sessions under {}", omp_root.display()))?
        .into_iter()
        .map(RawHit::Omp)
        .collect();
    let roots = claude::claude_projects_roots(lk.claude_config_dir, lk.home);
    hits.extend(
        claude::locate_claude_session(&roots, id)
            .context("scan Claude Code projects")?
            .into_iter()
            .map(RawHit::Claude),
    );
    Ok(hits)
}

/// 0 hits → `None` (capture fallback), 1 → that hit, several → an error that
/// lists every candidate so the caller can pass a longer id prefix.
fn pick_single(id: &str, mut hits: Vec<RawHit>) -> Result<Option<RawHit>> {
    match hits.len() {
        0 => Ok(None),
        1 => Ok(hits.pop()),
        n => {
            let mut msg =
                format!("`{id}` matches {n} sessions — pass a longer id prefix to pick one:");
            for h in &hits {
                msg.push_str(&format!(
                    "\n  {:<6} {}",
                    h.harness().as_str(),
                    h.path().display()
                ));
            }
            bail!(msg)
        }
    }
}

/// Read the source conversation: the harness's own file first (full
/// fidelity), then the newest kb capture.
fn load_conversation(id: &str, lk: &SourceLookup<'_>) -> Result<Conversation> {
    if !lk.from_capture {
        if let Some(hit) = pick_single(id, raw_hits(id, lk)?)? {
            let path = hit.path().to_path_buf();
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let shown = path.to_string_lossy().into_owned();
            let conv = match hit {
                RawHit::Omp(_) => omp::read_omp_session(&text, Some(shown.as_str())),
                RawHit::Claude(_) => {
                    claude::read_claude_jsonl(&text, SourceKind::Raw, Some(shown.as_str()))
                }
            };
            return conv.with_context(|| format!("read session {}", path.display()));
        }
    }
    let dir = match resolve_from(lk.from.clone()) {
        Ok(d) => d,
        Err(e) if lk.from_capture => return Err(e),
        Err(e) => bail!(
            "no omp or Claude Code session file matches `{id}`, and there is no capture \
             corpus to fall back on: {e}"
        ),
    };
    let hit = find_newest_capture(&dir, id)?.ok_or_else(|| {
        anyhow!(
            "session `{id}` not found: {}no capture under {}",
            if lk.from_capture {
                ""
            } else {
                "no omp or Claude Code session file matches and "
            },
            dir.display()
        )
    })?;
    let html = std::fs::read_to_string(&hit.path)
        .with_context(|| format!("read capture {}", hit.path.display()))?;
    let jsonl = recover_jsonl_from_capture(&html)
        .ok_or_else(|| anyhow!("capture {} has no <pre> transcript", hit.path.display()))?;
    let shown = hit.path.to_string_lossy().into_owned();
    let mut conv = claude::read_claude_jsonl(&jsonl, SourceKind::Capture, Some(shown.as_str()))
        .with_context(|| format!("read capture {}", hit.path.display()))?;
    let note = if conv.harness == Harness::Omp {
        "source is a kb capture, not the omp session file: lossy for omp (leaf chain only, \
         tool results capped at 2000 chars, no images)"
    } else {
        "source is a kb capture, not the Claude Code transcript"
    };
    eprintln!("note: {note}");
    conv.notes.push(note.to_string());
    Ok(conv)
}

// --- cwd ---------------------------------------------------------------------

/// The target working directory plus an optional warning for the caller.
/// `need_dir` (launch / transplant) makes a missing directory an error.
fn resolve_cwd(
    flag: Option<PathBuf>,
    conv_cwd: Option<&str>,
    need_dir: bool,
) -> Result<(String, Option<String>)> {
    if let Some(p) = flag {
        let p = expand_tilde(p);
        return match p.canonicalize() {
            Ok(c) if c.is_dir() => Ok((c.to_string_lossy().into_owned(), None)),
            _ if need_dir => bail!("--cwd {} is not an existing directory", p.display()),
            _ => Ok((
                p.to_string_lossy().into_owned(),
                Some(format!(
                    "--cwd {} is not an existing directory",
                    p.display()
                )),
            )),
        };
    }
    match conv_cwd {
        Some(c) if Path::new(c).is_dir() => {
            let abs = Path::new(c)
                .canonicalize()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| c.to_string());
            Ok((abs, None))
        }
        Some(c) if need_dir => {
            bail!("the session's cwd {c} does not exist on this machine; pass --cwd <DIR>")
        }
        Some(c) => Ok((
            c.to_string(),
            Some(format!(
                "the session's cwd {c} does not exist on this machine; pass --cwd <DIR>"
            )),
        )),
        None if need_dir => bail!("the session recorded no cwd; pass --cwd <DIR>"),
        None => {
            let here = std::env::current_dir()
                .context("current directory")?
                .to_string_lossy()
                .into_owned();
            Ok((
                here,
                Some("the session recorded no cwd; using the current directory".to_string()),
            ))
        }
    }
}

// --- brief -------------------------------------------------------------------

fn run_brief(
    args: &HandoffArgs,
    conv: &Conversation,
    target: Harness,
    home: &Path,
    explicit_cfg_dir: Option<&Path>,
    invoke_dir: &Path,
) -> Result<()> {
    let (cwd, warning) = resolve_cwd(args.cwd.clone(), conv.cwd.as_deref(), args.launch)?;
    let mut opts = BriefOptions::new(target);
    if let Some(b) = args.budget {
        opts.budget_chars = b;
    }
    if let Some(t) = args.tail {
        opts.tail_turns = t;
    }
    let brief = render_brief(conv, &opts);

    if args.print {
        if let Some(w) = &warning {
            eprintln!("warning: {w}");
        }
        let mut out = std::io::stdout().lock();
        out.write_all(brief.text.as_bytes())?;
        out.flush()?;
        return Ok(());
    }
    if args.launch {
        // Replace this process: the target harness owns the terminal now.
        let mut cmd = std::process::Command::new(target.as_str());
        cmd.arg(&brief.text).current_dir(&cwd);
        apply_claude_config_dir(&mut cmd, target, explicit_cfg_dir);
        return exec_harness(cmd, target);
    }

    let file = match args.out.clone() {
        Some(o) => absolutize(expand_tilde(o), invoke_dir),
        None => brief_cache_path(
            std::env::var_os("XDG_CACHE_HOME"),
            Some(home),
            &conv.session_id,
            target,
        )?,
    };
    if !args.dry_run {
        write_private(&file, brief.text.as_bytes())?;
    }
    let mut notes = conv.notes.clone();
    notes.extend(warning);
    let report = Report {
        mode: "brief",
        chars: Some(brief.text.chars().count()),
        truncated: brief.truncated,
        command: brief_command(&cwd, target, &file, explicit_cfg_dir),
        brief_path: Some(file),
        transcript_path: None,
        new_session_id: None,
        omitted_turns: None,
    };
    emit(conv, target, &cwd, &notes, &report, args.dry_run, args.json)
}

/// `$XDG_CACHE_HOME/kb/handoff/<sid>-to-<target>.md`, falling back to
/// `<home>/.cache/…` when the variable is unset, empty or not absolute (the
/// XDG spec says to ignore a relative value).
fn brief_cache_path(
    xdg_cache_home: Option<OsString>,
    home: Option<&Path>,
    session_id: &str,
    target: Harness,
) -> Result<PathBuf> {
    let base = match xdg_cache_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(x) => x,
        None => home
            .ok_or_else(|| anyhow!("neither XDG_CACHE_HOME nor HOME is set; pass --out <FILE>"))?
            .join(".cache"),
    };
    Ok(base.join("kb").join("handoff").join(format!(
        "{}-to-{}.md",
        sanitize_sid(session_id),
        target.as_str()
    )))
}

/// POSIX single-quote `s` for a copy-paste command; plain tokens stay bare.
fn sh_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `p` itself when absolute, else `p` joined onto `base`. Printed commands run
/// after a `cd`, and an absolute path cannot be mistaken for an option.
fn absolutize(p: PathBuf, base: &Path) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

/// `CLAUDE_CONFIG_DIR=<dir> ` for a copy-paste command that starts `claude`
/// against an explicitly chosen config dir; empty otherwise.
fn claude_env_prefix(target: Harness, cfg_dir: Option<&Path>) -> String {
    match (target, cfg_dir) {
        (Harness::Claude, Some(d)) => {
            format!("CLAUDE_CONFIG_DIR={} ", sh_quote(&d.to_string_lossy()))
        }
        _ => String::new(),
    }
}

/// Point a spawned `claude` at an explicitly chosen config dir.
fn apply_claude_config_dir(
    cmd: &mut std::process::Command,
    target: Harness,
    cfg_dir: Option<&Path>,
) {
    if let (Harness::Claude, Some(d)) = (target, cfg_dir) {
        cmd.env("CLAUDE_CONFIG_DIR", d);
    }
}

fn brief_command(
    cwd: &str,
    target: Harness,
    file: &Path,
    explicit_cfg_dir: Option<&Path>,
) -> String {
    format!(
        "cd {} && {}{} \"$(cat {})\"",
        sh_quote(cwd),
        claude_env_prefix(target, explicit_cfg_dir),
        target.as_str(),
        sh_quote(&file.to_string_lossy())
    )
}

/// The copy-paste command that resumes a transplanted session.
fn transplant_command(
    cwd: &str,
    target: Harness,
    session_id: &str,
    explicit_cfg_dir: Option<&Path>,
) -> String {
    match target {
        Harness::Claude => format!(
            "cd {} && {}{}",
            sh_quote(cwd),
            claude_env_prefix(target, explicit_cfg_dir),
            resume_hint("claude", session_id, None)
        ),
        Harness::Omp => resume_hint("omp", session_id, None),
    }
}

/// Write `bytes` to `path` (parents created), mode 0600, replacing an earlier
/// brief for the same session.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("write {}", path.display()))?;
    // `mode` only applies on creation; tighten a pre-existing file too.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))
}

// --- transplant --------------------------------------------------------------

fn run_transplant(
    args: &HandoffArgs,
    conv: Conversation,
    target: Harness,
    home: &Path,
    cfg_dir: Option<&Path>,
    explicit_cfg_dir: Option<&Path>,
) -> Result<()> {
    let (cwd, warning) = resolve_cwd(args.cwd.clone(), conv.cwd.as_deref(), true)?;
    let plan = plan_transplant(
        &conv,
        &TransplantOptions {
            target,
            new_session_id: new_session_id(target),
            cwd: cwd.clone(),
            now: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            tail_budget_chars: args
                .budget
                .unwrap_or(TransplantOptions::DEFAULT_TAIL_BUDGET_CHARS),
            tool_result_max_chars: TransplantOptions::DEFAULT_TOOL_RESULT_MAX_CHARS,
            target_model: None,
        },
    );

    let (path, contents) = match target {
        Harness::Claude => {
            let base = match cfg_dir {
                Some(d) => d.join("projects"),
                None => claude_projects_base()?,
            };
            let version = claude_version();
            (
                claude_transcript_path(&base, &cwd, &plan.new_session_id),
                claude::write_claude_transcript(&plan, &version),
            )
        }
        Harness::Omp => {
            let tmp = std::env::temp_dir();
            let file =
                omp::write_omp_session(&plan, &home.to_string_lossy(), &tmp.to_string_lossy());
            (
                omp::omp_sessions_root(home)
                    .join(&file.bucket)
                    .join(&file.file_name),
                file.contents,
            )
        }
    };
    if !args.dry_run {
        create_new_private(&path, contents.as_bytes())?;
    }

    let mut notes = conv.notes.clone();
    notes.extend(warning);
    let command = transplant_command(&cwd, target, &plan.new_session_id, explicit_cfg_dir);
    let report = Report {
        mode: "transplant",
        chars: None,
        truncated: Vec::new(),
        brief_path: None,
        transcript_path: Some(path),
        new_session_id: Some(plan.new_session_id.clone()),
        omitted_turns: Some(plan.omitted_turns),
        command,
    };
    emit(
        &conv,
        target,
        &cwd,
        &notes,
        &report,
        args.dry_run,
        args.json,
    )?;

    if args.launch {
        std::io::stdout().flush()?;
        let mut cmd = std::process::Command::new(target.as_str());
        cmd.args(["-r", &plan.new_session_id]).current_dir(&cwd);
        apply_claude_config_dir(&mut cmd, target, explicit_cfg_dir);
        return exec_harness(cmd, target);
    }
    Ok(())
}

/// `<projects base>/<slug(cwd)>/<id>.jsonl`.
fn claude_transcript_path(projects_base: &Path, cwd: &str, session_id: &str) -> PathBuf {
    projects_base
        .join(claude_project_slug(cwd))
        .join(format!("{session_id}.jsonl"))
}

/// Create `path` exclusively (mode 0600): a collision is an error, an
/// existing session is never touched.
fn create_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow!(
                    "{} already exists; refusing to overwrite it",
                    path.display()
                )
            } else {
                anyhow!("create {}: {e}", path.display())
            }
        })?;
    f.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))
}

/// First token of `claude --version` output (`2.1.318 (Claude Code)`).
fn parse_claude_version(stdout: &str) -> Option<String> {
    let tok = stdout.split_whitespace().next()?;
    tok.chars()
        .next()
        .filter(char::is_ascii_digit)
        .map(|_| tok.to_string())
}

/// The installed Claude Code version for the transcript's `version` field;
/// best effort, [`FALLBACK_CLAUDE_VERSION`] on any failure.
fn claude_version() -> String {
    std::process::Command::new("claude")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| parse_claude_version(&s))
        .unwrap_or_else(|| FALLBACK_CLAUDE_VERSION.to_string())
}

/// Replace the process with the target harness (no shell). Only returns on
/// failure.
fn exec_harness(mut cmd: std::process::Command, target: Harness) -> Result<()> {
    let err = cmd.exec();
    bail!("could not launch `{}`: {err}", target.as_str())
}

// --- output ------------------------------------------------------------------

fn kind_str(k: SourceKind) -> &'static str {
    match k {
        SourceKind::Raw => "raw",
        SourceKind::Capture => "capture",
    }
}

fn emit(
    conv: &Conversation,
    target: Harness,
    cwd: &str,
    notes: &[String],
    r: &Report,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    if json {
        let mut obj = serde_json::json!({
            "schema": HANDOFF_SCHEMA,
            "mode": r.mode,
            "source": {
                "harness": conv.harness.as_str(),
                "session_id": conv.session_id,
                "kind": kind_str(conv.source_kind),
                "path": conv.source_path,
            },
            "target": target.as_str(),
            "cwd": cwd,
            "notes": notes,
            "command": r.command,
            "dry_run": dry_run,
        });
        let map = obj.as_object_mut().expect("object literal");
        if let Some(c) = r.chars {
            map.insert("chars".into(), c.into());
            map.insert("truncated".into(), serde_json::json!(r.truncated));
        }
        if let Some(p) = &r.brief_path {
            map.insert("brief_path".into(), p.display().to_string().into());
        }
        if let Some(p) = &r.transcript_path {
            map.insert("transcript_path".into(), p.display().to_string().into());
        }
        if let Some(id) = &r.new_session_id {
            map.insert("new_session_id".into(), id.clone().into());
        }
        if let Some(n) = r.omitted_turns {
            map.insert("omitted_turns".into(), n.into());
        }
        println!("{}", serde_json::to_string_pretty(&obj)?);
        return Ok(());
    }

    let verb = match (r.mode, dry_run) {
        ("brief", false) => "Wrote handoff brief",
        ("brief", true) => "Would write handoff brief (dry run)",
        (_, false) => "Placed transplanted session",
        (_, true) => "Would place transplanted session (dry run)",
    };
    println!(
        "{verb}: {} session {} -> {}",
        conv.harness.as_str(),
        conv.session_id,
        target.as_str()
    );
    println!(
        "  source:     {} ({})",
        conv.source_path.as_deref().unwrap_or("-"),
        kind_str(conv.source_kind)
    );
    println!("  cwd:        {cwd}");
    if let Some(p) = &r.brief_path {
        println!("  brief:      {}", p.display());
    }
    if let Some(c) = r.chars {
        println!("  chars:      {c}");
    }
    if !r.truncated.is_empty() {
        println!("  truncated:  {}", r.truncated.join(", "));
    }
    if let Some(p) = &r.transcript_path {
        println!("  transcript: {}", p.display());
    }
    if let Some(id) = &r.new_session_id {
        println!("  new id:     {id}");
    }
    if let Some(n) = r.omitted_turns {
        println!("  omitted:    {n} older turn(s) not carried verbatim");
    }
    for n in notes {
        println!("  note:       {n}");
    }
    println!();
    println!(
        "{}",
        if r.mode == "brief" {
            "Start the new session with:"
        } else {
            "Resume it with:"
        }
    );
    println!("  {}", r.command);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OMP_ID: &str = "01a1e4a1-0d74-7548-9455-e37ffe71b0a1";
    const CLAUDE_ID: &str = "08cb5496-12c0-498a-b5ca-5838b38116b0";

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn omp_fixture(home: &Path, id: &str) -> PathBuf {
        let p = home
            .join(".omp/agent/sessions/-tmp-x")
            .join(format!("2026-11-16T15-00-00-000Z_{id}.jsonl"));
        write(
            &p,
            &format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-11-16T15:00:00.000Z\",\"cwd\":\"/tmp/x\"}}\n\
                 {{\"type\":\"message\",\"id\":\"u0000001\",\"parentId\":null,\"timestamp\":\"2026-11-16T15:00:02.000Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"hi\"}}],\"attribution\":\"user\",\"timestamp\":1794841202000}}}}\n"
            ),
        );
        p
    }

    fn claude_fixture(home: &Path, id: &str) -> PathBuf {
        let p = home
            .join(".claude/projects/-tmp-x")
            .join(format!("{id}.jsonl"));
        write(
            &p,
            &format!(
                "{{\"type\":\"user\",\"uuid\":\"bd77bd0f-0000-4000-8000-000000000001\",\"parentUuid\":null,\"timestamp\":\"2026-11-16T10:00:00.000Z\",\"sessionId\":\"{id}\",\"cwd\":\"/tmp/x\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n"
            ),
        );
        p
    }

    fn lookup<'a>(home: &'a Path, from: Option<PathBuf>) -> SourceLookup<'a> {
        SourceLookup {
            home,
            claude_config_dir: None,
            from_capture: false,
            from,
        }
    }

    #[test]
    fn sh_quote_leaves_plain_tokens_and_escapes_everything_else() {
        assert_eq!(sh_quote("/home/nik/proj-1/x.md"), "/home/nik/proj-1/x.md");
        assert_eq!(sh_quote("/home/my dir"), "'/home/my dir'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote("a$(rm -rf)b"), "'a$(rm -rf)b'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn brief_command_quotes_cwd_and_file() {
        let cmd = brief_command("/w/my proj", Harness::Claude, Path::new("/c/a b.md"), None);
        assert_eq!(cmd, "cd '/w/my proj' && claude \"$(cat '/c/a b.md')\"");
        let cmd = brief_command("/w", Harness::Omp, Path::new("/c/x.md"), None);
        assert_eq!(cmd, "cd /w && omp \"$(cat /c/x.md)\"");
    }

    #[test]
    fn brief_command_sets_config_dir_only_for_claude() {
        let d = Path::new("/cfg dir");
        let cmd = brief_command("/w", Harness::Claude, Path::new("/c/x.md"), Some(d));
        assert_eq!(
            cmd,
            "cd /w && CLAUDE_CONFIG_DIR='/cfg dir' claude \"$(cat /c/x.md)\""
        );
        let cmd = brief_command("/w", Harness::Omp, Path::new("/c/x.md"), Some(d));
        assert_eq!(cmd, "cd /w && omp \"$(cat /c/x.md)\"");
    }

    #[test]
    fn transplant_command_carries_config_dir_only_when_given() {
        let sid = "08cb5496-12c0-498a-b5ca-5838b38116b0";
        assert_eq!(
            transplant_command("/w", Harness::Claude, sid, None),
            format!("cd /w && claude -r {sid}")
        );
        assert_eq!(
            transplant_command("/w", Harness::Claude, sid, Some(Path::new("/c/d"))),
            format!("cd /w && CLAUDE_CONFIG_DIR=/c/d claude -r {sid}")
        );
        assert_eq!(
            transplant_command("/w", Harness::Claude, sid, Some(Path::new("/c d"))),
            format!("cd /w && CLAUDE_CONFIG_DIR='/c d' claude -r {sid}")
        );
        assert_eq!(
            transplant_command("/w", Harness::Omp, sid, Some(Path::new("/c/d"))),
            format!("omp -r {sid}")
        );
    }

    #[test]
    fn apply_claude_config_dir_sets_env_only_for_claude() {
        let env_of = |target, cfg: Option<&Path>| {
            let mut cmd = std::process::Command::new("x");
            apply_claude_config_dir(&mut cmd, target, cfg);
            cmd.get_envs()
                .find(|(k, _)| *k == "CLAUDE_CONFIG_DIR")
                .and_then(|(_, v)| v.map(|v| v.to_os_string()))
        };
        assert_eq!(
            env_of(Harness::Claude, Some(Path::new("/c/d"))),
            Some(OsString::from("/c/d"))
        );
        assert_eq!(env_of(Harness::Claude, None), None);
        assert_eq!(env_of(Harness::Omp, Some(Path::new("/c/d"))), None);
    }

    #[test]
    fn absolutize_resolves_relative_paths_against_the_given_dir() {
        let base = Path::new("/inv/dir");
        assert_eq!(
            absolutize(PathBuf::from("brief.md"), base),
            PathBuf::from("/inv/dir/brief.md")
        );
        // A leading dash can no longer be taken for an option by `cat`.
        let p = absolutize(PathBuf::from("-x.md"), base);
        assert_eq!(p, PathBuf::from("/inv/dir/-x.md"));
        assert!(p.is_absolute());
        assert_eq!(
            absolutize(PathBuf::from("/abs/b.md"), base),
            PathBuf::from("/abs/b.md")
        );
    }

    #[test]
    fn cache_path_prefers_absolute_xdg_then_falls_back_to_home() {
        let home = Path::new("/home/u");
        let p = brief_cache_path(Some("/xdg".into()), Some(home), OMP_ID, Harness::Claude).unwrap();
        assert_eq!(
            p,
            PathBuf::from(format!("/xdg/kb/handoff/{OMP_ID}-to-claude.md"))
        );
        for bad in [None, Some(OsString::new()), Some("relative/cache".into())] {
            let p = brief_cache_path(bad, Some(home), OMP_ID, Harness::Omp).unwrap();
            assert_eq!(
                p,
                PathBuf::from(format!("/home/u/.cache/kb/handoff/{OMP_ID}-to-omp.md"))
            );
        }
        assert!(brief_cache_path(None, None, OMP_ID, Harness::Omp).is_err());
    }

    #[test]
    fn claude_version_is_the_first_numeric_token() {
        assert_eq!(
            parse_claude_version("2.1.320 (Claude Code)\n").as_deref(),
            Some("2.1.320")
        );
        assert_eq!(parse_claude_version("command not found"), None);
        assert_eq!(parse_claude_version(""), None);
    }

    #[test]
    fn single_omp_hit_is_read_raw() {
        let home = tempfile::tempdir().unwrap();
        let path = omp_fixture(home.path(), OMP_ID);
        let conv = load_conversation("01a1e4a1", &lookup(home.path(), None)).unwrap();
        assert_eq!(conv.harness, Harness::Omp);
        assert_eq!(conv.session_id, OMP_ID);
        assert_eq!(conv.source_kind, SourceKind::Raw);
        assert_eq!(conv.source_path.as_deref(), Some(path.to_str().unwrap()));
    }

    #[test]
    fn single_claude_hit_is_read_raw() {
        let home = tempfile::tempdir().unwrap();
        claude_fixture(home.path(), CLAUDE_ID);
        let conv = load_conversation("08cb5496", &lookup(home.path(), None)).unwrap();
        assert_eq!(conv.harness, Harness::Claude);
        assert_eq!(conv.session_id, CLAUDE_ID);
        assert_eq!(conv.source_kind, SourceKind::Raw);
    }

    #[test]
    fn several_hits_error_listing_every_candidate() {
        let home = tempfile::tempdir().unwrap();
        let omp_path = omp_fixture(home.path(), "abc00000-0d74-7548-9455-e37ffe71b0a1");
        let claude_path = claude_fixture(home.path(), "abc11111-12c0-498a-b5ca-5838b38116b0");
        let err = load_conversation("abc", &lookup(home.path(), None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("longer id prefix"), "{err}");
        assert!(err.contains(omp_path.to_str().unwrap()), "{err}");
        assert!(err.contains(claude_path.to_str().unwrap()), "{err}");
    }

    #[test]
    fn no_raw_hit_falls_through_to_the_capture_corpus() {
        let home = tempfile::tempdir().unwrap();
        omp_fixture(home.path(), OMP_ID);
        let empty_corpus = tempfile::tempdir().unwrap();
        // Unknown id: nothing raw, and the (empty) corpus has no capture.
        let err = load_conversation(
            "deadbeef",
            &lookup(home.path(), Some(empty_corpus.path().into())),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains(empty_corpus.path().to_str().unwrap()), "{err}");
        // `--from-capture` skips an existing raw file entirely.
        let mut lk = lookup(home.path(), Some(empty_corpus.path().into()));
        lk.from_capture = true;
        let err = load_conversation(OMP_ID, &lk).unwrap_err().to_string();
        assert!(
            err.contains("not found") && !err.contains("no omp or Claude"),
            "{err}"
        );
    }

    #[test]
    fn transcript_path_uses_the_claude_slug() {
        let p = claude_transcript_path(Path::new("/c/projects"), "/home/u/my.proj", "sid1");
        assert_eq!(p, PathBuf::from("/c/projects/-home-u-my-proj/sid1.jsonl"));
    }

    #[test]
    fn create_new_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b/x.jsonl");
        create_new_private(&p, b"one").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let err = create_new_private(&p, b"two").unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
    }

    #[test]
    fn cwd_must_exist_only_when_a_directory_is_needed() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let (c, w) = resolve_cwd(Some(real.clone()), Some("/nope"), true).unwrap();
        assert_eq!(c, real.to_string_lossy());
        assert!(w.is_none());
        let (c, w) = resolve_cwd(None, Some("/definitely/not/here"), false).unwrap();
        assert_eq!(c, "/definitely/not/here");
        assert!(w.unwrap().contains("--cwd"));
        let err = resolve_cwd(None, Some("/definitely/not/here"), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--cwd"), "{err}");
        assert!(resolve_cwd(None, None, true).is_err());
    }

    #[test]
    fn flag_conflicts_are_rejected() {
        let base = || HandoffArgs {
            session_id: "s".into(),
            to: None,
            transplant: false,
            budget: None,
            tail: None,
            cwd: None,
            from_capture: false,
            from: None,
            claude_config_dir: None,
            out: None,
            print: false,
            launch: false,
            dry_run: false,
            json: false,
        };
        assert!(validate_flags(&base()).is_ok());
        assert!(validate_flags(&HandoffArgs {
            print: true,
            launch: true,
            ..base()
        })
        .is_err());
        assert!(validate_flags(&HandoffArgs {
            json: true,
            launch: true,
            ..base()
        })
        .is_err());
        assert!(validate_flags(&HandoffArgs {
            json: true,
            print: true,
            ..base()
        })
        .is_err());
        assert!(validate_flags(&HandoffArgs {
            transplant: true,
            tail: Some(3),
            ..base()
        })
        .is_err());
        assert!(validate_flags(&HandoffArgs {
            transplant: true,
            json: true,
            ..base()
        })
        .is_ok());
    }
}
