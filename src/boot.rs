//! `herdr-wake boot`: starts herdr-core at boot and waits for the resumes to
//! settle (Task 12).
//!
//! 1. Load `boot.env` (`setup::parse_boot_env`, never shell-evaluated) and merge it
//!    under the process environment for `hc` (see `child_env` for `PATH`).
//! 2. If core already runs (`Platform::core_pid` or a socket that answers),
//!    skip the start. Otherwise wait up to 30 s for a non-loopback address,
//!    then proceed regardless, and run `hc server start`. This is the only
//!    place herdr-wake ever runs `hc`.
//! 3. Poll once a second for up to 120 s until every record that could still
//!    resume has a live agent, has failed or has used up its attempts.
//! 4. Write the counts to `last-boot.json` atomically.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::core::{Core, CoreError};
use crate::platform::Platform;
use crate::record::State;
use crate::report::{self, Probes};

/// Longest wait for a network address before starting core anyway, in s.
pub const NETWORK_WAIT_S: u64 = 30;
/// Longest wait for the resumes to settle, in s.
pub const SETTLE_WAIT_S: u64 = 120;

/// What one `herdr-wake boot` run did; written to `last-boot.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootSummary {
    pub boot_id: String,
    /// Wall-clock time the run started, for display. The clock may be
    /// wrong this early in a boot and may be stepped while the run lasts.
    pub started_ms: u64,
    /// `started_ms` plus `duration_ms`: never before the start, even when
    /// the wall clock was stepped back during the run.
    pub finished_ms: u64,
    /// How long the run took, by the monotonic clock.
    #[serde(default)]
    pub duration_ms: u64,
    /// This run started core (`hc server start` exited 0).
    pub core_started: bool,
    /// Records whose agent runs on this boot.
    pub resumed: u32,
    /// Records marked failed, out of attempts, or still waiting at the
    /// timeout.
    pub failed: u32,
    /// Records ended by the user, opted out, or excluded.
    pub skipped: u32,
    /// Resumes that were started but whose agent never reported itself
    /// within its deadline (see `wake::confirm_check`). The agent may well
    /// be running; herdr-wake just cannot tell which session it holds.
    #[serde(default)]
    pub unconfirmed: u32,
    /// One line per notable step and per terminal with a record.
    pub details: Vec<String>,
}

/// Everything `run` reads besides the runner and sleeper.
pub struct BootParts<'a> {
    pub platform: &'a dyn Platform,
    /// The merged environment the child gets (see `child_env`).
    pub env: &'a BTreeMap<String, String>,
    pub home: &'a Path,
    pub cfg: &'a Config,
    /// This herdr-wake binary, for the `hc` next to it.
    pub exe: Option<PathBuf>,
    /// File checks for the `hc` lookup.
    pub probes: &'a dyn Probes,
    /// True once the machine has a non-loopback address.
    pub network_up: &'a dyn Fn() -> bool,
    /// A fresh connection to core; never starts one.
    pub connect: &'a dyn Fn() -> Result<Box<dyn Core>, CoreError>,
}

/// The child environment: `boot` (from `boot.env`) with `process` on top, so
/// a variable the boot job itself sets wins. `PATH` is the exception: the
/// boot job's own `PATH` is usually the bare system default (launchd), so the
/// result is `boot.env`'s folders first, then any of the process's folders
/// not already listed.
pub fn child_env(
    process: &BTreeMap<String, String>,
    boot: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = boot.clone();
    env.extend(process.iter().map(|(k, v)| (k.clone(), v.clone())));
    if let Some(boot_path) = boot.get("PATH") {
        let mut dirs: Vec<&str> = boot_path.split(':').filter(|d| !d.is_empty()).collect();
        if let Some(p) = process.get("PATH") {
            for d in p.split(':').filter(|d| !d.is_empty()) {
                if !dirs.contains(&d) {
                    dirs.push(d);
                }
            }
        }
        env.insert("PATH".to_string(), dirs.join(":"));
    }
    env
}

