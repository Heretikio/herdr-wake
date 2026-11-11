//! Linux adapter (Task 5): process facts from `/proc`, the kernel boot id,
//! foreground runs with job control, shutdown detection through logind or
//! systemd, and the boot job as a systemd user unit plus linger.
//!
//! Everything that touches the machine is injectable: the `/proc` and `/run`
//! roots, the command runner, and the boot service's paths. Tests build a
//! `Linux` with `Linux::with_parts` and never call systemctl or loginctl.

use std::collections::BTreeMap;
use std::ffi::{CStr, OsStr};
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};

use super::{Action, BootOpts, BootService, BootState, ExitInfo, Platform, ProcInfo};
use crate::record::ProcRef;

/// The systemd user unit's file name.
pub const UNIT_NAME: &str = "herdr-wake.service";

/// The unit text before `render_unit` fills in `@BIN@` and `@BOOT_ENV@`.
pub const UNIT_TEMPLATE: &str = include_str!("../../assets/systemd/herdr-wake.service");

/// Where logind keeps one empty file per lingering user.
pub const LINGER_DIR: &str = "/var/lib/systemd/linger";

/// One deadline across every shutdown probe.
const SHUTDOWN_BUDGET: Duration = Duration::from_millis(500);
/// systemctl and loginctl calls made by install and remove (polkit can stall).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// `systemctl --user is-enabled` from `state()`.
const STATE_TIMEOUT: Duration = Duration::from_secs(2);
/// A scheduled shutdown closer than this counts as in progress.
const SCHEDULED_SOON_MS: u64 = 60_000;
/// Executable names of herdr-core's server.
const CORE_NAMES: [&str; 2] = ["hc", "herdr-core"];

// --- Command runner ---------------------------------------------------------

/// What a finished command printed and how it exited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    /// Exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl RunOutput {
    /// A command that exited 0 after printing `stdout`.
    pub fn ok(stdout: &str) -> RunOutput {
        RunOutput {
            code: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Runs short helper commands (systemctl, loginctl, busctl).
pub trait Runner: Send + Sync {
    /// Runs `argv` with stdin closed and waits at most `timeout`. `Err` when
    /// it cannot start or runs past the timeout (it is then killed).
    fn run(&self, argv: &[&str], timeout: Duration) -> io::Result<RunOutput>;
}

/// The real runner: spawns the command with this process's environment.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, argv: &[&str], timeout: Duration) -> io::Result<RunOutput> {
        let Some((prog, args)) = argv.split_first() else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
        };
        let deadline = Instant::now() + timeout;
        let mut child = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let out = drain(child.stdout.take());
        let err = drain(child.stderr.take());
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{prog} ran longer than {} ms", timeout.as_millis()),
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        // A grandchild can hold a pipe open; never wait on it past the deadline.
        let left = || {
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(50))
        };
        Ok(RunOutput {
            code: status.code(),
            stdout: out.recv_timeout(left()).unwrap_or_default(),
            stderr: err.recv_timeout(left()).unwrap_or_default(),
        })
    }
}

/// Reads a pipe to its end on its own thread.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
        });
    }
    rx
}

// --- /proc parsing ----------------------------------------------------------

/// The fields of `/proc/<pid>/stat` herdr-wake uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    /// Field 3: R, S, D, Z, T, X and so on.
    state: char,
    /// Field 4.
    ppid: u32,
    /// Field 22: clock ticks after boot when the process started.
    start: u64,
}

