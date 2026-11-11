//! Linux adapter tests (Task 5). Real `/proc` for process facts and
//! foreground runs; a temporary `/proc`, `/run` and a scripted command runner
//! for everything else, so nothing here calls systemctl, loginctl or busctl.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herdr_wake::platform::linux::{
    render_unit, BootPaths, Linux, RunOutput, Runner, SystemdBoot, UNIT_NAME,
};
use herdr_wake::platform::{BootOpts, BootService, BootState, Platform};
use herdr_wake::record::ProcRef;

type Answer = Box<dyn Fn(&[&str]) -> io::Result<RunOutput> + Send + Sync>;

/// Records every command and answers from a closure.
struct Script {
    calls: Mutex<Vec<(Vec<String>, Duration)>>,
    answer: Answer,
}

impl Script {
    fn new(
        answer: impl Fn(&[&str]) -> io::Result<RunOutput> + Send + Sync + 'static,
    ) -> Arc<Script> {
        Arc::new(Script {
            calls: Mutex::new(Vec::new()),
            answer: Box::new(answer),
        })
    }

    /// Every command succeeds and prints nothing.
    fn ok() -> Arc<Script> {
        Script::new(|_| Ok(RunOutput::ok("")))
    }

    fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(argv, _)| argv.join(" "))
            .collect()
    }

    fn timeouts(&self) -> Vec<Duration> {
        self.calls.lock().unwrap().iter().map(|(_, t)| *t).collect()
    }
}

impl Runner for Script {
    fn run(&self, argv: &[&str], timeout: Duration) -> io::Result<RunOutput> {
        self.calls
            .lock()
            .unwrap()
            .push((argv.iter().map(|s| s.to_string()).collect(), timeout));
        (self.answer)(argv)
    }
}

fn not_found() -> io::Result<RunOutput> {
    Err(io::Error::new(io::ErrorKind::NotFound, "no such command"))
}

/// Boot paths inside `dir`, for user `u`.
fn boot_paths(dir: &Path) -> BootPaths {
    BootPaths {
        unit: dir.join("config/systemd/user").join(UNIT_NAME),
        boot_env: dir.join("config/herdr-wake/boot.env"),
        linger: dir.join("linger/u"),
        marker: dir.join("state/herdr-wake/linger-enabled"),
        backups: dir.join("state/herdr-wake/backups"),
        user: "u".to_string(),
        wsl_distro: None,
    }
}

/// An adapter over the real `/proc` with a scripted runner and boot paths
/// in `dir`.
fn real(dir: &Path, runner: Arc<Script>) -> Linux {
    let boot = SystemdBoot::new(boot_paths(dir), runner.clone());
    Linux::with_parts(PathBuf::from("/proc"), dir.join("run"), None, runner, boot)
}

fn opts() -> BootOpts {
    BootOpts {
        before_login: false,
        bin: PathBuf::from("/home/u/.local/bin/herdr-wake"),
    }
}

// --- Process facts on the real /proc ---------------------------------------

#[test]
fn process_reports_this_test_binary() {
    let dir = tempfile::tempdir().unwrap();
    let linux = real(dir.path(), Script::ok());
    let me = linux.process(std::process::id()).unwrap();
    let argv: Vec<String> = std::env::args().collect();
    assert_eq!(me.argv, argv);
    assert_eq!(me.ppid, std::os::unix::process::parent_id());
    assert_eq!(me.exe, std::env::current_exe().unwrap());
    let env = me.env.clone().unwrap();
    assert_eq!(
        env.get("PATH").map(String::as_str),
        std::env::var("PATH").ok().as_deref()
    );
    assert!(linux.alive(&me.proc_ref()));
}

#[test]
fn spawned_child_argv_round_trips_and_dies() {
    let dir = tempfile::tempdir().unwrap();
    let linux = real(dir.path(), Script::ok());
    // "; :" keeps the shell from exec'ing sleep, so the argv stays the shell's.
    let argv = [
        "sh",
        "-c",
        "sleep 30; :",
        "arg with  spaces",
        "",
        "(paren) x",
    ];
    let mut child = Command::new(argv[0]).args(&argv[1..]).spawn().unwrap();
    let pid = child.id();
    // Until the forked child execs, its cmdline is still this test's.
    let mut info = linux.process(pid).unwrap();
    for _ in 0..200 {
        if info.argv.first().map(String::as_str) == Some("sh") {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        info = linux.process(pid).unwrap();
    }
    assert_eq!(info.argv, argv);
    assert_eq!(info.ppid, std::process::id());
    let r = info.proc_ref();
    assert!(linux.alive(&r));
    assert!(!linux.alive(&ProcRef {
        pid,
        start: r.start + 1
    }));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!linux.alive(&r));
    assert!(linux.process(pid).is_none());
}

