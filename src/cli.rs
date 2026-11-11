//! Command-line parsing and subcommand dispatch (Task 13).
//!
//! `shell` and `hook` are dispatched by hand before clap sees the arguments:
//! herdr-core retries a shell that fails forever, and an agent can break on
//! a hook that prints or exits non-zero, so neither may reach a parser that
//! can do either. Everything else goes through clap.
//!
//! `status`, `doctor`, `forget`, `resume` and `adopt` reach herdr-core only
//! through its socket (a connection never starts a server) and never run
//! `hc`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use clap::{Parser, Subcommand};

use crate::agents::{self, Ctx};
use crate::config::Config;
use crate::core::{socket, Core, CoreError, Hello, Terminal, TerminalId};
use crate::hook::{self, HookOutcome, Start};
use crate::platform::{self, Action, Platform, RUN_PREFIX};
use crate::record::{self, AgentKind, ProcRef, State};
use crate::report::{self, Level, NoCore, SystemProbes};
use crate::{boot, paths, setup, trust, wake, watch};

/// Time budget for each call to herdr-core.
const CORE_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Parser)]
#[command(
    name = "herdr-wake",
    version,
    about = "Starts herdr-core at boot and resumes coding-agent sessions after a reboot"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Set up agent hooks, herdr-core's shell setting and the boot service
    #[command(
        long_about = "Set up agent hooks, herdr-core's shell setting and the boot service.\n\n\
        Only agents that are installed are set up: an agent counts as installed when its \
        config folder exists or its program is on PATH. The others are skipped and nothing \
        is created for them; run install again after installing one. Every file it changes \
        is backed up first under ~/.local/state/herdr-wake/backups. The boot job starts as \
        soon as it is installed, so herdr-core starts now if it is not already running.\n\n\
        Run it as your own user, never as root or under sudo."
    )]
    Install {
        /// Start herdr-core at boot, before anyone logs in
        #[arg(
            long,
            long_help = "Start herdr-core at boot, before anyone logs in.\n\n\
            macOS: prepares a LaunchDaemon instead of a LaunchAgent and prints the sudo \
            commands for you to run (herdr-wake itself never runs as root). Claude Code may \
            come up signed out before login, because its login sits in the Keychain.\n\
            Linux: has no effect. Install always tries to turn on linger, which already \
            starts the user service at boot."
        )]
        before_login: bool,
        /// Show what would change and write nothing
        #[arg(long)]
        dry_run: bool,
        /// Copy this binary to an SSH host and run install there
        #[arg(long, value_name = "NAME")]
        host: Option<String>,
    },
    /// Undo install: restore herdr-core's shell, then remove hooks and the boot service
    Uninstall {
        /// Show what would change and write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// List recorded agent sessions and whether each is running
    #[command(
        long_about = "List recorded agent sessions and whether each is running.\n\n\
        States: running, ended (you quit it), failed: <reason>, untrusted (the record does \
        not match herdr-wake's local copy, so it is not resumed; `herdr-wake resume \
        <terminal>` accepts it, `herdr-wake forget <terminal>` drops it), off (switched off \
        with `herdr-wake off`).\n\n\
        Reads herdr-core's socket and never starts herdr-core."
    )]
    Status {
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Check the setup and say how to fix what is wrong
    #[command(long_about = "Check the setup and say how to fix what is wrong.\n\n\
        Never starts herdr-core. Exits 1 when a check fails.")]
    Doctor {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Remove a terminal's record so its session is not resumed
    Forget {
        /// The terminal, for example t3
        terminal: String,
    },
    /// Switch herdr-wake off for one terminal: forget its record, record nothing there
    #[command(long_about = "Switch herdr-wake off for one terminal.\n\n\
        Removes the terminal's record and marks the terminal (label wake.off=1 in \
        herdr-core): from then on no agent started there is recorded and nothing is \
        resumed there. An agent that is running in it keeps running. `herdr-wake on \
        <terminal>` undoes it; `herdr-wake status` shows such terminals as off.")]
    Off {
        /// The terminal, for example t3
        terminal: String,
    },
    /// Switch herdr-wake back on for a terminal that was switched off
    #[command(
        long_about = "Switch herdr-wake back on for a terminal that was switched off.\n\n\
        Removes the mark that `herdr-wake off` set. The next agent started in the \
        terminal is recorded again. An agent that is already running there is not \
        recorded until it starts again (or, for Claude Code, until `herdr-wake adopt`)."
    )]
    On {
        /// The terminal, for example t3
        terminal: String,
    },
    /// Let a failed session resume again, and print the command to run it now
    #[command(
        long_about = "Let a failed session resume again, and print the command to run it now.\n\n\
        Resets a record that failed or used up its attempts, so the terminal resumes at \
        the next recovery (after a reboot or a herdr-core restart). It does not start \
        the agent itself: it prints the command to run in that terminal now.\n\n\
        It also accepts a record that `herdr-wake status` shows as untrusted. Read the \
        printed command first: you are saying that record is yours."
    )]
    Resume {
        /// The terminal, for example t3
        terminal: String,
    },
    /// Record agents that were already running before herdr-wake was installed
    #[command(
        long_about = "Record agents that were already running before herdr-wake was installed.\n\n\
        Looks at the foreground program of every herdr-core terminal. Claude Code \
        sessions are recorded from Claude's own session files. Codex, Pi and OpenCode \
        cannot be identified from outside: restart those agents once to record them."
    )]
    Adopt,
    /// Internal: called by agent hooks and plugins
    #[command(hide = true)]
    Hook { agent: String, event: String },
    /// Internal: waits for an agent to exit
    #[command(hide = true)]
    Watch {
        terminal: String,
        agent_pid: u32,
        agent_start: u64,
        shell_pid: u32,
        shell_start: u64,
    },
    /// Internal: herdr-core's shell for recovered terminals
    #[command(hide = true)]
    Shell,
    /// Internal: run by the boot service
    #[command(hide = true)]
    Boot,
}

