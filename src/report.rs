//! Read-only reports: `status` and `doctor` (Task 12).
//!
//! Neither report ever runs `hc`: any `hc` command starts herdr-core. Core is
//! reached only through the `Core` in the caller's `Ctx` (a socket connection
//! that never starts a server) and `Platform::core_pid`. Everything else that
//! touches the machine goes through `Probes`, so tests drive every check with
//! `FakeProbes`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::agents::Ctx;
use crate::core::{Core, CoreError, Hello, Terminal, TerminalId};
use crate::platform::{BootState, Platform};
use crate::record::{AgentKind, Record, State};

// ---------------------------------------------------------------- status

/// One terminal that carries a readable record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRow {
    pub terminal: TerminalId,
    /// `None` for a terminal that is switched off and has no record.
    pub agent: Option<AgentKind>,
    pub session_id: String,
    pub cwd: PathBuf,
    /// `running`, `ended`, or `failed: <reason>`.
    pub state: String,
    /// Resume attempts used on this boot.
    pub attempts: u8,
    /// The record's agent process is alive and was recorded on this boot.
    pub running: bool,
}

/// One row per live terminal whose `wake` label holds a record this build
/// can read, and one per terminal that is switched off (`herdr-wake off`),
/// sorted by terminal number.
pub fn status(ctx: &mut Ctx) -> Result<Vec<StatusRow>, CoreError> {
    let boot = ctx.platform.boot_id();
    let copies = crate::paths::records_dir(ctx.env, ctx.home);
    let mut rows: Vec<StatusRow> = ctx
        .core
        .list(false)?
        .into_iter()
        .filter_map(|t| {
            let off = crate::record::is_off(&t);
            let Some(rec) = record_of(&t) else {
                return off.then(|| StatusRow {
                    terminal: t.address.clone(),
                    agent: None,
                    session_id: String::new(),
                    cwd: PathBuf::new(),
                    state: OFF.to_string(),
                    attempts: 0,
                    running: false,
                });
            };
            let running = agent_live(&rec, ctx.platform, &boot);
            let attempts = rec.attempts_on(&boot);
            let state = if off {
                OFF.to_string()
            } else if untrusted(&rec, &t, &copies) {
                UNTRUSTED.to_string()
            } else {
                state_text(&rec.state)
            };
            Some(StatusRow {
                terminal: t.address,
                agent: Some(rec.agent),
                session_id: rec.session_id,
                cwd: rec.cwd,
                state,
                attempts,
                running,
            })
        })
        .collect();
    rows.sort_by(|a, b| terminal_order(&a.terminal, &b.terminal));
    Ok(rows)
}

/// The record in a terminal's labels, when this build can read it.
pub(crate) fn record_of(t: &Terminal) -> Option<Record> {
    crate::record::of_terminal(t)
}

/// The state `status` shows for a terminal that is switched off.
pub const OFF: &str = "off";

/// The state `status` shows for a record herdr-wake will not act on.
pub const UNTRUSTED: &str = "untrusted";

/// True when `rec` would resume by its state but does not match the local
/// copy in `copies` (`paths::records_dir`): the label was written or
/// changed by something other than herdr-wake, or the copy is gone.
pub(crate) fn untrusted(rec: &Record, t: &Terminal, copies: &Path) -> bool {
    rec.state == State::Running
        && !crate::record::copy_matches(copies, &t.address, t.created_ms, rec)
}

/// True when the record's agent process runs now and was recorded on this
/// boot (`boot` is the current boot id).
pub(crate) fn agent_live(rec: &Record, platform: &dyn Platform, boot: &str) -> bool {
    rec.boot_id == boot && rec.agent_proc.is_some_and(|p| platform.alive(&p))
}

fn state_text(s: &State) -> String {
    match s {
        State::Running => "running".into(),
        State::Ended => "ended".into(),
        State::Failed { reason } => format!("failed: {reason}"),
    }
}

/// Orders `t2` before `t10`: by the non-digit prefix, then the number.
pub(crate) fn terminal_order(a: &TerminalId, b: &TerminalId) -> std::cmp::Ordering {
    fn key(t: &TerminalId) -> (&str, u64, &str) {
        let s = t.as_str();
        let split = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
        let n = s[split..].parse().unwrap_or(u64::MAX);
        (&s[..split], n, s)
    }
    key(a).cmp(&key(b))
}

/// The status table, a one-line summary, or (when `core_running` is false)
/// a line saying core is down and that status did not start it.
pub fn render_status(rows: &[StatusRow], core_running: bool) -> String {
    if !core_running {
        return "herdr-core is not running, so no records can be read. \
                status did not start it (any hc command would); it starts at the next \
                boot or login, or when you open Herdr.\n"
            .to_string();
    }
    let mut out = String::new();
    if !rows.is_empty() {
        let header = [
            "TERMINAL", "AGENT", "STATE", "LIVE", "TRIES", "SESSION", "CWD",
        ];
        let body: Vec<[String; 7]> = rows
            .iter()
            .map(|r| {
                // Everything here but the yes/no and the count comes from
                // a label: no control characters reach the terminal.
                let clean = crate::trust::printable;
                [
                    clean(r.terminal.as_str()),
                    r.agent.map_or_else(|| "-".to_string(), |a| a.to_string()),
                    clean(&r.state),
                    if r.running { "yes" } else { "no" }.to_string(),
                    r.attempts.to_string(),
                    clean(&r.session_id),
                    clean(&r.cwd.display().to_string()),
                ]
            })
            .collect();
        let header: [String; 7] = header.map(String::from);
        out.push_str(&table(&header, &body));
    }
    out.push_str(&summary(rows));
    out.push('\n');
    out
}

