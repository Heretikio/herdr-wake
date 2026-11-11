//! Wake engine (Task 9): what `herdr-wake shell` does in a recovered
//! terminal.
//!
//! herdr-core launches `herdr-wake shell` for every recovered kept terminal
//! and retries a failing shell forever without falling back, so this module
//! has one hard rule: every path ends in an exec of a real shell. Errors,
//! core timeouts (2 s total per connection) and panics all lead there; only
//! when every exec fails does `shell_entry` exit 1.
//!
//! The flow: read the terminal's record over core's socket, `decide`, and on
//! `Resume` claim the session, wait at the start gate, bump the attempt
//! count, run the agent as the terminal's foreground job, and judge its exit:
//! a quit ends the record, a kill or a shutdown leaves it, a fast failure is
//! retried up to `attempts` times and then marked failed with a one-line
//! message. Then exec the real shell in the record's folder.

use std::collections::BTreeMap;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::agents::{self, Agent, Ctx, Plan};
use crate::config::Config;
use crate::core::{socket, Core, CoreError, TerminalId};
use crate::gate;
use crate::paths;
use crate::platform::{ExitInfo, Platform};
use crate::record::{self, AgentKind, Record, State};
use crate::trust;

/// Total time one core connection may take (connect, write and read).
pub const CORE_BUDGET: Duration = Duration::from_secs(2);
/// A child that exits non-zero sooner than this counts as a failed attempt.
pub const FAST_FAIL_MS: u64 = 5000;
/// Pause between a failed attempt and the next one.
pub const RETRY_PAUSE_MS: u64 = 2000;
/// Longest wait for one of a plan's `before` commands.
pub const BEFORE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a caller waiting at the gate re-checks its slot.
const GATE_POLL_MS: u64 = 250;
/// Set in the resumed agent's environment, so its hook knows the wrapper
/// supervises it.
pub const SUPERVISED_ENV: &str = "HERDR_WAKE_SUPERVISED";

/// What to do with a recovered terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Start the plain shell; nothing to resume.
    Shell,
    /// Run this plan, then the shell.
    Resume(Plan),
    /// Cannot resume: `reason` says why, `manual` is a shell command a person
    /// can paste to try it themselves (none when the record itself is what
    /// cannot be trusted).
    GiveUp {
        reason: String,
        manual: Option<String>,
    },
}

/// Decides what a terminal with record `rec` should do. Pure apart from what
/// the agent's `resume_plan` probes.
///
/// `Shell` when there is no record, it is `Ended` or `Failed`, resuming is
/// switched off, the folder is under an excluded folder, or the record is
/// from this boot and its agent is still running. `GiveUp` when the attempts
/// are used up, the plan is `Permanent`, there is no adapter, or the folder
/// fails `trust::folder_is_safe`. Otherwise `Resume`.
pub fn decide(rec: Option<&Record>, agent: Option<&dyn Agent>, ctx: &mut Ctx) -> Decision {
    let Some(rec) = rec else {
        return Decision::Shell;
    };
    if rec.state != State::Running || !ctx.cfg.enabled {
        return Decision::Shell;
    }
    if crate::config::is_excluded(&rec.cwd, &ctx.cfg.exclude_dirs, ctx.home) {
        return Decision::Shell;
    }
    if rec.boot_id == ctx.platform.boot_id()
        && rec.agent_proc.is_some_and(|p| ctx.platform.alive(&p))
    {
        return Decision::Shell;
    }
    let Some(agent) = agent else {
        return Decision::GiveUp {
            reason: format!("no adapter for {}", rec.agent),
            manual: None,
        };
    };
    // The label is data: nothing in it is used before it is checked. A
    // session id that could be read as an option or a path is never put on
    // a command line, not even a printed one.
    if !trust::valid_session_id(&rec.session_id) {
        return Decision::GiveUp {
            reason: "the recorded session id is not valid".into(),
            manual: None,
        };
    }
    // Only the variables a record may hold reach the agent, whatever the
    // label carries.
    let clean = Record {
        env: record::scrub_env(&rec.env),
        ..rec.clone()
    };
    let rec = &clean;
    let uid = trust::current_uid();
    // A folder that is gone is the adapter's to report ("folder missing").
    if std::fs::symlink_metadata(&rec.cwd).is_ok() {
        if let Err(why) = trust::folder_is_safe(&rec.cwd, uid) {
            return Decision::GiveUp {
                reason: format!("folder is not safe to resume in: {why}"),
                manual: Some(manual_hint(rec, agent, ctx.cfg)),
            };
        }
    }
    let plan = agent.resume_plan(rec, ctx);
    // Attempts count per boot: a record that ran out on an earlier boot
    // starts again at 0.
    let used = rec.attempts_on(&ctx.platform.boot_id());
    if used >= ctx.cfg.attempts {
        let manual = match &plan {
            Plan::Run { argv, cwd, .. } => manual_command(cwd, argv),
            Plan::Permanent(_) => manual_hint(rec, agent, ctx.cfg),
        };
        return Decision::GiveUp {
            reason: format!("{used} attempts failed"),
            manual: Some(manual),
        };
    }
    match plan {
        Plan::Permanent(reason) => Decision::GiveUp {
            reason,
            manual: Some(manual_hint(rec, agent, ctx.cfg)),
        },
        run => match trust::bin_is_safe(rec.agent, &rec.bin, uid) {
            Ok(()) => Decision::Resume(run),
            // No command to paste: it would name the very program that
            // failed the check.
            Err(why) => Decision::GiveUp {
                reason: format!("the recorded program is not safe to run: {why}"),
                manual: None,
            },
        },
    }
}

