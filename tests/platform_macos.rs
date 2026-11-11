//! Tests for the macOS adapter (`src/platform/macos.rs`). They read real
//! process facts and run real children, but never call `launchctl` and never
//! write under `~/Library`: the boot service gets a temporary home and a fake
//! `launchctl`.
//!
//! `run_foreground` tests run in a fresh copy of this test binary (see
//! `in_child`), so a developer's terminal is never handed around between
//! parallel tests: once with stdin from `/dev/null` (no tty), once on a
//! private pseudo-terminal.
#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use herdr_wake::platform::macos::{
    LaunchctlOutput, LaunchctlRunner, LaunchdEnv, LaunchdService, MacOs, LABEL, RUN_PREFIX,
};
use herdr_wake::platform::{BootOpts, BootService, BootState, Platform};
use herdr_wake::record::ProcRef;

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Polls `f` for up to 5 s.
fn eventually(mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

// --- Process facts ------------------------------------------------------------

#[test]
fn process_of_self_returns_this_test_binarys_argv() {
    let p = MacOs::new();
    let me = p.process(std::process::id()).expect("own process");
    let want: Vec<String> = std::env::args().collect();
    assert_eq!(me.argv, want);
    assert_eq!(me.pid, std::process::id());
    assert_eq!(me.ppid, std::os::unix::process::parent_id());
    assert_eq!(me.exe, std::env::current_exe().unwrap());
    assert!(me.start > 0);
}

#[test]
fn child_argv_with_spaces_and_empty_args_round_trips() {
    let argv = strings(&[
        "/bin/sh",
        "-c",
        "sleep 30; exit 0",
        "zero",
        "a b  c",
        "",
        "tab\there",
        "",
    ]);
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .env("HW_TEST_VAR", "with = sign")
        .env("PATH", "/usr/bin:/bin")
        .spawn()
        .unwrap();
    let p = MacOs::new();
    let pid = child.id();
    let got = eventually(|| p.process(pid).is_some_and(|i| i.argv == argv));
    let info = p.process(pid);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(got, "argv never matched: {info:?}");
    let info = info.unwrap();
    assert_eq!(info.ppid, std::process::id());
    // The environment is best effort: when the kernel shows it, it is exact.
    if let Some(env) = &info.env {
        assert_eq!(
            env.get("HW_TEST_VAR").map(String::as_str),
            Some("with = sign")
        );
        assert!(!env.contains_key("HOME"));
    }
}

#[test]
fn alive_is_true_while_running_and_false_after_exit() {
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let p = MacOs::new();
    let info = p.process(child.id()).expect("child info");
    let r = info.proc_ref();
    assert!(p.alive(&r));
    assert!(!p.alive(&ProcRef {
        pid: r.pid,
        start: r.start + 1
    }));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!p.alive(&r));
    assert!(p.process(r.pid).is_none());
}

#[test]
fn boot_id_is_non_empty_and_stable() {
    let p = MacOs::new();
    let a = p.boot_id();
    assert!(!a.is_empty());
    assert_eq!(a, p.boot_id());
    assert_eq!(a, MacOs::new().boot_id());
}

#[test]
fn login_shell_argv_adds_dash_l() {
    let p = MacOs::new();
    assert_eq!(
        p.login_shell_argv(Path::new("/bin/zsh")),
        strings(&["/bin/zsh", "-l"])
    );
}

#[test]
fn not_shutting_down_in_a_normal_session() {
    assert!(!MacOs::new().shutting_down());
}

#[test]
fn core_pid_finds_a_herdr_core_serve_process() {
    let p = MacOs::new();
    let before = p.core_pid();
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("herdr-core");
    std::fs::copy("/bin/zsh", &fake).unwrap();
    std::fs::write(dir.path().join("serve"), "sleep 30\n").unwrap();
    let mut child = Command::new(&fake)
        .arg("serve")
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let seen = eventually(|| {
        p.process(pid)
            .is_some_and(|i| i.argv.get(1).map(String::as_str) == Some("serve"))
    });
    let got = p.core_pid();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(seen);
    match before {
        None => assert_eq!(got, Some(pid)),
        Some(_) => assert!(got.is_some()),
    }
}

