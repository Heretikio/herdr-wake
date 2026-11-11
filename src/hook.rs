//! `herdr-wake hook <agent> <event>`: turns one agent hook call into a record
//! write (Task 10, the Record flow in `docs/DESIGN.md`).
//!
//! `main` is what the agent runs. It never prints and always exits 0: a hook
//! that fails loudly or exits non-zero can break the agent. Everything it
//! did goes to one line in the log file instead. `run` holds the logic and
//! takes its collaborators as parameters, so tests drive it with the fakes.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crate::agents::{self, Agent, Ctx, Observation};
use crate::config::Config;
use crate::core::{socket, TerminalId};
use crate::platform::{self, Platform, ProcInfo};
use crate::record::{self, AgentKind, ProcRef, Record, State};
use crate::{gate, paths, trust, wake};

/// Environment name under which the hook's parent pid is passed to adapters
/// (for Claude the parent is the claude process itself).
pub const PPID_ENV: &str = "HERDR_WAKE_PPID";

/// Set by `herdr-wake shell` in the agent it launches: the wrapper already
/// watches that agent, so the hook starts no watcher.
pub const SUPERVISED_ENV: &str = "HERDR_WAKE_SUPERVISED";

/// Most stdin a hook reads.
const MAX_PAYLOAD: u64 = 1024 * 1024;
/// How long a hook waits for its stdin, and for core.
const IO_BUDGET: Duration = Duration::from_secs(2);

/// What one hook call did, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// A `Running` record was saved.
    Recorded,
    /// The stored record was marked `Ended`.
    Ended,
    /// Nothing written; the text says why.
    Ignored(String),
    /// Something went wrong; nothing (or not everything) was written.
    Failed(String),
}

impl std::fmt::Display for HookOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookOutcome::Recorded => f.write_str("recorded"),
            HookOutcome::Ended => f.write_str("ended"),
            HookOutcome::Ignored(why) => write!(f, "ignored: {why}"),
            HookOutcome::Failed(why) => write!(f, "failed: {why}"),
        }
    }
}

/// Handles hook `event` for the agent `kind` with the real adapter. See
/// `run_with`.
pub fn run(
    kind: AgentKind,
    event: &str,
    payload: &[u8],
    ctx: &mut Ctx,
    spawn_watch: &dyn Fn(&TerminalId, ProcRef, Option<ProcRef>),
) -> HookOutcome {
    let agent = agents::get(kind);
    run_with(agent.as_ref(), event, payload, ctx, spawn_watch)
}

/// Handles hook `event` with `agent`:
///
/// - `Started`: saves a `Running` record on the session's terminal (the
///   observation's, else `HC_TERMINAL_ID` when `HC_TERMINAL_SOCKET` is also
///   set), unless herdr-wake is disabled or the folder is excluded. Then,
///   unless the agent is supervised by `herdr-wake shell` or has no shell to
///   outlive, calls `spawn_watch(terminal, agent, shell)`.
/// - `Ended { deliberate: Some(true) }`: marks the record of that session
///   ended, unless the machine is shutting down.
/// - Any other end, and `Ignore`: writes nothing.
pub fn run_with(
    agent: &dyn Agent,
    event: &str,
    payload: &[u8],
    ctx: &mut Ctx,
    spawn_watch: &dyn Fn(&TerminalId, ProcRef, Option<ProcRef>),
) -> HookOutcome {
    match agent.observe(event, payload, ctx) {
        Observation::Ignore(why) => HookOutcome::Ignored(why.to_string()),
        Observation::Started {
            session_id,
            session_file,
            cwd,
            terminal,
            agent_pid,
            argv,
        } => {
            let start = Start {
                session_id,
                session_file,
                cwd,
                terminal,
                agent_pid,
                argv,
            };
            started(agent, start, ctx, spawn_watch)
        }
        Observation::Ended {
            session_id,
            deliberate: Some(true),
        } => ended(&session_id, ctx),
        Observation::Ended { deliberate, .. } => HookOutcome::Ignored(match deliberate {
            Some(_) => "killed, not quit".to_string(),
            None => "end of unknown kind (left to the watcher)".to_string(),
        }),
    }
}