/// The shell command a person can paste to resume `rec` by hand: the
/// agent's own plan when it has one, else a best effort from the record.
/// `None` when the record's session id is not usable on a command line.
pub fn manual_for(rec: &Record, agent: &dyn Agent, ctx: &mut Ctx) -> Option<String> {
    if !trust::valid_session_id(&rec.session_id) {
        return None;
    }
    let clean = Record {
        env: record::scrub_env(&rec.env),
        ..rec.clone()
    };
    Some(match agent.resume_plan(&clean, ctx) {
        Plan::Run { argv, cwd, .. } => manual_command(&cwd, &argv),
        Plan::Permanent(_) => manual_hint(&clean, agent, ctx.cfg),
    })
}

/// `s` as one POSIX shell word: bare when it holds only safe characters,
/// else in single quotes (a `'` inside becomes `'\''`).
pub fn shell_quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `cd <cwd> && <argv...>`, every word quoted.
fn manual_command(cwd: &Path, argv: &[String]) -> String {
    let words: Vec<String> = argv.iter().map(|a| shell_quote(a)).collect();
    format!(
        "cd {} && {}",
        shell_quote(&cwd.to_string_lossy()),
        words.join(" ")
    )
}

/// A best-effort resume command from the record alone, for when the agent
/// has no plan to offer. The stored flags go through the agent's flag
/// policy again, so nothing it would drop now is printed.
fn manual_hint(rec: &Record, agent: &dyn Agent, cfg: &Config) -> String {
    let bin = rec.bin.to_string_lossy().into_owned();
    let mut launch = vec![bin.clone()];
    launch.extend(rec.flags.iter().cloned());
    let mut argv = vec![bin];
    let flags = agent.restorable_flags(&launch, cfg).into_iter();
    match rec.agent {
        AgentKind::Claude => {
            argv.extend(flags);
            argv.extend(["--resume".to_string(), rec.session_id.clone()]);
        }
        AgentKind::Codex => {
            argv.push("resume".to_string());
            argv.extend(flags);
            argv.push(rec.session_id.clone());
        }
        AgentKind::Pi => {
            argv.extend(flags);
            let target = rec
                .session_file
                .as_ref()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| rec.session_id.clone());
            argv.extend(["--session".to_string(), target]);
        }
        AgentKind::OpenCode => {
            argv.extend(flags);
            argv.extend(["--session".to_string(), rec.session_id.clone()]);
        }
    }
    manual_command(&rec.cwd, &argv)
}