// --- Foreground runs ------------------------------------------------------------

const CHILD_DONE: &str = "HW_TEST_CHILD_DONE";

/// In the parent: runs test `name` alone in a new copy of this binary, with
/// stdin from `/dev/null` or (with `pty`) on a new pseudo-terminal as its
/// controlling terminal, and checks it passed. In that child: returns true.
fn in_child(name: &str, pty: bool) -> bool {
    if std::env::var_os(CHILD_DONE).is_some() {
        return true;
    }
    let dir = tempfile::tempdir().unwrap();
    let done = dir.path().join("done");
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([name, "--exact", "--test-threads=1", "--nocapture"])
        .env(CHILD_DONE, &done);
    if pty {
        let (m, s) = open_pty();
        let slave_fd = std::os::fd::AsRawFd::as_raw_fd(&s);
        cmd.stdin(s.try_clone().unwrap())
            .stdout(s.try_clone().unwrap())
            .stderr(s.try_clone().unwrap());
        // SAFETY: setsid and ioctl are async-signal-safe.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        drop(s);
        // Drain the terminal so the child never blocks writing to it.
        let mut reader = m;
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = out.clone();
        let drain = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        let output = child.wait_with_output().unwrap();
        // Reading the master ends with EIO once the child closed the slave.
        let _ = drain.join();
        let text = String::from_utf8_lossy(&out.lock().unwrap()).into_owned();
        assert!(output.status.success(), "child failed:\n{text}");
    } else {
        let output = cmd.stdin(Stdio::null()).output().unwrap();
        assert!(
            output.status.success(),
            "child failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(done.exists(), "child test {name} did not run");
    false
}

fn child_done() {
    std::fs::write(std::env::var_os(CHILD_DONE).unwrap(), "ok").unwrap();
}

fn open_pty() -> (std::fs::File, std::fs::File) {
    use std::os::fd::FromRawFd;
    let (mut m, mut s) = (0, 0);
    // SAFETY: openpty writes two fds; the name, termios and size are unused.
    let r = unsafe {
        libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(r, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: both fds were just opened and are owned here.
    unsafe { (std::fs::File::from_raw_fd(m), std::fs::File::from_raw_fd(s)) }
}

fn base_env() -> BTreeMap<String, String> {
    BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())])
}

fn sh(script: &str) -> Vec<String> {
    strings(&["/bin/sh", "-c", script])
}

#[test]
fn run_foreground_reports_exit_code_and_time() {
    if !in_child("run_foreground_reports_exit_code_and_time", false) {
        return;
    }
    let p = MacOs::new();
    let info = p
        .run_foreground(&sh("sleep 0.2; exit 3"), &base_env(), Path::new("/"))
        .unwrap();
    assert_eq!(info.code, Some(3));
    assert_eq!(info.signal, None);
    assert!(info.pid > 0);
    assert!(info.ran_ms >= 200, "ran_ms {}", info.ran_ms);
    child_done();
}

#[test]
fn run_foreground_reports_signal_death() {
    if !in_child("run_foreground_reports_signal_death", false) {
        return;
    }
    let p = MacOs::new();
    let info = p
        .run_foreground(&sh("kill -TERM $$; sleep 5"), &base_env(), Path::new("/"))
        .unwrap();
    assert_eq!(info.code, None);
    assert_eq!(info.signal, Some(libc::SIGTERM));
    child_done();
}

#[test]
fn run_foreground_without_tty_uses_exact_env_cwd_and_own_group() {
    if !in_child(
        "run_foreground_without_tty_uses_exact_env_cwd_and_own_group",
        false,
    ) {
        return;
    }
    // SAFETY: isatty only reads.
    assert_eq!(
        unsafe { libc::isatty(0) },
        0,
        "stdin must not be a tty here"
    );
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let mut env = base_env();
    env.insert("HW_FOO".to_string(), "bar baz".to_string());
    std::env::set_var("HW_LEAK", "1");
    let script = format!(
        r#"[ "$HW_FOO" = "bar baz" ] || exit 11
[ -z "$HW_LEAK" ] || exit 12
[ -z "$HOME" ] || exit 13
[ "$(pwd -P)" = "{}" ] || exit 14
[ "$(ps -o pgid= -p $$ | tr -d ' ')" = "$$" ] || exit 15
exit 0"#,
        cwd.display()
    );
    let info = MacOs::new()
        .run_foreground(&sh(&script), &env, &cwd)
        .unwrap();
    assert_eq!(info.code, Some(0));
    child_done();
}

#[test]
fn run_foreground_continues_a_stopped_child() {
    if !in_child("run_foreground_continues_a_stopped_child", false) {
        return;
    }
    let info = MacOs::new()
        .run_foreground(&sh("kill -STOP $$; exit 5"), &base_env(), Path::new("/"))
        .unwrap();
    assert_eq!(info.code, Some(5));
    child_done();
}

#[test]
fn run_foreground_missing_program_is_an_error() {
    let r = MacOs::new().run_foreground(
        &strings(&["/nonexistent/hw-agent"]),
        &base_env(),
        Path::new("/"),
    );
    assert!(r.is_err());
    assert!(MacOs::new()
        .run_foreground(&[], &base_env(), Path::new("/"))
        .is_err());
}

fn sigttou_is_default() -> bool {
    // SAFETY: reads the current disposition only.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGTTOU, std::ptr::null(), &mut old);
        old.sa_sigaction == libc::SIG_DFL
    }
}