/// The name of `cmd` when it writes files in the user's home or changes
/// records, so it must run as that user and never as root.
fn writes_as_user(cmd: &Cmd) -> Option<&'static str> {
    match cmd {
        Cmd::Install { .. } => Some("install"),
        Cmd::Uninstall { .. } => Some("uninstall"),
        Cmd::Adopt => Some("adopt"),
        Cmd::Resume { .. } => Some("resume"),
        Cmd::Forget { .. } => Some("forget"),
        Cmd::Off { .. } => Some("off"),
        Cmd::On { .. } => Some("on"),
        _ => None,
    }
}

/// Refuses to run `command` as root (`euid` 0). As root, herdr-wake would
/// write root-owned files into the user's home and follow whatever links a
/// process of that user planted there; nothing it does needs root.
pub fn refuse_root(command: &str, euid: u32) -> Result<(), String> {
    if euid != 0 {
        return Ok(());
    }
    Err(format!(
        "`herdr-wake {command}` must not run as root or under sudo. Run it as your own user. \
         For `install --before-login`, herdr-wake prints the sudo commands for you to run \
         yourself; it never needs root."
    ))
}

/// The process entry point. Returns the exit code (`shell` never returns).
pub fn main() -> i32 {
    let args: Vec<OsString> = std::env::args_os().collect();
    match args.get(1).and_then(|a| a.to_str()) {
        Some("shell") => shell(),
        Some("hook") => return hook_command(&args[2..]),
        _ => {}
    }
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    if let Some(name) = writes_as_user(&cli.cmd) {
        // SAFETY: geteuid has no preconditions and cannot fail.
        if let Err(why) = refuse_root(name, unsafe { libc::geteuid() }) {
            eprintln!("herdr-wake: {why}");
            return 1;
        }
    }
    let result = match cli.cmd {
        Cmd::Shell => shell(),
        Cmd::Hook { agent, event } => {
            return hook_command(&[OsString::from(agent), OsString::from(event)])
        }
        Cmd::Watch {
            terminal,
            agent_pid,
            agent_start,
            shell_pid,
            shell_start,
        } => {
            return watch::main(
                &TerminalId(terminal),
                ProcRef {
                    pid: agent_pid,
                    start: agent_start,
                },
                ProcRef {
                    pid: shell_pid,
                    start: shell_start,
                },
            )
        }
        Cmd::Boot => return boot::main(),
        Cmd::Install {
            before_login,
            dry_run,
            host,
        } => install(before_login, dry_run, host.as_deref()),
        Cmd::Uninstall { dry_run } => uninstall(dry_run),
        Cmd::Status { json } => status(json),
        Cmd::Doctor { json } => doctor(json),
        Cmd::Forget { terminal } => forget(&TerminalId(terminal)),
        Cmd::Off { terminal } => switch(&TerminalId(terminal), true),
        Cmd::On { terminal } => switch(&TerminalId(terminal), false),
        Cmd::Resume { terminal } => resume(&TerminalId(terminal)),
        Cmd::Adopt => adopt(),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("herdr-wake: {e:#}");
            1
        }
    }
}