/// The `hc` to run, in order: `HERDR_WAKE_HC`, `hc` next to `exe`,
/// `~/.local/bin/hc`, the first `hc` on `PATH`. `Err` says why none was
/// found (an override that names a missing program is not passed over).
pub fn find_hc(
    env: &BTreeMap<String, String>,
    home: &Path,
    exe: Option<&Path>,
    probes: &dyn Probes,
) -> Result<PathBuf, String> {
    if let Some(o) = env.get("HERDR_WAKE_HC").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(o);
        return if probes.executable(&p) {
            Ok(p)
        } else {
            Err(format!(
                "HERDR_WAKE_HC names {o}, which is not an executable file"
            ))
        };
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe.and_then(Path::parent) {
        candidates.push(dir.join("hc"));
    }
    candidates.push(home.join(".local/bin/hc"));
    if let Some(path) = env.get("PATH") {
        candidates.extend(
            path.split(':')
                .filter(|d| !d.is_empty())
                .map(|d| Path::new(d).join("hc")),
        );
    }
    candidates
        .into_iter()
        .find(|p| probes.executable(p))
        .ok_or_else(|| {
            "hc not found (HERDR_WAKE_HC, next to herdr-wake, ~/.local/bin/hc, PATH): core not started"
                .to_string()
        })
}

/// Where the resumes stand on one poll.
#[derive(Debug, Default)]
struct Tally {
    resumed: u32,
    failed: u32,
    skipped: u32,
    unconfirmed: u32,
    waiting: Vec<String>,
    details: Vec<String>,
}

fn tally(parts: &BootParts, core: &mut dyn Core) -> Result<Tally, CoreError> {
    let boot = parts.platform.boot_id();
    let key = crate::gate::boot_key(parts.platform);
    let copies = crate::paths::records_dir(parts.env, parts.home);
    let mut terms = core.list(false)?;
    terms.sort_by(|a, b| report::terminal_order(&a.address, &b.address));
    let mut t = Tally::default();
    for term in terms {
        let Some(rec) = report::record_of(&term) else {
            continue;
        };
        let name = format!("{} {}", term.address, rec.agent);
        let excluded = crate::config::is_excluded(&rec.cwd, &parts.cfg.exclude_dirs, parts.home);
        match &rec.state {
            State::Ended => {
                t.skipped += 1;
                t.details.push(format!("{name}: ended"));
            }
            State::Failed { reason } => {
                t.failed += 1;
                t.details.push(format!("{name}: failed: {reason}"));
            }
            State::Running if report::agent_live(&rec, parts.platform, &boot) => {
                t.resumed += 1;
                t.details.push(format!("{name}: resumed"));
            }
            State::Running if crate::record::is_off(&term) => {
                t.skipped += 1;
                t.details.push(format!("{name}: skipped (switched off)"));
            }
            State::Running if !parts.cfg.enabled || excluded => {
                t.skipped += 1;
                let why = if excluded {
                    "folder excluded"
                } else {
                    "herdr-wake disabled"
                };
                t.details.push(format!("{name}: skipped ({why})"));
            }
            State::Running if report::untrusted(&rec, &term, &copies) => {
                t.failed += 1;
                t.details.push(format!(
                    "{name}: untrusted: record does not match the local copy, not resumed"
                ));
            }
            State::Running if rec.attempts_on(&boot) >= parts.cfg.attempts => {
                t.failed += 1;
                t.details
                    .push(format!("{name}: gave up after {} attempts", rec.attempts));
            }
            State::Running => {
                let marker =
                    crate::wake::unconfirmed_file(parts.env, parts.home, &key, &term.address);
                match std::fs::read_to_string(&marker) {
                    Ok(why) => {
                        t.unconfirmed += 1;
                        t.details
                            .push(format!("{name}: unconfirmed: {}", why.trim()));
                    }
                    Err(_) => t.waiting.push(name),
                }
            }
        }
    }
    // The lines hold text from records (reasons, names): they are printed
    // by `boot` and `status` and written to the log.
    for line in t.details.iter_mut().chain(t.waiting.iter_mut()) {
        *line = crate::trust::printable(line);
    }
    Ok(t)
}

fn core_up(parts: &BootParts) -> Option<String> {
    if let Some(pid) = parts.platform.core_pid() {
        return Some(format!("pid {pid}"));
    }
    (parts.connect)().ok().map(|_| "socket answers".to_string())
}