/// The replaceable parts of `shell_main`, so tests run the whole flow
/// without a socket, real sleeps, real adapters or stderr.
pub struct Seams<'a> {
    /// A fresh core connection with its own time budget.
    pub connect: &'a dyn Fn() -> Result<Box<dyn Core>, CoreError>,
    /// Sleeps this many milliseconds.
    pub sleep: &'a dyn Fn(u64),
    /// The adapter for an agent kind.
    pub agent: &'a dyn Fn(AgentKind) -> Box<dyn Agent>,
    /// Prints one line for the person at the terminal (stderr).
    pub say: &'a dyn Fn(&str),
    /// Arranges a `confirm_check` for a resume that is about to start,
    /// without blocking the caller.
    pub confirm: &'a dyn Fn(ConfirmJob),
}

/// How long a resumed agent has to report itself when its plan sets no
/// `confirm_within_ms`.
pub const DEFAULT_CONFIRM_MS: u64 = 30_000;

/// One resume whose agent should re-stamp the record within `within_ms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmJob {
    pub terminal: TerminalId,
    pub session_id: String,
    pub within_ms: u64,
    /// `gate::boot_key` at the time of the resume.
    pub boot_key: String,
}

/// `<state dir>/unconfirmed/<boot key>/<terminal>`: present while a resume
/// on this boot has not been confirmed by its agent. Holds the reason.
pub fn unconfirmed_file(
    env: &BTreeMap<String, String>,
    home: &Path,
    boot_key: &str,
    t: &TerminalId,
) -> PathBuf {
    let name: String = t
        .as_str()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    paths::unconfirmed_dir(env, home).join(boot_key).join(name)
}

/// Checks, once the deadline of `job` has passed, that the resumed agent
/// reported itself: the record still names the same session and holds a
/// live agent process. `Err` says what is wrong, for the log and the boot
/// summary. A record that is gone, ended or failed has nothing to confirm.
pub fn confirm_check(
    core: &mut dyn Core,
    platform: &dyn Platform,
    job: &ConfirmJob,
) -> Result<(), String> {
    let rec = match record::load(core, &job.terminal) {
        Ok(Some(rec)) => rec,
        Ok(None) => return Ok(()),
        Err(e) => return Err(format!("could not read the record: {e}")),
    };
    if rec.state != State::Running {
        return Ok(());
    }
    let live = rec.agent_proc.is_some_and(|p| platform.alive(&p));
    if rec.session_id != job.session_id {
        return Err(format!(
            "the agent came up as session {} instead of {}",
            rec.session_id, job.session_id
        ));
    }
    if !live {
        return Err(format!(
            "the agent did not report session {} within {} s (is its hook or plugin installed and trusted?)",
            job.session_id,
            job.within_ms / 1000
        ));
    }
    Ok(())
}

/// Deletes every folder in `parent` except `keep`: state keyed by an
/// earlier boot or core is never consulted again.
fn remove_other_dirs(parent: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name() != keep && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The real `Seams::confirm`: a thread that sleeps until the deadline, runs
/// `confirm_check` over a fresh connection, and on a failure logs it and
/// writes `unconfirmed_file`. It never touches the agent. The thread dies
/// with the wrapper when the agent exits first.
fn spawn_confirm_check(job: ConfirmJob, env: BTreeMap<String, String>, home: PathBuf) {
    let _ = std::thread::Builder::new()
        .name("herdr-wake-confirm".into())
        .spawn(move || {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                std::thread::sleep(Duration::from_millis(job.within_ms));
                let platform = crate::platform::current();
                let Ok(mut core) = socket::connect(&socket::socket_path(&env, &home), CORE_BUDGET)
                else {
                    return;
                };
                if let Err(why) = confirm_check(&mut core, platform.as_ref(), &job) {
                    mark_unconfirmed(&env, &home, &job, &why, platform.now_ms());
                }
            }));
        });
}

/// Logs an unconfirmed resume and leaves its marker for `herdr-wake boot`.
pub fn mark_unconfirmed(
    env: &BTreeMap<String, String>,
    home: &Path,
    job: &ConfirmJob,
    why: &str,
    now_ms: u64,
) {
    crate::hook::log_line(
        env,
        home,
        now_ms,
        &format!("shell {}: resume unconfirmed: {why}", job.terminal),
    );
    let file = unconfirmed_file(env, home, &job.boot_key, &job.terminal);
    remove_other_dirs(&paths::unconfirmed_dir(env, home), &job.boot_key);
    let _ = crate::private::write(&file, why.as_bytes());
}