/// Parses `/proc/<pid>/stat`. The command name (field 2) sits in
/// parentheses and may itself hold spaces and parentheses, so fields are
/// counted from the last `)`.
fn parse_stat(text: &str) -> Option<Stat> {
    let close = text.rfind(')')?;
    let fields: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    // fields[0] is field 3, so field n is fields[n - 3].
    Some(Stat {
        state: fields.first()?.chars().next()?,
        ppid: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

/// Splits `/proc/<pid>/cmdline` on NUL. Only the one trailing NUL is
/// dropped, so empty arguments survive.
fn parse_cmdline(bytes: &[u8]) -> Vec<String> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let body = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    body.split(|b| *b == 0)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect()
}

/// Parses `/proc/<pid>/environ` (NUL-separated `NAME=value`). Entries
/// without `=` or with an empty name are skipped; the first of two equal
/// names wins, as `getenv` does.
fn parse_environ(bytes: &[u8]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for entry in bytes.split(|b| *b == 0) {
        let entry = String::from_utf8_lossy(entry);
        if let Some((name, value)) = entry.split_once('=') {
            if !name.is_empty() {
                env.entry(name.to_string())
                    .or_insert_with(|| value.to_string());
            }
        }
    }
    env
}

/// The real uid (first number on the `Uid:` line) from `/proc/<pid>/status`.
fn parse_status_uid(text: &str) -> Option<u32> {
    text.lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The target of `/proc/<pid>/exe`, without the " (deleted)" suffix the
/// kernel adds once the file was replaced.
fn clean_exe(target: &Path) -> PathBuf {
    let bytes = target.as_os_str().as_bytes();
    let bytes = bytes.strip_suffix(b" (deleted)").unwrap_or(bytes);
    PathBuf::from(OsStr::from_bytes(bytes))
}

/// True for herdr-core's server: `<hc or herdr-core> serve ...`.
fn is_core_serve(info: &ProcInfo) -> bool {
    let named = |p: &Path| {
        p.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|n| CORE_NAMES.contains(&n))
    };
    info.argv.get(1).map(String::as_str) == Some("serve")
        && (info.argv.first().is_some_and(|a| named(Path::new(a))) || named(&info.exe))
}

// --- Shutdown parsing -------------------------------------------------------

/// `/run/systemd/shutdown/scheduled` holds `USEC=<realtime µs>`. A shutdown
/// counts as in progress when it is due within a minute, or when the file
/// names no time.
fn shutdown_imminent(scheduled: &str, now_ms: u64) -> bool {
    let usec = scheduled
        .lines()
        .find_map(|l| l.trim().strip_prefix("USEC="))
        .and_then(|v| v.trim().parse::<u64>().ok());
    match usec {
        Some(usec) => usec / 1000 <= now_ms.saturating_add(SCHEDULED_SOON_MS),
        None => true,
    }
}

/// busctl prints `b true` while logind prepares a shutdown or reboot.
fn logind_preparing(stdout: &str) -> bool {
    stdout.trim() == "b true"
}

/// `systemctl is-system-running` prints `stopping` during a shutdown.
fn system_stopping(stdout: &str) -> bool {
    stdout.lines().next().map(str::trim) == Some("stopping")
}

// --- The adapter ------------------------------------------------------------

/// A shutdown probe: a command and the test for its stdout.
type Probe = (&'static [&'static str], fn(&str) -> bool);

/// The Linux `Platform`.
pub struct Linux {
    proc_root: PathBuf,
    run_root: PathBuf,
    home: Option<PathBuf>,
    runner: Arc<dyn Runner>,
    boot: SystemdBoot,
}

impl Default for Linux {
    fn default() -> Self {
        Linux::new()
    }
}

impl std::fmt::Debug for Linux {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Linux")
            .field("proc_root", &self.proc_root)
            .field("run_root", &self.run_root)
            .field("home", &self.home)
            .field("boot", &self.boot.paths)
            .finish()
    }
}