#[test]
fn run_foreground_on_a_tty_hands_it_over_and_takes_it_back() {
    if !in_child(
        "run_foreground_on_a_tty_hands_it_over_and_takes_it_back",
        true,
    ) {
        return;
    }
    // SAFETY: plain reads of terminal and signal state.
    unsafe {
        assert_eq!(libc::isatty(0), 1);
        assert_eq!(libc::tcgetpgrp(0), libc::getpgrp());
        libc::signal(libc::SIGTTOU, libc::SIG_DFL);
    }
    // Exit 7 only when the child's own group is the terminal's foreground.
    let script = r#"[ "$(ps -o tpgid= -p $$ | tr -d ' ')" = "$$" ] && exit 7; exit 9"#;
    let info = MacOs::new()
        .run_foreground(&sh(script), &base_env(), Path::new("/"))
        .unwrap();
    assert_eq!(info.code, Some(7), "child was not the foreground job");
    // SAFETY: reads terminal state.
    assert_eq!(unsafe { libc::tcgetpgrp(0) }, unsafe { libc::getpgrp() });
    assert!(sigttou_is_default(), "SIGTTOU disposition not restored");
    child_done();
}

// --- Boot service -------------------------------------------------------------

type Calls = Arc<Mutex<Vec<Vec<String>>>>;

/// A fake `launchctl`: `print <target>` succeeds only for targets in
/// `loaded`, `print-disabled` prints `disabled`, `bootstrap` fails when
/// `bootstrap_fails`, everything else succeeds. Every call is recorded.
fn fake_launchctl(
    loaded: &[&str],
    disabled: &str,
    bootstrap_fails: bool,
) -> (LaunchctlRunner, Calls) {
    let calls: Calls = Arc::default();
    let rec = calls.clone();
    let loaded: Vec<String> = strings(loaded);
    let disabled = disabled.to_string();
    let run: LaunchctlRunner = Box::new(move |args: &[String]| {
        rec.lock().unwrap().push(args.to_vec());
        Ok(match args.first().map(String::as_str) {
            Some("print") if loaded.contains(&args[1]) => LaunchctlOutput::ok("state = waiting"),
            Some("print") => LaunchctlOutput::failed("Could not find service"),
            Some("print-disabled") => LaunchctlOutput::ok(disabled.clone()),
            Some("bootstrap") if bootstrap_fails => LaunchctlOutput::failed(
                "Bootstrap failed: 125: Domain does not support specified action",
            ),
            _ => LaunchctlOutput::ok(""),
        })
    });
    (run, calls)
}