/// `herdr-wake shell` as a process entry point: never returns. Exits 1 only
/// when every shell exec failed.
pub fn shell_entry(platform: &dyn Platform, env: BTreeMap<String, String>) -> ! {
    let home = env
        .get("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| passwd().and_then(|p| p.dir))
        .unwrap_or_else(|| PathBuf::from("/"));
    let err = shell_main(platform, &env, &home);
    let _ = writeln!(std::io::stderr(), "herdr-wake: cannot start a shell: {err}");
    std::process::exit(1)
}

/// The whole `herdr-wake shell` behavior over the real socket client, real
/// sleeps and the real adapters. Returns only when the final exec failed.
pub fn shell_main(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    home: &Path,
) -> std::io::Error {
    let connect = || -> Result<Box<dyn Core>, CoreError> {
        let path = socket::socket_path(env, home);
        Ok(Box::new(socket::connect(&path, CORE_BUDGET)?))
    };
    let sleep = |ms: u64| std::thread::sleep(Duration::from_millis(ms));
    let agent = |k: AgentKind| agents::get(k);
    let say = |line: &str| {
        let _ = writeln!(std::io::stderr(), "{line}");
    };
    let confirm = |job: ConfirmJob| spawn_confirm_check(job, env.clone(), home.to_path_buf());
    let seams = Seams {
        connect: &connect,
        sleep: &sleep,
        agent: &agent,
        say: &say,
        confirm: &confirm,
    };
    shell_main_with(platform, env, home, &seams)
}

/// `shell_main` with its seams supplied. Returns only when the final exec
/// failed (always, with a platform whose exec only records the call).
pub fn shell_main_with(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    home: &Path,
    seams: &Seams,
) -> std::io::Error {
    let loaded = catch_unwind(AssertUnwindSafe(|| {
        Config::load(&paths::config_file(env, home)).map_err(|e| format!("{e:#}"))
    }))
    .unwrap_or_else(|_| Err("it could not be read".to_string()));
    let cfg = match loaded {
        Ok(cfg) => cfg,
        Err(why) => {
            // A config that cannot be parsed says nothing about what the
            // user wants, and the defaults restore bypass flags: resume
            // nothing, start the plain shell, and say why in the log.
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let t = env.get("HC_TERMINAL_ID").map_or("-", String::as_str);
                crate::hook::log_line(
                    env,
                    home,
                    platform.now_ms(),
                    &format!(
                        "shell {t}: config.toml cannot be parsed, so nothing is resumed \
                         (`herdr-wake doctor` shows the error): {why}"
                    ),
                );
            }));
            return exec_shell(platform, env, home, &Config::default(), None);
        }
    };
    let folder = catch_unwind(AssertUnwindSafe(|| {
        wake_body(platform, env, home, &cfg, seams)
    }))
    .ok()
    .flatten();
    exec_shell(platform, env, home, &cfg, folder.as_deref())
}

/// Releases a session claim when dropped, including while unwinding.
struct ClaimGuard<'a> {
    dir: PathBuf,
    session_id: String,
    t: &'a TerminalId,
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        record::release_claim(&self.dir, &self.session_id, self.t);
    }
}

