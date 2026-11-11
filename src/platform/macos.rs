//! macOS adapter (Task 4).
//!
//! - Process facts: `proc_pidinfo(PROC_PIDTBSDINFO)` for parent, uid, status
//!   and start time; `sysctl(KERN_PROCARGS2)` for argv (read exactly `argc`
//!   strings, because the environment follows argv in the same buffer) and,
//!   best effort, the environment.
//! - Boot id: `kern.bootsessionuuid`.
//! - Foreground runs: the child gets its own process group and the terminal;
//!   afterwards this process takes the terminal back.
//! - Shutdown detection: loginwindow's restart and shutdown notifications,
//!   as herdr-core watches them (see [`ShutdownWatch`]).
//! - Boot service: a LaunchAgent (default) or a LaunchDaemon
//!   (`--before-login`), see [`LaunchdService`].

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use super::{Action, BootOpts, BootService, BootState, ExitInfo, Platform, ProcInfo};
use crate::record::ProcRef;

/// The macOS `Platform`.
pub struct MacOs {
    boot: LaunchdService,
    shutdown: ShutdownWatch,
}

impl MacOs {
    /// The adapter for this user: a boot service for the invoking user (the
    /// `sudo` caller when run as root) and a shutdown watch registered now,
    /// so a later `shutting_down()` sees notifications posted since.
    pub fn new() -> MacOs {
        MacOs {
            boot: LaunchdService::system(),
            shutdown: ShutdownWatch::register(),
        }
    }
}

impl Default for MacOs {
    fn default() -> Self {
        MacOs::new()
    }
}

impl std::fmt::Debug for MacOs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacOs")
            .field("boot", &self.boot)
            .finish_non_exhaustive()
    }
}

impl Platform for MacOs {
    fn process(&self, pid: u32) -> Option<ProcInfo> {
        let info = bsdinfo(pid)?;
        if info.pbi_status == libc::SZOMB {
            return None;
        }
        let args = parse_procargs2(&procargs2(pid)?)?;
        let exe = pid_path(pid).unwrap_or_else(|| PathBuf::from(&args.exec_path));
        Some(ProcInfo {
            pid,
            ppid: info.pbi_ppid,
            start: start_stamp(&info),
            argv: args.argv,
            exe,
            env: args.env,
        })
    }

    fn alive(&self, p: &ProcRef) -> bool {
        bsdinfo(p.pid).is_some_and(|i| i.pbi_status != libc::SZOMB && start_stamp(&i) == p.start)
    }

    fn boot_id(&self) -> String {
        sysctl_string(c"kern.bootsessionuuid")
            .or_else(boot_time_id)
            .unwrap_or_default()
    }

    fn login_shell_argv(&self, shell: &Path) -> Vec<String> {
        vec![shell.to_string_lossy().into_owned(), "-l".to_string()]
    }

    fn exec(&self, argv: &[String], env: &BTreeMap<String, String>, cwd: &Path) -> std::io::Error {
        super::unix_exec(argv, env, cwd)
    }

    fn run_foreground(
        &self,
        argv: &[String],
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> std::io::Result<ExitInfo> {
        run_foreground(argv, env, cwd)
    }

    fn shutting_down(&self) -> bool {
        self.shutdown.poll()
    }

    fn core_pid(&self) -> Option<u32> {
        let home = std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned());
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        let cores: Vec<ProcInfo> = pids_of_uid(uid)
            .into_iter()
            .filter(|&pid| bsdinfo(pid).is_some_and(|i| looks_like_core(&i)))
            .filter_map(|pid| self.process(pid))
            .filter(is_core_serve)
            .collect();
        pick_core(&cores, home.as_deref())
    }

    fn boot_service(&self) -> &dyn BootService {
        &self.boot
    }

    fn now_ms(&self) -> u64 {
        super::system_now_ms()
    }

    /// `CLOCK_MONOTONIC`: on macOS it keeps counting while the machine
    /// sleeps and is the same in every process.
    fn mono_ms(&self) -> u64 {
        super::clock_ms(libc::CLOCK_MONOTONIC)
    }
}

// --- Process facts ----------------------------------------------------------

fn bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
    let pid = c_int::try_from(pid).ok().filter(|&p| p > 0)?;
    // SAFETY: proc_bsdinfo is plain old data; all-zero is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as c_int;
    // SAFETY: the buffer is a proc_bsdinfo of exactly `size` bytes.
    let ret = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast::<c_void>(),
            size,
        )
    };
    (ret == size).then_some(info)
}

/// Start time in microseconds since the Unix epoch.
fn start_stamp(info: &libc::proc_bsdinfo) -> u64 {
    info.pbi_start_tvsec
        .saturating_mul(1_000_000)
        .saturating_add(info.pbi_start_tvusec)
}

fn pid_path(pid: u32) -> Option<PathBuf> {
    let pid = c_int::try_from(pid).ok()?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer holds `buf.len()` writable bytes.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(PathBuf::from(String::from_utf8_lossy(&buf).into_owned()))
}

/// The raw `KERN_PROCARGS2` buffer of `pid`.
fn procargs2(pid: u32) -> Option<Vec<u8>> {
    let pid = c_int::try_from(pid).ok()?;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // The size query returns kern.argmax-sized bounds; retry once if the
    // arguments grew between the calls.
    for _ in 0..2 {
        let mut size: libc::size_t = 0;
        // SAFETY: a size query with a null buffer.
        let ret = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if ret != 0 || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size];
        // SAFETY: the buffer holds `size` writable bytes.
        let ret = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if ret == 0 {
            buf.truncate(size);
            return Some(buf);
        }
        if io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
            return None;
        }
    }
    None
}

/// What a `KERN_PROCARGS2` buffer holds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcArgs {
    exec_path: String,
    argv: Vec<String>,
    env: Option<BTreeMap<String, String>>,
}

