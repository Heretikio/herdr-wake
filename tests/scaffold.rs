use herdr_wake::config::Config;

#[test]
fn config_defaults_match_the_plan() {
    let c = Config::default();
    assert!(c.enabled);
    assert!(c.restore_bypass);
    assert_eq!(c.attempts, 3);
    assert_eq!(c.stagger_ms, 1500);
    assert_eq!(c.first_alone_ms, 8000);
    assert!(c.exclude_dirs.is_empty());
    assert_eq!(c.real_shell, None);
}

#[test]
fn config_missing_file_loads_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let c = Config::load(&dir.path().join("nope/config.toml")).unwrap();
    assert_eq!(c, Config::default());
}

#[test]
fn config_save_keeps_user_comments_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr-wake/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "# my note\nattempts = 5\n").unwrap();

    let mut c = Config::load(&path).unwrap();
    assert_eq!(c.attempts, 5);
    assert!(c.enabled);
    c.real_shell = Some(vec!["/bin/zsh".into(), "-l".into()]);
    c.save(&path).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# my note\nattempts = 5\n"), "{text}");
    assert_eq!(Config::load(&path).unwrap(), c);

    c.real_shell = None;
    c.save(&path).unwrap();
    assert!(!std::fs::read_to_string(&path)
        .unwrap()
        .contains("real_shell"));
}

mod common;

use herdr_wake::record::{Record, State};

#[test]
fn record_round_trips_through_json_with_version_1() {
    let rec = common::sample_record();
    let json = serde_json::to_string(&rec).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["v"], 1);
    assert_eq!(value["agent"], "claude");
    let back: Record = serde_json::from_str(&json).unwrap();
    assert_eq!(back, rec);
}

#[test]
fn failed_state_keeps_its_reason_through_json() {
    let mut rec = common::sample_record();
    rec.state = State::Failed {
        reason: "folder missing".to_string(),
    };
    rec.agent_proc = None;
    rec.session_file = None;
    let back: Record = serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
    assert_eq!(back, rec);
}

#[test]
fn record_new_starts_running_at_version_1() {
    let rec = Record::new(
        herdr_wake::record::AgentKind::Codex,
        "abc",
        "/w".into(),
        "/bin/codex".into(),
        "b",
    );
    assert_eq!(rec.v, 1);
    assert_eq!(rec.state, State::Running);
    assert_eq!(rec.attempts, 0);
}

#[test]
fn agent_kind_uses_lowercase_names() {
    use herdr_wake::record::AgentKind;
    let names: Vec<String> = AgentKind::ALL.iter().map(|k| k.to_string()).collect();
    assert_eq!(names, ["claude", "codex", "pi", "opencode"]);
    assert_eq!(
        "opencode".parse::<AgentKind>().unwrap(),
        AgentKind::OpenCode
    );
    assert!("OpenCode".parse::<AgentKind>().is_err());
    assert_eq!(
        serde_json::to_string(&AgentKind::OpenCode).unwrap(),
        "\"opencode\""
    );
}

use std::collections::BTreeMap;

use herdr_wake::core::fake::FakeCore;
use herdr_wake::core::{Core, CoreError, Terminal, TerminalId};

#[test]
fn fake_core_reports_unknown_terminal_as_not_found() {
    let mut core = FakeCore::new();
    let err = core.get(&TerminalId::from("t9")).unwrap_err();
    assert_eq!(err, CoreError::NotFound);
    let err = core
        .label(&TerminalId::from("t9"), &BTreeMap::new())
        .unwrap_err();
    assert_eq!(err, CoreError::NotFound);
}