struct Home {
    dir: tempfile::TempDir,
    env: LaunchdEnv,
}

fn home() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let h = dir.path().join("Users/alice");
    std::fs::create_dir_all(&h).unwrap();
    let mut env = LaunchdEnv::for_user("alice", 501, &h);
    env.daemons_dir = dir.path().join("Library/LaunchDaemons");
    env.path = "/opt/homebrew/bin:/usr/bin:/bin".to_string();
    Home { dir, env }
}

fn opts(before_login: bool) -> BootOpts {
    BootOpts {
        before_login,
        bin: PathBuf::from("/Users/alice/.local/bin/herdr-wake"),
    }
}

fn mutating(calls: &Calls) -> Vec<Vec<String>> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| !matches!(c[0].as_str(), "print" | "print-disabled"))
        .cloned()
        .collect()
}

/// Every file under `root`, with its contents.
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.insert(p.clone(), Vec::new());
                stack.push(p);
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

fn golden(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/launchd")
            .join(name),
    )
    .unwrap()
}

#[test]
fn agent_plist_matches_golden_file() {
    let (run, _) = fake_launchctl(&[], "", false);
    let svc = LaunchdService::new(
        LaunchdEnv::for_user("alice", 501, Path::new("/Users/alice")),
        run,
    );
    assert_eq!(
        svc.render_agent(Path::new("/Users/alice/.local/bin/herdr-wake")),
        golden("agent-alice.plist")
    );
}

#[test]
fn daemon_plist_matches_golden_file() {
    let (run, _) = fake_launchctl(&[], "", false);
    let mut env = LaunchdEnv::for_user("alice", 501, Path::new("/Users/alice"));
    env.path = "/opt/homebrew/bin:/usr/bin:/bin".to_string();
    let svc = LaunchdService::new(env, run);
    assert_eq!(
        svc.render_daemon(Path::new("/Users/alice/.local/bin/herdr-wake")),
        golden("daemon-alice.plist")
    );
}

#[test]
fn install_agent_writes_plist_and_bootstraps_gui_domain() {
    let h = home();
    let (run, calls) = fake_launchctl(&[], "", false);
    let svc = LaunchdService::new(h.env.clone(), run);
    svc.install(&opts(false), false).unwrap();

    let plist = h
        .env
        .home
        .join("Library/LaunchAgents/dev.heretik.herdr-wake.plist");
    assert_eq!(plist, h.env.agent_plist());
    assert_eq!(
        std::fs::read_to_string(&plist).unwrap(),
        svc.render_agent(&opts(false).bin)
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&plist).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert!(h.env.home.join("Library/Logs/herdr-wake").is_dir());
    assert_eq!(
        mutating(&calls),
        vec![
            strings(&["enable", "gui/501/dev.heretik.herdr-wake"]),
            vec![
                "bootstrap".to_string(),
                "gui/501".to_string(),
                plist.to_string_lossy().into_owned()
            ],
        ]
    );
    assert_eq!(svc.state(), BootState::AtLogin);
}

#[test]
fn reinstalling_a_loaded_agent_boots_it_out_first() {
    let h = home();
    let (run, calls) = fake_launchctl(&["gui/501/dev.heretik.herdr-wake"], "", false);
    let svc = LaunchdService::new(h.env.clone(), run);
    svc.install(&opts(false), false).unwrap();
    let verbs: Vec<String> = mutating(&calls).iter().map(|c| c[0].clone()).collect();
    assert_eq!(verbs, strings(&["enable", "bootout", "bootstrap"]));
}

#[test]
fn agent_bootstrap_failure_without_gui_session_is_reported_not_fatal() {
    let h = home();
    let (run, _) = fake_launchctl(&[], "", true);
    let svc = LaunchdService::new(h.env.clone(), run);
    let actions = svc.install(&opts(false), false).unwrap();
    assert!(h.env.agent_plist().exists());
    assert!(actions
        .iter()
        .any(|a| a.what.contains("loads at the next login")));
}