/// Left-aligned columns two spaces apart; the last column is not padded.
fn table<const N: usize>(header: &[String; N], body: &[[String; N]]) -> String {
    let mut widths = [0usize; N];
    for row in std::iter::once(header).chain(body.iter()) {
        for (w, cell) in widths.iter_mut().zip(row.iter()) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in std::iter::once(header).chain(body.iter()) {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == N {
                line.push_str(cell);
            } else {
                line.push_str(cell);
                let pad = widths[i] - cell.chars().count() + 2;
                line.extend(std::iter::repeat_n(' ', pad));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn summary(rows: &[StatusRow]) -> String {
    let n = rows.len();
    let noun = if n == 1 { "session" } else { "sessions" };
    let running = rows.iter().filter(|r| r.running).count();
    let waiting = rows
        .iter()
        .filter(|r| !r.running && r.state == "running")
        .count();
    let untrusted = rows.iter().filter(|r| r.state == UNTRUSTED).count();
    let off = rows.iter().filter(|r| r.state == OFF).count();
    let ended = rows.iter().filter(|r| r.state == "ended").count();
    let failed = rows
        .iter()
        .filter(|r| r.state.starts_with("failed"))
        .count();
    let parts: Vec<String> = [
        (running, "running"),
        (waiting, "waiting to resume"),
        (untrusted, "untrusted"),
        (off, "off"),
        (ended, "ended"),
        (failed, "failed"),
    ]
    .into_iter()
    .filter(|(c, _)| *c > 0)
    .map(|(c, what)| format!("{c} {what}"))
    .collect();
    if parts.is_empty() {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}: {}", parts.join(", "))
    }
}

// ---------------------------------------------------------------- doctor

/// The oldest herdr-core with the `shell` setting.
pub const MIN_CORE_VERSION: (u32, u32, u32) = (0, 1, 16);

/// The oldest Pi herdr-wake's extension is written for (its signal-handler
/// fix; `docs/research/agents-pi-opencode.md`).
pub const MIN_PI_VERSION: (u32, u32, u32) = (0, 79, 4);

/// The oldest OpenCode 2.x herdr-wake's plugin is written for. Version 1
/// has no known floor.
pub const MIN_OPENCODE_V2_VERSION: (u32, u32, u32) = (2, 0, 28);

/// Time limit for `<agent> --version`.
const VERSION_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a kept, live terminal may show no program before doctor calls
/// it dead (core recovers within a fraction of a second when healthy).
pub const DEAD_AFTER_MS: u64 = 5000;

/// Time limit for each command doctor runs.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The label printed for each check result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

/// The result of one doctor check. `fix` says what to do, when anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub check: &'static str,
    pub detail: String,
    pub fix: Option<String>,
}

impl Finding {
    fn ok(check: &'static str, detail: impl Into<String>) -> Finding {
        Finding {
            level: Level::Ok,
            check,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(check: &'static str, detail: impl Into<String>, fix: Option<String>) -> Finding {
        Finding {
            level: Level::Warn,
            check,
            detail: detail.into(),
            fix,
        }
    }

    fn fail(check: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Finding {
        Finding {
            level: Level::Fail,
            check,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// What a probed command printed and how it exited (`code` is `None` when a
/// signal ended it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub code: Option<i32>,
    pub stdout: String,
}

/// Everything doctor and boot read from the machine beyond `Ctx`.
pub trait Probes {
    /// Runs `argv` with stdin closed and stderr discarded; `None` when it
    /// cannot start or does not finish within `timeout` (it is then killed).
    fn run(&self, argv: &[&str], timeout: Duration) -> Option<CmdOutput>;
    /// The file's text, or `None` when it is missing or unreadable.
    fn read(&self, path: &Path) -> Option<String>;
    /// True when the path exists (file or folder).
    fn exists(&self, path: &Path) -> bool;
    /// True when the path is a file with an execute bit set.
    fn executable(&self, path: &Path) -> bool;
    /// Waits `ms` milliseconds.
    fn sleep_ms(&self, ms: u64);
}

/// `Probes` on the real machine.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemProbes;

impl Probes for SystemProbes {
    fn run(&self, argv: &[&str], timeout: Duration) -> Option<CmdOutput> {
        use std::io::Read;
        use std::process::{Command, Stdio};
        use std::time::Instant;

        let (prog, args) = argv.split_first()?;
        let mut child = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut out = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = out.read_to_string(&mut s);
            s
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        };
        let stdout = reader.join().ok()?;
        Some(CmdOutput {
            code: status.code(),
            stdout,
        })
    }

    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn executable(&self, path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }

    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// In-memory `Probes` for tests: files, executables, scripted commands.
/// A path exists when it was added or when it is a folder above one that
/// was. Sleeping only adds to a counter. Unscripted commands do not start.
#[derive(Debug, Default)]
pub struct FakeProbes {
    files: RefCell<BTreeMap<PathBuf, String>>,
    dirs: RefCell<BTreeSet<PathBuf>>,
    executables: RefCell<BTreeSet<PathBuf>>,
    commands: RefCell<BTreeMap<Vec<String>, CmdOutput>>,
    ran: RefCell<Vec<Vec<String>>>,
    slept: RefCell<u64>,
}

impl FakeProbes {
    pub fn new() -> FakeProbes {
        FakeProbes::default()
    }

    /// Adds or replaces a plain (not executable) file.
    pub fn file(&self, path: impl AsRef<Path>, body: &str) {
        let path = path.as_ref().to_path_buf();
        self.executables.borrow_mut().remove(&path);
        self.files.borrow_mut().insert(path, body.to_string());
    }

    /// Adds an empty folder.
    pub fn dir(&self, path: impl AsRef<Path>) {
        self.dirs.borrow_mut().insert(path.as_ref().to_path_buf());
    }

    /// Adds an executable file.
    pub fn executable(&self, path: impl AsRef<Path>) {
        let path = path.as_ref().to_path_buf();
        self.files.borrow_mut().insert(path.clone(), String::new());
        self.executables.borrow_mut().insert(path);
    }

    /// Removes a file or folder added earlier.
    pub fn remove(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        self.files.borrow_mut().remove(path);
        self.dirs.borrow_mut().remove(path);
        self.executables.borrow_mut().remove(path);
    }

    /// Scripts the result of running `argv`.
    pub fn command(&self, argv: &[&str], code: i32, stdout: &str) {
        self.commands.borrow_mut().insert(
            argv.iter().map(|s| s.to_string()).collect(),
            CmdOutput {
                code: Some(code),
                stdout: stdout.to_string(),
            },
        );
    }

    /// Makes `argv` fail to start again.
    pub fn forget_command(&self, argv: &[&str]) {
        let key: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        self.commands.borrow_mut().remove(&key);
    }

    /// Every command `run` was asked to start, oldest first.
    pub fn commands_run(&self) -> Vec<Vec<String>> {
        self.ran.borrow().clone()
    }

    /// Total milliseconds passed to `sleep_ms`.
    pub fn slept_ms(&self) -> u64 {
        *self.slept.borrow()
    }
}

impl Probes for FakeProbes {
    fn run(&self, argv: &[&str], _timeout: Duration) -> Option<CmdOutput> {
        let key: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        self.ran.borrow_mut().push(key.clone());
        self.commands.borrow().get(&key).cloned()
    }

    fn read(&self, path: &Path) -> Option<String> {
        self.files.borrow().get(path).cloned()
    }

    fn exists(&self, path: &Path) -> bool {
        let below = |p: &PathBuf| p.starts_with(path);
        self.files.borrow().keys().any(below) || self.dirs.borrow().iter().any(below)
    }

    fn executable(&self, path: &Path) -> bool {
        self.executables.borrow().contains(path)
    }

    fn sleep_ms(&self, ms: u64) {
        *self.slept.borrow_mut() += ms;
    }
}

/// A `Core` for when no core is reachable: every call is `NotRunning`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCore;

impl Core for NoCore {
    fn hello(&mut self) -> Result<Hello, CoreError> {
        Err(CoreError::NotRunning)
    }
    fn get(&mut self, _t: &TerminalId) -> Result<Terminal, CoreError> {
        Err(CoreError::NotRunning)
    }
    fn label(
        &mut self,
        _t: &TerminalId,
        _changes: &BTreeMap<String, Option<String>>,
    ) -> Result<Terminal, CoreError> {
        Err(CoreError::NotRunning)
    }
    fn list(&mut self, _all: bool) -> Result<Vec<Terminal>, CoreError> {
        Err(CoreError::NotRunning)
    }
}

/// Which OS's checks doctor runs. Both are compiled everywhere so each can
/// be tested on either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    MacOs,
    Linux,
}

impl Os {
    /// The OS this binary was built for.
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

/// herdr-core's state folder (holder.log, core.log, terminals.json):
/// `$XDG_STATE_HOME/herdr-core/<session>`, default
/// `~/.local/state/herdr-core/default`; `<session>` is `HC_SESSION`.
pub fn core_state_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    let session = var(env, "HC_SESSION")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    var(env, "XDG_STATE_HOME")
        .map_or_else(|| home.join(".local/state"), PathBuf::from)
        .join("herdr-core")
        .join(session)
}

fn var<'a>(env: &'a BTreeMap<String, String>, k: &str) -> Option<&'a str> {
    env.get(k).map(String::as_str).filter(|v| !v.is_empty())
}

/// Every doctor check for this OS. Never runs `hc`.
pub fn doctor(ctx: &mut Ctx, probes: &dyn Probes) -> Vec<Finding> {
    doctor_on(Os::current(), ctx, probes)
}

/// Every doctor check, with the OS-specific ones for `os`.
pub fn doctor_on(os: Os, ctx: &mut Ctx, probes: &dyn Probes) -> Vec<Finding> {
    let mut out = vec![
        check_config(ctx, probes),
        check_core_shell(ctx, probes),
        check_real_shell(ctx, probes),
    ];
    let (core, hello) = check_core(ctx);
    out.push(core);
    out.push(check_core_version(hello.as_ref()));
    let up = hello.is_some();
    out.push(check_dead_terminals(ctx, probes, up));
    let boot_state = ctx.platform.boot_service().state();
    out.push(check_boot_service(os, boot_state));
    out.push(check_boot_env(ctx, probes, up));
    match os {
        Os::MacOs => macos_checks(ctx, probes, boot_state, &mut out),
        Os::Linux => linux_checks(ctx, probes, boot_state, &mut out),
    }
    claude_checks(os, ctx, probes, &mut out);
    codex_checks(ctx, probes, &mut out);
    out.push(check_pi(ctx, probes));
    out.extend(check_pi_version(ctx, probes));
    out.push(check_opencode(ctx, probes));
    out.extend(check_opencode_version(ctx, probes));
    out
}

const INSTALL_FIX: &str = "run `herdr-wake install`";

/// herdr-wake's own `config.toml`: a file that cannot be parsed stops
/// every resume and every recording until it is fixed.
fn check_config(ctx: &Ctx, probes: &dyn Probes) -> Finding {
    const C: &str = "config";
    let path = crate::paths::config_file(ctx.env, ctx.home);
    let shown = path.display();
    let Some(text) = probes.read(&path) else {
        return Finding::ok(C, format!("no {shown}: the defaults apply"));
    };
    match toml_edit::de::from_str::<crate::config::Config>(&text) {
        Ok(_) => Finding::ok(C, format!("{shown} is readable")),
        Err(e) => Finding::fail(
            C,
            format!(
                "{shown} cannot be parsed, so nothing is resumed and nothing is recorded until it is fixed: {}",
                e.to_string().trim()
            ),
            format!("fix the line named above in {shown}, or delete the file to go back to the defaults"),
        ),
    }
}

fn check_core_shell(ctx: &Ctx, probes: &dyn Probes) -> Finding {
    const C: &str = "core shell";
    let config = crate::setup::core_config_path(ctx.env, ctx.home);
    let shown = config.display();
    let Some(text) = probes.read(&config) else {
        return Finding::fail(
            C,
            format!("core config {shown} not found: core starts plain shells, so nothing resumes"),
            INSTALL_FIX,
        );
    };
    let doc = match text.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            return Finding::fail(
                C,
                format!("cannot parse {shown}: {}", e.to_string().trim()),
                format!("fix the TOML in {shown} by hand, then run `herdr-wake doctor` again"),
            )
        }
    };
    let shell: Option<Vec<String>> = doc.get("shell").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect()
    });
    let Some(shell) = shell else {
        return Finding::fail(
            C,
            format!(
                "{shown} has no `shell` setting: kept terminals come back as plain shells and nothing resumes"
            ),
            INSTALL_FIX,
        );
    };
    if shell.len() != 2 || shell[1] != "shell" {
        return Finding::fail(
            C,
            format!(
                "core's shell is {}, not herdr-wake: kept terminals come back as plain shells and nothing resumes",
                toml_array(&shell)
            ),
            INSTALL_FIX,
        );
    }
    let bin = Path::new(&shell[0]);
    if !bin.is_absolute() {
        return Finding::fail(
            C,
            format!(
                "core's shell program {} is not an absolute path; core needs one",
                bin.display()
            ),
            INSTALL_FIX,
        );
    }
    if !probes.exists(bin) {
        let restore = match &ctx.cfg.real_shell {
            Some(argv) if !argv.is_empty() => format!("set `shell = {}`", toml_array(argv)),
            _ => "delete the `shell` line".to_string(),
        };
        return Finding::fail(
            C,
            format!(
                "core's shell {} does not exist: every kept terminal stays dead (core retries it forever and never falls back)",
                bin.display()
            ),
            format!(
                "put herdr-wake back at {}, or restore core's shell: in {shown}, {restore}. Core applies the change within 60 s.",
                bin.display()
            ),
        );
    }
    if !probes.executable(bin) {
        return Finding::fail(
            C,
            format!(
                "core's shell {} is not executable: every kept terminal stays dead",
                bin.display()
            ),
            format!("chmod +x '{}'", bin.display()),
        );
    }
    Finding::ok(
        C,
        format!(
            "core starts {} shell for every kept terminal ({shown})",
            bin.display()
        ),
    )
}