#[test]
fn boot_id_is_the_kernel_uuid() {
    let dir = tempfile::tempdir().unwrap();
    let linux = real(dir.path(), Script::ok());
    let id = linux.boot_id();
    assert_eq!(id.len(), 36, "{id:?}");
    assert_eq!(id, linux.boot_id());
}

// --- A temporary /proc ------------------------------------------------------

/// Writes `/proc/<pid>` files: stat with `comm` and `start`, cmdline,
/// status with `uid`, environ.
fn fake_proc(root: &Path, pid: u32, comm: &str, start: u64, argv: &[&str], uid: u32, env: &str) {
    let dir = root.join(pid.to_string());
    fs::create_dir_all(&dir).unwrap();
    // Fields 3..=22: state, ppid, pgrp, session, tty, tpgid, flags, minflt,
    // cminflt, majflt, cmajflt, utime, stime, cutime, cstime, priority,
    // nice, threads, itrealvalue, starttime.
    let stat = format!(
        "{pid} ({comm}) S 1 {pid} {pid} 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 {start} 1000 10 \
         18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0\n"
    );
    fs::write(dir.join("stat"), stat).unwrap();
    let mut cmdline = argv.join("\0");
    cmdline.push('\0');
    fs::write(dir.join("cmdline"), cmdline).unwrap();
    fs::write(
        dir.join("status"),
        format!("Name:\t{comm}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
    )
    .unwrap();
    fs::write(dir.join("environ"), env).unwrap();
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn fake_linux(dir: &Path, home: Option<&str>, runner: Arc<Script>) -> Linux {
    let boot = SystemdBoot::new(boot_paths(dir), runner.clone());
    Linux::with_parts(
        dir.join("proc"),
        dir.join("run"),
        home.map(PathBuf::from),
        runner,
        boot,
    )
}

#[test]
fn process_parses_a_hostile_comm_and_keeps_empty_args() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proc");
    fake_proc(
        &root,
        77,
        "a) (b c",
        5150,
        &["prog", "", "x y"],
        uid(),
        "A=1\0B=2=3\0",
    );
    let linux = fake_linux(dir.path(), None, Script::ok());
    let p = linux.process(77).unwrap();
    assert_eq!((p.pid, p.ppid, p.start), (77, 1, 5150));
    assert_eq!(p.argv, ["prog", "", "x y"]);
    assert_eq!(p.exe, PathBuf::from("prog"));
    let env = p.env.unwrap();
    assert_eq!(env.get("B").map(String::as_str), Some("2=3"));
    assert!(linux.alive(&ProcRef {
        pid: 77,
        start: 5150
    }));
    assert!(!linux.alive(&ProcRef {
        pid: 77,
        start: 5151
    }));
    assert!(!linux.alive(&ProcRef {
        pid: 78,
        start: 5150
    }));
}

#[test]
fn zombies_are_not_alive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proc");
    fake_proc(&root, 9, "z", 10, &[], uid(), "");
    let stat = root.join("9/stat");
    let text = fs::read_to_string(&stat).unwrap().replace(") S ", ") Z ");
    fs::write(&stat, text).unwrap();
    let linux = fake_linux(dir.path(), None, Script::ok());
    assert!(!linux.alive(&ProcRef { pid: 9, start: 10 }));
    assert!(linux.process(9).is_none());
}

#[test]
fn boot_id_is_read_from_the_proc_root_and_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("proc/sys/kernel/random/boot_id");
    fs::create_dir_all(f.parent().unwrap()).unwrap();
    fs::write(&f, "6b1f0c2e-1d2a-4f1b-9c3d-0a1b2c3d4e5f\n").unwrap();
    let linux = fake_linux(dir.path(), None, Script::ok());
    assert_eq!(linux.boot_id(), "6b1f0c2e-1d2a-4f1b-9c3d-0a1b2c3d4e5f");
}

