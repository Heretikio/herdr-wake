//! Pi adapter (Task 8).
//!
//! # Hook contract
//!
//! Pi has no hooks file. The extension `assets/pi/herdr-wake.ts`, installed as
//! `${PI_CODING_AGENT_DIR:-~/.pi/agent}/extensions/herdr-wake.ts`, runs
//! `herdr-wake hook pi <event>` with one JSON object on stdin, only inside a
//! herdr-core terminal (`HC_TERMINAL_ID` set) and only in the interactive TUI
//! (`ctx.mode == "tui"`):
//!
//! | Event | When | Payload |
//! |---|---|---|
//! | `start` | `session_start` once the session file exists, and `agent_start` (the first prompt writes the file); once per session id | `{"reason": "startup"\|"new"\|"resume"\|"fork"\|"reload"\|"prompt", "id": "<session id>", "file": "<abs session file>", "cwd": "<abs folder>", "argv": ["pi", <launch args>...], "pid": <pi pid>}` |
//! | `end` | `session_shutdown` with reason `quit` | `{"id": "<recorded session id>", "signal": <true when SIGHUP or SIGTERM was seen first>}` |
//!
//! `argv[0]` only stands for the program; the flags after it are what
//! `restorable_flags` reads. `file` and `pid` may be missing. A quit with
//! `signal: false` is the person's (Ctrl+C, Ctrl+D, `/quit`); with `signal:
//! true` it is a closed terminal, a core stop or a shutdown, and the session
//! stays resumable.
//!
//! # Resume
//!
//! `cd <cwd> && <bin> <flags..> --session <file>`. By path, never by id: an id
//! known only in another project opens a y/N fork prompt, and a missing path
//! silently starts a new session, so the file must exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{Agent, Ctx, Edit, EditKind, Observation, Plan};
use crate::config::Config;
use crate::record::{AgentKind, Record};

/// The extension source with its placeholders; see `render_asset`.
pub const ASSET: &str = include_str!("../../assets/pi/herdr-wake.ts");

/// The extension as installed: `ASSET` with the binary path and this build's
/// version filled in.
pub fn render_asset(bin: &Path) -> String {
    render(ASSET, bin)
}

/// Fills the `"__HERDR_WAKE_BIN__"` string literal with `bin` (JSON-escaped,
/// which is a valid JS and TS string literal) and `__HERDR_WAKE_VERSION__`
/// with the crate version.
pub(crate) fn render(asset: &str, bin: &Path) -> String {
    let literal = serde_json::Value::String(bin.to_string_lossy().into_owned()).to_string();
    asset
        .replace("\"__HERDR_WAKE_BIN__\"", &literal)
        .replace("__HERDR_WAKE_VERSION__", env!("CARGO_PKG_VERSION"))
}

/// The env var the wake engine sets on every resumed agent.
const SUPERVISED: &str = "HERDR_WAKE_SUPERVISED";

#[derive(Debug, Default)]
pub struct Pi;