/// A TOML array of strings, as it would appear in a config file.
fn toml_array(items: &[String]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|s| toml_edit::Value::from(s.as_str()).to_string())
        .collect();
    format!("[{}]", quoted.join(", "))
}

/// `name` as given when it contains a slash, else the first executable
/// match in the `PATH` folders.
fn resolve(name: &str, env: &BTreeMap<String, String>, probes: &dyn Probes) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return probes.executable(&p).then_some(p);
    }
    var(env, "PATH")?
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| probes.executable(p))
}

fn check_real_shell(ctx: &Ctx, probes: &dyn Probes) -> Finding {
    const C: &str = "real shell";
    match &ctx.cfg.real_shell {
        Some(argv) if !argv.is_empty() => match resolve(&argv[0], ctx.env, probes) {
            Some(p) => {
                let shown = if Path::new(&argv[0]) == p {
                    String::new()
                } else {
                    format!(" ({})", p.display())
                };
                Finding::ok(C, format!("terminals end in {}{shown}", argv.join(" ")))
            }
            None => Finding::warn(
                C,
                format!(
                    "the saved real shell {} was not found: terminals fall back to $SHELL, then /bin/sh",
                    argv[0]
                ),
                Some("set `real_shell` in herdr-wake's config.toml to your shell".into()),
            ),
        },
        _ => match var(ctx.env, "SHELL").and_then(|s| resolve(s, ctx.env, probes)) {
            Some(p) => Finding::ok(
                C,
                format!("no saved shell: terminals use $SHELL ({})", p.display()),
            ),
            None => Finding::warn(
                C,
                "no saved shell and $SHELL is unset or missing: terminals fall back to /bin/sh",
                Some("set `real_shell` in herdr-wake's config.toml to your shell".into()),
            ),
        },
    }
}