/// Everything before the final exec. Returns the folder the shell should
/// start in (the record's), or `None` for the terminal's own folder.
fn wake_body(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    home: &Path,
    cfg: &Config,
    seams: &Seams,
) -> Option<PathBuf> {
    let tid = TerminalId::from(
        env.get("HC_TERMINAL_ID")
            .filter(|t| !t.is_empty())?
            .as_str(),
    );
    let mut core = (seams.connect)().ok()?;
    let term = core.get(&tid).ok()?;
    if record::is_off(&term) {
        // `herdr-wake off <terminal>`: a plain shell, whatever the label says.
        return None;
    }
    let rec = record::of_terminal(&term)?;
    let created_ms = term.created_ms;
    if rec.state == State::Running && cfg.enabled && !trusted(env, home, &tid, created_ms, &rec) {
        untrusted(platform, env, home, &tid);
        return None;
    }
    let folder = Some(rec.cwd.clone());
    let agent = (seams.agent)(rec.agent);
    let decision = {
        let mut ctx = Ctx {
            platform,
            core: &mut *core,
            env,
            home,
            cfg,
        };
        decide(Some(&rec), Some(&*agent), &mut ctx)
    };
    let mut plan = match decision {
        Decision::Shell => return folder,
        Decision::GiveUp { reason, manual } => {
            give_up(&mut *core, &tid, &rec, &reason, manual.as_deref(), seams);
            return folder;
        }
        Decision::Resume(plan) => plan,
    };
    drop(core);

    let key = gate::boot_key(platform);
    // Claims are keyed by boot and core, so they lapse at a restart; the
    // folders of earlier boots are dead weight.
    remove_other_dirs(&paths::claims_dir(env, home), &key);
    let claims = paths::claims_dir(env, home).join(&key);
    if !record::claim(&claims, &rec.session_id, &tid) {
        return folder;
    }
    let _claim = ClaimGuard {
        dir: claims,
        session_id: rec.session_id.clone(),
        t: &tid,
    };
    wait_at_gate(platform, env, home, cfg, &key, rec.agent, seams);

    let shell_program = shell_argvs(platform, env, cfg)
        .first()
        .and_then(|a| a.first().cloned());
    let mut rec = rec;
    let mut tries: u8 = 0;
    loop {
        let Plan::Run {
            argv,
            env: plan_env,
            cwd,
            before,
            confirm_within_ms,
        } = plan
        else {
            return Some(rec.cwd);
        };
        let mut core = (seams.connect)().ok()?;
        let ours = record::begin_attempt(&mut *core, &tid, &rec.session_id, &platform.boot_id())
            .ok()??;
        tries = tries.saturating_add(1);
        drop(core);

        let mut child_env = env.clone();
        if let Some(sh) = &shell_program {
            child_env
                .entry("SHELL".to_string())
                .or_insert_with(|| sh.clone());
        }
        child_env.extend(plan_env);
        child_env.insert(SUPERVISED_ENV.to_string(), "1".to_string());
        for cmd in &before {
            run_before(cmd, &child_env, &cwd);
        }
        (seams.confirm)(ConfirmJob {
            terminal: tid.clone(),
            session_id: ours.session_id.clone(),
            within_ms: confirm_within_ms.unwrap_or(DEFAULT_CONFIRM_MS),
            boot_key: key.clone(),
        });
        let exit = platform.run_foreground(&argv, &child_env, &cwd);

        let mut core = (seams.connect)().ok()?;
        let cur = record::load(&mut *core, &tid).ok()?;
        let folder = cur.as_ref().map_or(rec.cwd.clone(), |c| c.cwd.clone());
        // The stored record is still this wrapper's to change when the
        // resumed child re-stamped it, or when nobody wrote since our bump.
        let is_ours = |c: &Record, child: Option<u32>| {
            (child.is_some() && c.agent_proc.map(|p| p.pid) == child)
                || (c.session_id == ours.session_id && c.seq == ours.seq)
        };
        let failure = match &exit {
            Ok(x) => {
                if killed(x) || platform.shutting_down() {
                    return Some(folder);
                }
                if quit(x) {
                    if let Some(c) = cur.filter(|c| is_ours(c, Some(x.pid))) {
                        if let Ok(true) = record::end(&mut *core, &tid, &c.session_id, c.agent_proc)
                        {
                            record::remove_copy_of(
                                &paths::records_dir(env, home),
                                &tid,
                                &c.session_id,
                            );
                        }
                    }
                    return Some(folder);
                }
                if x.ran_ms >= FAST_FAIL_MS {
                    return Some(folder);
                }
                describe(x)
            }
            Err(e) => {
                if platform.shutting_down() {
                    return Some(folder);
                }
                format!("it could not start: {e}")
            }
        };
        let child = exit.as_ref().ok().map(|x| x.pid);
        let Some(cur) = cur.filter(|c| is_ours(c, child)) else {
            return Some(folder);
        };
        drop(core);

        if tries < cfg.attempts {
            (seams.sleep)(RETRY_PAUSE_MS);
        }
        let mut core = (seams.connect)().ok()?;
        rec = record::load(&mut *core, &tid).ok()?.unwrap_or(cur);
        if !trusted(env, home, &tid, created_ms, &rec) {
            untrusted(platform, env, home, &tid);
            return None;
        }
        let mut probe = rec.clone();
        probe.attempts = probe.attempts.max(tries);
        let decision = {
            let mut ctx = Ctx {
                platform,
                core: &mut *core,
                env,
                home,
                cfg,
            };
            decide(Some(&probe), Some(&*agent), &mut ctx)
        };
        plan = match decision {
            Decision::Shell => return Some(rec.cwd),
            Decision::GiveUp { reason, manual } => {
                let reason = format!("{failure} ({reason})");
                give_up(&mut *core, &tid, &rec, &reason, manual.as_deref(), seams);
                return Some(rec.cwd);
            }
            Decision::Resume(p) => p,
        };
    }
}