#[test]
fn fake_core_label_sets_and_removes_keys() {
    let mut core = FakeCore::new();
    let mut t = Terminal::new("t1");
    t.labels.insert("keepme".into(), "1".into());
    t.labels.insert("wake".into(), "old".into());
    core.insert(t);

    let mut changes = BTreeMap::new();
    changes.insert("wake".to_string(), None);
    changes.insert("new".to_string(), Some("v".to_string()));
    let after = core.label(&TerminalId::from("t1"), &changes).unwrap();

    let expected: BTreeMap<String, String> = [("keepme", "1"), ("new", "v")]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .into();
    assert_eq!(after.labels, expected);
    assert_eq!(core.get(&TerminalId::from("t1")).unwrap().labels, expected);
}

#[test]
fn fake_core_can_fail_every_call() {
    let mut core = FakeCore::new();
    core.insert(Terminal::new("t1"));
    core.fail_with(Some(CoreError::Timeout));
    assert_eq!(core.hello().unwrap_err(), CoreError::Timeout);
    assert_eq!(
        core.get(&TerminalId::from("t1")).unwrap_err(),
        CoreError::Timeout
    );
    assert_eq!(core.list(true).unwrap_err(), CoreError::Timeout);
}

#[test]
fn fake_core_list_hides_finished_terminals_unless_all() {
    let mut core = FakeCore::new();
    core.insert(Terminal::new("t1"));
    let mut done = Terminal::new("t2");
    done.state = "finished".into();
    core.insert(done);
    let live: Vec<String> = core
        .list(false)
        .unwrap()
        .into_iter()
        .map(|t| t.address.0)
        .collect();
    assert_eq!(live, ["t1"]);
    assert_eq!(core.list(true).unwrap().len(), 2);
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn paths_default_under_home() {
    use herdr_wake::paths;
    use std::path::Path;
    let home = Path::new("/home/u");
    let e = env(&[]);
    assert_eq!(
        paths::config_dir(&e, home),
        Path::new("/home/u/.config/herdr-wake")
    );
    assert_eq!(
        paths::state_dir(&e, home),
        Path::new("/home/u/.local/state/herdr-wake")
    );
    assert_eq!(
        paths::config_file(&e, home),
        Path::new("/home/u/.config/herdr-wake/config.toml")
    );
    assert_eq!(
        paths::boot_env_file(&e, home),
        Path::new("/home/u/.config/herdr-wake/boot.env")
    );
    assert_eq!(
        paths::backups_dir(&e, home),
        Path::new("/home/u/.local/state/herdr-wake/backups")
    );
}

#[test]
fn paths_honor_absolute_xdg_dirs_only() {
    use herdr_wake::paths;
    use std::path::Path;
    let home = Path::new("/home/u");
    let e = env(&[("XDG_CONFIG_HOME", "/cfg"), ("XDG_STATE_HOME", "/st")]);
    assert_eq!(paths::config_dir(&e, home), Path::new("/cfg/herdr-wake"));
    assert_eq!(paths::state_dir(&e, home), Path::new("/st/herdr-wake"));
    let e = env(&[("XDG_CONFIG_HOME", ""), ("XDG_STATE_HOME", "relative")]);
    assert_eq!(
        paths::config_dir(&e, home),
        Path::new("/home/u/.config/herdr-wake")
    );
    assert_eq!(
        paths::state_dir(&e, home),
        Path::new("/home/u/.local/state/herdr-wake")
    );
}

use herdr_wake::platform::fake::FakePlatform;
use herdr_wake::platform::{BootOpts, BootState, Platform, ProcInfo};
use herdr_wake::record::ProcRef;

#[test]
fn fake_platform_answers_from_its_process_table() {
    let p = FakePlatform::new();
    p.add_process(ProcInfo::new(
        10,
        1,
        555,
        vec!["claude".into(), "--resume".into()],
    ));
    assert_eq!(p.process(10).unwrap().argv, ["claude", "--resume"]);
    assert_eq!(p.process(11), None);
    assert!(p.alive(&ProcRef {
        pid: 10,
        start: 555
    }));
    assert!(
        !p.alive(&ProcRef {
            pid: 10,
            start: 556
        }),
        "reused pid must not match"
    );
    p.remove_process(10);
    assert!(!p.alive(&ProcRef {
        pid: 10,
        start: 555
    }));
}

#[test]
fn fake_platform_boot_id_and_clock_are_settable() {
    let p = FakePlatform::new();
    p.set_boot_id("boot-2");
    assert_eq!(p.boot_id(), "boot-2");
    p.set_now(1_000);
    p.advance(500);
    assert_eq!(p.now_ms(), 1_500);
}

#[test]
fn fake_platform_records_exec_instead_of_running_it() {
    let p = FakePlatform::new();
    let argv = p.login_shell_argv(std::path::Path::new("/bin/zsh"));
    assert_eq!(argv, ["/bin/zsh", "-l"]);
    let _err = p.exec(
        &argv,
        &env(&[("HOME", "/home/u")]),
        std::path::Path::new("/w"),
    );
    let calls = p.execs();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].argv, ["/bin/zsh", "-l"]);
    assert_eq!(calls[0].env["HOME"], "/home/u");
    assert_eq!(calls[0].cwd, std::path::Path::new("/w"));
}