fn check_core(ctx: &mut Ctx) -> (Finding, Option<Hello>) {
    const C: &str = "core";
    let pid = ctx.platform.core_pid();
    let pid_text = pid.map_or("pid unknown".to_string(), |p| format!("pid {p}"));
    match ctx.core.hello() {
        Ok(h) => (
            Finding::ok(C, format!("running ({pid_text}, version {})", h.version)),
            Some(h),
        ),
        Err(CoreError::NotRunning) => match pid {
            None => (
                Finding::warn(
                    C,
                    "herdr-core is not running (doctor did not start it)",
                    Some(
                        "it starts with the boot service at the next boot or login; `herdr-wake boot` starts it now"
                            .into(),
                    ),
                ),
                None,
            ),
            Some(p) => (
                Finding::warn(
                    C,
                    format!(
                        "a herdr-core process runs (pid {p}) but nothing answers on the socket at {}: it may use another HOME, XDG_* or HC_SESSION",
                        crate::core::socket::socket_path(ctx.env, ctx.home).display()
                    ),
                    None,
                ),
                None,
            ),
        },
        Err(CoreError::Timeout) => (
            Finding::fail(
                C,
                format!("herdr-core ({pid_text}) accepts connections but does not answer: it may be hung"),
                format!(
                    "look at core.log in {}; `hc server restart` restarts it",
                    core_state_dir(ctx.env, ctx.home).display()
                ),
            ),
            None,
        ),
        Err(e) => (
            Finding::fail(
                C,
                format!("herdr-core ({pid_text}) answered with an error: {e}"),
                "run `herdr-wake doctor` again; if it persists, restart core with `hc server restart`",
            ),
            None,
        ),
    }
}

/// `major.minor.patch` from a version string, ignoring any `-pre` or
/// `+build` suffix; a missing part counts as 0.
pub(crate) fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some((major, minor, patch))
}