/// The facts of one started session, as `started` records them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    pub session_id: String,
    pub session_file: Option<PathBuf>,
    pub cwd: PathBuf,
    /// `None`: the terminal named by the caller's own environment.
    pub terminal: Option<TerminalId>,
    pub agent_pid: Option<u32>,
    /// The launch command line from the agent's own payload, when it sent
    /// one; preferred over the process table's.
    pub argv: Option<Vec<String>>,
}

/// The terminal named by the hook's own environment, when it runs inside a
/// herdr-core terminal.
fn env_terminal(env: &BTreeMap<String, String>) -> Result<TerminalId, HookOutcome> {
    let id = env
        .get("HC_TERMINAL_ID")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| HookOutcome::Ignored("no terminal".into()))?;
    if env.get("HC_TERMINAL_SOCKET").is_none_or(|v| v.is_empty()) {
        return Err(HookOutcome::Ignored(
            "HC_TERMINAL_ID without HC_TERMINAL_SOCKET".into(),
        ));
    }
    Ok(TerminalId(id.clone()))
}

/// The shell an agent process should outlive: its parent, unless that
/// parent is core's holder (the agent is the terminal's own program) or
/// init (the agent was orphaned).
fn shell_of(platform: &dyn Platform, ppid: u32) -> Option<ProcRef> {
    if ppid <= 1 {
        return None;
    }
    let parent = platform.process(ppid)?;
    let name = parent
        .argv
        .first()
        .map(|a| a.rsplit('/').next().unwrap_or(a))
        .unwrap_or("");
    if name.starts_with("herdr-core") {
        return None;
    }
    Some(parent.proc_ref())
}