#[test]
fn before_login_without_root_stages_plist_and_returns_sudo_commands() {
    let h = home();
    let (run, calls) = fake_launchctl(&[], "", false);
    let svc = LaunchdService::new(h.env.clone(), run);
    let actions = svc.install(&opts(true), false).unwrap();

    let dest = h.env.daemons_dir.join("dev.heretik.herdr-wake.alice.plist");
    assert_eq!(dest, h.env.daemon_plist());
    assert!(
        !dest.exists(),
        "never writes the system folder without root"
    );
    let staged = h.env.staged_daemon_plist();
    assert_eq!(
        std::fs::read_to_string(&staged).unwrap(),
        svc.render_daemon(&opts(true).bin)
    );
    assert!(mutating(&calls).is_empty(), "runs no launchctl change");

    let cmds: Vec<&str> = actions
        .iter()
        .filter_map(|a| a.what.strip_prefix(RUN_PREFIX))
        .collect();
    assert_eq!(
        cmds,
        vec![
            format!("sudo plutil -lint {}", staged.display()),
            format!(
                "sudo install -m 0644 -o root -g wheel {} {}",
                staged.display(),
                dest.display()
            ),
            "sudo launchctl enable system/dev.heretik.herdr-wake.alice".to_string(),
            format!("sudo launchctl bootstrap system {}", dest.display()),
        ]
    );
}

#[test]
fn before_login_over_an_agent_also_lists_the_agent_removal() {
    let h = home();
    let (run, _) = fake_launchctl(&[], "", false);
    let svc = LaunchdService::new(h.env.clone(), run);
    svc.install(&opts(false), false).unwrap();
    let (run, _) = fake_launchctl(&["gui/501/dev.heretik.herdr-wake"], "", false);
    let svc = LaunchdService::new(h.env.clone(), run);
    let actions = svc.install(&opts(true), false).unwrap();
    let cmds: Vec<&str> = actions
        .iter()
        .filter_map(|a| a.what.strip_prefix(RUN_PREFIX))
        .collect();
    assert!(cmds.contains(&"launchctl bootout gui/501/dev.heretik.herdr-wake"));
    let mv = format!(
        "mv {} {}",
        h.env.agent_plist().display(),
        h.env
            .trash_dir
            .join("dev.heretik.herdr-wake.plist")
            .display()
    );
    assert!(cmds.contains(&mv.as_str()), "{cmds:?}");
    assert!(h.env.agent_plist().exists(), "left for the user to remove");
}

#[test]
fn dry_run_writes_nothing_and_changes_no_job() {
    let h = home();
    let before = tree(h.dir.path());
    for before_login in [false, true] {
        let (run, calls) = fake_launchctl(&[], "", false);
        let svc = LaunchdService::new(h.env.clone(), run);
        let actions = svc.install(&opts(before_login), true).unwrap();
        assert!(!actions.is_empty());
        assert!(mutating(&calls).is_empty());
    }
    assert_eq!(tree(h.dir.path()), before);

    // A dry-run removal of an installed agent leaves it in place.
    let (run, _) = fake_launchctl(&[], "", false);
    LaunchdService::new(h.env.clone(), run)
        .install(&opts(false), false)
        .unwrap();
    let installed = tree(h.dir.path());
    let (run, calls) = fake_launchctl(&["gui/501/dev.heretik.herdr-wake"], "", false);
    let actions = LaunchdService::new(h.env.clone(), run)
        .remove(true)
        .unwrap();
    assert!(!actions.is_empty());
    assert!(mutating(&calls).is_empty());
    assert_eq!(tree(h.dir.path()), installed);
}

