//! In-memory `Platform` for tests in this crate and in `tests/`. Holds a
//! process table, a settable boot id and clock, a scripted queue of
//! `run_foreground` results, settable `shutting_down` and `core_pid`, and
//! records `exec` and `run_foreground` calls instead of running anything.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{Action, BootOpts, BootService, BootState, ExitInfo, Platform, ProcInfo};
use crate::record::ProcRef;

/// One recorded `Platform::exec` or `Platform::run_foreground` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecCall {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
}

/// See the module docs. Starts with no processes, boot id `boot-1` and the
/// clock at 1_700_000_000_000 ms, the monotonic clock at 1_000_000 ms, an empty `run_foreground` queue, not
/// shutting down, and no core pid.
#[derive(Debug)]
pub struct FakePlatform {
    procs: Mutex<BTreeMap<u32, ProcInfo>>,
    boot_id: Mutex<String>,
    now: Mutex<u64>,
    mono: Mutex<u64>,
    execs: Mutex<Vec<ExecCall>>,
    runs: Mutex<Vec<ExecCall>>,
    run_results: Mutex<VecDeque<std::io::Result<ExitInfo>>>,
    shutting_down: Mutex<bool>,
    core_pid: Mutex<Option<u32>>,
    boot: FakeBootService,
}

impl Default for FakePlatform {
    fn default() -> Self {
        FakePlatform {
            procs: Mutex::new(BTreeMap::new()),
            boot_id: Mutex::new("boot-1".to_string()),
            now: Mutex::new(1_700_000_000_000),
            mono: Mutex::new(1_000_000),
            execs: Mutex::new(Vec::new()),
            runs: Mutex::new(Vec::new()),
            run_results: Mutex::new(VecDeque::new()),
            shutting_down: Mutex::new(false),
            core_pid: Mutex::new(None),
            boot: FakeBootService::default(),
        }
    }
}

impl FakePlatform {
    pub fn new() -> FakePlatform {
        FakePlatform::default()
    }

    /// Adds or replaces the process with `p.pid`.
    pub fn add_process(&self, p: ProcInfo) {
        self.procs.lock().unwrap().insert(p.pid, p);
    }

    pub fn remove_process(&self, pid: u32) {
        self.procs.lock().unwrap().remove(&pid);
    }

    pub fn set_boot_id(&self, id: &str) {
        *self.boot_id.lock().unwrap() = id.to_string();
    }

    /// Steps the wall clock (forwards or backwards), as a time daemon
    /// does. The monotonic clock is not touched.
    pub fn set_now(&self, ms: u64) {
        *self.now.lock().unwrap() = ms;
    }

    /// Sets the monotonic clock.
    pub fn set_mono(&self, ms: u64) {
        *self.mono.lock().unwrap() = ms;
    }

    /// Lets `ms` of time pass: both clocks move forward.
    pub fn advance(&self, ms: u64) {
        *self.now.lock().unwrap() += ms;
        *self.mono.lock().unwrap() += ms;
    }

    /// Every `exec` call so far, oldest first.
    pub fn execs(&self) -> Vec<ExecCall> {
        self.execs.lock().unwrap().clone()
    }

    /// Queues the result of a later `run_foreground` call; calls take
    /// results in the order they were pushed.
    pub fn push_run_result(&self, r: std::io::Result<ExitInfo>) {
        self.run_results.lock().unwrap().push_back(r);
    }

    /// Every `run_foreground` call so far, oldest first.
    pub fn runs(&self) -> Vec<ExecCall> {
        self.runs.lock().unwrap().clone()
    }

    pub fn set_shutting_down(&self, v: bool) {
        *self.shutting_down.lock().unwrap() = v;
    }

    pub fn set_core_pid(&self, pid: Option<u32>) {
        *self.core_pid.lock().unwrap() = pid;
    }

    /// The fake boot service, to inspect or preset its state.
    pub fn boot(&self) -> &FakeBootService {
        &self.boot
    }
}

impl Platform for FakePlatform {
    fn process(&self, pid: u32) -> Option<ProcInfo> {
        self.procs.lock().unwrap().get(&pid).cloned()
    }

    fn alive(&self, p: &ProcRef) -> bool {
        self.procs
            .lock()
            .unwrap()
            .get(&p.pid)
            .is_some_and(|q| q.start == p.start)
    }

    fn boot_id(&self) -> String {
        self.boot_id.lock().unwrap().clone()
    }

    /// Always `[shell, "-l"]`, like macOS.
    fn login_shell_argv(&self, shell: &Path) -> Vec<String> {
        vec![shell.to_string_lossy().into_owned(), "-l".to_string()]
    }

    /// Records the call and returns an `Other` error ("fake exec").
    fn exec(&self, argv: &[String], env: &BTreeMap<String, String>, cwd: &Path) -> std::io::Error {
        self.execs.lock().unwrap().push(ExecCall {
            argv: argv.to_vec(),
            env: env.clone(),
            cwd: cwd.to_path_buf(),
        });
        std::io::Error::other("fake exec")
    }

    /// Records the call and returns the next queued result, or an `Other`
    /// error ("fake run_foreground: no scripted result") when none is queued.
    fn run_foreground(
        &self,
        argv: &[String],
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> std::io::Result<ExitInfo> {
        self.runs.lock().unwrap().push(ExecCall {
            argv: argv.to_vec(),
            env: env.clone(),
            cwd: cwd.to_path_buf(),
        });
        self.run_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                Err(std::io::Error::other(
                    "fake run_foreground: no scripted result",
                ))
            })
    }

    fn shutting_down(&self) -> bool {
        *self.shutting_down.lock().unwrap()
    }

    fn core_pid(&self) -> Option<u32> {
        *self.core_pid.lock().unwrap()
    }

    fn boot_service(&self) -> &dyn BootService {
        &self.boot
    }

    fn now_ms(&self) -> u64 {
        *self.now.lock().unwrap()
    }

    fn mono_ms(&self) -> u64 {
        *self.mono.lock().unwrap()
    }
}

/// A boot service that only tracks its state. `install` reports one action
/// on `/fake/boot-service` and, unless `dry_run`, moves to `AtLogin` or
/// `BeforeLogin`; `remove` moves to `NotInstalled`.
#[derive(Debug)]
pub struct FakeBootService {
    state: Mutex<BootState>,
}

impl Default for FakeBootService {
    fn default() -> Self {
        FakeBootService {
            state: Mutex::new(BootState::NotInstalled),
        }
    }
}

impl FakeBootService {
    pub fn set_state(&self, s: BootState) {
        *self.state.lock().unwrap() = s;
    }
}

impl BootService for FakeBootService {
    fn install(&self, o: &BootOpts, dry_run: bool) -> anyhow::Result<Vec<Action>> {
        if !dry_run {
            self.set_state(if o.before_login {
                BootState::BeforeLogin
            } else {
                BootState::AtLogin
            });
        }
        Ok(vec![Action::new(
            "install boot service",
            Some(PathBuf::from("/fake/boot-service")),
        )])
    }

    fn remove(&self, dry_run: bool) -> anyhow::Result<Vec<Action>> {
        if !dry_run {
            self.set_state(BootState::NotInstalled);
        }
        Ok(vec![Action::new(
            "remove boot service",
            Some(PathBuf::from("/fake/boot-service")),
        )])
    }

    fn state(&self) -> BootState {
        *self.state.lock().unwrap()
    }
}