impl Linux {
    /// The real machine: `/proc`, `/run`, the system runner, and the boot
    /// service for this user (name and home from the password database,
    /// `HOME` and `XDG_*` from the environment).
    pub fn new() -> Linux {
        let env: BTreeMap<String, String> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        let (pw_user, pw_home) = passwd();
        let home = env
            .get("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
            .or(pw_home)
            .unwrap_or_else(|| PathBuf::from("/"));
        let user = pw_user
            .or_else(|| env.get("USER").cloned())
            .or_else(|| env.get("LOGNAME").cloned())
            .unwrap_or_default();
        let runner: Arc<dyn Runner> = Arc::new(SystemRunner);
        let boot = SystemdBoot::new(BootPaths::new(&env, &home, &user), runner.clone());
        Linux::with_parts(
            PathBuf::from("/proc"),
            PathBuf::from("/run"),
            Some(home),
            runner,
            boot,
        )
    }

    /// An adapter over other roots, for tests. `proc_root` stands for
    /// `/proc`, `run_root` for `/run`; `home` is the `HOME` a core must have
    /// for `core_pid` (`None`: any).
    pub fn with_parts(
        proc_root: PathBuf,
        run_root: PathBuf,
        home: Option<PathBuf>,
        runner: Arc<dyn Runner>,
        boot: SystemdBoot,
    ) -> Linux {
        Linux {
            proc_root,
            run_root,
            home,
            runner,
            boot,
        }
    }

    /// The concrete boot service (also reachable as `&dyn BootService`).
    pub fn systemd(&self) -> &SystemdBoot {
        &self.boot
    }

    fn stat(&self, pid: u32) -> Option<Stat> {
        let text = fs::read_to_string(self.proc_root.join(pid.to_string()).join("stat")).ok()?;
        parse_stat(&text)
    }

    /// A running (not zombie) process's stat.
    fn live_stat(&self, pid: u32) -> Option<Stat> {
        self.stat(pid)
            .filter(|s| !matches!(s.state, 'Z' | 'X' | 'x'))
    }

    fn owner(&self, pid: u32) -> Option<u32> {
        let text = fs::read_to_string(self.proc_root.join(pid.to_string()).join("status")).ok()?;
        parse_status_uid(&text)
    }

    /// True when `env` is unreadable or names our `HOME`.
    fn same_home(&self, env: Option<&BTreeMap<String, String>>) -> bool {
        match (env.and_then(|e| e.get("HOME")), &self.home) {
            (Some(theirs), Some(ours)) => Path::new(theirs) == ours,
            _ => true,
        }
    }
}

impl Platform for Linux {
    fn process(&self, pid: u32) -> Option<ProcInfo> {
        let dir = self.proc_root.join(pid.to_string());
        let stat = self.live_stat(pid)?;
        let argv = parse_cmdline(&fs::read(dir.join("cmdline")).unwrap_or_default());
        let mut info = ProcInfo::new(pid, stat.ppid, stat.start, argv);
        if let Ok(target) = fs::read_link(dir.join("exe")) {
            info.exe = clean_exe(&target);
        }
        info.env = fs::read(dir.join("environ"))
            .ok()
            .map(|bytes| parse_environ(&bytes));
        // The pid may have been reused between the reads above.
        (self.live_stat(pid)?.start == stat.start).then_some(info)
    }

    fn alive(&self, p: &ProcRef) -> bool {
        self.live_stat(p.pid).is_some_and(|s| s.start == p.start)
    }