/// `herdr-wake shell`: nothing here may fail or print before the wake
/// engine takes over. If even the platform adapter cannot be built, start a
/// plain shell.
fn shell() -> ! {
    std::panic::set_hook(Box::new(|_| {}));
    let env = hook::process_env();
    match std::panic::catch_unwind(platform::current) {
        Ok(p) => wake::shell_entry(p.as_ref(), env),
        Err(_) => {
            let mut shells = vec!["/bin/sh".to_string()];
            if let Some(s) = env.get("SHELL").filter(|s| !s.is_empty()) {
                shells.insert(0, s.clone());
            }
            for sh in shells {
                let _ = platform::unix_exec(&[sh], &env, Path::new("/"));
            }
            std::process::exit(1)
        }
    }
}

/// `herdr-wake hook <agent> <event>`: prints nothing and returns 0 whatever
/// the arguments are.
fn hook_command(args: &[OsString]) -> i32 {
    let (Some(agent), Some(event)) = (
        args.first().and_then(|a| a.to_str()),
        args.get(1).and_then(|a| a.to_str()),
    ) else {
        return 0;
    };
    match agent.parse::<AgentKind>() {
        Ok(kind) => hook::main(kind, event),
        Err(_) => 0,
    }
}

/// A `Core` that connects afresh for every call, so each call gets its own
/// time budget and a long command (doctor watches terminals for seconds)
/// never runs out. Connecting never starts a core.
struct LiveCore {
    socket: PathBuf,
}

impl LiveCore {
    fn open(&self) -> Result<socket::SocketCore, CoreError> {
        socket::connect(&self.socket, CORE_BUDGET)
    }
}

impl Core for LiveCore {
    fn hello(&mut self) -> Result<Hello, CoreError> {
        self.open()?.hello()
    }
    fn get(&mut self, t: &TerminalId) -> Result<Terminal, CoreError> {
        self.open()?.get(t)
    }
    fn label(
        &mut self,
        t: &TerminalId,
        changes: &BTreeMap<String, Option<String>>,
    ) -> Result<Terminal, CoreError> {
        self.open()?.label(t, changes)
    }
    fn list(&mut self, all: bool) -> Result<Vec<Terminal>, CoreError> {
        self.open()?.list(all)
    }
}

/// What every user command works with.
struct Session {
    env: BTreeMap<String, String>,
    home: PathBuf,
    platform: Box<dyn Platform>,
    cfg: Config,
}

impl Session {
    fn new() -> anyhow::Result<Session> {
        Session::with_config(Config::load)
    }

    /// For `doctor`: a config that cannot be parsed does not stop the
    /// command (the defaults stand in); doctor reports it as a failed check.
    fn for_doctor() -> anyhow::Result<Session> {
        Session::with_config(|path| Ok(Config::load(path).unwrap_or_default()))
    }