#[test]
fn core_pid_is_the_oldest_own_serve_with_our_home() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proc");
    let me = uid();
    let home = "HOME=/home/u\0";
    fake_proc(
        &root,
        300,
        "herdr-core",
        900,
        &["/home/u/.local/bin/herdr-core", "serve"],
        me,
        home,
    );
    fake_proc(
        &root,
        200,
        "hc",
        500,
        &["/home/u/.local/bin/hc", "serve"],
        me,
        home,
    );
    // Older, but a holder, another user, or another HOME.
    fake_proc(
        &root,
        100,
        "hc",
        100,
        &["/home/u/.local/bin/hc", "hold", "t1"],
        me,
        home,
    );
    fake_proc(&root, 101, "hc", 101, &["/bin/hc", "serve"], me + 1, home);
    fake_proc(
        &root,
        102,
        "hc",
        102,
        &["/bin/hc", "serve"],
        me,
        "HOME=/tmp/scratch\0",
    );
    fs::create_dir_all(root.join("self")).unwrap();
    let linux = fake_linux(dir.path(), Some("/home/u"), Script::ok());
    assert_eq!(linux.core_pid(), Some(200));
}

#[test]
fn core_pid_is_none_without_a_server() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proc");
    fake_proc(&root, 5, "zsh", 1, &["-zsh"], uid(), "");
    let linux = fake_linux(dir.path(), Some("/home/u"), Script::ok());
    assert_eq!(linux.core_pid(), None);
}

// --- Shutdown detection -----------------------------------------------------

#[test]
fn logind_preparing_for_shutdown_counts() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::new(|argv| match argv[0] {
        "busctl" => Ok(RunOutput::ok("b true\n")),
        _ => Ok(RunOutput::ok("running\n")),
    });
    let linux = fake_linux(dir.path(), None, runner.clone());
    assert!(linux.shutting_down());
    assert!(runner.calls()[0].ends_with("org.freedesktop.login1.Manager PreparingForShutdown"));
}

#[test]
fn systemd_stopping_counts_when_logind_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::new(|argv| match argv[0] {
        "busctl" => not_found(),
        _ => Ok(RunOutput {
            code: Some(1),
            stdout: "stopping\n".to_string(),
            stderr: String::new(),
        }),
    });
    let linux = fake_linux(dir.path(), None, runner);
    assert!(linux.shutting_down());
}

#[test]
fn nothing_available_means_not_shutting_down_within_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::new(|_| not_found());
    let linux = fake_linux(dir.path(), None, runner.clone());
    assert!(!linux.shutting_down());
    let longest = runner.timeouts().iter().copied().max().unwrap_or_default();
    assert!(longest <= Duration::from_millis(500), "{longest:?}");
}

#[test]
fn a_running_system_is_not_shutting_down() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::new(|argv| match argv[0] {
        "busctl" => Ok(RunOutput::ok("b false\n")),
        _ => Ok(RunOutput::ok("running\n")),
    });
    assert!(!fake_linux(dir.path(), None, runner).shutting_down());
}

#[test]
fn a_shutdown_due_now_counts_and_one_far_away_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("run/systemd/shutdown/scheduled");
    fs::create_dir_all(f.parent().unwrap()).unwrap();
    let runner = Script::new(|_| not_found());
    let linux = fake_linux(dir.path(), None, runner.clone());
    let now_us = linux.now_ms() * 1000;
    fs::write(&f, format!("USEC={now_us}\nMODE=reboot\n")).unwrap();
    assert!(linux.shutting_down());
    assert!(runner.calls().is_empty(), "the file alone decides");
    fs::write(
        &f,
        format!("USEC={}\nMODE=reboot\n", now_us + 3_600_000_000),
    )
    .unwrap();
    assert!(!linux.shutting_down());
}

// --- Foreground runs ---------------------------------------------------------

/// Signal dispositions are process-wide and tests run on parallel threads:
/// one foreground run at a time.
static FOREGROUND: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    FOREGROUND.lock().unwrap_or_else(|e| e.into_inner())
}

fn sh(script: &str) -> Vec<String> {
    vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()]
}

fn path_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    env
}

#[test]
fn foreground_exit_code_and_signal() {
    let _serial = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let linux = real(dir.path(), Script::ok());
    let e = linux
        .run_foreground(&sh("exit 3"), &path_env(), Path::new("/"))
        .unwrap();
    assert_eq!((e.code, e.signal), (Some(3), None));
    let e = linux
        .run_foreground(&sh("kill -TERM $$"), &path_env(), Path::new("/"))
        .unwrap();
    assert_eq!((e.code, e.signal), (None, Some(libc::SIGTERM)));
}