#[test]
fn fake_boot_service_dry_run_changes_nothing() {
    let p = FakePlatform::new();
    let opts = BootOpts {
        before_login: false,
        bin: "/usr/local/bin/herdr-wake".into(),
    };
    let svc = p.boot_service();
    assert_eq!(svc.state(), BootState::NotInstalled);
    assert!(!svc.install(&opts, true).unwrap().is_empty());
    assert_eq!(svc.state(), BootState::NotInstalled);
    svc.install(&opts, false).unwrap();
    assert_eq!(svc.state(), BootState::AtLogin);
    svc.remove(false).unwrap();
    assert_eq!(svc.state(), BootState::NotInstalled);
}

#[test]
fn current_platform_starts_a_login_shell_on_macos_only() {
    let p = herdr_wake::platform::current();
    let argv = p.login_shell_argv(std::path::Path::new("/bin/zsh"));
    if cfg!(target_os = "macos") {
        assert_eq!(argv, ["/bin/zsh", "-l"]);
    } else {
        assert_eq!(argv, ["/bin/zsh"]);
    }
    assert!(p.now_ms() > 1_600_000_000_000);
}

use herdr_wake::agents::{self, Observation, Plan};
use herdr_wake::record::AgentKind;

#[test]
fn all_agents_are_registered_in_order() {
    let kinds: Vec<AgentKind> = agents::all().iter().map(|a| a.kind()).collect();
    assert_eq!(kinds, AgentKind::ALL);
    for k in AgentKind::ALL {
        assert_eq!(agents::get(k).kind(), k);
    }
}

#[test]
fn agent_calls_run_against_a_ctx_over_the_fakes() {
    let mut fx = common::Fixture::new();
    let rec = common::sample_record();
    for a in agents::all() {
        let mut ctx = fx.ctx();
        // Stub adapters answer without panicking; real ones replace these.
        let _: Observation = a.observe("start", b"{}", &mut ctx);
        let _: Plan = a.resume_plan(&rec, &mut ctx);
        let _ = a.integration(std::path::Path::new("/usr/local/bin/herdr-wake"), &mut ctx);
        let _ = a.restorable_flags(&["x".to_string()], ctx.cfg);
    }
}

use herdr_wake::platform::ExitInfo;

#[test]
fn fake_platform_replays_scripted_foreground_runs_in_order() {
    let p = FakePlatform::new();
    p.push_run_result(Ok(ExitInfo::exited(77, 0, 12_000)));
    p.push_run_result(Err(std::io::Error::other("spawn failed")));
    let argv = vec![
        "claude".to_string(),
        "--resume".to_string(),
        "x".to_string(),
    ];
    let e = env(&[("HERDR_WAKE_SUPERVISED", "1")]);
    let cwd = std::path::Path::new("/w");

    let first = p.run_foreground(&argv, &e, cwd).unwrap();
    assert_eq!(
        first,
        ExitInfo {
            pid: 77,
            code: Some(0),
            signal: None,
            ran_ms: 12_000
        }
    );
    assert_eq!(
        p.run_foreground(&argv, &e, cwd).unwrap_err().to_string(),
        "spawn failed"
    );
    assert!(
        p.run_foreground(&argv, &e, cwd).is_err(),
        "an empty queue is an error"
    );

    let runs = p.runs();
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0].argv, argv);
    assert_eq!(runs[0].env["HERDR_WAKE_SUPERVISED"], "1");
    assert_eq!(runs[0].cwd, cwd);
    assert!(p.execs().is_empty(), "runs are not execs");
}