/// Parses `[argc: i32][exec_path\0][\0 padding][argv\0 x argc][env\0 ...][\0]`.
/// Exactly `argc` strings are argv (empty ones included); the environment is
/// what follows, up to the first empty string, and `None` when the kernel
/// hid it or it was empty.
fn parse_procargs2(buf: &[u8]) -> Option<ProcArgs> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok()?;
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    let exec_path = String::from_utf8_lossy(&rest[..exec_end]).into_owned();
    let mut pos = exec_end;
    while pos < rest.len() && rest[pos] == 0 {
        pos += 1;
    }
    let next = |pos: &mut usize| -> Option<String> {
        if *pos >= rest.len() {
            return None;
        }
        let end = rest[*pos..]
            .iter()
            .position(|&b| b == 0)
            .map_or(rest.len(), |n| *pos + n);
        let s = String::from_utf8_lossy(&rest[*pos..end]).into_owned();
        *pos = end + 1;
        Some(s)
    };
    let mut argv = Vec::with_capacity(argc);
    for _ in 0..argc {
        match next(&mut pos) {
            Some(s) => argv.push(s),
            None => break,
        }
    }
    let mut env = BTreeMap::new();
    while let Some(s) = next(&mut pos) {
        if s.is_empty() {
            break;
        }
        if let Some((k, v)) = s.split_once('=') {
            if !k.is_empty() {
                env.insert(k.to_string(), v.to_string());
            }
        }
    }
    Some(ProcArgs {
        exec_path,
        argv,
        env: (!env.is_empty()).then_some(env),
    })
}

fn sysctl_string(name: &CStr) -> Option<String> {
    let mut buf = [0u8; 128];
    let mut size = buf.len();
    // SAFETY: the buffer holds `size` writable bytes.
    let ret = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret != 0 {
        return None;
    }
    let s = String::from_utf8_lossy(&buf[..size.min(buf.len())]);
    let s = s.trim_end_matches('\0').trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Fallback boot id from `kern.boottime`, used only if the uuid is missing.
fn boot_time_id() -> Option<String> {
    // SAFETY: timeval is plain old data.
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::timeval>();
    // SAFETY: the buffer is a timeval of `size` bytes.
    let ret = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            (&mut tv as *mut libc::timeval).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (ret == 0 && tv.tv_sec > 0).then(|| format!("boottime-{}.{:06}", tv.tv_sec, tv.tv_usec))
}

// --- Finding herdr-core -----------------------------------------------------

const PROC_UID_ONLY: u32 = 4;

/// Pids whose effective uid is `uid`.
fn pids_of_uid(uid: libc::uid_t) -> Vec<u32> {
    let mut capacity = 1024usize;
    for _ in 0..6 {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as c_int;
        // SAFETY: the buffer holds `bytes` writable bytes.
        let got =
            unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, pids.as_mut_ptr().cast(), bytes) };
        if got <= 0 {
            return Vec::new();
        }
        if got < bytes {
            let n = got as usize / std::mem::size_of::<libc::pid_t>();
            return pids[..n]
                .iter()
                .filter(|&&p| p > 0)
                .map(|&p| p as u32)
                .collect();
        }
        capacity *= 2;
    }
    Vec::new()
}

/// Cheap pre-filter on the kernel's short command name.
fn looks_like_core(info: &libc::proc_bsdinfo) -> bool {
    if info.pbi_status == libc::SZOMB {
        return false;
    }
    let comm: Vec<u8> = info
        .pbi_comm
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    comm == b"herdr-core" || comm == b"hc"
}

fn base_name(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// `herdr-core serve ...` (or `hc serve ...`), by argv and executable name.
fn is_core_serve(p: &ProcInfo) -> bool {
    let named = |s: &str| matches!(base_name(s), "herdr-core" | "hc");
    p.argv.get(1).map(String::as_str) == Some("serve")
        && (p.argv.first().is_some_and(|a| named(a)) || p.exe.to_str().is_some_and(named))
}

/// The oldest core whose `HOME` equals ours; failing that, the oldest whose
/// environment cannot be read. A core that is known to run under another
/// `HOME` serves other terminals on another socket and is never ours (the
/// Linux adapter has the same rule).
fn pick_core(cores: &[ProcInfo], home: Option<&str>) -> Option<u32> {
    let home_of = |p: &ProcInfo| p.env.as_ref().and_then(|e| e.get("HOME").cloned());
    let same_home = |p: &&ProcInfo| home.is_some_and(|h| home_of(p).as_deref() == Some(h));
    let unknown = |p: &&ProcInfo| home.is_none() || home_of(p).is_none();
    cores
        .iter()
        .filter(same_home)
        .min_by_key(|p| p.start)
        .or_else(|| cores.iter().filter(unknown).min_by_key(|p| p.start))
        .map(|p| p.pid)
}

// --- Foreground runs ----------------------------------------------------------

/// Restores a signal's previous disposition when dropped.
struct SignalGuard {
    sig: c_int,
    old: libc::sigaction,
}

impl SignalGuard {
    fn ignore(sig: c_int) -> io::Result<SignalGuard> {
        // SAFETY: sigaction structs are plain old data; SIG_IGN installs no
        // handler.
        unsafe {
            let mut new: libc::sigaction = std::mem::zeroed();
            new.sa_sigaction = libc::SIG_IGN;
            libc::sigemptyset(&mut new.sa_mask);
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, &new, &mut old) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(SignalGuard { sig, old })
        }
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        // SAFETY: reinstalls the disposition saved by `ignore`.
        unsafe {
            libc::sigaction(self.sig, &self.old, std::ptr::null_mut());
        }
    }
}