#[test]
fn remove_boots_out_and_moves_plist_to_the_trash() {
    let h = home();
    let (run, _) = fake_launchctl(&[], "", false);
    LaunchdService::new(h.env.clone(), run)
        .install(&opts(false), false)
        .unwrap();
    std::fs::create_dir_all(&h.env.trash_dir).unwrap();
    std::fs::write(h.env.trash_dir.join("dev.heretik.herdr-wake.plist"), "old").unwrap();
    let text = std::fs::read_to_string(h.env.agent_plist()).unwrap();

    let (run, calls) = fake_launchctl(&["gui/501/dev.heretik.herdr-wake"], "", false);
    let svc = LaunchdService::new(h.env.clone(), run);
    svc.remove(false).unwrap();
    assert_eq!(
        mutating(&calls),
        vec![strings(&["bootout", "gui/501/dev.heretik.herdr-wake"])]
    );
    assert!(!h.env.agent_plist().exists());
    assert_eq!(
        std::fs::read_to_string(h.env.trash_dir.join("dev.heretik.herdr-wake 2.plist")).unwrap(),
        text
    );
    let (run, _) = fake_launchctl(&[], "", false);
    assert_eq!(
        LaunchdService::new(h.env.clone(), run).state(),
        BootState::NotInstalled
    );
}

#[test]
fn remove_of_a_daemon_without_root_returns_sudo_commands() {
    let h = home();
    std::fs::create_dir_all(&h.env.daemons_dir).unwrap();
    std::fs::write(h.env.daemon_plist(), "x").unwrap();
    let (run, calls) = fake_launchctl(&["system/dev.heretik.herdr-wake.alice"], "", false);
    let actions = LaunchdService::new(h.env.clone(), run)
        .remove(false)
        .unwrap();
    assert!(mutating(&calls).is_empty());
    assert!(h.env.daemon_plist().exists());
    let cmds: Vec<&str> = actions
        .iter()
        .filter_map(|a| a.what.strip_prefix(RUN_PREFIX))
        .collect();
    assert_eq!(
        cmds[0],
        "sudo launchctl bootout system/dev.heretik.herdr-wake.alice"
    );
    assert!(cmds[1].starts_with("sudo mv "));
}

#[test]
fn state_reads_plists_and_launchctl() {
    let h = home();
    let (run, _) = fake_launchctl(&[], "", false);
    assert_eq!(
        LaunchdService::new(h.env.clone(), run).state(),
        BootState::NotInstalled
    );

    let (run, _) = fake_launchctl(&["gui/501/dev.heretik.herdr-wake"], "", false);
    assert_eq!(
        LaunchdService::new(h.env.clone(), run).state(),
        BootState::AtLogin,
        "loaded counts as installed"
    );

    std::fs::create_dir_all(&h.env.agents_dir).unwrap();
    std::fs::write(h.env.agent_plist(), "x").unwrap();
    let off = format!("disabled services = {{\n\t\"{LABEL}\" => disabled\n}}\n");
    let (run, calls) = fake_launchctl(&[], &off, false);
    assert_eq!(
        LaunchdService::new(h.env.clone(), run).state(),
        BootState::Disabled
    );
    assert!(calls
        .lock()
        .unwrap()
        .contains(&strings(&["print-disabled", "gui/501"])));

    std::fs::create_dir_all(&h.env.daemons_dir).unwrap();
    std::fs::write(h.env.daemon_plist(), "x").unwrap();
    let (run, _) = fake_launchctl(&[], "", false);
    assert_eq!(
        LaunchdService::new(h.env.clone(), run).state(),
        BootState::BeforeLogin
    );
}

// ---- M-4: no write through a planted symlink ----

#[test]
fn the_agent_plist_is_not_written_through_a_planted_symlink() {
    let h = home();
    let (run, _) = fake_launchctl(&[], "", false);
    let svc = LaunchdService::new(h.env.clone(), run);
    let victim = h.dir.path().join("victim");
    std::fs::write(&victim, "precious").unwrap();
    let plist = h.env.agent_plist();
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    // Both the plist and the old predictable temporary name are links.
    std::os::unix::fs::symlink(&victim, &plist).unwrap();
    let tmp = plist.with_file_name(format!(
        ".dev.heretik.herdr-wake.plist.tmp-{}",
        std::process::id()
    ));
    std::os::unix::fs::symlink(&victim, &tmp).unwrap();

    svc.install(&opts(false), false).unwrap();

    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
    assert!(std::fs::symlink_metadata(&plist)
        .unwrap()
        .file_type()
        .is_file());
}