fn check_core_version(hello: Option<&Hello>) -> Finding {
    const C: &str = "core version";
    let (a, b, c) = MIN_CORE_VERSION;
    let min = format!("{a}.{b}.{c}");
    let Some(h) = hello else {
        return Finding::ok(C, "skipped: core is not running");
    };
    match parse_version(&h.version) {
        Some(v) if v >= MIN_CORE_VERSION => {
            Finding::ok(C, format!("{} (minimum {min})", h.version))
        }
        Some(_) => Finding::fail(
            C,
            format!(
                "herdr-core {} is older than {min}, the first version with the `shell` setting herdr-wake needs",
                h.version
            ),
            "update herdr-core",
        ),
        None => Finding::warn(
            C,
            format!("cannot read core's version {:?}; herdr-wake needs {min} or newer", h.version),
            None,
        ),
    }
}

/// Kept, live terminals with no program: what a broken `shell` setting looks
/// like through core's interface.
fn dead_terminals(core: &mut dyn Core) -> Option<Vec<TerminalId>> {
    let terms = core.list(false).ok()?;
    Some(
        terms
            .into_iter()
            .filter(|t| t.keep && t.state == Terminal::LIVE && t.pid.is_none())
            .map(|t| t.address)
            .collect(),
    )
}

fn check_dead_terminals(ctx: &mut Ctx, probes: &dyn Probes, up: bool) -> Finding {
    const C: &str = "dead terminals";
    if !up {
        return Finding::ok(C, "skipped: core is not running");
    }
    let Some(mut dead) = dead_terminals(ctx.core) else {
        return Finding::warn(C, "could not list core's terminals", None);
    };
    // A healthy recovery gives a terminal its program within a fraction of a
    // second, so watch for longer than DEAD_AFTER_MS before deciding.
    let mut waited = 0;
    while !dead.is_empty() && waited <= DEAD_AFTER_MS {
        probes.sleep_ms(1000);
        waited += 1000;
        let Some(now) = dead_terminals(ctx.core) else {
            break;
        };
        dead.retain(|t| now.contains(t));
    }
    if dead.is_empty() {
        return Finding::ok(C, "every kept terminal has a running program");
    }
    dead.sort_by(terminal_order);
    let names: Vec<&str> = dead.iter().map(TerminalId::as_str).collect();
    let log = core_state_dir(ctx.env, ctx.home).join("holder.log");
    let quote = match probes.read(&log) {
        Some(text) => match text.lines().rev().find(|l| !l.trim().is_empty()) {
            Some(line) => format!("last line of {}: {}", log.display(), line.trim()),
            None => format!("{} is empty", log.display()),
        },
        None => format!("{} not found", log.display()),
    };
    Finding::fail(
        C,
        format!(
            "{} {} had no program for over {} s: core cannot start the shell; {quote}",
            if names.len() == 1 {
                "kept terminal"
            } else {
                "kept terminals"
            },
            if names.len() == 1 {
                format!("{} has", names[0])
            } else {
                format!("{} have", names.join(", "))
            },
            DEAD_AFTER_MS / 1000
        ),
        "fix core's shell setting (see `core shell`); core retries within 60 s",
    )
}

fn check_boot_service(os: Os, state: BootState) -> Finding {
    const C: &str = "boot service";
    let enable = match os {
        Os::MacOs => "launchctl enable gui/$(id -u)/dev.heretik.herdr-wake",
        Os::Linux => "systemctl --user enable herdr-wake.service",
    };
    match state {
        BootState::NotInstalled => Finding::warn(
            C,
            "not installed: nothing starts herdr-core at boot or login",
            Some(INSTALL_FIX.into()),
        ),
        BootState::Disabled => Finding::warn(
            C,
            "installed but disabled: nothing starts herdr-core at boot or login",
            Some(enable.into()),
        ),
        BootState::AtLogin => Finding::ok(C, "installed, starts core at login"),
        BootState::BeforeLogin => Finding::ok(C, "installed, starts core before login"),
    }
}

fn check_boot_env(ctx: &mut Ctx, probes: &dyn Probes, up: bool) -> Finding {
    const C: &str = "boot env";
    let path = crate::paths::boot_env_file(ctx.env, ctx.home);
    let Some(text) = probes.read(&path) else {
        return Finding::warn(
            C,
            format!(
                "{} not found: the boot job starts core with a minimal PATH, so agents may not be found",
                path.display()
            ),
            Some(INSTALL_FIX.into()),
        );
    };
    let env = crate::setup::parse_boot_env(&text);
    let Some(path_var) = env.get("PATH") else {
        return Finding::warn(
            C,
            format!("{} has no PATH", path.display()),
            Some(INSTALL_FIX.into()),
        );
    };
    if !up {
        return Finding::ok(
            C,
            format!(
                "{} is present (agent folders not checked: core is not running)",
                path.display()
            ),
        );
    }
    let have: BTreeSet<&str> = path_var.split(':').collect();
    let mut missing = BTreeSet::new();
    for t in ctx.core.list(false).unwrap_or_default() {
        let Some(rec) = record_of(&t) else { continue };
        if let Some(dir) = rec.bin.parent().filter(|_| rec.bin.is_absolute()) {
            let dir = dir.display().to_string();
            if !have.contains(dir.as_str()) {
                missing.insert(dir);
            }
        }
    }
    if missing.is_empty() {
        return Finding::ok(
            C,
            format!("{} PATH covers every recorded agent", path.display()),
        );
    }
    let missing: Vec<String> = missing.into_iter().collect();
    Finding::warn(
        C,
        format!(
            "PATH in {} lacks {}, where recorded agents live: tools they call may not be found after a boot",
            path.display(),
            missing.join(", ")
        ),
        Some(format!(
            "add them to PATH in {}, or rerun `herdr-wake install` from a shell where the agents are on PATH",
            path.display()
        )),
    )
}

fn user(env: &BTreeMap<String, String>) -> Option<&str> {
    var(env, "USER").or_else(|| var(env, "LOGNAME"))
}

const NO_BOOT_JOB: &str = "only when herdr-core starts some other way (opening Herdr, or any hc command): no boot service runs it";