    fn with_config(load: impl FnOnce(&Path) -> anyhow::Result<Config>) -> anyhow::Result<Session> {
        let env = hook::process_env();
        let home = env
            .get("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set"))?;
        let cfg = load(&paths::config_file(&env, &home))?;
        Ok(Session {
            env,
            home,
            platform: platform::current(),
            cfg,
        })
    }

    fn core(&self) -> LiveCore {
        LiveCore {
            socket: socket::socket_path(&self.env, &self.home),
        }
    }

    fn ctx<'a>(&'a self, core: &'a mut dyn Core) -> Ctx<'a> {
        Ctx {
            platform: self.platform.as_ref(),
            core,
            env: &self.env,
            home: &self.home,
            cfg: &self.cfg,
        }
    }
}

const CORE_DOWN: &str =
    "herdr-core is not running, so its records cannot be read or changed (this command did not start it)";

fn core_error(t: &TerminalId, e: CoreError) -> anyhow::Error {
    match e {
        CoreError::NotRunning => anyhow!(CORE_DOWN),
        CoreError::NotFound => anyhow!("herdr-core has no terminal {t}"),
        other => anyhow!("{other}"),
    }
}

// ---------------------------------------------------------------- install

/// The path of this binary to record at install: never a versioned one
/// (see `setup::pick_stable_path`).
fn this_binary() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("finding this herdr-wake binary")?;
    let canonical =
        std::fs::canonicalize(&exe).with_context(|| format!("resolving {}", exe.display()))?;
    // `herdr-wake` on PATH and in the usual bin folders, with its target.
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/home/linuxbrew/.linuxbrew/bin",
        ]
        .map(PathBuf::from),
    );
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".local/bin"));
    }
    let links: Vec<(PathBuf, PathBuf)> = dirs
        .into_iter()
        .filter(|d| d.is_absolute())
        .map(|d| d.join("herdr-wake"))
        .filter_map(|link| Some((link.clone(), std::fs::canonicalize(&link).ok()?)))
        .collect();
    setup::pick_stable_path(&exe, &canonical, &links).map_err(|why| anyhow!(why))
}

fn install(before_login: bool, dry_run: bool, host: Option<&str>) -> anyhow::Result<i32> {
    let bin = this_binary()?;
    if let Some(host) = host {
        if dry_run {
            println!("herdr-wake install --host {host}: dry run, nothing was run. It would run:");
            for argv in setup::remote::remote_install_commands(host, &bin, before_login) {
                let words: Vec<String> = argv.iter().map(|a| wake::shell_quote(a)).collect();
                println!("  {}", words.join(" "));
            }
            return Ok(0);
        }
        setup::remote::run_remote_install(host, &bin, before_login)?;
        return Ok(0);
    }
    let s = Session::new()?;
    let mut core = NoCore;
    let opts = setup::Options {
        before_login,
        dry_run,
        bin,
    };
    let report = setup::install(&opts, &mut s.ctx(&mut core))?;
    print!("{}", render_setup("install", dry_run, &report));
    Ok(0)
}

fn uninstall(dry_run: bool) -> anyhow::Result<i32> {
    let s = Session::new()?;
    let mut core = NoCore;
    let report = setup::uninstall(dry_run, &mut s.ctx(&mut core))?;
    print!("{}", render_setup("uninstall", dry_run, &report));
    Ok(0)
}