#[test]
fn fake_platform_shutdown_and_core_pid_are_settable() {
    let p = FakePlatform::new();
    assert!(!p.shutting_down());
    assert_eq!(p.core_pid(), None);
    p.set_shutting_down(true);
    p.set_core_pid(Some(900));
    assert!(p.shutting_down());
    assert_eq!(p.core_pid(), Some(900));
    assert_eq!(
        ExitInfo::signaled(5, 15, 30),
        ExitInfo {
            pid: 5,
            code: None,
            signal: Some(15),
            ran_ms: 30
        }
    );
}

// ---- auto_continue is gone until it is implemented ----

#[test]
fn a_config_that_still_has_auto_continue_loads_and_the_key_is_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr-wake/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "auto_continue = true\nattempts = 2\n").unwrap();
    let c = Config::load(&path).unwrap();
    assert_eq!(c.attempts, 2);

    // A fresh config file does not mention it.
    let fresh = dir.path().join("fresh/config.toml");
    Config::default().save(&fresh).unwrap();
    let text = std::fs::read_to_string(&fresh).unwrap();
    assert!(!text.contains("auto_continue"), "{text}");
}

#[test]
fn nothing_the_user_reads_mentions_auto_continue_as_a_setting() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ["README.md", "docs/LIMITS.md", "docs/INSTALL_PROMPT.md"] {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        for line in text.lines().filter(|l| l.contains("auto_continue")) {
            assert!(
                line.contains("removed") || line.contains("not a setting"),
                "{file} still presents auto_continue: {line}"
            );
        }
    }
    let help = std::process::Command::new(env!("CARGO_BIN_EXE_herdr-wake"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&help.stdout).contains("auto_continue"));
}

// ---- exclude_dirs: one rule everywhere ----

#[test]
fn is_excluded_expands_a_leading_tilde_and_compares_whole_components() {
    use herdr_wake::config::is_excluded;
    use std::path::{Path, PathBuf};
    let home = Path::new("/home/u");
    let dirs: Vec<PathBuf> = ["~/private", "/srv/secret/", "relative/dir", "~"]
        .iter()
        .map(PathBuf::from)
        .collect();
    let ex = |cwd: &str, dirs: &[PathBuf]| is_excluded(Path::new(cwd), dirs, home);
    assert!(ex("/home/u/private", &dirs[..1]));
    assert!(ex("/home/u/private/deep/er", &dirs[..1]));
    assert!(!ex("/home/u/private-notes", &dirs[..1]), "a sibling");
    assert!(!ex("/home/u", &dirs[..1]));
    assert!(ex("/srv/secret", &dirs[1..2]), "a trailing slash is fine");
    assert!(ex("/srv/secret/x", &dirs[1..2]));
    assert!(!ex("/srv/secrets", &dirs[1..2]));
    assert!(
        !ex("/x/relative/dir", &dirs[2..3]),
        "relative entries never match"
    );
    assert!(!ex("relative/dir", &dirs[2..3]));
    assert!(ex("/home/u/anything", &dirs[3..]), "a bare ~ is the home");
    assert!(!ex("/home/other", &dirs[3..]));
    // `~name` is not expanded: it is a relative entry.
    assert!(!ex("/home/u/x", &[PathBuf::from("~u/x")]));
    assert!(!ex("/home/u", &[]));
}