/// Stdin, when it is a terminal whose foreground job is our process group.
fn foreground_tty() -> Option<c_int> {
    let fd = libc::STDIN_FILENO;
    // SAFETY: isatty, tcgetpgrp and getpgrp only read state.
    unsafe { (libc::isatty(fd) == 1 && libc::tcgetpgrp(fd) == libc::getpgrp()).then_some(fd) }
}

fn run_foreground(
    argv: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> io::Result<ExitInfo> {
    let Some((prog, args)) = argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    };
    let tty = foreground_tty();
    // While the child holds the terminal, a tcsetpgrp from this (background)
    // process group would stop us with SIGTTOU.
    let ttou = SignalGuard::ignore(libc::SIGTTOU)?;

    let mut cmd = std::process::Command::new(prog);
    cmd.args(args).env_clear().envs(env).current_dir(cwd);
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if let Some(fd) = tty {
                // SIGTTOU is still ignored here (inherited), so this cannot
                // stop the child.
                libc::tcsetpgrp(fd, libc::getpid());
            }
            // Start the agent like a job-control shell would: default
            // dispositions for the job-control and terminal signals.
            for sig in [
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTSTP,
                libc::SIGTTIN,
                libc::SIGTTOU,
            ] {
                libc::signal(sig, libc::SIG_DFL);
            }
            Ok(())
        });
    }

    let started = Instant::now();
    let child = cmd.spawn()?;
    let pid = child.id() as libc::pid_t;
    // Both sides set the group and the terminal, so neither order loses.
    // SAFETY: plain syscalls on our own child.
    unsafe {
        libc::setpgid(pid, pid);
        if let Some(fd) = tty {
            libc::tcsetpgrp(fd, pid);
        }
    }
    let status = wait_foreground(pid, tty);
    let ran_ms = started.elapsed().as_millis() as u64;
    if let Some(fd) = tty {
        // SAFETY: SIGTTOU is still ignored, so taking the terminal back from
        // the background cannot stop us.
        unsafe {
            libc::tcsetpgrp(fd, libc::getpgrp());
        }
    }
    drop(ttou);
    // The child is reaped by `wait_foreground`; dropping `Child` never waits.
    drop(child);

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
/// again and continued: there is no job-control shell to resume it later.
fn wait_foreground(pid: libc::pid_t, tty: Option<c_int>) -> io::Result<c_int> {
    loop {
        let mut status: c_int = 0;
        // SAFETY: waits on our own child.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
        if r == -1 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if libc::WIFSTOPPED(status) {
            // SAFETY: plain syscalls on our own child's process group.
            unsafe {
                if let Some(fd) = tty {
                    libc::tcsetpgrp(fd, pid);
                }
                libc::kill(-pid, libc::SIGCONT);
            }
            continue;
        }
        return Ok(status);
    }
}

// --- Shutdown detection -------------------------------------------------------

/// Watches loginwindow's restart and shutdown notifications, the same
/// `com.apple.system.loginwindow.*` keys herdr-core watches
/// (`platform/macos_shutdown.rs`), with the same rule: a restart or shutdown
/// intent (`restartinitiated`, `shutdownInitiated`, `likelyShutdown`) not
/// cancelled (`logoutcancelled`) and followed within 2 hours by the point of
/// no return (`logoutNoReturn`, `shutdownNoReturn`) means a shutdown is in
/// progress for the next 5 minutes. A plain logout (no-return without an
/// intent) does not count.
///
/// Core registers dispatch blocks; this uses `notify_register_check` and
/// polls on each `shutting_down()` call instead (no Blocks runtime).
///
/// Limits:
/// - Only notifications posted after `MacOs::new()` are seen; a process
///   started after loginwindow posted them sees nothing.
/// - Polling loses the order of events between two calls; events seen in one
///   poll are applied as cancel, then intent, then no-return, which errs
///   towards "shutting down".
/// - `notify_check` may report false positives (notify(3)); requiring both
///   an intent and a no-return limits their effect.
/// - Shutdowns that bypass loginwindow (`shutdown -r now`, `reboot`, a
///   power loss, a kernel panic) post nothing and are not detected.
struct ShutdownWatch {
    tokens: Vec<(c_int, ShutdownEvent)>,
    state: Mutex<ShutdownState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownEvent {
    Intent,
    Cancelled,
    NoReturn,
}

const LOGINWINDOW_KEYS: [(&str, ShutdownEvent); 6] = [
    ("restartinitiated", ShutdownEvent::Intent),
    ("shutdownInitiated", ShutdownEvent::Intent),
    ("likelyShutdown", ShutdownEvent::Intent),
    ("logoutcancelled", ShutdownEvent::Cancelled),
    ("logoutNoReturn", ShutdownEvent::NoReturn),
    ("shutdownNoReturn", ShutdownEvent::NoReturn),
];

const INTENT_LIFETIME_NS: u64 = 2 * 60 * 60 * 1_000_000_000;
const SHUTDOWN_LIFETIME_NS: u64 = 5 * 60 * 1_000_000_000;

// libSystem provides notify(3).
extern "C" {
    fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32;
    fn notify_check(token: c_int, check: *mut c_int) -> u32;
    fn notify_cancel(token: c_int) -> u32;
}

/// The pure part of the shutdown rule, in monotonic nanoseconds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ShutdownState {
    intent: Option<u64>,
    armed: Option<u64>,
}

impl ShutdownState {
    fn observe(&mut self, event: ShutdownEvent, now_ns: u64) {
        match event {
            ShutdownEvent::Intent => self.intent = Some(now_ns),
            ShutdownEvent::Cancelled => self.intent = None,
            ShutdownEvent::NoReturn => {
                let recent = self
                    .intent
                    .take()
                    .is_some_and(|at| now_ns.saturating_sub(at) <= INTENT_LIFETIME_NS);
                if recent {
                    self.armed = Some(now_ns);
                }
            }
        }
    }