#[test]
fn foreground_child_gets_its_own_group_exact_env_and_cwd() {
    let _serial = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let linux = real(dir.path(), Script::ok());
    let mut env = path_env();
    env.insert("FOO".to_string(), "bar baz".to_string());
    // Field 5 of stat is the process group; comm is "sh", so no spaces.
    let script = r#"set -- $(cat /proc/$$/stat); [ "$5" = "$$" ] || exit 10
        [ "$FOO" = "bar baz" ] || exit 11
        [ -z "${HOME+x}" ] || exit 12
        [ "$(pwd -P)" = "$WANT" ] || exit 13"#;
    let cwd = fs::canonicalize(dir.path()).unwrap();
    env.insert("WANT".to_string(), cwd.to_string_lossy().into_owned());
    let e = linux.run_foreground(&sh(script), &env, &cwd).unwrap();
    assert_eq!(e.code, Some(0), "{e:?}");
}

fn disposition(sig: libc::c_int) -> libc::sighandler_t {
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(sig, std::ptr::null(), &mut old);
        old.sa_sigaction
    }
}

#[test]
fn foreground_restores_dispositions_and_child_sees_the_originals() {
    let _serial = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let linux = real(dir.path(), Script::ok());
    let before: Vec<_> = [libc::SIGINT, libc::SIGQUIT, libc::SIGTTOU]
        .into_iter()
        .map(disposition)
        .collect();
    // SigIgn in /proc/self/status is a hex mask; bit (n-1) is signal n.
    let script = r#"mask=$(sed -n 's/^SigIgn:[[:space:]]*//p' /proc/$$/status)
        m=$((0x$mask)); for s in 2 3 22; do [ $(( (m >> (s - 1)) & 1 )) -eq 0 ] || exit $s; done"#;
    let e = linux
        .run_foreground(&sh(script), &path_env(), Path::new("/"))
        .unwrap();
    let after: Vec<_> = [libc::SIGINT, libc::SIGQUIT, libc::SIGTTOU]
        .into_iter()
        .map(disposition)
        .collect();
    assert_eq!(before, after);
    if before.iter().all(|d| *d == libc::SIG_DFL) {
        assert_eq!(e.code, Some(0), "child inherited an ignored signal: {e:?}");
    }
}

#[test]
fn foreground_missing_program_is_an_error_and_restores_dispositions() {
    let _serial = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let linux = real(dir.path(), Script::ok());
    let before = disposition(libc::SIGTTOU);
    let r = linux.run_foreground(
        &["/nonexistent/agent".to_string()],
        &path_env(),
        Path::new("/"),
    );
    assert!(r.is_err());
    assert_eq!(disposition(libc::SIGTTOU), before);
    assert!(linux
        .run_foreground(&[], &path_env(), Path::new("/"))
        .is_err());
}

// --- Boot service ------------------------------------------------------------

const GOLDEN_UNIT: &str = "# Written by herdr-wake install; herdr-wake uninstall removes it.
[Unit]
Description=Start herdr-core and resume agent sessions

[Service]
Type=oneshot
RemainAfterExit=yes
EnvironmentFile=-/home/u/.config/herdr-wake/boot.env
ExecStart=/home/u/.local/bin/herdr-wake boot
TimeoutStartSec=180

[Install]
WantedBy=default.target
";

#[test]
fn unit_text_matches_the_golden_file() {
    let text = render_unit(
        Path::new("/home/u/.local/bin/herdr-wake"),
        Path::new("/home/u/.config/herdr-wake/boot.env"),
    );
    assert_eq!(text, GOLDEN_UNIT);
    let spaced = render_unit(Path::new("/home/a b/hw"), Path::new("/home/a b/boot.env"));
    assert!(spaced.contains("ExecStart=\"/home/a b/hw\" boot\n"));
    assert!(spaced.contains("EnvironmentFile=-/home/a b/boot.env\n"));
}