/// Saves a `Running` record for a started session and starts its watcher
/// (see `run_with`). Also what `herdr-wake adopt` calls for an agent that
/// was already running.
pub fn started(
    agent: &dyn Agent,
    s: Start,
    ctx: &mut Ctx,
    spawn_watch: &dyn Fn(&TerminalId, ProcRef, Option<ProcRef>),
) -> HookOutcome {
    let terminal = match s.terminal {
        Some(t) => t,
        None => match env_terminal(ctx.env) {
            Ok(t) => t,
            Err(out) => return out,
        },
    };
    if !ctx.cfg.enabled {
        return HookOutcome::Ignored("disabled in config".into());
    }
    if !trust::valid_session_id(&s.session_id) {
        return HookOutcome::Ignored("no usable session id".into());
    }
    if crate::config::is_excluded(&s.cwd, &ctx.cfg.exclude_dirs, ctx.home) {
        return HookOutcome::Ignored(format!("{} is excluded", s.cwd.display()));
    }
    // A session in a folder other people can write is never resumed
    // unattended, so it is not recorded either. A folder that cannot be
    // seen at all is judged at wake time.
    if std::fs::symlink_metadata(&s.cwd).is_ok() {
        if let Err(why) = trust::folder_is_safe(&s.cwd, trust::current_uid()) {
            return HookOutcome::Ignored(format!(
                "{} is not safe to resume in: {why}",
                s.cwd.display()
            ));
        }
    }

    let proc_info = s.agent_pid.and_then(|pid| ctx.platform.process(pid));
    let env = record::scrub_env(
        proc_info
            .as_ref()
            .and_then(|p| p.env.as_ref())
            .unwrap_or(ctx.env),
    );
    // The payload's own command line wins: a script agent's process shows
    // its interpreter and possibly a renamed title.
    let launch = s
        .argv
        .as_ref()
        .or(proc_info.as_ref().map(|p| &p.argv))
        .filter(|a| !a.is_empty());
    let flags = launch.map_or_else(Vec::new, |a| agent.restorable_flags(a, ctx.cfg));
    let withheld = launch.map_or_else(Vec::new, |a| agent.withheld_flags(a, ctx.cfg));
    if !withheld.is_empty() {
        // Names only: the values are the reason they are left out.
        log_line(
            ctx.env,
            ctx.home,
            ctx.platform.now_ms(),
            &format!(
                "hook {}: {} not restorable: a value that can hold a secret (inline JSON, \
                 a credential, a URL with a password) is never stored; the resumed session \
                 starts without it",
                agent.kind(),
                withheld.join(", ")
            ),
        );
    }
    let bin = resolve_bin(agent.kind(), proc_info.as_ref(), env.get("PATH"));
    let agent_proc = proc_info.as_ref().map(|p| p.proc_ref());
    let supervised = ctx.env.get(SUPERVISED_ENV).is_some_and(|v| v == "1");
    // A supervised agent's parent is the `herdr-wake shell` wrapper, which
    // judges the exit itself: there is no shell to outlive.
    let shell_proc = proc_info
        .as_ref()
        .filter(|_| !supervised)
        .and_then(|p| shell_of(ctx.platform, p.ppid));

    let term = match ctx.core.get(&terminal) {
        Ok(t) => t,
        Err(e) => return HookOutcome::Failed(format!("reading {terminal}: {e}")),
    };
    if record::is_off(&term) {
        return HookOutcome::Ignored(format!(
            "terminal is switched off (`herdr-wake on {terminal}` turns it back on)"
        ));
    }
    let prev = record::of_terminal(&term);
    // A supervised start is a resume that worked, so its attempts start
    // over; otherwise a session resumed three times in one boot (core
    // restarts) would be given up on.
    let attempts = prev
        .as_ref()
        .filter(|r| !supervised && r.session_id == s.session_id)
        .map_or(0, |r| r.attempts);
    let rec = Record {
        v: record::VERSION,
        agent: agent.kind(),
        session_id: s.session_id,
        session_file: s.session_file,
        cwd: s.cwd,
        flags,
        env,
        bin,
        agent_proc,
        shell_proc,
        boot_id: ctx.platform.boot_id(),
        state: State::Running,
        attempts,
        seq: record::next_seq(ctx.platform.now_ms(), prev.as_ref().map(|r| r.seq)),
    };
    match record::save(ctx.core, &terminal, &rec) {
        Ok(true) => {}
        Ok(false) => return HookOutcome::Ignored("a newer record is stored".into()),
        Err(e) => return HookOutcome::Failed(format!("writing {terminal}: {e}")),
    }
    // The private copy that lets `herdr-wake shell` tell this record from
    // a label written by something else. Without it the record is never
    // resumed, so failing to write it is a failure of the hook.
    if let Err(e) = record::write_copy(
        &paths::records_dir(ctx.env, ctx.home),
        &terminal,
        term.created_ms,
        &rec,
    ) {
        return HookOutcome::Failed(format!(
            "recorded on {terminal}, but the local copy could not be written, so it will not resume: {e}"
        ));
    }

    if supervised {
        // The resumed agent re-stamped its record: the next agent of this
        // kind may start, and any "unconfirmed" mark is out of date.
        let key = gate::boot_key(ctx.platform);
        gate::confirm(
            &paths::gate_dir(ctx.env, ctx.home),
            agent.kind(),
            &key,
            // The same clock the wrappers took their slots on.
            ctx.platform.mono_ms(),
        );
        let _ = std::fs::remove_file(wake::unconfirmed_file(ctx.env, ctx.home, &key, &terminal));
    } else if let (Some(a), Some(sh)) = (agent_proc, shell_proc) {
        spawn_watch(&terminal, a, Some(sh));
    }
    HookOutcome::Recorded
}