/// True when the label's record is the one herdr-wake itself stored for
/// this terminal (see `record::copy_matches`).
fn trusted(
    env: &BTreeMap<String, String>,
    home: &Path,
    t: &TerminalId,
    created_ms: u64,
    rec: &Record,
) -> bool {
    record::copy_matches(&paths::records_dir(env, home), t, created_ms, rec)
}

/// Logs that a record was not acted on. Nothing from the record is printed
/// or logged, and the record is left as it is.
fn untrusted(platform: &dyn Platform, env: &BTreeMap<String, String>, home: &Path, t: &TerminalId) {
    crate::hook::log_line(
        env,
        home,
        platform.now_ms(),
        &format!(
            "shell {t}: record does not match the local copy; not resumed \
             (`herdr-wake status` shows it as untrusted, `herdr-wake resume {t}` accepts it)"
        ),
    );
}

/// Marks the record failed and prints the one line a person sees.
fn give_up(
    core: &mut dyn Core,
    t: &TerminalId,
    rec: &Record,
    reason: &str,
    manual: Option<&str>,
    seams: &Seams,
) {
    let _ = record::fail(core, t, &rec.session_id, reason);
    (seams.say)(&give_up_line(rec, reason, manual));
}

/// The one line a person sees when a session cannot be resumed. Control
/// characters from the record are stripped.
pub fn give_up_line(rec: &Record, reason: &str, manual: Option<&str>) -> String {
    let id = if trust::valid_session_id(&rec.session_id) {
        rec.session_id.as_str()
    } else {
        "(unusable id)"
    };
    let retry = manual.map_or_else(String::new, |m| format!(" To retry: {m}"));
    trust::printable(&format!(
        "herdr-wake: could not resume {} session {id}: {reason}.{retry}",
        rec.agent
    ))
}

/// Takes a gate slot and sleeps until it comes, re-checking now and then so
/// a `gate::confirm` can shorten the wait. Never sleeps longer in total than
/// the wait first given.
fn wait_at_gate(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    home: &Path,
    cfg: &Config,
    key: &str,
    kind: AgentKind,
    seams: &Seams,
) {
    let dir = paths::gate_dir(env, home);
    // Slots are compared across processes for the length of one boot: the
    // monotonic clock, never the wall clock (which can step during boot).
    let ticket = gate::enter_ticket(&dir, kind, cfg, key, platform.mono_ms());
    let mut budget = ticket.wait_ms;
    let mut left = budget;
    while left > 0 {
        let step = left.min(GATE_POLL_MS);
        (seams.sleep)(step);
        budget -= step;
        left = gate::remaining(&dir, kind, cfg, key, &ticket, platform.mono_ms()).min(budget);
    }
}

/// Killed by a hang-up, terminate or kill signal: the machine or core is
/// going down, or someone killed it on purpose. Not a quit.
fn killed(x: &ExitInfo) -> bool {
    matches!(x.signal, Some(s) if s == libc::SIGHUP || s == libc::SIGTERM || s == libc::SIGKILL)
}

/// Exit 0, exit 130 or death by SIGINT: the person quit the agent.
fn quit(x: &ExitInfo) -> bool {
    matches!(x.code, Some(0) | Some(130)) || x.signal == Some(libc::SIGINT)
}

fn describe(x: &ExitInfo) -> String {
    match (x.code, x.signal) {
        (Some(c), _) => format!("it exited with code {c} after {} ms", x.ran_ms),
        (None, Some(s)) => format!("it died from signal {s} after {} ms", x.ran_ms),
        (None, None) => format!("it ended after {} ms", x.ran_ms),
    }
}