    /// Applies the events seen in one poll in a fixed order.
    fn observe_batch(&mut self, events: &[ShutdownEvent], now_ns: u64) {
        for kind in [
            ShutdownEvent::Cancelled,
            ShutdownEvent::Intent,
            ShutdownEvent::NoReturn,
        ] {
            if events.contains(&kind) {
                self.observe(kind, now_ns);
            }
        }
    }

    fn in_progress(&self, now_ns: u64) -> bool {
        self.armed
            .is_some_and(|at| now_ns.saturating_sub(at) <= SHUTDOWN_LIFETIME_NS)
    }
}

fn monotonic_ns() -> u64 {
    // SAFETY: timespec is plain old data.
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    // Darwin CLOCK_MONOTONIC includes sleep, so an abandoned intent ages.
    // SAFETY: writes one timespec.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec as u64)
}

impl ShutdownWatch {
    fn register() -> ShutdownWatch {
        let mut tokens = Vec::with_capacity(LOGINWINDOW_KEYS.len());
        for (key, event) in LOGINWINDOW_KEYS {
            let Ok(name) = CString::new(format!("com.apple.system.loginwindow.{key}")) else {
                continue;
            };
            let mut token: c_int = -1;
            // SAFETY: a valid C string and an out pointer.
            if unsafe { notify_register_check(name.as_ptr(), &mut token) } != 0 {
                continue;
            }
            // The first check of a new token always reports true; consume it.
            let mut check: c_int = 0;
            // SAFETY: a token from notify_register_check.
            unsafe { notify_check(token, &mut check) };
            tokens.push((token, event));
        }
        ShutdownWatch {
            tokens,
            state: Mutex::new(ShutdownState::default()),
        }
    }

    fn poll(&self) -> bool {
        let fired: Vec<ShutdownEvent> = self
            .tokens
            .iter()
            .filter(|(token, _)| {
                let mut check: c_int = 0;
                // SAFETY: a token from notify_register_check.
                let status = unsafe { notify_check(*token, &mut check) };
                status == 0 && check != 0
            })
            .map(|&(_, event)| event)
            .collect();
        let now = monotonic_ns();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.observe_batch(&fired, now);
        state.in_progress(now)
    }
}

impl Drop for ShutdownWatch {
    fn drop(&mut self) {
        for (token, _) in &self.tokens {
            // SAFETY: a token from notify_register_check, cancelled once.
            unsafe { notify_cancel(*token) };
        }
    }
}

// --- Boot service -------------------------------------------------------------

/// The launchd label of the LaunchAgent; the LaunchDaemon's is
/// `dev.heretik.herdr-wake.<user>`.
pub const LABEL: &str = "dev.heretik.herdr-wake";

/// Prefix of an `Action::what` that is a command left for the user to run
/// (for example `run: sudo launchctl bootstrap system <plist>`), rather than a
/// step herdr-wake took.
pub const RUN_PREFIX: &str = super::RUN_PREFIX;

const AGENT_TEMPLATE: &str = include_str!("../../assets/launchd/agent.plist");
const DAEMON_TEMPLATE: &str = include_str!("../../assets/launchd/daemon.plist");

/// What one `launchctl` call printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchctlOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl LaunchctlOutput {
    /// A successful call that printed `stdout`.
    pub fn ok(stdout: impl Into<String>) -> LaunchctlOutput {
        LaunchctlOutput {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// A failed call that printed `stderr`.
    pub fn failed(stderr: impl Into<String>) -> LaunchctlOutput {
        LaunchctlOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }
}

/// Runs `launchctl` with the given arguments (without the program name).
pub type LaunchctlRunner = Box<dyn Fn(&[String]) -> io::Result<LaunchctlOutput> + Send + Sync>;

/// The user, folders and captured values the boot service works with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchdEnv {
    pub user: String,
    pub uid: u32,
    pub home: PathBuf,
    /// `SHELL` for the LaunchDaemon's environment.
    pub shell: String,
    /// `PATH` captured at install for the LaunchDaemon's environment.
    pub path: String,
    /// `~/Library/LaunchAgents`.
    pub agents_dir: PathBuf,
    /// `/Library/LaunchDaemons`.
    pub daemons_dir: PathBuf,
    /// `~/Library/Logs/herdr-wake`.
    pub logs_dir: PathBuf,
    /// `~/.Trash`.
    pub trash_dir: PathBuf,
    /// Where a LaunchDaemon plist is written for the user to `sudo install`.
    pub stage_dir: PathBuf,
}

impl LaunchdEnv {
    /// Standard folders under `home`, `SHELL=/bin/zsh`, the system `PATH`,
    /// staging in `~/.local/state/herdr-wake`.
    pub fn for_user(user: &str, uid: u32, home: &Path) -> LaunchdEnv {
        LaunchdEnv {
            user: user.to_string(),
            uid,
            home: home.to_path_buf(),
            shell: "/bin/zsh".to_string(),
            path: "/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
            agents_dir: home.join("Library/LaunchAgents"),
            daemons_dir: PathBuf::from("/Library/LaunchDaemons"),
            logs_dir: home.join("Library/Logs/herdr-wake"),
            trash_dir: home.join(".Trash"),
            stage_dir: home.join(".local/state/herdr-wake"),
        }
    }

    /// This process's user. herdr-wake never installs anything as root
    /// (`install` refuses to run as root): the LaunchDaemon is always
    /// staged, and the user runs the printed `sudo` commands.
    pub fn current() -> LaunchdEnv {
        let env: BTreeMap<String, String> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        LaunchdEnv::resolve(&env, uid, passwd(uid))
    }