#[derive(Deserialize)]
struct StartPayload {
    id: String,
    #[serde(default)]
    file: Option<String>,
    cwd: PathBuf,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    argv: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct EndPayload {
    id: String,
    #[serde(default)]
    signal: bool,
}

/// What a flag needs after it.
#[derive(Clone, Copy)]
enum Arity {
    /// Nothing.
    None,
    /// The next word, whatever it is (Pi takes it when one exists).
    Next,
    /// The next word unless it starts with `-`.
    Plain,
}

/// What happens to a flag on resume.
#[derive(Clone, Copy, PartialEq)]
enum Verdict {
    Restore,
    /// Restored only when `restore_bypass` is on.
    Bypass,
    /// Never restored, or needs validation first.
    Drop,
}

/// Pi's flags (`dist/cli/args.js` in 1.5.2) with their policy from
/// `docs/research/agents-pi-opencode.md` section 6.
const FLAGS: &[(&[&str], Arity, Verdict)] = &[
    // restore
    (&["--provider"], Arity::Next, Verdict::Restore),
    (&["--model"], Arity::Next, Verdict::Restore),
    (&["--models"], Arity::Next, Verdict::Restore),
    (&["--thinking"], Arity::Next, Verdict::Restore),
    (&["--session-dir"], Arity::Next, Verdict::Restore),
    (&["--tools", "-t"], Arity::Next, Verdict::Restore),
    (&["--exclude-tools", "-xt"], Arity::Next, Verdict::Restore),
    (&["--tui-mode"], Arity::Plain, Verdict::Restore),
    (&["--no-tools", "-nt"], Arity::None, Verdict::Restore),
    (
        &["--no-builtin-tools", "-nbt"],
        Arity::None,
        Verdict::Restore,
    ),
    (&["--no-skills", "-ns"], Arity::None, Verdict::Restore),
    (
        &["--no-context-files", "-nc"],
        Arity::None,
        Verdict::Restore,
    ),
    (&["--offline"], Arity::None, Verdict::Restore),
    // project trust: granting it is the bypass class; denying it only tightens
    (&["--approve", "-a"], Arity::None, Verdict::Bypass),
    (&["--no-approve", "-na"], Arity::None, Verdict::Restore),
    // needs validation: loads code or instructions, or may disable herdr-wake
    (&["--extension", "-e"], Arity::Next, Verdict::Drop),
    (&["--no-extensions", "-ne"], Arity::None, Verdict::Drop),
    (&["--skill"], Arity::Next, Verdict::Drop),
    (&["--prompt-template"], Arity::Next, Verdict::Drop),
    (&["--theme"], Arity::Next, Verdict::Drop),
    (&["--system-prompt"], Arity::Next, Verdict::Drop),
    (&["--append-system-prompt"], Arity::Next, Verdict::Drop),
    // never
    (&["--session"], Arity::Next, Verdict::Drop),
    (&["--session-id"], Arity::Next, Verdict::Drop),
    (&["--fork"], Arity::Next, Verdict::Drop),
    (&["--continue", "-c"], Arity::None, Verdict::Drop),
    (&["--resume", "-r"], Arity::None, Verdict::Drop),
    (&["--no-session"], Arity::None, Verdict::Drop),
    (&["--print", "-p"], Arity::None, Verdict::Drop),
    (&["--mode"], Arity::Plain, Verdict::Drop),
    (&["--api-key"], Arity::Next, Verdict::Drop),
    (&["--name", "-n"], Arity::Next, Verdict::Drop),
    (&["--export"], Arity::Next, Verdict::Drop),
    (&["--use-theme"], Arity::Plain, Verdict::Drop),
    (&["--list-models"], Arity::Plain, Verdict::Drop),
    (&["--help", "-h"], Arity::None, Verdict::Drop),
    (&["--version", "-v"], Arity::None, Verdict::Drop),
    (&["--no-mcp"], Arity::None, Verdict::Drop),
    (
        &["--no-prompt-templates", "-np"],
        Arity::None,
        Verdict::Drop,
    ),
    (&["--no-themes"], Arity::None, Verdict::Drop),
    (&["--verbose"], Arity::None, Verdict::Drop),
];

fn valid_value(flag: &str, value: &str) -> bool {
    match flag {
        "--tui-mode" => value == "regular" || value == "fullscreen",
        _ => true,
    }
}

fn filter_flags(argv: &[String], cfg: &Config) -> Vec<String> {
    filter_flags_noting(argv, cfg, &mut Vec::new())
}

/// `filter_flags`, adding to `withheld` the names of flags left out because
/// their value holds a URL with a user name or password.
fn filter_flags_noting(argv: &[String], cfg: &Config, withheld: &mut Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i].as_str();
        if arg == "--" {
            break; // the rest are messages
        }
        let next = argv.get(i + 1).map(String::as_str);
        let Some(&(_, arity, verdict)) = FLAGS.iter().find(|(names, _, _)| names.contains(&arg))
        else {
            // An unknown long flag (an extension's) takes a plain next word.
            if arg.starts_with("--")
                && !arg.contains('=')
                && next.is_some_and(|n| !n.starts_with('-') && !n.starts_with('@'))
            {
                i += 1;
            }
            i += 1;
            continue;
        };
        let value = match arity {
            Arity::None => None,
            Arity::Next => match next {
                Some(v) => Some(v),
                None => break, // Pi ignores a value flag with nothing after it
            },
            Arity::Plain => next.filter(|v| !v.starts_with('-')),
        };
        let keep = match verdict {
            Verdict::Restore => true,
            Verdict::Bypass => cfg.restore_bypass,
            Verdict::Drop => false,
        };
        let takes_value = !matches!(arity, Arity::None);
        let usable = !takes_value || value.is_some_and(|v| valid_value(arg, v));
        if keep && usable && value.is_some_and(crate::trust::has_url_userinfo) {
            withheld.push(arg.to_string());
        } else if keep && usable {
            out.push(arg.to_string());
            out.extend(value.map(str::to_string));
        }
        i += 1 + usize::from(value.is_some());
    }
    out
}

/// `PI_CODING_AGENT_DIR` (with `~` expanded) or `<home>/.pi/agent`.
fn agent_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    match env.get("PI_CODING_AGENT_DIR").map(String::as_str) {
        Some("") | None => home.join(".pi/agent"),
        Some("~") => home.to_path_buf(),
        Some(v) => match v.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(v),
        },
    }
}

/// `~` or `~/x` under `home`, anything else as given.
fn expand_home(v: &str, home: &Path) -> PathBuf {
    match v.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if v == "~" => home.to_path_buf(),
        None => PathBuf::from(v),
    }
}