/// The text `install` and `uninstall` print: each step taken, the warnings,
/// and last (so they are not missed) the commands the user still has to run.
pub fn render_setup(command: &str, dry_run: bool, report: &setup::Report) -> String {
    let (notes, actions): (Vec<&Action>, Vec<&Action>) = report
        .actions
        .iter()
        .partition(|a| a.what.starts_with(setup::SKIPPED_PREFIX));
    let (done, manual): (Vec<&Action>, Vec<&Action>) = actions
        .into_iter()
        .partition(|a| !a.what.starts_with(RUN_PREFIX));
    let mut out = String::new();
    if dry_run {
        out.push_str(&format!(
            "herdr-wake {command}: dry run, nothing was changed. It would:\n"
        ));
    } else if done.is_empty() {
        out.push_str(&format!("herdr-wake {command}: nothing to change\n"));
    } else {
        out.push_str(&format!("herdr-wake {command}:\n"));
    }
    for a in &done {
        match &a.path {
            Some(p) if !a.what.contains(&p.display().to_string()) => {
                out.push_str(&format!("  {} ({})\n", a.what, p.display()));
            }
            _ => out.push_str(&format!("  {}\n", a.what)),
        }
    }
    // Agents that are not installed: left alone, and said so.
    for a in &notes {
        out.push_str(&format!("  {}\n", a.what));
    }
    if !notes.is_empty() {
        out.push_str(
            "  Run `herdr-wake install` again after you install another agent: it picks up \
             the new one and changes nothing else.\n",
        );
    }
    for w in &report.warnings {
        out.push_str(&format!("warning: {w}\n"));
    }
    if !manual.is_empty() {
        out.push_str(if dry_run {
            "\nYOU WOULD STILL HAVE TO RUN THESE YOURSELF:\n"
        } else {
            "\nNOT FINISHED. RUN THESE YOURSELF:\n"
        });
        for a in &manual {
            out.push_str(&format!(
                "  {}\n",
                a.what.strip_prefix(RUN_PREFIX).unwrap_or(&a.what)
            ));
        }
    }
    if command == "install" {
        out.push_str(if dry_run {
            "\nInstalling starts the boot job at once: herdr-core would start right away if it is not running.\n"
        } else {
            "\nThe boot job starts as soon as it is installed: herdr-core starts now if it was not running.\n\
             Run `herdr-wake doctor` to check the setup.\n"
        });
    }
    out
}

// ----------------------------------------------------------------- status

fn last_boot(s: &Session) -> Option<boot::BootSummary> {
    let text = std::fs::read_to_string(paths::last_boot_file(&s.env, &s.home)).ok()?;
    serde_json::from_str(&text).ok()
}

fn status(json: bool) -> anyhow::Result<i32> {
    let s = Session::new()?;
    let mut core = s.core();
    let (rows, up) = match report::status(&mut s.ctx(&mut core)) {
        Ok(rows) => (rows, true),
        Err(CoreError::NotRunning) => (Vec::new(), false),
        Err(e) => bail!("{e}"),
    };
    let last = last_boot(&s);
    if json {
        let sessions: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "terminal": r.terminal.as_str(),
                    "agent": r.agent.map(AgentKind::as_str),
                    "session_id": r.session_id,
                    "cwd": r.cwd,
                    "state": r.state,
                    "attempts": r.attempts,
                    "live": r.running,
                })
            })
            .collect();
        let out = serde_json::json!({
            "core_running": up,
            "sessions": sessions,
            "last_boot": last,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(0);
    }
    print!("{}", report::render_status(&rows, up));
    if let Some(b) = last {
        let when = if b.boot_id == s.platform.boot_id() {
            "this boot"
        } else {
            "an earlier boot"
        };
        println!("last boot run ({when}): {}", boot::render_summary(&b));
    }
    Ok(0)
}

fn doctor(json: bool) -> anyhow::Result<i32> {
    let s = Session::for_doctor()?;
    let mut core = s.core();
    let findings = report::doctor(&mut s.ctx(&mut core), &SystemProbes);
    let failed = findings.iter().filter(|f| f.level == Level::Fail).count();
    if json {
        let warnings = findings.iter().filter(|f| f.level == Level::Warn).count();
        let list: Vec<serde_json::Value> = findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "level": match f.level {
                        Level::Ok => "ok",
                        Level::Warn => "warn",
                        Level::Fail => "fail",
                    },
                    "check": f.check,
                    "detail": f.detail,
                    "fix": f.fix,
                })
            })
            .collect();
        let out = serde_json::json!({
            "findings": list,
            "failed": failed,
            "warnings": warnings,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", report::render_doctor(&findings));
    }
    Ok(i32::from(failed > 0))
}