/// Runs one `before` command to completion, killing it after
/// `BEFORE_TIMEOUT`. Its output is discarded and failures are ignored: the
/// agent's own start decides whether the resume worked.
fn run_before(argv: &[String], env: &BTreeMap<String, String>, cwd: &Path) {
    let Some((prog, args)) = argv.split_first() else {
        return;
    };
    let Ok(mut child) = Command::new(prog)
        .args(args)
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let deadline = Instant::now() + BEFORE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

/// True when `prog` is herdr-wake itself, which must never be the shell.
fn is_self(prog: &str) -> bool {
    let p = Path::new(prog);
    if p.file_name().is_some_and(|n| n == "herdr-wake") {
        return true;
    }
    match (std::fs::canonicalize(p), std::env::current_exe()) {
        (Ok(a), Ok(b)) => std::fs::canonicalize(b).is_ok_and(|b| a == b),
        _ => false,
    }
}

/// The shells to try, best first: the saved real shell (as given), else
/// `$SHELL`, else the passwd login shell (both as login shells), else
/// `/bin/sh`; then `$SHELL` as a login shell; then `/bin/sh`.
fn shell_argvs(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    cfg: &Config,
) -> Vec<Vec<String>> {
    let usable = |s: &str| !s.is_empty() && !is_self(s);
    let from_env = env
        .get("SHELL")
        .filter(|s| usable(s))
        .map(|s| platform.login_shell_argv(Path::new(s)));
    let primary = cfg
        .real_shell
        .clone()
        .filter(|a| a.first().is_some_and(|p| usable(p)))
        .or_else(|| from_env.clone())
        .or_else(|| {
            passwd()
                .and_then(|p| p.shell)
                .filter(|s| usable(&s.to_string_lossy()))
                .map(|s| platform.login_shell_argv(&s))
        })
        .unwrap_or_else(|| vec!["/bin/sh".to_string()]);
    let mut out = vec![primary];
    for a in [from_env, Some(vec!["/bin/sh".to_string()])]
        .into_iter()
        .flatten()
    {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// Execs the real shell, falling back down `shell_argvs`, in `folder` when
/// it exists, else the terminal's own folder; last of all `/bin/sh` in `/`.
/// Returns the last exec error.
fn exec_shell(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    home: &Path,
    cfg: &Config,
    folder: Option<&Path>,
) -> std::io::Error {
    let argvs = catch_unwind(AssertUnwindSafe(|| shell_argvs(platform, env, cfg)))
        .unwrap_or_else(|_| vec![vec!["/bin/sh".to_string()]]);
    let cwd = folder
        .filter(|f| f.is_dir())
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok().filter(|d| d.is_dir()))
        .or_else(|| env.get("PWD").map(PathBuf::from).filter(|d| d.is_dir()))
        .or_else(|| Some(home.to_path_buf()).filter(|d| d.is_dir()))
        .unwrap_or_else(|| PathBuf::from("/"));
    let mut shell_env = env.clone();
    if let Some(sh) = argvs.first().and_then(|a| a.first()) {
        shell_env
            .entry("SHELL".to_string())
            .or_insert_with(|| sh.clone());
    }
    let mut last = std::io::Error::other("no shell to start");
    for argv in &argvs {
        last = platform.exec(argv, &shell_env, &cwd);
    }
    if cwd != Path::new("/") {
        last = platform.exec(&["/bin/sh".to_string()], &shell_env, Path::new("/"));
    }
    last
}

/// The current user's passwd entry: login shell and home folder.
struct Passwd {
    shell: Option<PathBuf>,
    dir: Option<PathBuf>,
}

fn passwd() -> Option<Passwd> {
    use std::ffi::CStr;
    // SAFETY: getpwuid_r writes into `pwd` and `buf`, both owned here and
    // large enough as declared; the strings read afterwards point into `buf`
    // and are copied out before it is dropped.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut buf = vec![0 as libc::c_char; 16 * 1024];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        );
        if rc != 0 || result.is_null() {
            return None;
        }
        let field = |p: *const libc::c_char| {
            if p.is_null() {
                return None;
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            (!s.is_empty()).then(|| PathBuf::from(s))
        };
        Some(Passwd {
            shell: field(pwd.pw_shell),
            dir: field(pwd.pw_dir),
        })
    }
}