/// The folders Pi keeps this record's session files in:
/// `PI_CODING_AGENT_SESSION_DIR` when set, else
/// `${PI_CODING_AGENT_DIR:-~/.pi/agent}/sessions`, from the record's
/// environment and then the caller's; plus the folder of a restorable
/// `--session-dir` flag, which moves a session's files there.
pub fn session_dirs(rec: &Record, env: &BTreeMap<String, String>, home: &Path) -> Vec<PathBuf> {
    let mut merged = env.clone();
    merged.extend(rec.env.iter().map(|(k, v)| (k.clone(), v.clone())));
    let mut dirs = vec![match merged.get("PI_CODING_AGENT_SESSION_DIR") {
        Some(v) if !v.is_empty() => expand_home(v, home),
        _ => agent_dir(&merged, home).join("sessions"),
    }];
    let mut flags = rec.flags.iter();
    while let Some(f) = flags.next() {
        if f == "--session-dir" {
            if let Some(v) = flags.next() {
                let p = expand_home(v, home);
                dirs.push(if p.is_absolute() { p } else { rec.cwd.join(p) });
            }
        }
    }
    dirs
}

/// The session file a resume would hand to Pi must be an absolute path to
/// a regular file (not a symlink) that the user owns, inside one of
/// `session_dirs`.
fn session_file_is_safe(file: &Path, rec: &Record, ctx: &Ctx) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if !file.is_absolute() {
        return Err("its path is not absolute".into());
    }
    let md = std::fs::symlink_metadata(file).map_err(|e| format!("it cannot be read ({e})"))?;
    if !md.file_type().is_file() {
        return Err("it is not a regular file".into());
    }
    if md.uid() != crate::trust::current_uid() {
        return Err("it is not owned by you".into());
    }
    let real = file
        .canonicalize()
        .map_err(|e| format!("it cannot be resolved ({e})"))?;
    let inside = session_dirs(rec, ctx.env, ctx.home)
        .iter()
        .filter_map(|d| d.canonicalize().ok())
        .any(|d| real.starts_with(d));
    if inside {
        Ok(())
    } else {
        Err("it is not in Pi's session folder".into())
    }
}

impl Agent for Pi {
    fn kind(&self) -> AgentKind {
        AgentKind::Pi
    }

    fn observe(&self, event: &str, payload: &[u8], _ctx: &mut Ctx) -> Observation {
        match event {
            "start" => {
                let Ok(p) = serde_json::from_slice::<StartPayload>(payload) else {
                    return Observation::Ignore("bad payload");
                };
                if p.id.is_empty() {
                    return Observation::Ignore("no session id");
                }
                if !p.cwd.is_absolute() {
                    return Observation::Ignore("bad cwd");
                }
                Observation::Started {
                    session_id: p.id,
                    session_file: p.file.filter(|f| !f.is_empty()).map(PathBuf::from),
                    cwd: p.cwd,
                    terminal: None,
                    agent_pid: p.pid,
                    argv: p.argv.filter(|a| !a.is_empty()),
                }
            }
            "end" => {
                let Ok(p) = serde_json::from_slice::<EndPayload>(payload) else {
                    return Observation::Ignore("bad payload");
                };
                if p.id.is_empty() {
                    return Observation::Ignore("no session id");
                }
                if p.signal {
                    return Observation::Ignore("signal");
                }
                Observation::Ended {
                    session_id: p.id,
                    deliberate: Some(true),
                }
            }
            _ => Observation::Ignore("unknown event"),
        }
    }

    fn restorable_flags(&self, argv: &[String], cfg: &Config) -> Vec<String> {
        filter_flags(argv, cfg)
    }

    fn withheld_flags(&self, argv: &[String], cfg: &Config) -> Vec<String> {
        let mut withheld = Vec::new();
        filter_flags_noting(argv, cfg, &mut withheld);
        withheld
    }

    fn resume_plan(&self, rec: &Record, ctx: &mut Ctx) -> Plan {
        if !rec.cwd.is_dir() {
            return Plan::Permanent("folder missing".into());
        }
        let Some(file) = rec.session_file.as_ref().filter(|f| f.is_file()) else {
            return Plan::Permanent("session not found".into());
        };
        if let Err(why) = session_file_is_safe(file, rec, ctx) {
            return Plan::Permanent(format!("the recorded session file is not usable: {why}"));
        }
        let bin = rec.bin.to_string_lossy().into_owned();
        let mut launch = vec![bin.clone()];
        launch.extend(rec.flags.iter().cloned());
        let mut argv = vec![bin];
        argv.extend(filter_flags(&launch, ctx.cfg));
        argv.push("--session".into());
        argv.push(file.to_string_lossy().into_owned());
        let mut env = rec.env.clone();
        env.insert(SUPERVISED.into(), "1".into());
        Plan::Run {
            argv,
            env,
            cwd: rec.cwd.clone(),
            before: Vec::new(),
            confirm_within_ms: None,
        }
    }

    fn present(&self, ctx: &Ctx) -> bool {
        super::installed(self.kind(), &agent_dir(ctx.env, ctx.home), ctx.env)
    }

    fn integration(&self, bin: &Path, ctx: &mut Ctx) -> Vec<Edit> {
        vec![Edit {
            path: agent_dir(ctx.env, ctx.home).join("extensions/herdr-wake.ts"),
            kind: EditKind::WriteFile {
                body: render_asset(bin),
                mode: 0o644,
            },
        }]
    }
}