fn macos_checks(ctx: &Ctx, probes: &dyn Probes, boot: BootState, out: &mut Vec<Finding>) {
    let filevault = probes
        .run(&["fdesetup", "isactive"], PROBE_TIMEOUT)
        .map(|o| o.stdout.trim().to_string());
    let fv_on = match filevault.as_deref() {
        Some("true") => {
            out.push(Finding::ok(
                "filevault",
                "FileVault is on: auto-login is unavailable, and a before-login job waits for the disk to be unlocked",
            ));
            true
        }
        Some("false") => {
            out.push(Finding::ok("filevault", "FileVault is off"));
            false
        }
        _ => {
            out.push(Finding::warn(
                "filevault",
                "could not check FileVault (`fdesetup isactive` failed)",
                None,
            ));
            false
        }
    };
    let auto = probes
        .run(
            &[
                "defaults",
                "read",
                "/Library/Preferences/com.apple.loginwindow",
                "autoLoginUser",
            ],
            PROBE_TIMEOUT,
        )
        .filter(|o| o.code == Some(0))
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty());
    let me = user(ctx.env);
    let auto_me = auto.as_deref().is_some_and(|a| Some(a) == me);
    out.push(match &auto {
        Some(a) if auto_me => Finding::ok("auto-login", format!("auto-login is on for {a}")),
        Some(a) => Finding::ok(
            "auto-login",
            format!("auto-login is on for another user ({a})"),
        ),
        None => Finding::ok("auto-login", "auto-login is off"),
    });
    const C: &str = "wake time";
    out.push(match boot {
        BootState::NotInstalled | BootState::Disabled => Finding::warn(
            C,
            format!("agents come back {NO_BOOT_JOB}"),
            Some(INSTALL_FIX.into()),
        ),
        BootState::AtLogin if auto_me => Finding::ok(C, "agents come back at boot via auto-login"),
        BootState::AtLogin => Finding::ok(
            C,
            "agents come back at login (when you log in at the Mac's screen), or when the first client connects",
        ),
        BootState::BeforeLogin => {
            let when = if fv_on {
                "agents come back before login (daemon), once the disk is unlocked (FileVault is on)"
            } else {
                "agents come back at boot, before login (daemon)"
            };
            Finding::warn(
                C,
                format!(
                    "{when}. Claude Code keeps its login in the macOS Keychain, which stays locked until someone logs in at the screen, so Claude sessions resumed before login may come up signed out"
                ),
                Some(
                    "give Claude a CLAUDE_CODE_OAUTH_TOKEN (from `claude setup-token`) or the credentials file, or reinstall without --before-login"
                        .into(),
                ),
            )
        }
    });
}

fn linux_checks(ctx: &Ctx, probes: &dyn Probes, boot: BootState, out: &mut Vec<Finding>) {
    out.push(
        match probes
            .read(Path::new("/proc/1/comm"))
            .map(|s| s.trim().to_string())
        {
            Some(s) if s == "systemd" => Finding::ok("init", "PID 1 is systemd"),
            Some(s) => Finding::fail(
                "init",
                format!("PID 1 is {s}, not systemd: the user service cannot start herdr-core at boot"),
                "start `herdr-wake boot` yourself at boot (for example a cron @reboot entry); not supported in v1",
            ),
            None => Finding::warn("init", "could not read /proc/1/comm", None),
        },
    );
    let linger =
        user(ctx.env).map(|u| probes.exists(&Path::new("/var/lib/systemd/linger").join(u)));
    out.push(match linger {
        Some(true) => Finding::ok("linger", "linger is on: your user manager starts at boot"),
        Some(false) => Finding::warn(
            "linger",
            "linger is off: your user manager, and so herdr-core, starts only at your first login",
            Some("loginctl enable-linger".into()),
        ),
        None => Finding::warn("linger", "cannot check linger: USER is not set", None),
    });
    let enabled = probes
        .run(
            &["systemctl", "--user", "is-enabled", "herdr-wake"],
            PROBE_TIMEOUT,
        )
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty());
    out.push(match enabled.as_deref() {
        Some("enabled") => Finding::ok("systemd unit", "herdr-wake.service is enabled"),
        Some(other) => Finding::warn(
            "systemd unit",
            format!("herdr-wake.service is {other}"),
            Some("systemctl --user enable herdr-wake.service".into()),
        ),
        None => Finding::warn(
            "systemd unit",
            "herdr-wake.service is not known to systemd --user",
            Some(INSTALL_FIX.into()),
        ),
    });
    const C: &str = "wake time";
    out.push(match boot {
        BootState::NotInstalled | BootState::Disabled => Finding::warn(
            C,
            format!("agents come back {NO_BOOT_JOB}"),
            Some(INSTALL_FIX.into()),
        ),
        _ if linger == Some(true) => {
            Finding::ok(C, "agents come back at boot (user service with linger)")
        }
        _ => Finding::ok(C, "agents come back at login (no linger)"),
    });
    let wsl = var(ctx.env, "WSL_DISTRO_NAME").is_some()
        || probes
            .read(Path::new("/proc/sys/kernel/osrelease"))
            .is_some_and(|s| s.to_lowercase().contains("microsoft"));
    if wsl {
        out.push(Finding::warn(
            "wsl",
            "this is WSL: systemd needs `[boot] systemd=true`, nothing starts WSL when Windows boots, and an idle distro stops after about 15 s; untested in v1",
            Some("add a Windows logon task that runs `wsl.exe -d <distro>`, and set instanceIdleTimeout=-1 in .wslconfig".into()),
        ));
    }
}

/// Parses JSON with comments and trailing commas. `None` when malformed;
/// `Some(Null)` for an empty file.
fn parse_jsonc(text: &str) -> Option<serde_json::Value> {
    let v = jsonc_parser::parse_to_value(text, &jsonc_parser::ParseOptions::default()).ok()?;
    Some(v.map_or(serde_json::Value::Null, to_serde))
}