// ------------------------------------------------- forget, resume, adopt

fn forget(t: &TerminalId) -> anyhow::Result<i32> {
    let s = Session::new()?;
    let mut core = s.core();
    let removed = record::forget(&mut core, t).map_err(|e| core_error(t, e))?;
    record::remove_copy(&paths::records_dir(&s.env, &s.home), t);
    if removed {
        println!("{t}: record removed; its session will not be resumed");
    } else {
        println!("{t}: no record to remove");
    }
    Ok(0)
}

/// `herdr-wake off <terminal>` and `herdr-wake on <terminal>`.
fn switch(t: &TerminalId, off: bool) -> anyhow::Result<i32> {
    let s = Session::new()?;
    let mut core = s.core();
    let term = core.get(t).map_err(|e| core_error(t, e))?;
    let was_off = record::is_off(&term);
    record::set_off(&mut core, t, off).map_err(|e| core_error(t, e))?;
    if off {
        let removed = record::forget(&mut core, t).map_err(|e| core_error(t, e))?;
        record::remove_copy(&paths::records_dir(&s.env, &s.home), t);
        let record = if removed {
            "its record was removed"
        } else {
            "it had no record"
        };
        println!(
            "{t}: switched off ({record}). Nothing is recorded or resumed there until \
             `herdr-wake on {t}`."
        );
    } else if was_off {
        println!(
            "{t}: switched on. The next agent started there is recorded; one that is \
             already running is not, until it starts again."
        );
    } else {
        println!("{t}: was not switched off; nothing to change");
    }
    Ok(0)
}

fn resume(t: &TerminalId) -> anyhow::Result<i32> {
    let s = Session::new()?;
    let mut core = s.core();
    let term = core.get(t).map_err(|e| core_error(t, e))?;
    let Some(rec) = record::of_terminal(&term) else {
        bail!("{t} has no record, so there is nothing to resume");
    };
    let copies = paths::records_dir(&s.env, &s.home);
    if rec.state != State::Ended && !record::copy_matches(&copies, t, term.created_ms, &rec) {
        // `resume` is the user saying "this record is mine": from here on
        // the wrapper accepts it. Everything it would run is still checked
        // at wake (program, folder, flags, session id).
        record::write_copy(&copies, t, term.created_ms, &rec)
            .with_context(|| format!("writing the local copy for {t}"))?;
        println!(
            "{t}: this record did not match herdr-wake's local copy (it was untrusted); \
             it is accepted now. Check the command below before relying on it."
        );
    }
    let boot = s.platform.boot_id();
    let live = report::agent_live(&rec, s.platform.as_ref(), &boot);
    let exhausted = rec.state == State::Running && rec.attempts_on(&boot) >= s.cfg.attempts;
    // Record text goes to the terminal without control characters.
    let what = trust::printable(&format!("{t}: {} session {}", rec.agent, rec.session_id));
    let failed = matches!(rec.state, State::Failed { .. });
    if !live && (failed || exhausted) {
        if !record::rearm(&mut core, t, &rec.session_id).map_err(|e| core_error(t, e))? {
            bail!("{t}: the record changed while it was being reset; run resume again");
        }
        println!(
            "{what} is set to resume again. It starts by itself at the next recovery \
             (after a reboot or a herdr-core restart), not now."
        );
    } else if rec.state == State::Ended {
        println!("{what} was quit, so it is not resumed, and `resume` leaves that as it is.");
    } else if live {
        println!("{what} is running now; nothing to reset.");
    } else {
        println!("{what} is already set to resume at the next recovery.");
    }
    if !live {
        let agent = agents::get(rec.agent);
        match wake::manual_for(&rec, agent.as_ref(), &mut s.ctx(&mut core)) {
            Some(manual) => println!(
                "To run it now, type this in {t}:\n  {}",
                trust::printable(&manual)
            ),
            None => println!(
                "Its session id is not usable, so there is no command to run; \
                 `herdr-wake forget {t}` removes the record."
            ),
        }
    }
    Ok(0)
}