    fn boot_id(&self) -> String {
        fs::read_to_string(self.proc_root.join("sys/kernel/random/boot_id"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    fn login_shell_argv(&self, shell: &Path) -> Vec<String> {
        vec![shell.to_string_lossy().into_owned()]
    }

    fn exec(&self, argv: &[String], env: &BTreeMap<String, String>, cwd: &Path) -> io::Error {
        super::unix_exec(argv, env, cwd)
    }

    fn run_foreground(
        &self,
        argv: &[String],
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> io::Result<ExitInfo> {
        run_foreground(argv, env, cwd)
    }

    fn shutting_down(&self) -> bool {
        let deadline = Instant::now() + SHUTDOWN_BUDGET;
        if let Ok(text) = fs::read_to_string(self.run_root.join("systemd/shutdown/scheduled")) {
            if shutdown_imminent(&text, super::system_now_ms()) {
                return true;
            }
        }
        let probes: [Probe; 2] = [
            (
                &[
                    "busctl",
                    "get-property",
                    "org.freedesktop.login1",
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                    "PreparingForShutdown",
                ],
                logind_preparing,
            ),
            (&["systemctl", "is-system-running"], system_stopping),
        ];
        for (argv, says_yes) in probes {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            if let Ok(out) = self.runner.run(argv, left) {
                if says_yes(&out.stdout) {
                    return true;
                }
            }
        }
        false
    }

    /// The oldest `hc serve` or `herdr-core serve` owned by this uid whose
    /// `HOME` matches ours (or is unreadable). A second `serve` that lost the
    /// lock race exits soon, so the oldest is the one serving.
    fn core_pid(&self) -> Option<u32> {
        let uid = unsafe { libc::getuid() };
        let mut best: Option<(u64, u32)> = None;
        for entry in fs::read_dir(&self.proc_root).ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if self.owner(pid) != Some(uid) {
                continue;
            }
            let Some(info) = self.process(pid) else {
                continue;
            };
            if !is_core_serve(&info) || !self.same_home(info.env.as_ref()) {
                continue;
            }
            let key = (info.start, pid);
            if best.is_none_or(|b| key < b) {
                best = Some(key);
            }
        }
        best.map(|(_, pid)| pid)
    }

    fn boot_service(&self) -> &dyn BootService {
        &self.boot
    }

    fn now_ms(&self) -> u64 {
        super::system_now_ms()
    }

    /// `CLOCK_BOOTTIME`: time since boot, suspend included.
    fn mono_ms(&self) -> u64 {
        super::clock_ms(libc::CLOCK_BOOTTIME)
    }
}

/// This user's name and home from the password database.
fn passwd() -> (Option<String>, Option<PathBuf>) {
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid out-parameter; getpwuid_r fills
    // it with pointers into `buf`, which outlives every read below.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        );
        if rc != 0 || found.is_null() {
            return (None, None);
        }
        let name = (!pwd.pw_name.is_null())
            .then(|| CStr::from_ptr(pwd.pw_name).to_string_lossy().into_owned())
            .filter(|n| !n.is_empty());
        let dir = (!pwd.pw_dir.is_null())
            .then(|| PathBuf::from(OsStr::from_bytes(CStr::from_ptr(pwd.pw_dir).to_bytes())))
            .filter(|d| d.is_absolute());
        (name, dir)
    }
}

// --- Foreground runs --------------------------------------------------------

/// Signals this process ignores while a foreground child runs. Ignoring
/// SIGTTOU lets it hand the terminal over and take it back from a
/// background group; SIGINT and SIGQUIT go to the child's group anyway.
const HELD: [libc::c_int; 3] = [libc::SIGINT, libc::SIGQUIT, libc::SIGTTOU];

/// Saved dispositions for `HELD`, to put back afterwards (in this process)
/// and before exec (in the child, since an ignored signal survives exec).
#[derive(Clone, Copy)]
struct Saved([(libc::c_int, libc::sigaction); 3]);

impl Saved {
    fn ignore_held() -> io::Result<Saved> {
        // SAFETY: all-zero sigaction is SIG_DFL with an empty mask.
        let zero: libc::sigaction = unsafe { std::mem::zeroed() };
        let mut saved = Saved([(0, zero); 3]);
        for (i, sig) in HELD.iter().enumerate() {
            let mut ignore = zero;
            ignore.sa_sigaction = libc::SIG_IGN;
            let mut old = zero;
            // SAFETY: installing SIG_IGN runs no code in this process.
            if unsafe { libc::sigaction(*sig, &ignore, &mut old) } != 0 {
                let err = io::Error::last_os_error();
                saved.restore_first(i);
                return Err(err);
            }
            saved.0[i] = (*sig, old);
        }
        Ok(saved)
    }

    fn restore_first(&self, n: usize) {
        for (sig, old) in self.0.iter().take(n) {
            // SAFETY: puts back a disposition read from the kernel earlier.
            unsafe {
                libc::sigaction(*sig, old, std::ptr::null_mut());
            }
        }
    }

    /// Async-signal-safe: also used between fork and exec.
    fn restore(&self) {
        self.restore_first(self.0.len());
    }
}

/// True when stdin is this process's controlling terminal.
fn stdin_is_controlling_tty() -> bool {
    // SAFETY: plain queries on fd 0.
    unsafe { libc::isatty(0) == 1 && libc::tcgetpgrp(0) >= 0 }
}

/// Gives the terminal on fd 0 to `pgrp` (no-op without one).
fn give_tty(tty: bool, pgrp: libc::pid_t) {
    if tty {
        // SAFETY: SIGTTOU is ignored while this runs, so a background
        // caller is not stopped; failure leaves the terminal as it was.
        unsafe {
            libc::tcsetpgrp(0, pgrp);
        }
    }
}

fn run_foreground(
    argv: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> io::Result<ExitInfo> {
    let Some((prog, args)) = argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    };
    let tty = stdin_is_controlling_tty();
    let saved = Saved::ignore_held()?;
    let started = Instant::now();
    let mut cmd = Command::new(prog);
    cmd.args(args).env_clear().envs(env).current_dir(cwd);
    // SAFETY: the hook runs between fork and exec and calls only
    // async-signal-safe functions (setpgid, getpid, tcsetpgrp, sigaction).
    unsafe {
        cmd.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if tty {
                libc::tcsetpgrp(0, libc::getpid());
            }
            saved.restore();
            Ok(())
        });
    }
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            // SAFETY: getpgrp cannot fail.
            give_tty(tty, unsafe { libc::getpgrp() });
            saved.restore();
            return Err(e);
        }
    };
    let pid = child.id() as libc::pid_t;
    // Parent and child both set the group and the terminal, so whichever
    // runs first wins and the agent never reads the terminal from the
    // background.
    // SAFETY: setpgid on our own child; EACCES after its exec is harmless.
    unsafe {
        libc::setpgid(pid, pid);
    }
    give_tty(tty, pid);
    let status = wait_foreground(pid, tty);
    // SAFETY: getpgrp cannot fail.
    give_tty(tty, unsafe { libc::getpgrp() });
    saved.restore();
    drop(child);
    let ran_ms = started.elapsed().as_millis() as u64;
    let status = status?;
    let pid = pid as u32;
    if libc::WIFEXITED(status) {
        Ok(ExitInfo::exited(pid, libc::WEXITSTATUS(status), ran_ms))
    } else if libc::WIFSIGNALED(status) {
        Ok(ExitInfo::signaled(pid, libc::WTERMSIG(status), ran_ms))
    } else {
        Err(io::Error::other(format!("unexpected wait status {status}")))
    }
}