fn to_serde(v: jsonc_parser::JsonValue) -> serde_json::Value {
    use jsonc_parser::JsonValue as J;
    use serde_json::Value as V;
    match v {
        J::String(s) => V::String(s.into_owned()),
        J::Number(n) => serde_json::from_str(n).unwrap_or(V::Null),
        J::Boolean(b) => V::Bool(b),
        J::Null => V::Null,
        J::Array(a) => V::Array(a.into_iter().map(to_serde).collect()),
        J::Object(o) => V::Object(
            o.into_iter()
                .map(|(k, v)| (k.into_owned(), to_serde(v)))
                .collect(),
        ),
    }
}

/// Every hook `command` under `hooks.<event>[].hooks[]`.
fn hook_commands(v: &serde_json::Value, event: &str) -> Vec<String> {
    v.pointer(&format!("/hooks/{event}"))
        .and_then(|e| e.as_array())
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks")?.as_array())
        .flatten()
        .filter_map(|h| h.get("command")?.as_str().map(String::from))
        .collect()
}

fn has_hook(v: &serde_json::Value, event: &str, tail: &str) -> bool {
    hook_commands(v, event)
        .iter()
        .any(|c| c.contains("herdr-wake") && c.trim_end().ends_with(tail))
}

/// The file parsed as JSONC: `Ok(None)` when missing, `Err` when malformed.
fn read_jsonc(probes: &dyn Probes, path: &Path) -> Result<Option<serde_json::Value>, String> {
    match probes.read(path) {
        None => Ok(None),
        Some(text) => parse_jsonc(&text)
            .map(Some)
            .ok_or_else(|| format!("cannot parse {}", path.display())),
    }
}

fn claude_checks(os: Os, ctx: &Ctx, probes: &dyn Probes, out: &mut Vec<Finding>) {
    const C: &str = "claude hook";
    let dir =
        var(ctx.env, "CLAUDE_CONFIG_DIR").map_or_else(|| ctx.home.join(".claude"), PathBuf::from);
    if !probes.exists(&dir) {
        out.push(Finding::ok(
            C,
            format!(
                "skipped: Claude Code not installed ({} not found)",
                dir.display()
            ),
        ));
        return;
    }
    let settings = dir.join("settings.json");
    let user = match read_jsonc(probes, &settings) {
        Ok(v) => v.unwrap_or(serde_json::Value::Null),
        Err(e) => {
            out.push(Finding::fail(
                C,
                e,
                format!("fix the JSON in {} by hand", settings.display()),
            ));
            return;
        }
    };
    let start = has_hook(&user, "SessionStart", "hook claude start");
    let end = has_hook(&user, "SessionEnd", "hook claude end");
    out.push(match (start, end) {
        (false, _) => Finding::fail(
            C,
            format!(
                "{} has no herdr-wake SessionStart hook: Claude sessions are not recorded",
                settings.display()
            ),
            INSTALL_FIX,
        ),
        (true, false) => Finding::warn(
            C,
            format!(
                "{} has the SessionStart hook but no SessionEnd hook: a quit is noticed only by the watcher",
                settings.display()
            ),
            Some(INSTALL_FIX.into()),
        ),
        (true, true) => Finding::ok(C, format!("hooks present in {}", settings.display())),
    });

    let managed = PathBuf::from(match os {
        Os::MacOs => "/Library/Application Support/ClaudeCode/managed-settings.json",
        Os::Linux => "/etc/claude-code/managed-settings.json",
    });
    let mut blocked = Vec::new();
    for (path, v) in [
        (settings, Some(user)),
        (managed.clone(), read_jsonc(probes, &managed).ok().flatten()),
    ] {
        let Some(v) = v else { continue };
        for key in ["disableAllHooks", "allowManagedHooksOnly"] {
            if v.get(key).and_then(|b| b.as_bool()) == Some(true) {
                blocked.push(format!("{key} is true in {}", path.display()));
            }
        }
    }
    out.push(if blocked.is_empty() {
        Finding::ok(
            "claude hooks allowed",
            "nothing switches Claude's hooks off",
        )
    } else {
        Finding::fail(
            "claude hooks allowed",
            format!(
                "{}: Claude runs no herdr-wake hook, so nothing is recorded",
                blocked.join("; ")
            ),
            "remove that setting (a managed setting needs your administrator)",
        )
    });
}

fn codex_checks(ctx: &Ctx, probes: &dyn Probes, out: &mut Vec<Finding>) {
    const C: &str = "codex hook";
    let dir = crate::agents::codex::codex_home(ctx.env, ctx.home);
    if !probes.exists(&dir) {
        out.push(Finding::ok(
            C,
            format!("skipped: Codex not installed ({} not found)", dir.display()),
        ));
        return;
    }
    let hooks = dir.join("hooks.json");
    let v = match read_jsonc(probes, &hooks) {
        Ok(v) => v.unwrap_or(serde_json::Value::Null),
        Err(e) => {
            out.push(Finding::fail(
                C,
                e,
                format!("fix the JSON in {} by hand", hooks.display()),
            ));
            return;
        }
    };
    let config = dir.join("config.toml");
    let switched_off = probes
        .read(&config)
        .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
        .is_some_and(|doc| {
            ["hooks", "codex_hooks"].iter().any(|k| {
                doc.get("features")
                    .and_then(|f| f.get(k))
                    .and_then(|v| v.as_bool())
                    == Some(false)
            })
        });
    if switched_off {
        out.push(Finding::fail(
            C,
            format!(
                "hooks are switched off in {} ([features] hooks = false)",
                config.display()
            ),
            format!(
                "remove `hooks = false` from [features] in {}",
                config.display()
            ),
        ));
        return;
    }
    if !has_hook(&v, "SessionStart", "hook codex start") {
        out.push(Finding::fail(
            C,
            format!(
                "{} has no herdr-wake SessionStart hook: Codex sessions are not recorded",
                hooks.display()
            ),
            INSTALL_FIX,
        ));
        return;
    }
    out.push(Finding::ok(
        C,
        format!("hook present in {}", hooks.display()),
    ));
    out.push(Finding::warn(
        "codex trust",
        "Codex runs a new hook only after you approve it once in /hooks; herdr-wake cannot read that trust state reliably, so this reminder always shows",
        Some("open Codex, type /hooks and trust the herdr-wake hook (once)".into()),
    ));
}