#[test]
fn standard_paths_follow_xdg_and_the_user() {
    let mut env = BTreeMap::new();
    let home = Path::new("/home/u");
    let p = BootPaths::new(&env, home, "u");
    assert_eq!(
        p.unit,
        PathBuf::from("/home/u/.config/systemd/user/herdr-wake.service")
    );
    assert_eq!(
        p.boot_env,
        PathBuf::from("/home/u/.config/herdr-wake/boot.env")
    );
    assert_eq!(p.linger, PathBuf::from("/var/lib/systemd/linger/u"));
    assert_eq!(
        p.marker,
        PathBuf::from("/home/u/.local/state/herdr-wake/linger-enabled")
    );
    assert_eq!(p.wsl_distro, None);
    env.insert("XDG_CONFIG_HOME".to_string(), "/cfg".to_string());
    env.insert("WSL_DISTRO_NAME".to_string(), "Ubuntu".to_string());
    let p = BootPaths::new(&env, home, "u");
    assert_eq!(
        p.unit,
        PathBuf::from("/cfg/systemd/user/herdr-wake.service")
    );
    assert_eq!(p.wsl_distro.as_deref(), Some("Ubuntu"));
}

#[test]
fn install_writes_the_unit_enables_it_and_turns_on_linger() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::ok();
    let boot = SystemdBoot::new(boot_paths(dir.path()), runner.clone());
    let p = boot.paths().clone();
    let acts = boot.install(&opts(), false).unwrap();
    let want = render_unit(&opts().bin, &p.boot_env);
    assert_eq!(fs::read_to_string(&p.unit).unwrap(), want);
    assert_eq!(
        runner.calls(),
        [
            "systemctl --user daemon-reload",
            "systemctl --user enable herdr-wake.service",
            "loginctl --no-ask-password enable-linger u",
        ]
    );
    assert!(p.marker.exists());
    assert!(acts
        .iter()
        .any(|a| a.path.as_deref() == Some(p.unit.as_path())));
    assert!(acts
        .iter()
        .any(|a| a.path.as_deref() == Some(p.marker.as_path())));
}

#[test]
fn install_leaves_existing_linger_alone_and_writes_no_marker() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::ok();
    let boot = SystemdBoot::new(boot_paths(dir.path()), runner.clone());
    let p = boot.paths().clone();
    fs::create_dir_all(p.linger.parent().unwrap()).unwrap();
    fs::write(&p.linger, "").unwrap();
    boot.install(&opts(), false).unwrap();
    assert!(!runner.calls().iter().any(|c| c.starts_with("loginctl")));
    assert!(!p.marker.exists());
}

#[test]
fn install_reports_refused_linger_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::new(|argv| match argv[0] {
        "loginctl" => Ok(RunOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "Access denied\n".to_string(),
        }),
        _ => Ok(RunOutput::ok("")),
    });
    let boot = SystemdBoot::new(boot_paths(dir.path()), runner);
    let acts = boot.install(&opts(), false).unwrap();
    assert!(!boot.paths().marker.exists());
    assert!(acts
        .iter()
        .any(|a| a.what.contains("sudo loginctl enable-linger u")));
}

#[test]
fn install_fails_when_systemctl_fails() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::new(|argv| match argv.get(2) {
        Some(&"enable") => Ok(RunOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "Failed to connect to bus\n".to_string(),
        }),
        _ => Ok(RunOutput::ok("")),
    });
    let boot = SystemdBoot::new(boot_paths(dir.path()), runner);
    let err = boot.install(&opts(), false).unwrap_err();
    assert!(format!("{err:#}").contains("Failed to connect to bus"));
}

#[test]
fn dry_run_install_and_remove_write_nothing_and_run_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::ok();
    let boot = SystemdBoot::new(boot_paths(dir.path()), runner.clone());
    let acts = boot.install(&opts(), true).unwrap();
    assert!(acts.len() >= 4, "{acts:?}");
    assert!(runner.calls().is_empty());
    let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
    assert!(entries.is_empty(), "{entries:?}");

    // Installed for real, then a dry-run remove leaves it all in place.
    boot.install(&opts(), false).unwrap();
    let before = runner.calls().len();
    let acts = boot.remove(true).unwrap();
    assert!(!acts.is_empty());
    assert_eq!(runner.calls().len(), before);
    assert!(boot.paths().unit.exists());
    assert!(boot.paths().marker.exists());
}

#[test]
fn remove_backs_up_the_unit_and_disables_linger_only_with_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::ok();
    let boot = SystemdBoot::new(boot_paths(dir.path()), runner.clone());
    let p = boot.paths().clone();
    boot.install(&opts(), false).unwrap();
    let unit_text = fs::read_to_string(&p.unit).unwrap();
    let start = runner.calls().len();
    boot.remove(false).unwrap();
    assert_eq!(
        runner.calls()[start..],
        [
            "systemctl --user disable herdr-wake.service",
            "systemctl --user daemon-reload",
            "loginctl --no-ask-password disable-linger u",
        ]
    );
    assert!(!p.unit.exists());
    assert!(!p.marker.exists());
    let backups: Vec<_> = fs::read_dir(&p.backups).unwrap().flatten().collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read_to_string(backups[0].path()).unwrap(), unit_text);
}