/// Waits for `pid` to end. A stopped child (Ctrl-Z) is given the terminal
/// back and continued: nothing here could resume it later.
fn wait_foreground(pid: libc::pid_t, tty: bool) -> io::Result<libc::c_int> {
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: waits on our own child.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
        if r < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if libc::WIFSTOPPED(status) {
            give_tty(tty, pid);
            // SAFETY: signals our child's own process group.
            unsafe {
                libc::kill(-pid, libc::SIGCONT);
            }
            continue;
        }
        return Ok(status);
    }
}

// --- Boot service -----------------------------------------------------------

/// Fills the unit template: `ExecStart` runs `bin boot`, `EnvironmentFile`
/// reads `boot_env` when it exists. `%` and `$` are escaped for systemd, and
/// a binary path with spaces or quotes is double-quoted.
pub fn render_unit(bin: &Path, boot_env: &Path) -> String {
    let boot_env = boot_env.to_string_lossy().replace('%', "%%");
    UNIT_TEMPLATE
        .replace("@BOOT_ENV@", &boot_env)
        .replace("@BIN@", &exec_word(&bin.to_string_lossy()))
}

/// One word of an `ExecStart=` line.
fn exec_word(s: &str) -> String {
    let s = s.replace('%', "%%").replace('$', "$$");
    if s.chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | ';'))
    {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s
    }
}

/// What `systemctl --user is-enabled` says: `Some(true)` only for
/// `enabled` (`enabled-runtime` ends with this boot), `None` when it printed
/// nothing.
fn parse_is_enabled(stdout: &str) -> Option<bool> {
    let word = stdout.lines().next()?.trim();
    (!word.is_empty()).then_some(word == "enabled")
}

/// Every path the boot service reads or writes, plus the user it acts for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootPaths {
    /// `$XDG_CONFIG_HOME/systemd/user/herdr-wake.service`.
    pub unit: PathBuf,
    /// `boot.env`, named in the unit's `EnvironmentFile=`.
    pub boot_env: PathBuf,
    /// `/var/lib/systemd/linger/<user>`: present while linger is on.
    pub linger: PathBuf,
    /// Present when herdr-wake turned linger on (so remove turns it off).
    pub marker: PathBuf,
    /// Where remove copies the unit before deleting it.
    pub backups: PathBuf,
    pub user: String,
    /// `WSL_DISTRO_NAME`, when running under WSL.
    pub wsl_distro: Option<String>,
}

impl BootPaths {
    /// The standard paths for `user` given the environment and home folder.
    /// The marker is `<state dir>/linger-enabled`.
    pub fn new(env: &BTreeMap<String, String>, home: &Path, user: &str) -> BootPaths {
        let config_home = match env.get("XDG_CONFIG_HOME") {
            Some(v) if Path::new(v).is_absolute() => PathBuf::from(v),
            _ => home.join(".config"),
        };
        BootPaths {
            unit: config_home.join("systemd/user").join(UNIT_NAME),
            boot_env: crate::paths::boot_env_file(env, home),
            linger: Path::new(LINGER_DIR).join(user),
            marker: crate::paths::state_dir(env, home).join("linger-enabled"),
            backups: crate::paths::backups_dir(env, home),
            user: user.to_string(),
            wsl_distro: env
                .get("WSL_DISTRO_NAME")
                .filter(|v| !v.is_empty())
                .cloned(),
        }
    }