fn check_pi(ctx: &Ctx, probes: &dyn Probes) -> Finding {
    const C: &str = "pi extension";
    let dir = var(ctx.env, "PI_CODING_AGENT_DIR")
        .map_or_else(|| ctx.home.join(".pi/agent"), PathBuf::from);
    if !probes.exists(&dir) {
        return Finding::ok(
            C,
            format!("skipped: Pi not installed ({} not found)", dir.display()),
        );
    }
    let ext = dir.join("extensions").join("herdr-wake.ts");
    if probes.exists(&ext) {
        Finding::ok(C, format!("{} present", ext.display()))
    } else {
        Finding::fail(
            C,
            format!("{} missing: Pi sessions are not recorded", ext.display()),
            INSTALL_FIX,
        )
    }
}

/// The version `<name> --version` prints, as text and as numbers. `None`
/// when the program is not on `PATH`, does not answer within
/// `VERSION_TIMEOUT`, or prints nothing that reads as a version.
fn agent_version(name: &str, ctx: &Ctx, probes: &dyn Probes) -> Option<(String, (u32, u32, u32))> {
    let program = resolve(name, ctx.env, probes)?;
    let out = probes.run(&[&program.to_string_lossy(), "--version"], VERSION_TIMEOUT)?;
    out.stdout
        .split_whitespace()
        .find_map(|word| parse_version(word).map(|v| (word.trim_start_matches('v').to_string(), v)))
}

fn dotted(v: (u32, u32, u32)) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

/// A warning when the installed Pi is older than `MIN_PI_VERSION`; nothing
/// at all when Pi is not installed or its version cannot be read.
fn check_pi_version(ctx: &Ctx, probes: &dyn Probes) -> Option<Finding> {
    const C: &str = "pi version";
    let (text, v) = agent_version("pi", ctx, probes)?;
    let min = dotted(MIN_PI_VERSION);
    Some(if v >= MIN_PI_VERSION {
        Finding::ok(C, format!("{text} (minimum {min})"))
    } else {
        Finding::warn(
            C,
            format!(
                "Pi {text} is older than {min}, the oldest version herdr-wake's extension is written for: sessions may not be recorded or may count a signal as a quit"
            ),
            Some("update Pi".into()),
        )
    })
}

/// A warning when the installed OpenCode is a 2.x older than
/// `MIN_OPENCODE_V2_VERSION`; nothing when OpenCode is not installed or its
/// version cannot be read. Version 1 has no floor.
fn check_opencode_version(ctx: &Ctx, probes: &dyn Probes) -> Option<Finding> {
    const C: &str = "opencode version";
    let (text, v) = agent_version("opencode", ctx, probes)?;
    let min = dotted(MIN_OPENCODE_V2_VERSION);
    Some(if v.0 < 2 {
        Finding::ok(C, format!("{text} (version 1: no minimum known)"))
    } else if v >= MIN_OPENCODE_V2_VERSION {
        Finding::ok(C, format!("{text} (minimum for version 2: {min})"))
    } else {
        Finding::warn(
            C,
            format!(
                "OpenCode {text} is older than {min}, the oldest 2.x herdr-wake's plugin is written for: sessions may not be recorded or resumed"
            ),
            Some("update OpenCode".into()),
        )
    })
}

fn check_opencode(ctx: &Ctx, probes: &dyn Probes) -> Finding {
    const C: &str = "opencode plugin";
    let dir = var(ctx.env, "OPENCODE_CONFIG_DIR").map_or_else(
        || {
            var(ctx.env, "XDG_CONFIG_HOME")
                .map_or_else(|| ctx.home.join(".config"), PathBuf::from)
                .join("opencode")
        },
        PathBuf::from,
    );
    if !probes.exists(&dir) {
        return Finding::ok(
            C,
            format!(
                "skipped: OpenCode not installed ({} not found)",
                dir.display()
            ),
        );
    }
    // v2: `plugins` in cli.json; v1: `plugin` in tui.json or tui.jsonc.
    let files = [
        ("cli.json", "plugins"),
        ("tui.json", "plugin"),
        ("tui.jsonc", "plugin"),
    ];
    for (name, key) in files {
        let path = dir.join(name);
        let Ok(Some(v)) = read_jsonc(probes, &path) else {
            continue;
        };
        let listed = v.get(key).and_then(|a| a.as_array()).is_some_and(|a| {
            a.iter()
                .any(|e| e.as_str().is_some_and(|s| s.contains("herdr-wake")))
        });
        if listed {
            return Finding::ok(C, format!("plugin listed in {}", path.display()));
        }
    }
    Finding::fail(
        C,
        format!(
            "no herdr-wake plugin entry in {}/cli.json or tui.json: OpenCode sessions are not recorded",
            dir.display()
        ),
        INSTALL_FIX,
    )
}

/// One line per finding (`ok`, `warn` or `FAIL`, the check, the detail), a
/// `fix:` line under each finding that has one, and a summary line.
pub fn render_doctor(findings: &[Finding]) -> String {
    let mut out = String::new();
    for f in findings {
        let tag = match f.level {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        // Details can quote records and other tools' files.
        let clean = crate::trust::printable;
        out.push_str(&format!("{tag:<6}{}: {}\n", f.check, clean(&f.detail)));
        if let Some(fix) = &f.fix {
            out.push_str(&format!("      fix: {}\n", clean(fix)));
        }
    }
    let failed = findings.iter().filter(|f| f.level == Level::Fail).count();
    let warned = findings.iter().filter(|f| f.level == Level::Warn).count();
    let mut parts = Vec::new();
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if warned > 0 {
        let s = if warned == 1 { "" } else { "s" };
        parts.push(format!("{warned} warning{s}"));
    }
    if parts.is_empty() {
        out.push_str("all checks passed\n");
    } else {
        out.push_str(&parts.join(", "));
        out.push('\n');
    }
    out
}