/// The first executable file called `name` in the folders of `path`.
fn which(name: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.split(':')
        .filter(|d| d.starts_with('/'))
        .map(|d| Path::new(d).join(name))
        .find(|f| {
            std::fs::metadata(f).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// The program to run at resume time for the agent `kind`, from its process
/// `p` and the `PATH` it ran with.
///
/// - The process's own executable, when it is the agent itself. If the
///   agent's name on `PATH` leads to that same file, the `PATH` entry is
///   kept instead: it is the stable name (Claude's `~/.local/bin/claude`
///   points at a versioned file that an update deletes).
/// - When the executable is an interpreter (`node`, `bun`, `sh`, ...), or
///   there is no process to read: the agent's name looked up on `PATH`.
/// - Failing all that, the bare agent name, found on `PATH` at wake time.
fn resolve_bin(kind: AgentKind, p: Option<&ProcInfo>, path: Option<&String>) -> PathBuf {
    let on_path = path.and_then(|path| which(kind.as_str(), path));
    let same_file = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    };
    let bare = || PathBuf::from(kind.as_str());
    match p {
        Some(p) if p.exe.is_absolute() && !agents::is_interpreter(&p.exe) => match on_path {
            Some(f) if same_file(&f, &p.exe) => f,
            _ => p.exe.clone(),
        },
        Some(p) if !p.exe.is_absolute() => on_path.unwrap_or_else(|| {
            p.argv
                .first()
                .filter(|a| !a.is_empty())
                .map_or_else(bare, PathBuf::from)
        }),
        _ => on_path.unwrap_or_else(bare),
    }
}

fn ended(session_id: &str, ctx: &mut Ctx) -> HookOutcome {
    if ctx.platform.shutting_down() {
        return HookOutcome::Ignored("shutting down".into());
    }
    let terminal = match env_terminal(ctx.env) {
        Ok(t) => t,
        Err(out) => return out,
    };
    match record::end(ctx.core, &terminal, session_id, None) {
        Ok(true) => {
            record::remove_copy_of(
                &paths::records_dir(ctx.env, ctx.home),
                &terminal,
                session_id,
            );
            HookOutcome::Ended
        }
        Ok(false) => HookOutcome::Ignored("stored record is another session".into()),
        Err(e) => HookOutcome::Failed(format!("ending on {terminal}: {e}")),
    }
}

/// Starts `<bin> watch <terminal> <agent pid> <agent start> <shell pid>
/// <shell start>` fully detached: its own session (`setsid`), stdio on
/// `/dev/null`, working folder `/`, and never waited on, so the agent that
/// ran the hook does not wait for it either.
pub fn spawn_watch_detached(
    bin: &Path,
    t: &TerminalId,
    agent: ProcRef,
    shell: ProcRef,
) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(bin);
    cmd.arg("watch")
        .arg(t.as_str())
        .arg(agent.pid.to_string())
        .arg(agent.start.to_string())
        .arg(shell.pid.to_string())
        .arg(shell.start.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir("/");
    // SAFETY: setsid is async-signal-safe and touches no memory.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // Dropping the handle does not wait; the hook exits right after, and the
    // watcher is reparented to init.
    cmd.spawn().map(drop)
}

/// The process environment as UTF-8 strings (lossy).
pub(crate) fn process_env() -> BTreeMap<String, String> {
    std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// `$HOME`, or `/` when it is unset.
pub(crate) fn home_of(env: &BTreeMap<String, String>) -> PathBuf {
    env.get("HOME")
        .filter(|h| !h.is_empty())
        .map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// Appends one line to herdr-wake's log. Errors are dropped: logging must
/// never make a hook or watcher fail.
pub(crate) fn log_line(env: &BTreeMap<String, String>, home: &Path, now_ms: u64, line: &str) {
    let _ = crate::private::append(
        &paths::log_file(env, home),
        // One line per call, whatever `line` holds (it often quotes a
        // record or a payload): control characters are stripped.
        &format!(
            "{now_ms} pid={} {}\n",
            std::process::id(),
            trust::printable(line)
        ),
    );
}

/// Reads at most `MAX_PAYLOAD` bytes of stdin, waiting at most `IO_BUDGET`.
/// On a timeout the payload is empty.
fn read_stdin() -> Vec<u8> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::stdin().take(MAX_PAYLOAD).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx.recv_timeout(IO_BUDGET).unwrap_or_default()
}

/// Entry point for `herdr-wake hook <agent> <event>`. Reads the payload from
/// stdin, connects to core, runs `run`, logs one line and returns 0. Prints
/// nothing, even on a panic.
pub fn main(kind: AgentKind, event: &str) -> i32 {
    std::panic::set_hook(Box::new(|_| {}));
    let mut env = process_env();
    env.insert(
        PPID_ENV.to_string(),
        std::os::unix::process::parent_id().to_string(),
    );
    let home = home_of(&env);
    let platform = platform::current();
    let now = platform.now_ms();

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        main_inner(kind, event, &env, &home, platform.as_ref())
    }))
    .unwrap_or_else(|_| HookOutcome::Failed("panic".into()));

    log_line(&env, &home, now, &format!("hook {kind} {event}: {outcome}"));
    0
}