/// The fields of Claude's `<config dir>/sessions/<pid>.json` adopt reads.
#[derive(serde::Deserialize)]
struct ClaudeSessionFile {
    #[serde(rename = "sessionId")]
    session_id: String,
    cwd: PathBuf,
    #[serde(default)]
    kind: Option<String>,
}

/// What adopt can learn about the agent process `p` without a hook: for
/// Claude, its own session file. `Err` says why the agent cannot be adopted.
fn adoptable(
    kind: AgentKind,
    p: &platform::ProcInfo,
    env: &BTreeMap<String, String>,
    home: &Path,
) -> Result<Start, String> {
    if kind != AgentKind::Claude {
        return Err(format!(
            "needs a restart of the agent to be recorded ({kind} cannot be identified from outside)"
        ));
    }
    let dir = agents::claude::config_dir(p.env.as_ref().unwrap_or(env), home);
    let file = dir.join("sessions").join(format!("{}.json", p.pid));
    let text = std::fs::read_to_string(&file).map_err(|_| {
        format!(
            "needs a restart of the agent to be recorded (no {})",
            file.display()
        )
    })?;
    let f: ClaudeSessionFile =
        serde_json::from_str(&text).map_err(|_| format!("cannot read {}", file.display()))?;
    if f.kind.as_deref().is_some_and(|k| k != "interactive") {
        return Err("not an interactive session".to_string());
    }
    if !agents::claude::valid_session_id(&f.session_id) || !f.cwd.is_absolute() {
        return Err(format!("{} holds no usable session", file.display()));
    }
    Ok(Start {
        session_id: f.session_id,
        session_file: None,
        cwd: f.cwd,
        terminal: None,
        agent_pid: Some(p.pid),
        argv: None,
    })
}

fn adopt() -> anyhow::Result<i32> {
    let s = Session::new()?;
    let mut core = s.core();
    let mut terms = core.list(false).map_err(|e| match e {
        CoreError::NotRunning => anyhow!(CORE_DOWN),
        other => anyhow!("{other}"),
    })?;
    terms.sort_by(|a, b| report::terminal_order(&a.address, &b.address));
    let me = std::env::current_exe().ok();
    let spawn = |t: &TerminalId, a: ProcRef, sh: Option<ProcRef>| {
        if let (Some(bin), Some(sh)) = (me.as_deref(), sh) {
            let _ = hook::spawn_watch_detached(bin, t, a, sh);
        }
    };
    let (mut recorded, mut seen) = (0, 0);
    for term in terms {
        let Some(p) = term.foreground_pid.and_then(|pid| s.platform.process(pid)) else {
            continue;
        };
        let Some(kind) = agents::identify(&p) else {
            continue;
        };
        seen += 1;
        let t = &term.address;
        let copies = paths::records_dir(&s.env, &s.home);
        let already = report::record_of(&term).is_some_and(|r| {
            r.state == State::Running
                && r.agent_proc.is_some_and(|a| a.pid == p.pid)
                && record::copy_matches(&copies, t, term.created_ms, &r)
        });
        if already {
            println!("{t}: {kind} is already recorded");
            continue;
        }
        let mut start = match adoptable(kind, &p, &s.env, &s.home) {
            Ok(start) => start,
            Err(why) => {
                println!("{t}: {kind} {why}");
                continue;
            }
        };
        start.terminal = Some(t.clone());
        let id = trust::printable(&start.session_id);
        let agent = agents::get(kind);
        match hook::started(agent.as_ref(), start, &mut s.ctx(&mut core), &spawn) {
            HookOutcome::Recorded => {
                recorded += 1;
                println!("{t}: recorded {kind} session {id}");
            }
            other => println!("{t}: {kind} not recorded ({other})"),
        }
    }
    if seen == 0 {
        println!("no running agents found in herdr-core's terminals");
    } else {
        println!("{recorded} of {seen} running agents newly recorded");
    }
    Ok(0)
}