    /// The boot service's view of user `uid` with user database entry `pw`
    /// and process environment `env`.
    ///
    /// The user name comes from the user database only, never from `USER`
    /// or `LOGNAME`: it becomes the LaunchDaemon's `UserName`, its label and
    /// its file name under `/Library/LaunchDaemons`, and an environment
    /// variable is whatever the calling shell says it is. Without an entry
    /// the name is the numeric uid. `HOME`, `SHELL` and `PATH` are taken
    /// from the environment first: they decide where the user's own files
    /// are, not who the job runs as.
    pub fn resolve(
        env: &BTreeMap<String, String>,
        uid: u32,
        pw: Option<PasswdEntry>,
    ) -> LaunchdEnv {
        let pick = |var: &str, from_pw: Option<String>| -> Option<String> {
            env.get(var).filter(|v| !v.is_empty()).cloned().or(from_pw)
        };
        let home =
            pick("HOME", pw.as_ref().map(|p| p.home.clone())).unwrap_or_else(|| "/".to_string());
        let user = pw
            .as_ref()
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| uid.to_string());
        let home = PathBuf::from(home);
        let mut out = LaunchdEnv::for_user(&user, uid, &home);
        if let Some(shell) = pick("SHELL", pw.as_ref().map(|p| p.shell.clone())) {
            out.shell = shell;
        }
        if let Some(path) = env.get("PATH").filter(|p| !p.is_empty()) {
            out.path = path.clone();
        }
        out.stage_dir = crate::paths::state_dir(env, &home);
        out
    }

    /// `~/Library/LaunchAgents/dev.heretik.herdr-wake.plist`.
    pub fn agent_plist(&self) -> PathBuf {
        self.agents_dir.join(format!("{LABEL}.plist"))
    }

    /// `dev.heretik.herdr-wake.<user>`.
    pub fn daemon_label(&self) -> String {
        format!("{LABEL}.{}", self.user)
    }

    /// `/Library/LaunchDaemons/dev.heretik.herdr-wake.<user>.plist`.
    pub fn daemon_plist(&self) -> PathBuf {
        self.daemons_dir
            .join(format!("{}.plist", self.daemon_label()))
    }

    /// The LaunchDaemon plist staged for `sudo install` when not root.
    pub fn staged_daemon_plist(&self) -> PathBuf {
        self.stage_dir
            .join(format!("{}.plist", self.daemon_label()))
    }

    /// `~/Library/Logs/herdr-wake/boot.log`.
    pub fn log_file(&self) -> PathBuf {
        self.logs_dir.join("boot.log")
    }

    fn gui_domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn gui_target(&self) -> String {
        format!("gui/{}/{LABEL}", self.uid)
    }

    fn system_target(&self) -> String {
        format!("system/{}", self.daemon_label())
    }
}

/// One user database entry: login name, home folder, login shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswdEntry {
    pub name: String,
    pub home: String,
    pub shell: String,
}

/// True when `name` can safely be a LaunchDaemon's `UserName`, part of its
/// label and of its file name: 1 to 64 characters from `[A-Za-z0-9._-]`,
/// the first a letter or digit.
pub fn valid_user_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn passwd(uid: u32) -> Option<PasswdEntry> {
    let mut buf = vec![0 as c_char; 4096];
    // SAFETY: passwd is plain old data.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: getpwuid_r writes into `pw` and `buf`, both owned here.
    let ret = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut out) };
    if ret != 0 || out.is_null() {
        return None;
    }
    let field = |p: *const c_char| -> String {
        if p.is_null() {
            String::new()
        } else {
            // SAFETY: getpwuid_r points the fields at NUL-terminated strings
            // inside `buf`.
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };
    Some(PasswdEntry {
        name: field(pw.pw_name),
        home: field(pw.pw_dir),
        shell: field(pw.pw_shell),
    })
}