// ---- audit item 7 and L-7: the LaunchDaemon ----

#[test]
fn the_daemon_plist_names_no_log_file_for_launchd_to_open() {
    let h = home();
    let (run, _) = fake_launchctl(&[], "", false);
    let svc = LaunchdService::new(h.env.clone(), run);
    let text = svc.render_daemon(&opts(true).bin);
    assert!(!text.contains("StandardOutPath"), "{text}");
    assert!(!text.contains("StandardErrorPath"), "{text}");
    assert!(!text.contains("boot.log"), "{text}");
    // The LaunchAgent runs as the user in the user's session and keeps its log.
    assert!(svc
        .render_agent(&opts(false).bin)
        .contains("StandardOutPath"));
}

#[test]
fn the_daemons_user_comes_from_the_user_database_not_the_environment() {
    use herdr_wake::platform::macos::PasswdEntry;
    let mut env = BTreeMap::new();
    env.insert("USER".to_string(), "root".to_string());
    env.insert("LOGNAME".to_string(), "root".to_string());
    env.insert("HOME".to_string(), "/Users/alice".to_string());
    let pw = PasswdEntry {
        name: "alice".into(),
        home: "/Users/alice".into(),
        shell: "/bin/zsh".into(),
    };
    let e = LaunchdEnv::resolve(&env, 501, Some(pw));
    assert_eq!(e.user, "alice");
    assert_eq!(e.daemon_label(), "dev.heretik.herdr-wake.alice");
    // No entry in the user database: the numeric uid, never $USER.
    let e = LaunchdEnv::resolve(&env, 501, None);
    assert_eq!(e.user, "501");
}

#[test]
fn a_user_name_that_is_not_plain_cannot_install_a_daemon() {
    use herdr_wake::platform::macos::valid_user_name;
    for good in ["alice", "a.b-c_d", "u501", "A1"] {
        assert!(valid_user_name(good), "{good}");
    }
    for bad in [
        "",
        "a/b",
        "../x",
        "a b",
        "root;x",
        "caf\u{e9}",
        "a\nb",
        "-x",
    ] {
        assert!(!valid_user_name(bad), "{bad:?}");
    }

    let mut h = home();
    h.env.user = "../../tmp/x".into();
    let (run, calls) = fake_launchctl(&[], "", false);
    let svc = LaunchdService::new(h.env.clone(), run);
    let err = svc.install(&opts(true), false).unwrap_err().to_string();
    assert!(err.contains("user name"), "{err}");
    assert!(mutating(&calls).is_empty());
    assert!(tree(h.dir.path()).keys().all(|p| !p.ends_with(".plist")));
}

#[test]
fn the_daemon_plist_is_staged_privately() {
    use std::os::unix::fs::PermissionsExt;
    let h = home();
    // The state folder exists already, readable by everyone.
    std::fs::create_dir_all(&h.env.stage_dir).unwrap();
    std::fs::set_permissions(&h.env.stage_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (run, _) = fake_launchctl(&[], "", false);
    let svc = LaunchdService::new(h.env.clone(), run);
    svc.install(&opts(true), false).unwrap();
    let staged = h.env.staged_daemon_plist();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(staged.parent().unwrap()), 0o700);
    assert_eq!(mode(&staged), 0o600);
}

// ---- the monotonic clock ----

#[test]
fn mono_ms_only_moves_forward_and_tracks_real_time() {
    let p = MacOs::new();
    let a = p.mono_ms();
    assert!(a > 0);
    std::thread::sleep(Duration::from_millis(30));
    let b = p.mono_ms();
    assert!(b >= a + 25, "{a} then {b}");
    assert!(b < a + 5000, "{a} then {b}");
    // It is not the wall clock: milliseconds since boot, not since 1970.
    assert!(b < p.now_ms() / 2, "mono {b} wall {}", p.now_ms());
}