/// Runs the boot sequence (see the module docs) with `runner` running a
/// command and returning its exit code, and `sleeper` waiting the given ms.
/// Writes `last-boot.json` and returns the summary. Never fails: problems
/// end up in `details`.
pub fn run(
    parts: &BootParts,
    runner: &dyn Fn(&[String]) -> io::Result<i32>,
    sleeper: &dyn Fn(u64),
) -> BootSummary {
    let began = parts.platform.mono_ms();
    let mut s = BootSummary {
        boot_id: parts.platform.boot_id(),
        started_ms: parts.platform.now_ms(),
        finished_ms: 0,
        duration_ms: 0,
        core_started: false,
        resumed: 0,
        failed: 0,
        skipped: 0,
        unconfirmed: 0,
        details: Vec::new(),
    };
    if let Some(how) = core_up(parts) {
        s.details
            .push(format!("core already running ({how}): start skipped"));
    } else if !start_core(parts, runner, sleeper, &mut s) {
        return finish(parts, s, began);
    }
    settle(parts, sleeper, &mut s);
    finish(parts, s, began)
}

/// Waits for the network, then runs `hc server start`. False when there is
/// no point in waiting for resumes (no `hc`, or it could not be run).
fn start_core(
    parts: &BootParts,
    runner: &dyn Fn(&[String]) -> io::Result<i32>,
    sleeper: &dyn Fn(u64),
    s: &mut BootSummary,
) -> bool {
    let mut waited = 0;
    while !(parts.network_up)() {
        if waited >= NETWORK_WAIT_S {
            s.details.push(format!(
                "no network address after {NETWORK_WAIT_S} s: starting core anyway"
            ));
            break;
        }
        sleeper(1000);
        waited += 1;
    }
    if waited > 0 && waited < NETWORK_WAIT_S {
        s.details.push(format!("network came up after {waited} s"));
    }
    let hc = match find_hc(parts.env, parts.home, parts.exe.as_deref(), parts.probes) {
        Ok(hc) => hc,
        Err(why) => {
            s.details.push(why);
            return false;
        }
    };
    let argv = vec![
        hc.display().to_string(),
        "server".to_string(),
        "start".to_string(),
    ];
    match runner(&argv) {
        Ok(0) => {
            s.core_started = true;
            s.details
                .push(format!("started core: {} server start", hc.display()));
            true
        }
        Ok(code) => {
            s.details
                .push(format!("{} server start exited with {code}", hc.display()));
            true
        }
        Err(e) => {
            s.details
                .push(format!("could not run {}: {e}", hc.display()));
            false
        }
    }
}

/// Polls until no record is still waiting to resume, or the timeout.
fn settle(parts: &BootParts, sleeper: &dyn Fn(u64), s: &mut BootSummary) {
    let mut last: Option<Tally> = None;
    let mut last_err = None;
    for second in 0..=SETTLE_WAIT_S {
        match (parts.connect)().and_then(|mut core| tally(parts, core.as_mut())) {
            Ok(t) => {
                let done = t.waiting.is_empty();
                last = Some(t);
                if done {
                    break;
                }
            }
            Err(e) => last_err = Some(e),
        }
        if second < SETTLE_WAIT_S {
            sleeper(1000);
        }
    }
    let Some(mut t) = last else {
        let why = last_err.map_or_else(String::new, |e| format!(": {e}"));
        s.details
            .push(format!("core did not answer within {SETTLE_WAIT_S} s{why}"));
        return;
    };
    if !t.waiting.is_empty() {
        s.details.push(format!(
            "timed out after {SETTLE_WAIT_S} s with {} still waiting",
            t.waiting.len()
        ));
        for name in t.waiting.drain(..) {
            t.failed += 1;
            t.details.push(format!(
                "{name}: did not come back within {SETTLE_WAIT_S} s"
            ));
        }
    }
    s.resumed = t.resumed;
    s.failed = t.failed;
    s.skipped = t.skipped;
    s.unconfirmed = t.unconfirmed;
    s.details.append(&mut t.details);
}

fn finish(parts: &BootParts, mut s: BootSummary, began: u64) -> BootSummary {
    s.duration_ms = parts.platform.mono_ms().saturating_sub(began);
    s.finished_ms = s.started_ms.saturating_add(s.duration_ms);
    let path = crate::paths::last_boot_file(parts.env, parts.home);
    if let Err(e) = write_summary(&path, &s) {
        s.details
            .push(format!("could not write {}: {e}", path.display()));
    }
    // The run keeps its own log: a LaunchDaemon's plist names no output
    // file (launchd would open it as root, in a folder the user can write).
    let now = parts.platform.now_ms();
    let log = |line: &str| crate::hook::log_line(parts.env, parts.home, now, line);
    log(&format!("boot: {}", render_summary(&s)));
    for d in &s.details {
        log(&format!("boot:   {d}"));
    }
    s
}