/// How often a hook whose session could not be bound to a terminal yet
/// tries again, and the pause between tries in ms.
pub const BIND_RETRIES: u32 = 8;
pub const BIND_PAUSE_MS: u64 = 250;

/// Runs `once`, and again after a pause while its outcome is "unbound", up
/// to `BIND_RETRIES` more times.
///
/// An adapter that binds a session by the terminal's foreground process
/// (Codex) depends on a fact herdr-core refreshes only about twice a
/// second, so a hook that fires right after the agent starts can still see
/// the shell (or the wrapper) in the foreground. A moment later it binds.
pub fn retry_unbound(once: &mut dyn FnMut() -> HookOutcome, sleep: &dyn Fn(u64)) -> HookOutcome {
    let mut out = once();
    for _ in 0..BIND_RETRIES {
        if out != HookOutcome::Ignored(agents::codex::UNBOUND.to_string()) {
            break;
        }
        sleep(BIND_PAUSE_MS);
        out = once();
    }
    out
}

fn main_inner(
    kind: AgentKind,
    event: &str,
    env: &BTreeMap<String, String>,
    home: &Path,
    platform: &dyn Platform,
) -> HookOutcome {
    let payload = read_stdin();
    let cfg = match Config::load(&paths::config_file(env, home)) {
        Ok(c) => c,
        Err(e) => return HookOutcome::Failed(format!("config: {e:#}")),
    };
    let mut once = || main_once(kind, event, &payload, &cfg, env, home, platform);
    retry_unbound(&mut once, &|ms| {
        std::thread::sleep(Duration::from_millis(ms))
    })
}

/// One pass of the hook over a fresh core connection (each has its own
/// time budget).
fn main_once(
    kind: AgentKind,
    event: &str,
    payload: &[u8],
    cfg: &Config,
    env: &BTreeMap<String, String>,
    home: &Path,
    platform: &dyn Platform,
) -> HookOutcome {
    let mut core = match socket::connect(&socket::socket_path(env, home), IO_BUDGET) {
        Ok(c) => c,
        Err(e) => return HookOutcome::Failed(format!("core: {e}")),
    };
    let me = std::env::current_exe().ok();
    let spawn = |t: &TerminalId, a: ProcRef, s: Option<ProcRef>| {
        if let (Some(bin), Some(s)) = (me.as_deref(), s) {
            if let Err(e) = spawn_watch_detached(bin, t, a, s) {
                log_line(
                    env,
                    home,
                    platform.now_ms(),
                    &format!("hook: cannot start watcher for {t}: {e}"),
                );
            }
        }
    };
    let mut ctx = Ctx {
        platform,
        core: &mut core,
        env,
        home,
        cfg,
    };
    run(kind, event, payload, &mut ctx, &spawn)
}