fn system_launchctl(args: &[String]) -> io::Result<LaunchctlOutput> {
    let out = std::process::Command::new("/bin/launchctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()?;
    Ok(LaunchctlOutput {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// The launchd boot job. Paths, user and the `launchctl` runner are injected
/// so tests can render into a temporary home and record `launchctl` calls.
///
/// Default (`before_login == false`): the LaunchAgent in
/// `~/Library/LaunchAgents`, loaded with `launchctl enable` and
/// `launchctl bootstrap gui/<uid>`. With no GUI session the bootstrap fails
/// and the agent loads at the next login, which is reported, not an error.
///
/// `before_login`: the LaunchDaemon in `/Library/LaunchDaemons`, root:wheel
/// 0644. herdr-wake never writes it: the plist is staged in `stage_dir` and
/// the returned actions carry the exact `sudo` commands (prefixed with
/// [`RUN_PREFIX`]) for the user to run. There is no root mode.
///
/// Installing one mode removes the other. Removal boots the job out and moves
/// the plist to the Trash.
pub struct LaunchdService {
    env: LaunchdEnv,
    run: LaunchctlRunner,
}

impl std::fmt::Debug for LaunchdService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchdService")
            .field("env", &self.env)
            .finish_non_exhaustive()
    }
}

/// One planned step; `describe` gives its `Action`, `apply` performs it.
enum Step {
    Mkdir(PathBuf),
    Write {
        path: PathBuf,
        text: String,
        what: &'static str,
    },
    /// The LaunchDaemon plist, written privately for the user to install.
    Stage {
        path: PathBuf,
        text: String,
    },
    /// A `launchctl` call; `required` makes a failure an error.
    Launchctl {
        args: Vec<String>,
        required: bool,
    },
    Trash {
        from: PathBuf,
        to: PathBuf,
    },
    /// A command for the user to run.
    User(String),
}

impl LaunchdService {
    pub fn new(env: LaunchdEnv, run: LaunchctlRunner) -> LaunchdService {
        LaunchdService { env, run }
    }

    /// The service for this process's user, running `/bin/launchctl`.
    pub fn system() -> LaunchdService {
        LaunchdService::new(LaunchdEnv::current(), Box::new(system_launchctl))
    }

    pub fn env(&self) -> &LaunchdEnv {
        &self.env
    }

    /// The LaunchAgent plist text for `bin`.
    pub fn render_agent(&self, bin: &Path) -> String {
        render(AGENT_TEMPLATE, &self.vars(bin))
    }

    /// The LaunchDaemon plist text for `bin`.
    pub fn render_daemon(&self, bin: &Path) -> String {
        render(DAEMON_TEMPLATE, &self.vars(bin))
    }

    fn vars(&self, bin: &Path) -> Vec<(&'static str, String)> {
        let e = &self.env;
        vec![
            ("LABEL", LABEL.to_string()),
            ("DAEMON_LABEL", e.daemon_label()),
            ("BIN", bin.to_string_lossy().into_owned()),
            ("LOG", e.log_file().to_string_lossy().into_owned()),
            ("USER", e.user.clone()),
            ("HOME", e.home.to_string_lossy().into_owned()),
            ("SHELL", e.shell.clone()),
            ("PATH", e.path.clone()),
        ]
    }

    fn launchctl(&self, args: &[&str]) -> io::Result<LaunchctlOutput> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        (self.run)(&args)
    }

    fn loaded(&self, target: &str) -> bool {
        self.launchctl(&["print", target]).is_ok_and(|o| o.success)
    }

    fn disabled(&self, domain: &str, label: &str) -> bool {
        self.launchctl(&["print-disabled", domain])
            .is_ok_and(|o| o.success && disabled_in(&o.stdout, label))
    }

    /// A free name for `path` in the Trash: `name.plist`, then
    /// `name 2.plist`, `name 3.plist`, ...
    fn trash_dest(&self, path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .map_or_else(|| "herdr-wake".into(), |n| n.to_string_lossy().into_owned());
        let first = self.env.trash_dir.join(&name);
        if !first.exists() {
            return first;
        }
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) => (s.to_string(), format!(".{e}")),
            None => (name.clone(), String::new()),
        };
        (2..)
            .map(|n| self.env.trash_dir.join(format!("{stem} {n}{ext}")))
            .find(|p| !p.exists())
            .unwrap_or(first)
    }

    fn agent_removal(&self, steps: &mut Vec<Step>, as_commands: bool) {
        let e = &self.env;
        let plist = e.agent_plist();
        let loaded = self.loaded(&e.gui_target());
        if as_commands {
            if loaded {
                steps.push(Step::User(format!("launchctl bootout {}", e.gui_target())));
            }
            if plist.exists() {
                let to = self.trash_dest(&plist);
                steps.push(Step::User(format!("mv {} {}", quote(&plist), quote(&to))));
            }
            return;
        }
        if loaded {
            steps.push(Step::Launchctl {
                args: vec!["bootout".into(), e.gui_target()],
                required: false,
            });
        }
        if plist.exists() {
            let to = self.trash_dest(&plist);
            steps.push(Step::Trash { from: plist, to });
        }
    }

    fn daemon_removal(&self, steps: &mut Vec<Step>) {
        let e = &self.env;
        let plist = e.daemon_plist();
        let loaded = self.loaded(&e.system_target());
        if loaded {
            steps.push(Step::User(format!(
                "sudo launchctl bootout {}",
                e.system_target()
            )));
        }
        if plist.exists() {
            let to = self.trash_dest(&plist);
            steps.push(Step::User(format!(
                "sudo mv {} {}",
                quote(&plist),
                quote(&to)
            )));
        }
        let staged = e.staged_daemon_plist();
        if staged.exists() {
            let to = self.trash_dest(&staged);
            steps.push(Step::Trash { from: staged, to });
        }
    }

    fn install_steps(&self, o: &BootOpts) -> Vec<Step> {
        let e = &self.env;
        let mut steps = vec![Step::Mkdir(e.logs_dir.clone())];
        if !o.before_login {
            steps.push(Step::Write {
                path: e.agent_plist(),
                text: self.render_agent(&o.bin),
                what: "write the LaunchAgent plist",
            });
            steps.push(Step::Launchctl {
                args: vec!["enable".into(), e.gui_target()],
                required: false,
            });
            if self.loaded(&e.gui_target()) {
                steps.push(Step::Launchctl {
                    args: vec!["bootout".into(), e.gui_target()],
                    required: false,
                });
            }
            steps.push(Step::Launchctl {
                args: vec![
                    "bootstrap".into(),
                    e.gui_domain(),
                    e.agent_plist().to_string_lossy().into_owned(),
                ],
                required: false,
            });
            self.daemon_removal(&mut steps);
            return steps;
        }

        let plist = e.daemon_plist();
        let text = self.render_daemon(&o.bin);
        let loaded = self.loaded(&e.system_target());
        // herdr-wake never writes `/Library/LaunchDaemons` itself: the plist
        // is staged and the user runs the commands below.
        let staged = e.staged_daemon_plist();
        steps.push(Step::Stage {
            path: staged.clone(),
            text,
        });
        // First, so a staged file that is not a well-formed plist stops the
        // user before anything is installed.
        steps.push(Step::User(format!("sudo plutil -lint {}", quote(&staged))));
        steps.push(Step::User(format!(
            "sudo install -m 0644 -o root -g wheel {} {}",
            quote(&staged),
            quote(&plist)
        )));
        steps.push(Step::User(format!(
            "sudo launchctl enable {}",
            e.system_target()
        )));
        if loaded {
            steps.push(Step::User(format!(
                "sudo launchctl bootout {}",
                e.system_target()
            )));
        }
        steps.push(Step::User(format!(
            "sudo launchctl bootstrap system {}",
            quote(&plist)
        )));
        self.agent_removal(&mut steps, true);
        steps
    }

    fn describe(step: &Step) -> Action {
        match step {
            Step::Mkdir(p) => Action::new("create the log folder", Some(p.clone())),
            Step::Write { path, what, .. } => Action::new(*what, Some(path.clone())),
            Step::Stage { path, .. } => Action::new(
                "stage the LaunchDaemon plist for sudo install",
                Some(path.clone()),
            ),
            Step::Launchctl { args, .. } => Action::new(
                format!(
                    "launchctl {}",
                    args.iter()
                        .map(|a| quote(Path::new(a)))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                None,
            ),
            Step::Trash { from, to } => Action::new(
                format!("move to the Trash as {}", to.display()),
                Some(from.clone()),
            ),
            Step::User(cmd) => Action::new(format!("{RUN_PREFIX}{cmd}"), None),
        }
    }

    /// Performs `step`; may add a note to `actions`.
    fn apply(&self, step: &Step, actions: &mut Vec<Action>) -> anyhow::Result<()> {
        match step {
            Step::Mkdir(p) => {
                std::fs::create_dir_all(p)?;
            }
            Step::Write { path, text, .. } => {
                write_file(path, text)?;
            }
            Step::Stage { path, text } => {
                // 0600 in a 0700 folder: between staging and the user's
                // `sudo install`, nobody else can read or swap the file.
                crate::private::write(path, text.as_bytes())?;
            }
            Step::Launchctl { args, required } => {
                let out = (self.run)(args)?;
                if !out.success {
                    let why = out.stderr.trim();
                    if *required {
                        anyhow::bail!("launchctl {} failed: {why}", args.join(" "));
                    }
                    if args.first().map(String::as_str) == Some("bootstrap") {
                        actions.push(Action::new(
                            format!(
                                "launchctl bootstrap failed ({why}); the LaunchAgent loads at the next login"
                            ),
                            None,
                        ));
                    }
                }
            }
            Step::Trash { from, to } => {
                std::fs::create_dir_all(&self.env.trash_dir)?;
                move_file(from, to)?;
            }
            Step::User(_) => {}
        }
        Ok(())
    }

    fn run_steps(&self, steps: Vec<Step>, dry_run: bool) -> anyhow::Result<Vec<Action>> {
        let mut actions = Vec::with_capacity(steps.len());
        for step in &steps {
            actions.push(Self::describe(step));
            if !dry_run {
                self.apply(step, &mut actions)?;
            }
        }
        Ok(actions)
    }
}

