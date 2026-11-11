//! The agent seam: one adapter per coding agent turns hook payloads into
//! observations, rebuilds resume commands from records, and lists the files
//! and edits its integration needs. Adapters: `claude` (Task 6), `codex`
//! (Task 7), `pi` and `opencode` (Task 8). Each adapter type implements
//! `Default`, which `all` and `get` use to build it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::core::{Core, TerminalId};
use crate::platform::{Platform, ProcInfo};
use crate::record::{AgentKind, Record};

pub mod claude;
pub mod codex;
pub mod opencode;
pub mod pi;

/// What an adapter may consult: the OS, core, the caller's environment,
/// the home folder and the settings.
pub struct Ctx<'a> {
    pub platform: &'a dyn Platform,
    pub core: &'a mut dyn Core,
    pub env: &'a BTreeMap<String, String>,
    pub home: &'a Path,
    pub cfg: &'a Config,
}

/// What a hook event means for the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// Not a session worth recording (sub-agent, one-shot run, ...); the
    /// text is a short reason for the log.
    Ignore(&'static str),
    /// A session started or changed id in a terminal. `terminal` is `None`
    /// when the hook's own environment names the terminal; `agent_pid` is the
    /// agent process when the adapter can tell. `argv` is the agent's launch
    /// command line when its payload carries one (`argv[0]` only stands for
    /// the program): it wins over the process table, which shows `node` and
    /// the wrong words for agents that are scripts.
    Started {
        session_id: String,
        session_file: Option<PathBuf>,
        cwd: PathBuf,
        terminal: Option<TerminalId>,
        agent_pid: Option<u32>,
        argv: Option<Vec<String>>,
    },
    /// The session ended. `deliberate`: `Some(true)` the user quit,
    /// `Some(false)` it was killed, `None` unknown (the watcher decides).
    Ended {
        session_id: String,
        deliberate: Option<bool>,
    },
}

/// How to resume a record, rebuilt at wake time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Run `argv` with `env` in `cwd`, after running each `before` command to
    /// completion. `confirm_within_ms`: the resume counts as failed unless
    /// the agent reports the same session within this time.
    Run {
        argv: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
        before: Vec<Vec<String>>,
        confirm_within_ms: Option<u64>,
    },
    /// Cannot resume and retrying will not help; the text says why.
    Permanent(String),
}

/// One change to another tool's file that install applies and uninstall
/// reverses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub path: PathBuf,
    pub kind: EditKind,
}

/// The kinds of change setup knows how to make and undo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditKind {
    /// Create or replace a file herdr-wake owns, with Unix permission bits
    /// `mode` (for example `0o644`). Uninstall removes the file.
    WriteFile { body: String, mode: u32 },
    /// A command hook in a JSON settings file shaped like Claude's
    /// `settings.json` and Codex's `hooks.json`:
    /// `hooks.<event>[] = {matcher?, hooks: [{type: "command", command, timeout}]}`.
    /// `timeout` is in seconds. Uninstall removes entries whose command
    /// equals `command`.
    JsonHook {
        event: String,
        matcher: Option<String>,
        command: String,
        timeout: u32,
    },
    /// The string `value` in the top-level JSONC array `key` (for example
    /// OpenCode's `plugins` in `cli.json`), added once. Uninstall removes it.
    JsoncListEntry { key: String, value: String },
}

/// One coding agent's rules.
pub trait Agent {
    fn kind(&self) -> AgentKind;
    /// Interprets hook `event` (the name used in the hook command line, for
    /// example `start` or `end`) with its stdin `payload`.
    fn observe(&self, event: &str, payload: &[u8], ctx: &mut Ctx) -> Observation;
    /// The launch flags worth restoring from the agent's `argv` as launched
    /// (`argv[0]` included), filtered by the flag policy and `cfg`.
    fn restorable_flags(&self, argv: &[String], cfg: &Config) -> Vec<String>;
    /// The names (never the values) of flags in `argv` that are left out
    /// of the record because their value could hold a secret: inline JSON,
    /// a credential-looking override, a URL with a password. For the log.
    fn withheld_flags(&self, _argv: &[String], _cfg: &Config) -> Vec<String> {
        Vec::new()
    }
    fn resume_plan(&self, rec: &Record, ctx: &mut Ctx) -> Plan;
    /// The files and edits that make the agent call `bin` from its hooks.
    fn integration(&self, bin: &Path, ctx: &mut Ctx) -> Vec<Edit>;
    /// True when the agent is installed for this user (see `installed`).
    /// `install` sets up only agents that are present, and creates nothing
    /// for the others.
    fn present(&self, _ctx: &Ctx) -> bool {
        true
    }
    /// Edits an earlier `install` may have made that `integration` no
    /// longer returns (another layout of the same agent). `uninstall`
    /// reverts these too.
    fn former_integration(&self, _bin: &Path, _ctx: &mut Ctx) -> Vec<Edit> {
        Vec::new()
    }
}

/// The first executable file called `name` in the absolute folders of the
/// `PATH` in `env`.
pub fn on_path(name: &str, env: &BTreeMap<String, String>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    env.get("PATH")?
        .split(':')
        .filter(|d| d.starts_with('/'))
        .map(|d| Path::new(d).join(name))
        .find(|f| {
            std::fs::metadata(f).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// True when the agent `kind` is installed for this user: its config
/// folder `config_dir` already exists, or its program is on `PATH`.
pub fn installed(kind: AgentKind, config_dir: &Path, env: &BTreeMap<String, String>) -> bool {
    config_dir.is_dir() || on_path(kind.as_str(), env).is_some()
}

/// One adapter per agent, in `AgentKind::ALL` order.
pub fn all() -> Vec<Box<dyn Agent>> {
    AgentKind::ALL.into_iter().map(get).collect()
}

/// The adapter for `kind`.
pub fn get(kind: AgentKind) -> Box<dyn Agent> {
    match kind {
        AgentKind::Claude => Box::<claude::Claude>::default(),
        AgentKind::Codex => Box::<codex::Codex>::default(),
        AgentKind::Pi => Box::<pi::Pi>::default(),
        AgentKind::OpenCode => Box::<opencode::OpenCode>::default(),
    }
}

/// Program names that run a script given as their first argument.
const INTERPRETERS: [&str; 8] = [
    "node", "bun", "deno", "sh", "bash", "zsh", "dash", "python3",
];

/// True when `exe` is a script interpreter (`node`, `bun`, `sh`, ...), so
/// the process's own executable says nothing about which agent it runs.
pub fn is_interpreter(exe: &Path) -> bool {
    exe.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| INTERPRETERS.contains(&n))
}

/// Which agent process `p` is, by its name: `argv[0]` or the executable
/// named after the agent, or an interpreter running a script named after it.
/// `None` for anything else.
pub fn identify(p: &ProcInfo) -> Option<AgentKind> {
    if claude::looks_like_claude(p) {
        return Some(AgentKind::Claude);
    }
    if codex::codex_args(p).is_some() {
        return Some(AgentKind::Codex);
    }
    let base = |s: &str| {
        let name = s.rsplit('/').next().unwrap_or(s);
        name.strip_suffix(".js").unwrap_or(name).to_string()
    };
    let argv0 = p.argv.first().map(|a| base(a));
    let exe = p.exe.to_str().map(base);
    let script = is_interpreter(&p.exe)
        .then(|| p.argv.get(1).map(|a| base(a)))
        .flatten();
    [AgentKind::Pi, AgentKind::OpenCode].into_iter().find(|k| {
        [&argv0, &exe, &script]
            .into_iter()
            .any(|n| n.as_deref() == Some(k.as_str()))
    })
}
