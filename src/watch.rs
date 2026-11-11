//! `herdr-wake watch`: a detached watcher that marks a session ended when
//! the user quit the agent (Task 10, the Forget flow in `docs/DESIGN.md`).
//!
//! The hook starts one per agent the user launched by hand. When the agent
//! exits and its shell is still the same live process a moment later, the
//! user quit, so the record is marked ended. Every doubt leaves the record
//! in place, which leans toward resuming: a stop signal (the OS kills
//! processes at shutdown, and an interactive zsh ignores SIGTERM, so "the
//! shell outlived the agent" can be faked), a shutdown in progress, a gone
//! shell, a new boot, an unreachable core, or a stored record that no longer
//! names the watched agent and shell.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::core::{socket, Core, CoreError, TerminalId};
use crate::hook::{home_of, log_line, process_env};
use crate::platform::{self, Platform};
use crate::record::{self, ProcRef, State};

/// How often the agent is checked, in ms.
const POLL_MS: u64 = 1000;
/// How long to wait after the agent is gone before judging, in ms.
const GRACE_MS: u64 = 2000;
/// After this long the watcher gives up, in ms.
const GIVE_UP_MS: u64 = 30 * 24 * 3600 * 1000;
/// Time budget for the core connection.
const CORE_BUDGET: Duration = Duration::from_secs(2);

/// What a watcher did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchOutcome {
    /// The user quit: the record was marked `Ended`.
    EndedRecord,
    /// The record was left as it was; the text says why.
    LeftRecord(&'static str),
}

/// Watches `agent` in terminal `t` until it exits (polling every second,
/// giving up after 30 days), waits 2 s, and marks the record ended only if
/// none of these hold: `stop()`, a shutdown in progress, `shell` not alive,
/// a boot id other than the one at start. The record must still be this
/// agent's (compare-and-set on `agent_proc`), `Running`, and name `shell`
/// as its shell. Shutdown is checked again just before the write.
pub fn run(
    t: &TerminalId,
    agent: ProcRef,
    shell: ProcRef,
    platform: &dyn Platform,
    connect: &dyn Fn() -> Result<Box<dyn Core>, CoreError>,
    sleeper: &dyn Fn(u64),
    stop: &dyn Fn() -> bool,
) -> WatchOutcome {
    let boot = platform.boot_id();
    let mut waited = 0u64;
    while platform.alive(&agent) {
        if stop() {
            return WatchOutcome::LeftRecord("stopped");
        }
        if waited >= GIVE_UP_MS {
            return WatchOutcome::LeftRecord("gave up after 30 days");
        }
        sleeper(POLL_MS);
        waited += POLL_MS;
    }

    sleeper(GRACE_MS);
    if stop() {
        return WatchOutcome::LeftRecord("stopped");
    }
    if platform.shutting_down() {
        return WatchOutcome::LeftRecord("shutting down");
    }
    if !platform.alive(&shell) {
        return WatchOutcome::LeftRecord("shell gone with the agent");
    }
    if platform.boot_id() != boot {
        return WatchOutcome::LeftRecord("boot id changed");
    }

    let mut core = match connect() {
        Ok(c) => c,
        Err(_) => return WatchOutcome::LeftRecord("core not reachable"),
    };
    let rec = match record::load(core.as_mut(), t) {
        Ok(Some(r)) => r,
        Ok(None) => return WatchOutcome::LeftRecord("no record"),
        Err(_) => return WatchOutcome::LeftRecord("core not reachable"),
    };
    if rec.agent_proc != Some(agent) {
        return WatchOutcome::LeftRecord("record belongs to another agent process");
    }
    if rec.shell_proc != Some(shell) {
        return WatchOutcome::LeftRecord("record has no shell to outlive");
    }
    if rec.state != State::Running {
        return WatchOutcome::LeftRecord("record not running");
    }

    if stop() {
        return WatchOutcome::LeftRecord("stopped");
    }
    if platform.shutting_down() {
        return WatchOutcome::LeftRecord("shutting down");
    }
    match record::end(core.as_mut(), t, &rec.session_id, Some(agent)) {
        Ok(true) => WatchOutcome::EndedRecord,
        Ok(false) => WatchOutcome::LeftRecord("record changed before the write"),
        Err(_) => WatchOutcome::LeftRecord("core not reachable"),
    }
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
    // A stop signal means the OS or the user is tearing things down: leave
    // at once, writing nothing. `_exit` is async-signal-safe.
    // SAFETY: `_exit` ends the process without running any Rust code.
    unsafe { libc::_exit(0) }
}

fn install_stop_handler() {
    for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        // SAFETY: the handler only stores an atomic and calls `_exit`, both
        // async-signal-safe; the zeroed sigaction is a valid starting value.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = 0;
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

/// Entry point for `herdr-wake watch <terminal> <agent pid> <agent start>
/// <shell pid> <shell start>`. Installs the SIGTERM, SIGHUP and SIGINT
/// handler (exit at once without writing), runs `run` against the real
/// platform and core, logs one line and returns 0. Prints nothing.
pub fn main(t: &TerminalId, agent: ProcRef, shell: ProcRef) -> i32 {
    install_stop_handler();
    std::panic::set_hook(Box::new(|_| {}));
    let env = process_env();
    let home = home_of(&env);
    let platform = platform::current();
    let socket = socket::socket_path(&env, &home);

    let connect = || -> Result<Box<dyn Core>, CoreError> {
        Ok(Box::new(socket::connect(&socket, CORE_BUDGET)?))
    };
    let sleeper = |ms: u64| std::thread::sleep(Duration::from_millis(ms));
    let stop = || STOP.load(Ordering::SeqCst);

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(
            t,
            agent,
            shell,
            platform.as_ref(),
            &connect,
            &sleeper,
            &stop,
        )
    }));
    if let Ok(WatchOutcome::EndedRecord) = outcome {
        // The session was quit: its local copy goes too, so the record
        // cannot be made to resume by flipping the label back to running.
        if let Ok(Some(rec)) = connect().and_then(|mut c| record::load(c.as_mut(), t)) {
            if rec.state == State::Ended {
                record::remove_copy_of(&crate::paths::records_dir(&env, &home), t, &rec.session_id);
            }
        }
    }
    let text = match outcome {
        Ok(WatchOutcome::EndedRecord) => "ended record (user quit)".to_string(),
        Ok(WatchOutcome::LeftRecord(why)) => format!("left record: {why}"),
        Err(_) => "left record: panic".to_string(),
    };
    log_line(
        &env,
        &home,
        platform.now_ms(),
        &format!(
            "watch {t} agent={}:{} shell={}:{}: {text}",
            agent.pid, agent.start, shell.pid, shell.start
        ),
    );
    0
}