impl BootService for LaunchdService {
    fn install(&self, o: &BootOpts, dry_run: bool) -> anyhow::Result<Vec<Action>> {
        if o.before_login && !valid_user_name(&self.env.user) {
            anyhow::bail!(
                "the user name of uid {} cannot be used for a LaunchDaemon (only letters, \
                 digits, `.`, `_` and `-` are accepted); install without --before-login",
                self.env.uid
            );
        }
        let steps = self.install_steps(o);
        self.run_steps(steps, dry_run)
    }

    fn remove(&self, dry_run: bool) -> anyhow::Result<Vec<Action>> {
        let mut steps = Vec::new();
        self.agent_removal(&mut steps, false);
        self.daemon_removal(&mut steps);
        self.run_steps(steps, dry_run)
    }

    /// From the plist files, `launchctl print` (loaded jobs) and
    /// `launchctl print-disabled` (switched off).
    fn state(&self) -> BootState {
        let e = &self.env;
        let label = e.daemon_label();
        if e.daemon_plist().exists() || self.loaded(&e.system_target()) {
            return if self.disabled("system", &label) {
                BootState::Disabled
            } else {
                BootState::BeforeLogin
            };
        }
        if e.agent_plist().exists() || self.loaded(&e.gui_target()) {
            return if self.disabled(&e.gui_domain(), LABEL) {
                BootState::Disabled
            } else {
                BootState::AtLogin
            };
        }
        BootState::NotInstalled
    }
}

/// True when `launchctl print-disabled` output marks `label` disabled
/// (`"label" => disabled`, or `=> true` on older macOS).
fn disabled_in(out: &str, label: &str) -> bool {
    let key = format!("\"{label}\"");
    out.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix(&key)
            .and_then(|rest| rest.trim_start().strip_prefix("=>"))
            .is_some_and(|v| matches!(v.trim(), "disabled" | "true"))
    })
}