    /// The symlink `systemctl --user enable` creates.
    fn wants_link(&self) -> PathBuf {
        let dir = self.unit.parent().unwrap_or(Path::new("/"));
        dir.join("default.target.wants").join(UNIT_NAME)
    }
}

/// The boot job as a systemd user unit; linger makes it start at boot.
pub struct SystemdBoot {
    paths: BootPaths,
    runner: Arc<dyn Runner>,
}

impl SystemdBoot {
    pub fn new(paths: BootPaths, runner: Arc<dyn Runner>) -> SystemdBoot {
        SystemdBoot { paths, runner }
    }

    pub fn paths(&self) -> &BootPaths {
        &self.paths
    }

    /// Runs `argv`; an error names the command and its stderr.
    fn run_checked(&self, argv: &[&str]) -> anyhow::Result<()> {
        let shown = argv.join(" ");
        let out = self
            .runner
            .run(argv, COMMAND_TIMEOUT)
            .with_context(|| format!("cannot run `{shown}`"))?;
        if !out.success() {
            bail!("`{shown}` failed: {}", out.stderr.trim());
        }
        Ok(())
    }

    /// Records `argv` as an action and, unless `dry_run`, runs it.
    fn step(&self, argv: &[&str], dry_run: bool, acts: &mut Vec<Action>) -> anyhow::Result<()> {
        acts.push(Action::new(argv.join(" "), None));
        if dry_run {
            return Ok(());
        }
        self.run_checked(argv)
    }

    fn wsl_note(&self, acts: &mut Vec<Action>) {
        if let Some(distro) = &self.paths.wsl_distro {
            acts.push(Action::new(
                format!(
                    "WSL ({distro}): the unit runs only while this distro runs; Windows does not \
                     start WSL at boot. Untested in v1: add a Windows logon task running \
                     `wsl.exe -d {distro}` and enable systemd in /etc/wsl.conf"
                ),
                None,
            ));
        }
    }
}

impl std::fmt::Debug for SystemdBoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemdBoot")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

/// Writes `text` to `path` through a temporary file and a rename.
fn write_atomic(path: &Path, text: &str, mode: u32) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent folder", path.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        name.unwrap_or_default(),
        std::process::id()
    ));
    let write = || -> io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        io::Write::write_all(&mut f, text.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        anyhow::Error::new(e).context(format!("cannot write {}", path.display()))
    })
}

impl BootService for SystemdBoot {
    /// Writes the unit, reloads and enables it, then turns on linger when it
    /// is off (recording the marker). `before_login` changes nothing: the
    /// same unit starts before login once linger is on. A refused
    /// `enable-linger` (polkit) is reported as an action, not an error.
    fn install(&self, o: &BootOpts, dry_run: bool) -> anyhow::Result<Vec<Action>> {
        let p = &self.paths;
        let mut acts = Vec::new();
        acts.push(Action::new(
            "write the systemd user unit",
            Some(p.unit.clone()),
        ));
        if !dry_run {
            write_atomic(&p.unit, &render_unit(&o.bin, &p.boot_env), 0o644)?;
        }
        self.step(
            &["systemctl", "--user", "daemon-reload"],
            dry_run,
            &mut acts,
        )?;
        self.step(
            &["systemctl", "--user", "enable", UNIT_NAME],
            dry_run,
            &mut acts,
        )?;
        if p.linger.exists() {
            acts.push(Action::new(
                format!("linger is already on for {}; left as it is", p.user),
                Some(p.linger.clone()),
            ));
        } else {
            let argv = [
                "loginctl",
                "--no-ask-password",
                "enable-linger",
                p.user.as_str(),
            ];
            acts.push(Action::new(argv.join(" "), None));
            match if dry_run {
                Ok(())
            } else {
                self.run_checked(&argv)
            } {
                Ok(()) => {
                    acts.push(Action::new(
                        "record that herdr-wake turned on linger",
                        Some(p.marker.clone()),
                    ));
                    if !dry_run {
                        crate::private::write(&p.marker, format!("{}\n", p.user).as_bytes())
                            .with_context(|| format!("cannot write {}", p.marker.display()))?;
                    }
                }
                Err(e) => acts.push(Action::new(
                    format!(
                        "could not turn on linger ({e:#}); the unit starts at login until \
                         `sudo loginctl enable-linger {}`",
                        p.user
                    ),
                    None,
                )),
            }
        }
        self.wsl_note(&mut acts);
        Ok(acts)
    }

