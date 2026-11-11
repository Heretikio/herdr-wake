//! The OS seam: process facts, boot identity, exec and the boot service.
//! Adapters: `macos` (Task 4), `linux` (Task 5) and `fake` for tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::record::ProcRef;

pub mod fake;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

/// Prefix of an `Action::what` that is a command the user still has to run
/// (for example the `sudo` steps of a before-login install on macOS).
pub const RUN_PREFIX: &str = "run: ";

/// Facts about one running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    /// Opaque per-platform start stamp; pid plus start identifies a process.
    pub start: u64,
    pub argv: Vec<String>,
    pub exe: PathBuf,
    /// The process environment, when the OS lets us read it.
    pub env: Option<BTreeMap<String, String>>,
}

impl ProcInfo {
    /// A process with `exe` taken from `argv[0]` and no environment.
    pub fn new(pid: u32, ppid: u32, start: u64, argv: Vec<String>) -> ProcInfo {
        let exe = argv.first().map(PathBuf::from).unwrap_or_default();
        ProcInfo {
            pid,
            ppid,
            start,
            argv,
            exe,
            env: None,
        }
    }

    /// The `ProcRef` naming this process.
    pub fn proc_ref(&self) -> ProcRef {
        ProcRef {
            pid: self.pid,
            start: self.start,
        }
    }
}

/// How a foreground child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExitInfo {
    pub pid: u32,
    /// Exit code when the child exited normally.
    pub code: Option<i32>,
    /// Signal number when the child was killed by a signal.
    pub signal: Option<i32>,
    /// Milliseconds from start to exit.
    pub ran_ms: u64,
}

impl ExitInfo {
    /// A child that exited with `code`.
    pub fn exited(pid: u32, code: i32, ran_ms: u64) -> ExitInfo {
        ExitInfo {
            pid,
            code: Some(code),
            signal: None,
            ran_ms,
        }
    }

    /// A child killed by signal `signal`.
    pub fn signaled(pid: u32, signal: i32, ran_ms: u64) -> ExitInfo {
        ExitInfo {
            pid,
            code: None,
            signal: Some(signal),
            ran_ms,
        }
    }
}

/// Operating-system calls herdr-wake needs.
pub trait Platform {
    /// Facts about `pid`, or `None` when it is gone or unreadable.
    fn process(&self, pid: u32) -> Option<ProcInfo>;
    /// True when `p.pid` is running and its start stamp equals `p.start`.
    fn alive(&self, p: &ProcRef) -> bool;
    /// An id that changes on every boot.
    fn boot_id(&self) -> String;
    /// The argv that starts `shell` as the terminal's shell: adds `-l` on
    /// macOS (core drops its own `-l` once `shell` is set), unchanged on Linux.
    fn login_shell_argv(&self, shell: &Path) -> Vec<String>;
    /// Replaces this process with `argv`, with exactly `env` as its
    /// environment and `cwd` as its folder. Returns only on failure.
    fn exec(&self, argv: &[String], env: &BTreeMap<String, String>, cwd: &Path) -> std::io::Error;
    /// Runs `argv` with exactly `env` in `cwd` as the terminal's foreground
    /// job and waits for it: the child gets its own process group and the
    /// terminal; afterwards this process takes the terminal back and restores
    /// its signal dispositions.
    fn run_foreground(
        &self,
        argv: &[String],
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> std::io::Result<ExitInfo>;
    /// True while the OS is shutting down or restarting.
    fn shutting_down(&self) -> bool;
    /// The running herdr-core server's pid, found by process lookup (never
    /// by running `hc`).
    fn core_pid(&self) -> Option<u32>;
    fn boot_service(&self) -> &dyn BootService;
    /// Wall-clock milliseconds since the Unix epoch. For display (log
    /// lines, the summary's timestamps) and for record sequence numbers
    /// only: the wall clock can step, backwards too, while herdr-wake runs
    /// (a time daemon correcting a wrong hardware clock during boot).
    fn now_ms(&self) -> u64;
    /// Milliseconds on a clock that only moves forward, never steps, keeps
    /// counting while the machine sleeps, and reads the same in every
    /// process of one boot (`CLOCK_BOOTTIME` on Linux, `CLOCK_MONOTONIC`
    /// on macOS). Everything that measures elapsed time or orders events
    /// within one boot uses this: gate slots, stagger, the first-alone
    /// hold, the gate confirm and the boot run's duration.
    fn mono_ms(&self) -> u64;
}

/// The launchd or systemd job that runs `herdr-wake boot`.
pub trait BootService {
    /// Installs the job. With `dry_run`, writes nothing and returns the
    /// actions it would take.
    fn install(&self, o: &BootOpts, dry_run: bool) -> anyhow::Result<Vec<Action>>;
    /// Removes the job. With `dry_run`, writes nothing.
    fn remove(&self, dry_run: bool) -> anyhow::Result<Vec<Action>>;
    fn state(&self) -> BootState;
}

/// How the boot job is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BootState {
    NotInstalled,
    /// Starts at login or auto-login (macOS LaunchAgent, Linux user unit).
    AtLogin,
    /// Starts before login (macOS LaunchDaemon, Linux user unit plus linger).
    BeforeLogin,
    /// Installed but switched off.
    Disabled,
}

/// Options for `BootService::install`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootOpts {
    pub before_login: bool,
    /// Absolute path of the herdr-wake binary the job runs.
    pub bin: PathBuf,
}

/// One step a setup or boot-service call took (or would take in a dry run),
/// for reports. `what` is a short sentence; `path` the file it touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub what: String,
    pub path: Option<PathBuf>,
}

impl Action {
    pub fn new(what: impl Into<String>, path: Option<PathBuf>) -> Action {
        Action {
            what: what.into(),
            path,
        }
    }
}

/// The adapter for the OS this binary was built for.
#[cfg(target_os = "macos")]
pub fn current() -> Box<dyn Platform> {
    Box::new(macos::MacOs::new())
}

/// The adapter for the OS this binary was built for.
#[cfg(target_os = "linux")]
pub fn current() -> Box<dyn Platform> {
    Box::new(linux::Linux::new())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("herdr-wake supports macOS and Linux only");

/// Shared by the Unix adapters: restores default dispositions for SIGINT,
/// SIGQUIT and SIGTTOU, then `execve` with a cleared environment.
#[cfg(unix)]
pub(crate) fn unix_exec(
    argv: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let Some((prog, args)) = argv.split_first() else {
        return std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty argv");
    };
    for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTTOU] {
        // SAFETY: resetting a disposition to SIG_DFL installs no handler.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
        }
    }
    std::process::Command::new(prog)
        .args(args)
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .exec()
}

/// Milliseconds on the system clock `clock` (0 if it cannot be read).
#[cfg(unix)]
pub(crate) fn clock_ms(clock: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec, owned here.
    if unsafe { libc::clock_gettime(clock, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64)
        .saturating_mul(1000)
        .saturating_add(ts.tv_nsec as u64 / 1_000_000)
}

/// Wall-clock milliseconds since the Unix epoch (0 if the clock is before it).
pub(crate) fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