/// Replaces each `@NAME@` in `template` in one pass, XML-escaping the value.
fn render(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len() + 256);
    let mut rest = template;
    while let Some(start) = rest.find('@') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let hit = after.find('@').and_then(|end| {
            let name = &after[..end];
            vars.iter().find(|(k, _)| *k == name).map(|(_, v)| (end, v))
        });
        match hit {
            Some((end, value)) => {
                out.push_str(&xml_escape(value));
                rest = &after[end + 1..];
            }
            None => {
                out.push('@');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Shell-quotes `p` when it holds anything outside a safe set.
fn quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@%".contains(c));
    if safe {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Writes `text` to `path` (mode 0644) through a temporary file and a rename.
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent folder"))?;
    std::fs::create_dir_all(dir)?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = dir.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    // `create_new` (O_EXCL) never follows a symlink planted at the
    // temporary name, and the rename replaces a symlink at `path` itself
    // instead of writing through it.
    let result = (|| {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        f.write_all(text.as_bytes())?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Renames `from` to `to`, copying across volumes.
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(argc: i32, parts: &[&[u8]]) -> Vec<u8> {
        let mut b = argc.to_ne_bytes().to_vec();
        for p in parts {
            b.extend_from_slice(p);
        }
        b
    }

    #[test]
    fn procargs_reads_exactly_argc_strings_then_env() {
        let b = buf(
            3,
            &[
                b"/bin/x\0\0\0\0",
                b"x\0a b\0\0",
                b"HOME=/Users/a\0EMPTY=\0\0",
                b"ptr_munge=\0",
            ],
        );
        let p = parse_procargs2(&b).unwrap();
        assert_eq!(p.exec_path, "/bin/x");
        assert_eq!(p.argv, vec!["x", "a b", ""]);
        let env = p.env.unwrap();
        assert_eq!(env.get("HOME").map(String::as_str), Some("/Users/a"));
        assert_eq!(env.get("EMPTY").map(String::as_str), Some(""));
        assert!(!env.contains_key("ptr_munge"));
    }

    #[test]
    fn procargs_with_hidden_env_has_none() {
        let b = buf(2, &[b"/bin/x\0\0", b"x\0y\0", b"\0\0\0"]);
        let p = parse_procargs2(&b).unwrap();
        assert_eq!(p.argv, vec!["x", "y"]);
        assert_eq!(p.env, None);
    }

    #[test]
    fn procargs_env_never_spills_into_argv() {
        let b = buf(1, &[b"/bin/x\0\0", b"x\0", b"A=1\0B=2\0\0"]);
        let p = parse_procargs2(&b).unwrap();
        assert_eq!(p.argv, vec!["x"]);
        assert_eq!(p.env.unwrap().len(), 2);
    }

    #[test]
    fn procargs_too_short_is_none() {
        assert_eq!(parse_procargs2(&[1, 0]), None);
        assert_eq!(parse_procargs2(&(-1i32).to_ne_bytes()), None);
    }

    const S: u64 = 1_000_000_000;

    #[test]
    fn shutdown_needs_an_intent_then_no_return() {
        let mut s = ShutdownState::default();
        s.observe(ShutdownEvent::NoReturn, 10 * S);
        assert!(!s.in_progress(10 * S), "a plain logout is not a shutdown");
        s.observe(ShutdownEvent::Intent, 20 * S);
        assert!(!s.in_progress(20 * S), "an intent alone is not enough");
        s.observe(ShutdownEvent::NoReturn, 30 * S);
        assert!(s.in_progress(30 * S));
        assert!(s.in_progress(30 * S + SHUTDOWN_LIFETIME_NS));
        assert!(!s.in_progress(31 * S + SHUTDOWN_LIFETIME_NS));
    }

    #[test]
    fn cancelled_or_stale_intent_does_not_arm() {
        let mut s = ShutdownState::default();
        s.observe(ShutdownEvent::Intent, S);
        s.observe(ShutdownEvent::Cancelled, 2 * S);
        s.observe(ShutdownEvent::NoReturn, 3 * S);
        assert!(!s.in_progress(3 * S));

        let mut s = ShutdownState::default();
        s.observe(ShutdownEvent::Intent, S);
        s.observe(ShutdownEvent::NoReturn, 2 * S + INTENT_LIFETIME_NS);
        assert!(!s.in_progress(2 * S + INTENT_LIFETIME_NS));
    }

    #[test]
    fn one_poll_with_every_event_counts_as_shutdown() {
        let mut s = ShutdownState::default();
        s.observe_batch(
            &[
                ShutdownEvent::NoReturn,
                ShutdownEvent::Intent,
                ShutdownEvent::Cancelled,
            ],
            S,
        );
        assert!(s.in_progress(S));
    }

    #[test]
    fn real_watch_is_quiet_without_a_shutdown() {
        let w = ShutdownWatch::register();
        assert_eq!(w.tokens.len(), LOGINWINDOW_KEYS.len());
        assert!(!w.poll());
    }

    fn info(pid: u32, start: u64, home: Option<&str>) -> ProcInfo {
        let mut p = ProcInfo::new(pid, 1, start, vec!["herdr-core".into(), "serve".into()]);
        p.env = home.map(|h| BTreeMap::from([("HOME".to_string(), h.to_string())]));
        p
    }

    #[test]
    fn core_pick_prefers_same_home_and_never_takes_another_homes_core() {
        let cores = [
            info(10, 5, Some("/Users/other")),
            info(11, 9, Some("/Users/me")),
            info(12, 7, None),
        ];
        assert_eq!(pick_core(&cores, Some("/Users/me")), Some(11));
        // No core with our home: one whose home cannot be read may be ours,
        // one that runs under another home never is.
        assert_eq!(pick_core(&cores, Some("/Users/nobody")), Some(12));
        assert_eq!(pick_core(&cores[..2], Some("/Users/nobody")), None);
        assert_eq!(pick_core(&cores, None), Some(10));
        assert_eq!(pick_core(&[], Some("/Users/me")), None);
    }

    #[test]
    fn core_serve_matches_by_argv_or_exe() {
        let mut p = ProcInfo::new(1, 1, 1, vec!["/x/hc".into(), "serve".into()]);
        assert!(is_core_serve(&p));
        p.argv = vec!["/x/herdr-core".into(), "hold".into()];
        assert!(!is_core_serve(&p));
        p.argv = vec!["weird".into(), "serve".into()];
        p.exe = PathBuf::from("/a/Contents/MacOS/herdr-core");
        assert!(is_core_serve(&p));
    }

    #[test]
    fn disabled_parsing_handles_both_formats() {
        let out = "disabled services = {\n\t\"dev.heretik.herdr-wake\" => disabled\n\t\"x\" => enabled\n}\n";
        assert!(disabled_in(out, "dev.heretik.herdr-wake"));
        assert!(!disabled_in(out, "x"));
        assert!(disabled_in("\"a.b\" => true", "a.b"));
        assert!(!disabled_in("\"a.b\" => false", "a.b"));
        assert!(!disabled_in("\"a.b.c\" => true", "a.b"));
    }

    #[test]
    fn render_escapes_and_keeps_unknown_markers() {
        let out = render("<s>@A@</s> me@host @B@", &[("A", "x&y".to_string())]);
        assert_eq!(out, "<s>x&amp;y</s> me@host @B@");
    }

    #[test]
    fn quote_leaves_plain_paths_and_wraps_others() {
        assert_eq!(quote(Path::new("/a/b.plist")), "/a/b.plist");
        assert_eq!(quote(Path::new("/a b/it's")), r"'/a b/it'\''s'");
    }
}