/// Writes `s` as JSON to `path` atomically (temporary file, then rename),
/// creating the folder.
pub fn write_summary(path: &Path, s: &BootSummary) -> io::Result<()> {
    let body = serde_json::to_string_pretty(s).map_err(io::Error::other)?;
    crate::private::write(path, (body + "\n").as_bytes())
}

/// The one-line result, for example `9 resumed, 1 failed`, with
/// `, N unconfirmed` and `, N skipped` when there were any.
pub fn render_summary(s: &BootSummary) -> String {
    let mut line = format!("{} resumed, {} failed", s.resumed, s.failed);
    if s.unconfirmed > 0 {
        line.push_str(&format!(", {} unconfirmed", s.unconfirmed));
    }
    if s.skipped > 0 {
        line.push_str(&format!(", {} skipped", s.skipped));
    }
    line
}

/// True when any interface has an address that is not loopback, link-local
/// or unspecified. When the interfaces cannot be read, assumes the network
/// is up (so boot never waits for nothing).
pub fn network_up() -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `ifap` with a list we free below; we only
    // read entries while the list is alive and check every pointer first.
    unsafe {
        if libc::getifaddrs(&mut ifap) != 0 {
            return true;
        }
        let mut up = false;
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            let loopback = ifa.ifa_flags & (libc::IFF_LOOPBACK as libc::c_uint) != 0;
            if !ifa.ifa_addr.is_null() && !loopback {
                match i32::from((*ifa.ifa_addr).sa_family) {
                    libc::AF_INET => {
                        let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                        up |= !(ip.is_loopback() || ip.is_link_local() || ip.is_unspecified());
                    }
                    libc::AF_INET6 => {
                        let sin6 = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                        let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                        let link_local = ip.segments()[0] & 0xffc0 == 0xfe80;
                        up |= !(ip.is_loopback() || ip.is_unspecified() || link_local);
                    }
                    _ => {}
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        up
    }
}

/// `herdr-wake boot`. Prints the summary line and details; always returns 0
/// (a boot job that fails would only be retried or flagged by the OS).
pub fn main() -> i32 {
    let result = std::panic::catch_unwind(|| {
        let s = main_inner();
        println!("herdr-wake boot: {}", render_summary(&s));
        for d in &s.details {
            println!("  {d}");
        }
    });
    if result.is_err() {
        eprintln!("herdr-wake boot: internal error");
    }
    0
}

fn main_inner() -> BootSummary {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let process: BTreeMap<String, String> = std::env::vars().collect();
    let home = process
        .get("HOME")
        .filter(|h| !h.is_empty())
        .map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let boot_env = std::fs::read_to_string(crate::paths::boot_env_file(&process, &home))
        .map(|t| crate::setup::parse_boot_env(&t))
        .unwrap_or_default();
    let env = child_env(&process, &boot_env);
    let platform = crate::platform::current();
    // With a config that cannot be parsed the wrapper resumes nothing, so
    // there is nothing to wait for: core is still started, and the summary
    // counts the records as skipped.
    let cfg = Config::load(&crate::paths::config_file(&env, &home)).unwrap_or_else(|e| {
        crate::hook::log_line(
            &env,
            &home,
            platform.now_ms(),
            &format!("boot: config.toml cannot be parsed, so nothing is resumed: {e:#}"),
        );
        Config {
            enabled: false,
            ..Config::default()
        }
    });
    let socket = crate::core::socket::socket_path(&env, &home);
    let connect = || {
        crate::core::socket::connect(&socket, Duration::from_secs(2))
            .map(|c| Box::new(c) as Box<dyn Core>)
    };
    let probes = report::SystemProbes;
    let parts = BootParts {
        platform: platform.as_ref(),
        env: &env,
        home: &home,
        cfg: &cfg,
        exe: std::env::current_exe().ok(),
        probes: &probes,
        network_up: &network_up,
        connect: &connect,
    };
    let runner = |argv: &[String]| -> io::Result<i32> {
        let (prog, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
        let status = Command::new(prog)
            .args(args)
            .env_clear()
            .envs(&env)
            .current_dir(&home)
            .stdin(Stdio::null())
            .status()?;
        Ok(status.code().unwrap_or(-1))
    };
    let sleeper = |ms: u64| std::thread::sleep(Duration::from_millis(ms));
    run(&parts, &runner, &sleeper)
}