    /// Disables the unit, backs it up, deletes it and reloads; turns linger
    /// off only when the marker says herdr-wake turned it on.
    fn remove(&self, dry_run: bool) -> anyhow::Result<Vec<Action>> {
        let p = &self.paths;
        let mut acts = Vec::new();
        if p.unit.exists() {
            self.step(
                &["systemctl", "--user", "disable", UNIT_NAME],
                dry_run,
                &mut acts,
            )?;
            let backup = p
                .backups
                .join(format!("{UNIT_NAME}.{}.bak", super::system_now_ms()));
            acts.push(Action::new(
                "back up the systemd user unit",
                Some(backup.clone()),
            ));
            acts.push(Action::new(
                "remove the systemd user unit",
                Some(p.unit.clone()),
            ));
            if !dry_run {
                fs::read(&p.unit)
                    .and_then(|bytes| crate::private::write(&backup, &bytes))
                    .with_context(|| format!("cannot back up {}", p.unit.display()))?;
                fs::remove_file(&p.unit)
                    .with_context(|| format!("cannot remove {}", p.unit.display()))?;
            }
            self.step(
                &["systemctl", "--user", "daemon-reload"],
                dry_run,
                &mut acts,
            )?;
        }
        if p.marker.exists() {
            let argv = [
                "loginctl",
                "--no-ask-password",
                "disable-linger",
                p.user.as_str(),
            ];
            acts.push(Action::new(argv.join(" "), None));
            match if dry_run {
                Ok(())
            } else {
                self.run_checked(&argv)
            } {
                Ok(()) => {
                    acts.push(Action::new(
                        "remove the linger marker",
                        Some(p.marker.clone()),
                    ));
                    if !dry_run {
                        fs::remove_file(&p.marker)
                            .with_context(|| format!("cannot remove {}", p.marker.display()))?;
                    }
                }
                Err(e) => acts.push(Action::new(
                    format!(
                        "could not turn off linger ({e:#}); run `loginctl disable-linger {}`",
                        p.user
                    ),
                    None,
                )),
            }
        }
        Ok(acts)
    }