#[test]
fn remove_keeps_linger_that_herdr_wake_did_not_turn_on() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::ok();
    let boot = SystemdBoot::new(boot_paths(dir.path()), runner.clone());
    let p = boot.paths().clone();
    fs::create_dir_all(p.linger.parent().unwrap()).unwrap();
    fs::write(&p.linger, "").unwrap();
    boot.install(&opts(), false).unwrap();
    boot.remove(false).unwrap();
    assert!(!runner.calls().iter().any(|c| c.contains("disable-linger")));
}

#[test]
fn remove_with_nothing_installed_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Script::ok();
    let boot = SystemdBoot::new(boot_paths(dir.path()), runner.clone());
    assert!(boot.remove(false).unwrap().is_empty());
    assert!(runner.calls().is_empty());
}

#[test]
fn state_follows_unit_enablement_and_linger() {
    let dir = tempfile::tempdir().unwrap();
    let answer = Arc::new(Mutex::new("enabled\n".to_string()));
    let said = answer.clone();
    let runner = Script::new(move |argv| match argv.get(2) {
        Some(&"is-enabled") => Ok(RunOutput::ok(&said.lock().unwrap())),
        _ => Ok(RunOutput::ok("")),
    });
    let boot = SystemdBoot::new(boot_paths(dir.path()), runner);
    let p = boot.paths().clone();
    assert_eq!(boot.state(), BootState::NotInstalled);
    fs::create_dir_all(p.linger.parent().unwrap()).unwrap();
    fs::write(&p.linger, "").unwrap(); // linger on: no marker
    boot.install(&opts(), false).unwrap();
    assert_eq!(boot.state(), BootState::BeforeLogin);
    fs::remove_file(&p.linger).unwrap();
    assert_eq!(boot.state(), BootState::AtLogin);
    *answer.lock().unwrap() = "disabled\n".to_string();
    assert_eq!(boot.state(), BootState::Disabled);
}

#[test]
fn state_falls_back_to_the_wants_link_without_systemctl() {
    let dir = tempfile::tempdir().unwrap();
    let boot = SystemdBoot::new(boot_paths(dir.path()), Script::new(|_| not_found()));
    let p = boot.paths().clone();
    fs::create_dir_all(p.unit.parent().unwrap()).unwrap();
    fs::write(&p.unit, GOLDEN_UNIT).unwrap();
    assert_eq!(boot.state(), BootState::Disabled);
    let wants = p.unit.parent().unwrap().join("default.target.wants");
    fs::create_dir_all(&wants).unwrap();
    std::os::unix::fs::symlink(&p.unit, wants.join(UNIT_NAME)).unwrap();
    assert_eq!(boot.state(), BootState::AtLogin);
}

#[test]
fn wsl_install_notes_the_limitation() {
    let dir = tempfile::tempdir().unwrap();
    let mut paths = boot_paths(dir.path());
    paths.wsl_distro = Some("Ubuntu".to_string());
    let boot = SystemdBoot::new(paths, Script::ok());
    let acts = boot.install(&opts(), true).unwrap();
    assert!(acts
        .iter()
        .any(|a| a.what.starts_with("WSL (Ubuntu)") && a.what.contains("wsl.exe -d Ubuntu")));
}

#[test]
fn the_real_adapter_exposes_its_boot_service() {
    let linux = Linux::new();
    let p = linux.systemd().paths();
    assert!(p.unit.ends_with("systemd/user/herdr-wake.service"));
    assert!(p.linger.starts_with("/var/lib/systemd/linger"));
}

// ---- the monotonic clock ----

#[test]
fn mono_ms_only_moves_forward_and_tracks_real_time() {
    let p = Linux::new();
    let a = p.mono_ms();
    assert!(a > 0);
    std::thread::sleep(Duration::from_millis(30));
    let b = p.mono_ms();
    assert!(b >= a + 25, "{a} then {b}");
    assert!(b < a + 5000, "{a} then {b}");
    // It is not the wall clock: milliseconds since boot, not since 1970.
    assert!(b < p.now_ms() / 2, "mono {b} wall {}", p.now_ms());
}