    /// No unit file: `NotInstalled`. Not enabled: `Disabled`. Enabled with
    /// linger on: `BeforeLogin`, else `AtLogin`. Without an answer from
    /// systemctl, the `default.target.wants` link decides "enabled".
    fn state(&self) -> BootState {
        let p = &self.paths;
        if !p.unit.exists() {
            return BootState::NotInstalled;
        }
        let answer = self
            .runner
            .run(
                &["systemctl", "--user", "is-enabled", UNIT_NAME],
                STATE_TIMEOUT,
            )
            .ok()
            .and_then(|out| parse_is_enabled(&out.stdout));
        let enabled = answer.unwrap_or_else(|| p.wants_link().symlink_metadata().is_ok());
        if !enabled {
            BootState::Disabled
        } else if p.linger.exists() {
            BootState::BeforeLogin
        } else {
            BootState::AtLogin
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real stat line shape (52 fields) with a hostile command name.
    const STAT: &str = "4242 (my (odd) prog) S 4200 4242 4200 34816 4242 4194304 \
        100 0 0 0 5 3 0 0 20 0 1 0 987654 12345678 900 18446744073709551615 \
        1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0 0 0 0 0 0 0 0 0\n";

    #[test]
    fn stat_fields_are_counted_from_the_last_paren() {
        let s = parse_stat(STAT).unwrap();
        assert_eq!(s.state, 'S');
        assert_eq!(s.ppid, 4200);
        assert_eq!(s.start, 987654);
    }

    #[test]
    fn stat_with_a_paren_and_digits_in_comm() {
        let line = "7 () 1 2 3) R 1 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 55 0 0\n";
        let s = parse_stat(line).unwrap();
        assert_eq!((s.state, s.ppid, s.start), ('R', 1, 55));
    }

    #[test]
    fn truncated_stat_is_none() {
        assert_eq!(parse_stat("1 (init) S 0 1 1"), None);
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn cmdline_keeps_empty_args_and_drops_one_trailing_nul() {
        assert_eq!(
            parse_cmdline(b"sh\0-c\0\0a b\0"),
            vec!["sh", "-c", "", "a b"]
        );
        assert_eq!(parse_cmdline(b"x\0\0"), vec!["x", ""]);
        assert_eq!(parse_cmdline(b"\0"), vec![""]);
        assert_eq!(parse_cmdline(b""), Vec::<String>::new());
        assert_eq!(parse_cmdline(b"no-nul"), vec!["no-nul"]);
    }

    #[test]
    fn environ_splits_on_the_first_equals_and_first_name_wins() {
        let env = parse_environ(b"A=1\0B=x=y\0NOEQ\0=v\0A=2\0C=\0");
        let want: BTreeMap<String, String> = [("A", "1"), ("B", "x=y"), ("C", "")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(env, want);
    }

    #[test]
    fn status_uid_is_the_real_uid() {
        let text = "Name:\tbash\nUmask:\t0022\nUid:\t1000\t0\t0\t0\nGid:\t100\t100\t100\t100\n";
        assert_eq!(parse_status_uid(text), Some(1000));
        assert_eq!(parse_status_uid("Name:\tx\n"), None);
    }

    #[test]
    fn deleted_suffix_is_dropped_from_exe() {
        assert_eq!(
            clean_exe(Path::new("/usr/bin/claude (deleted)")),
            PathBuf::from("/usr/bin/claude")
        );
        assert_eq!(clean_exe(Path::new("/bin/sh")), PathBuf::from("/bin/sh"));
    }

    #[test]
    fn core_serve_is_recognised_by_argv_or_exe() {
        let p = |argv: &[&str], exe: &str| {
            let mut i = ProcInfo::new(1, 0, 0, argv.iter().map(|s| s.to_string()).collect());
            i.exe = PathBuf::from(exe);
            is_core_serve(&i)
        };
        assert!(p(
            &["/home/u/.local/bin/hc", "serve"],
            "/home/u/.local/bin/hc"
        ));
        assert!(p(
            &["herdr-core", "serve", "--socket", "/x"],
            "/opt/herdr-core"
        ));
        assert!(p(&["weird", "serve"], "/opt/bin/herdr-core"));
        assert!(!p(&["hc", "hold", "t1"], "/bin/hc"));
        assert!(!p(&["hc"], "/bin/hc"));
        assert!(!p(&["node", "serve"], "/usr/bin/node"));
    }

    #[test]
    fn scheduled_shutdown_counts_when_due_within_a_minute() {
        let now = 1_700_000_000_000;
        let at = |ms: u64| format!("USEC={}\nWARN_WALL=1\nMODE=reboot\n", ms * 1000);
        assert!(shutdown_imminent(&at(now - 5), now));
        assert!(shutdown_imminent(&at(now + 30_000), now));
        assert!(!shutdown_imminent(&at(now + 600_000), now));
        assert!(shutdown_imminent("MODE=poweroff\n", now));
    }

    #[test]
    fn probe_outputs() {
        assert!(logind_preparing("b true\n"));
        assert!(!logind_preparing("b false\n"));
        assert!(!logind_preparing(""));
        assert!(system_stopping("stopping\n"));
        assert!(!system_stopping("running\n"));
        assert!(!system_stopping("degraded\n"));
    }

    #[test]
    fn is_enabled_words() {
        assert_eq!(parse_is_enabled("enabled\n"), Some(true));
        assert_eq!(parse_is_enabled("disabled\n"), Some(false));
        assert_eq!(parse_is_enabled("enabled-runtime\n"), Some(false));
        assert_eq!(parse_is_enabled("masked\n"), Some(false));
        assert_eq!(parse_is_enabled(""), None);
    }

    #[test]
    fn exec_words_are_escaped_for_systemd() {
        assert_eq!(
            exec_word("/home/u/.local/bin/herdr-wake"),
            "/home/u/.local/bin/herdr-wake"
        );
        assert_eq!(exec_word("/opt/100%/hw"), "/opt/100%%/hw");
        assert_eq!(exec_word("/opt/$x/hw"), "/opt/$$x/hw");
        assert_eq!(exec_word("/home/a b/hw"), "\"/home/a b/hw\"");
        assert_eq!(exec_word("/q\"x/hw"), "\"/q\\\"x/hw\"");
    }
}
