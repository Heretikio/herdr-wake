//! `hook::run_with` against the fakes and a test-local agent.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::Fixture;
use herdr_wake::agents::{Agent, Ctx, Edit, Observation, Plan};
use herdr_wake::config::Config;
use herdr_wake::core::{Terminal, TerminalId};
use herdr_wake::hook::{run_with, HookOutcome};
use herdr_wake::platform::{Platform, ProcInfo};
use herdr_wake::record::{self, AgentKind, ProcRef, Record, State};

const SID: &str = "11111111-2222-4333-8444-555555555555";

/// Returns a fixed observation; keeps every flag except `argv[0]` and
/// `--drop-me`.
struct TestAgent {
    obs: Observation,
}

impl Agent for TestAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }
    fn observe(&self, _event: &str, _payload: &[u8], _ctx: &mut Ctx) -> Observation {
        self.obs.clone()
    }
    fn restorable_flags(&self, argv: &[String], _cfg: &Config) -> Vec<String> {
        argv.iter()
            .skip(1)
            .filter(|a| a.as_str() != "--drop-me")
            .cloned()
            .collect()
    }
    fn resume_plan(&self, _rec: &Record, _ctx: &mut Ctx) -> Plan {
        Plan::Permanent("test".into())
    }
    fn integration(&self, _bin: &Path, _ctx: &mut Ctx) -> Vec<Edit> {
        Vec::new()
    }
}

fn started(terminal: Option<&str>, agent_pid: Option<u32>) -> TestAgent {
    TestAgent {
        obs: Observation::Started {
            session_id: SID.to_string(),
            session_file: Some(PathBuf::from("/home/u/.claude/projects/p/s.jsonl")),
            cwd: PathBuf::from("/home/u/proj"),
            terminal: terminal.map(TerminalId::from),
            agent_pid,
            argv: None,
        },
    }
}

fn ended(session_id: &str, deliberate: Option<bool>) -> TestAgent {
    TestAgent {
        obs: Observation::Ended {
            session_id: session_id.to_string(),
            deliberate,
        },
    }
}

/// A fixture inside terminal t1 with a shell (pid 400) running an agent
/// (pid 500) whose environment holds a secret.
fn fixture() -> Fixture {
    let mut f = Fixture::temp();
    f.core.insert(Terminal::new("t1"));
    f.env.insert("HC_TERMINAL_ID".into(), "t1".into());
    f.env
        .insert("HC_TERMINAL_SOCKET".into(), "/tmp/hc/c.sock".into());
    f.env.insert("PATH".into(), "/hook/path".into());
    f.platform.add_process(ProcInfo::new(
        300,
        1,
        30,
        vec!["herdr-core".into(), "hold".into()],
    ));
    f.platform
        .add_process(ProcInfo::new(400, 300, 40, vec!["-zsh".into()]));
    let mut agent = ProcInfo::new(
        500,
        400,
        50,
        vec![
            "claude".into(),
            "--model".into(),
            "opus".into(),
            "--drop-me".into(),
        ],
    );
    agent.exe = PathBuf::from("/opt/claude/bin/claude");
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/agent/path".to_string());
    env.insert(
        "ANTHROPIC_API_KEY".to_string(),
        "not-a-real-key".to_string(),
    );
    env.insert("CLAUDE_CONFIG_DIR".to_string(), "/home/u/.cc".to_string());
    agent.env = Some(env);
    f.platform.add_process(agent);
    f
}

type Spawned = RefCell<Vec<(TerminalId, ProcRef, Option<ProcRef>)>>;

fn run(f: &mut Fixture, agent: &TestAgent, spawned: &Spawned) -> HookOutcome {
    let spawn = |t: &TerminalId, a: ProcRef, s: Option<ProcRef>| {
        spawned.borrow_mut().push((t.clone(), a, s));
    };
    let mut ctx = f.ctx();
    run_with(agent, "start", b"{}", &mut ctx, &spawn)
}

fn stored(f: &mut Fixture, t: &str) -> Option<Record> {
    record::load(&mut f.core, &TerminalId::from(t)).unwrap()
}

#[test]
fn started_records_the_session_and_spawns_a_watcher() {
    let mut f = fixture();
    let spawned = Spawned::default();

    let out = run(&mut f, &started(None, Some(500)), &spawned);

    assert_eq!(out, HookOutcome::Recorded);
    let rec = stored(&mut f, "t1").expect("record written");
    assert_eq!(rec.v, 1);
    assert_eq!(rec.agent, AgentKind::Claude);
    assert_eq!(rec.session_id, SID);
    assert_eq!(
        rec.session_file,
        Some(PathBuf::from("/home/u/.claude/projects/p/s.jsonl"))
    );
    assert_eq!(rec.cwd, PathBuf::from("/home/u/proj"));
    assert_eq!(rec.flags, vec!["--model".to_string(), "opus".to_string()]);
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/agent/path".to_string());
    env.insert("CLAUDE_CONFIG_DIR".to_string(), "/home/u/.cc".to_string());
    assert_eq!(rec.env, env);
    assert_eq!(rec.bin, PathBuf::from("/opt/claude/bin/claude"));
    assert_eq!(
        rec.agent_proc,
        Some(ProcRef {
            pid: 500,
            start: 50
        })
    );
    assert_eq!(
        rec.shell_proc,
        Some(ProcRef {
            pid: 400,
            start: 40
        })
    );
    assert_eq!(rec.boot_id, "boot-1");
    assert_eq!(rec.state, State::Running);
    assert_eq!(rec.attempts, 0);
    assert_eq!(rec.seq, 1_700_000_000_000);
    assert_eq!(
        *spawned.borrow(),
        vec![(
            TerminalId::from("t1"),
            ProcRef {
                pid: 500,
                start: 50
            },
            Some(ProcRef {
                pid: 400,
                start: 40
            })
        )]
    );
}

#[test]
fn unreadable_process_env_falls_back_to_the_hook_env() {
    let mut f = fixture();
    let mut p = f.platform.process(500).unwrap();
    p.env = None;
    f.platform.add_process(p);

    run(&mut f, &started(None, Some(500)), &Spawned::default());

    let rec = stored(&mut f, "t1").unwrap();
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/hook/path".to_string());
    assert_eq!(rec.env, env);
}

#[test]
fn relative_exe_falls_back_to_argv0() {
    let mut f = fixture();
    let mut p = f.platform.process(500).unwrap();
    p.exe = PathBuf::new();
    p.argv[0] = "/usr/local/bin/claude".into();
    f.platform.add_process(p);

    run(&mut f, &started(None, Some(500)), &Spawned::default());

    assert_eq!(
        stored(&mut f, "t1").unwrap().bin,
        PathBuf::from("/usr/local/bin/claude")
    );
}

#[test]
fn observation_terminal_wins_over_the_env_terminal() {
    let mut f = fixture();
    f.core.insert(Terminal::new("t9"));
    f.env.remove("HC_TERMINAL_SOCKET");

    let out = run(&mut f, &started(Some("t9"), Some(500)), &Spawned::default());

    assert_eq!(out, HookOutcome::Recorded);
    assert!(stored(&mut f, "t9").is_some());
    assert!(stored(&mut f, "t1").is_none());
}

#[test]
fn seq_grows_and_attempts_carry_over_for_the_same_session() {
    let mut f = fixture();
    let mut old = Record::new(
        AgentKind::Claude,
        SID,
        PathBuf::from("/home/u/proj"),
        PathBuf::from("/x/claude"),
        "boot-0",
    );
    old.attempts = 2;
    // Stored seq ahead of the clock: the new seq must still be larger.
    old.seq = 1_800_000_000_000;
    record::save(&mut f.core, &TerminalId::from("t1"), &old).unwrap();

    let out = run(&mut f, &started(None, Some(500)), &Spawned::default());

    assert_eq!(out, HookOutcome::Recorded);
    let rec = stored(&mut f, "t1").unwrap();
    assert_eq!(rec.attempts, 2);
    assert_eq!(rec.seq, 1_800_000_000_001);
    assert_eq!(rec.boot_id, "boot-1");
}

#[test]
fn attempts_reset_for_a_new_session() {
    let mut f = fixture();
    let mut old = Record::new(
        AgentKind::Claude,
        "older-session",
        PathBuf::from("/home/u/proj"),
        PathBuf::from("/x/claude"),
        "boot-1",
    );
    old.attempts = 2;
    old.seq = 5;
    record::save(&mut f.core, &TerminalId::from("t1"), &old).unwrap();

    run(&mut f, &started(None, Some(500)), &Spawned::default());

    let rec = stored(&mut f, "t1").unwrap();
    assert_eq!(rec.session_id, SID);
    assert_eq!(rec.attempts, 0);
}

#[test]
fn agent_run_by_cores_holder_has_no_shell_and_no_watcher() {
    let mut f = fixture();
    let mut p = f.platform.process(500).unwrap();
    p.ppid = 300; // herdr-core hold
    f.platform.add_process(p);
    let spawned = Spawned::default();

    let out = run(&mut f, &started(None, Some(500)), &spawned);

    assert_eq!(out, HookOutcome::Recorded);
    assert_eq!(stored(&mut f, "t1").unwrap().shell_proc, None);
    assert!(spawned.borrow().is_empty());
}

#[test]
fn agent_whose_parent_is_pid_1_has_no_shell() {
    let mut f = fixture();
    f.platform
        .add_process(ProcInfo::new(1, 0, 1, vec!["/sbin/launchd".into()]));
    let mut p = f.platform.process(500).unwrap();
    p.ppid = 1;
    f.platform.add_process(p);
    let spawned = Spawned::default();

    run(&mut f, &started(None, Some(500)), &spawned);

    assert_eq!(stored(&mut f, "t1").unwrap().shell_proc, None);
    assert!(spawned.borrow().is_empty());
}

#[test]
fn supervised_agent_is_recorded_without_a_watcher() {
    let mut f = fixture();
    f.env.insert("HERDR_WAKE_SUPERVISED".into(), "1".into());
    let spawned = Spawned::default();

    let out = run(&mut f, &started(None, Some(500)), &spawned);

    assert_eq!(out, HookOutcome::Recorded);
    // Changed in Task 13: the parent of a supervised agent is the wrapper,
    // not a shell to outlive, so no shell is recorded (the integration
    // suite found the wrapper's pid stored here).
    assert_eq!(stored(&mut f, "t1").unwrap().shell_proc, None);
    assert!(spawned.borrow().is_empty());
}

#[test]
fn disabled_config_records_nothing() {
    let mut f = fixture();
    f.cfg.enabled = false;
    let spawned = Spawned::default();

    let out = run(&mut f, &started(None, Some(500)), &spawned);

    assert!(matches!(out, HookOutcome::Ignored(_)), "{out:?}");
    assert!(stored(&mut f, "t1").is_none());
    assert!(spawned.borrow().is_empty());
}

#[test]
fn excluded_folder_records_nothing() {
    let mut f = fixture();
    f.cfg.exclude_dirs = vec![PathBuf::from("/home/u")];
    let spawned = Spawned::default();

    let out = run(&mut f, &started(None, Some(500)), &spawned);

    assert!(matches!(out, HookOutcome::Ignored(_)), "{out:?}");
    assert!(stored(&mut f, "t1").is_none());
    assert!(spawned.borrow().is_empty());
}

#[test]
fn sibling_of_an_excluded_folder_is_recorded() {
    let mut f = fixture();
    f.cfg.exclude_dirs = vec![PathBuf::from("/home/u/pro")];

    let out = run(&mut f, &started(None, Some(500)), &Spawned::default());

    assert_eq!(out, HookOutcome::Recorded);
}

#[test]
fn ignore_writes_nothing() {
    let mut f = fixture();
    let calls_before = f.core.calls;
    let spawned = Spawned::default();
    let agent = TestAgent {
        obs: Observation::Ignore("sub-agent"),
    };

    let out = run(&mut f, &agent, &spawned);

    assert_eq!(out, HookOutcome::Ignored("sub-agent".into()));
    assert_eq!(f.core.calls, calls_before);
    assert!(f.core.terminal(&"t1".into()).unwrap().labels.is_empty());
    assert!(spawned.borrow().is_empty());
}

#[test]
fn missing_terminal_is_ignored() {
    let mut f = fixture();
    f.env.remove("HC_TERMINAL_ID");

    let out = run(&mut f, &started(None, Some(500)), &Spawned::default());

    assert_eq!(out, HookOutcome::Ignored("no terminal".into()));
    assert!(stored(&mut f, "t1").is_none());
}

#[test]
fn env_terminal_without_a_socket_is_ignored() {
    let mut f = fixture();
    f.env.remove("HC_TERMINAL_SOCKET");

    let out = run(&mut f, &started(None, Some(500)), &Spawned::default());

    assert!(matches!(out, HookOutcome::Ignored(_)), "{out:?}");
    assert!(stored(&mut f, "t1").is_none());
}

#[test]
fn core_error_is_a_failure_not_a_panic() {
    let mut f = fixture();
    f.core
        .fail_with(Some(herdr_wake::core::CoreError::NotRunning));

    let out = run(&mut f, &started(None, Some(500)), &Spawned::default());

    assert!(matches!(out, HookOutcome::Failed(_)), "{out:?}");
}

fn running_record(f: &mut Fixture, session_id: &str) {
    let mut rec = Record::new(
        AgentKind::Claude,
        session_id,
        PathBuf::from("/home/u/proj"),
        PathBuf::from("/x/claude"),
        "boot-1",
    );
    rec.seq = 10;
    record::save(&mut f.core, &TerminalId::from("t1"), &rec).unwrap();
}

#[test]
fn deliberate_end_ends_the_matching_session() {
    let mut f = fixture();
    running_record(&mut f, SID);

    let out = run(&mut f, &ended(SID, Some(true)), &Spawned::default());

    assert_eq!(out, HookOutcome::Ended);
    assert_eq!(stored(&mut f, "t1").unwrap().state, State::Ended);
}

#[test]
fn deliberate_end_for_another_session_changes_nothing() {
    let mut f = fixture();
    running_record(&mut f, SID);

    let out = run(&mut f, &ended("older", Some(true)), &Spawned::default());

    assert!(matches!(out, HookOutcome::Ignored(_)), "{out:?}");
    assert_eq!(stored(&mut f, "t1").unwrap().state, State::Running);
}

#[test]
fn deliberate_end_is_skipped_while_shutting_down() {
    let mut f = fixture();
    running_record(&mut f, SID);
    f.platform.set_shutting_down(true);

    let out = run(&mut f, &ended(SID, Some(true)), &Spawned::default());

    assert!(matches!(out, HookOutcome::Ignored(_)), "{out:?}");
    assert_eq!(stored(&mut f, "t1").unwrap().state, State::Running);
}

#[test]
fn unknown_or_killed_end_changes_nothing() {
    for deliberate in [None, Some(false)] {
        let mut f = fixture();
        running_record(&mut f, SID);

        let out = run(&mut f, &ended(SID, deliberate), &Spawned::default());

        assert!(matches!(out, HookOutcome::Ignored(_)), "{out:?}");
        assert_eq!(stored(&mut f, "t1").unwrap().state, State::Running);
    }
}

#[test]
fn started_without_a_readable_agent_process_still_records() {
    let mut f = fixture();
    let spawned = Spawned::default();

    let out = run(&mut f, &started(None, Some(999)), &spawned);

    assert_eq!(out, HookOutcome::Recorded);
    let rec = stored(&mut f, "t1").unwrap();
    assert_eq!(rec.bin, PathBuf::from("claude"));
    assert!(rec.flags.is_empty());
    assert_eq!(rec.agent_proc, None);
    assert_eq!(rec.shell_proc, None);
    assert_eq!(rec.env.get("PATH").map(String::as_str), Some("/hook/path"));
    assert!(spawned.borrow().is_empty());
}

#[test]
fn detached_watcher_gets_its_arguments_and_its_own_session() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let script = dir.path().join("fake-herdr-wake");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n{{ echo \"$@\"; echo $$; ps -o pgid= -p $$ | tr -d ' '; pwd; }} > '{0}.tmp'\nmv '{0}.tmp' '{0}'\n",
            out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut tries = 0;
    loop {
        match herdr_wake::hook::spawn_watch_detached(
            &script,
            &TerminalId::from("t7"),
            ProcRef { pid: 11, start: 12 },
            ProcRef { pid: 13, start: 14 },
        ) {
            Ok(()) => break,
            // Another test thread forking at the same moment can hold the
            // script open for writing for an instant (ETXTBSY on Linux).
            Err(e) if tries < 20 && e.raw_os_error() == Some(libc::ETXTBSY) => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("spawn failed: {e}"),
        }
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    while !out.exists() {
        assert!(Instant::now() < deadline, "watcher never ran");
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "watch t7 11 12 13 14");
    assert_eq!(lines[1], lines[2], "watcher leads its own process group");
    assert_eq!(lines[3], "/");
}

// ------------------------------------------ Task 13: argv, bin, gate confirm

fn executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn set_agent(f: &Fixture, exe: &Path, argv: &[&str], path: &str) {
    let mut agent = ProcInfo::new(500, 400, 50, argv.iter().map(|s| s.to_string()).collect());
    agent.exe = exe.to_path_buf();
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), path.to_string());
    agent.env = Some(env);
    f.platform.add_process(agent);
}

#[test]
fn payload_argv_wins_and_an_interpreter_is_replaced_by_the_agent_on_path() {
    let dir = tempfile::tempdir().unwrap();
    let on_path = dir.path().join("bin/claude");
    executable(&on_path);
    let mut f = fixture();
    // A script agent: the process table shows node and a renamed command.
    set_agent(
        &f,
        Path::new("/usr/local/bin/node"),
        &["node", "/lib/agent/cli.js", "--drop-me"],
        &format!("/nonexistent:{}", dir.path().join("bin").display()),
    );
    let mut agent = started(None, Some(500));
    if let Observation::Started { argv, .. } = &mut agent.obs {
        *argv = Some(vec!["claude".into(), "--model".into(), "opus".into()]);
    }
    assert_eq!(
        run(&mut f, &agent, &Spawned::default()),
        HookOutcome::Recorded
    );
    let rec = stored(&mut f, "t1").unwrap();
    assert_eq!(rec.flags, vec!["--model".to_string(), "opus".to_string()]);
    assert_eq!(rec.bin, on_path);
}

#[test]
fn an_interpreter_with_no_agent_on_path_records_the_bare_name() {
    let mut f = fixture();
    set_agent(
        &f,
        Path::new("/bin/bash"),
        &["claude", "/x/impl"],
        "/nonexistent",
    );
    assert_eq!(
        run(&mut f, &started(None, Some(500)), &Spawned::default()),
        HookOutcome::Recorded
    );
    assert_eq!(stored(&mut f, "t1").unwrap().bin, PathBuf::from("claude"));
}

#[test]
fn the_stable_path_name_is_kept_when_it_leads_to_the_running_binary() {
    let dir = tempfile::tempdir().unwrap();
    let versioned = dir.path().join("share/claude/versions/2.1.0");
    executable(&versioned);
    let link = dir.path().join("bin/claude");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&versioned, &link).unwrap();
    let mut f = fixture();
    set_agent(
        &f,
        &versioned,
        &["claude"],
        &dir.path().join("bin").display().to_string(),
    );
    assert_eq!(
        run(&mut f, &started(None, Some(500)), &Spawned::default()),
        HookOutcome::Recorded
    );
    assert_eq!(stored(&mut f, "t1").unwrap().bin, link);

    // Another claude on PATH is not the running one: keep the executable.
    let other = tempfile::tempdir().unwrap();
    executable(&other.path().join("claude"));
    set_agent(
        &f,
        &versioned,
        &["claude"],
        &other.path().display().to_string(),
    );
    f.platform.advance(10);
    run(&mut f, &started(None, Some(500)), &Spawned::default());
    assert_eq!(stored(&mut f, "t1").unwrap().bin, versioned);
}

#[test]
fn a_supervised_start_confirms_the_gate_resets_attempts_and_clears_the_mark() {
    use herdr_wake::{gate, paths, wake};
    let home = tempfile::tempdir().unwrap();
    let mut f = fixture();
    f.home = home.path().to_path_buf();
    f.env
        .insert("HOME".into(), home.path().display().to_string());
    f.env.insert("HERDR_WAKE_SUPERVISED".into(), "1".into());
    f.platform.set_core_pid(Some(77));
    let key = gate::boot_key(&f.platform);
    let dir = paths::gate_dir(&f.env, &f.home);
    let now = f.platform.now_ms();

    // Two wrappers wait at the gate; the second would wait first_alone_ms.
    let first = gate::enter_ticket(&dir, AgentKind::Claude, &f.cfg, &key, now);
    let second = gate::enter_ticket(&dir, AgentKind::Claude, &f.cfg, &key, now);
    assert_eq!((first.wait_ms, second.wait_ms), (0, f.cfg.first_alone_ms));

    // The wrapper had counted one attempt and a check had marked it.
    let t1 = TerminalId::from("t1");
    let mut prev = Record::new(
        AgentKind::Claude,
        SID,
        PathBuf::from("/home/u/proj"),
        PathBuf::from("claude"),
        "boot-1",
    );
    prev.attempts = 2;
    prev.seq = 5;
    assert!(record::save(&mut f.core, &t1, &prev).unwrap());
    let mark = wake::unconfirmed_file(&f.env, &f.home, &key, &t1);
    std::fs::create_dir_all(mark.parent().unwrap()).unwrap();
    std::fs::write(&mark, "late").unwrap();

    let spawned = Spawned::default();
    assert_eq!(
        run(&mut f, &started(None, Some(500)), &spawned),
        HookOutcome::Recorded
    );
    assert!(spawned.borrow().is_empty(), "the wrapper supervises it");
    assert_eq!(stored(&mut f, "t1").unwrap().attempts, 0);
    assert!(!mark.exists());
    let left = gate::remaining(&dir, AgentKind::Claude, &f.cfg, &key, &second, now);
    assert_eq!(
        left, f.cfg.stagger_ms,
        "confirmed: only the stagger is left"
    );
}

#[test]
fn an_unbound_hook_is_retried_until_it_binds_and_no_longer_than_the_limit() {
    use herdr_wake::hook::{retry_unbound, BIND_PAUSE_MS, BIND_RETRIES};
    let unbound = || HookOutcome::Ignored("unbound".to_string());
    let slept = RefCell::new(Vec::new());
    let sleep = |ms: u64| slept.borrow_mut().push(ms);

    // Binds on the third try.
    let mut calls = 0;
    let mut once = || {
        calls += 1;
        if calls < 3 {
            unbound()
        } else {
            HookOutcome::Recorded
        }
    };
    assert_eq!(retry_unbound(&mut once, &sleep), HookOutcome::Recorded);
    assert_eq!(*slept.borrow(), vec![BIND_PAUSE_MS; 2]);

    // Never binds: gives up after the limit with the same outcome.
    slept.borrow_mut().clear();
    let mut calls = 0;
    let mut once = || {
        calls += 1;
        unbound()
    };
    assert_eq!(retry_unbound(&mut once, &sleep), unbound());
    assert_eq!(calls, BIND_RETRIES + 1);

    // Any other outcome is final at once.
    slept.borrow_mut().clear();
    let mut once = || HookOutcome::Ignored("ambiguous".to_string());
    retry_unbound(&mut once, &sleep);
    assert!(slept.borrow().is_empty());
}

// ------------------------------------------------- folder safety (M-1)

#[test]
fn a_session_in_a_folder_others_can_write_is_not_recorded() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = fixture();
    let t = tempfile::tempdir().unwrap();
    let open = t.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
    let mut agent = started(None, Some(500));
    if let Observation::Started { cwd, .. } = &mut agent.obs {
        *cwd = open.clone();
    }
    let spawned = Spawned::default();

    let out = run(&mut f, &agent, &spawned);

    assert_eq!(
        out,
        HookOutcome::Ignored(format!(
            "{} is not safe to resume in: it is writable by others",
            open.display()
        ))
    );
    assert_eq!(stored(&mut f, "t1"), None);
    assert!(spawned.borrow().is_empty());
}

// ------------------------------------------------- label trust (M-5)

fn copy_dir(f: &Fixture) -> PathBuf {
    herdr_wake::paths::records_dir(&f.env, &f.home)
}

#[test]
fn a_session_id_that_is_not_plain_is_not_recorded() {
    for bad in ["--dangerously-skip-permissions", "../../x", "a b", ""] {
        let mut f = fixture();
        let mut agent = started(None, Some(500));
        if let Observation::Started { session_id, .. } = &mut agent.obs {
            *session_id = bad.to_string();
        }
        let spawned = Spawned::default();
        assert_eq!(
            run(&mut f, &agent, &spawned),
            HookOutcome::Ignored("no usable session id".into()),
            "{bad:?}"
        );
        assert_eq!(stored(&mut f, "t1"), None);
    }
}

#[test]
fn recording_writes_a_private_local_copy_that_matches_the_label() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = fixture();
    f.core
        .terminals
        .get_mut(&TerminalId::from("t1"))
        .unwrap()
        .created_ms = 1234;
    let spawned = Spawned::default();
    assert_eq!(
        run(&mut f, &started(None, Some(500)), &spawned),
        HookOutcome::Recorded
    );
    let rec = stored(&mut f, "t1").unwrap();
    let dir = copy_dir(&f);
    let t1 = TerminalId::from("t1");
    assert!(record::copy_matches(&dir, &t1, 1234, &rec));
    let file = record::copy_file(&dir, &t1);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&dir), 0o700);
}

#[test]
fn a_deliberate_end_removes_the_local_copy() {
    let mut f = fixture();
    let spawned = Spawned::default();
    run(&mut f, &started(None, Some(500)), &spawned);
    let file = record::copy_file(&copy_dir(&f), &TerminalId::from("t1"));
    assert!(file.exists());

    // An end for another session leaves it.
    let other = ended("another-session", Some(true));
    run(&mut f, &other, &spawned);
    assert!(file.exists());

    assert_eq!(
        run(&mut f, &ended(SID, Some(true)), &spawned),
        HookOutcome::Ended
    );
    assert!(!file.exists());
}

#[test]
fn a_copy_that_cannot_be_written_is_a_failure_of_the_hook() {
    let mut f = fixture();
    // A file where the state folder should be.
    let state = herdr_wake::paths::state_dir(&f.env, &f.home);
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(&state, "in the way").unwrap();
    let spawned = Spawned::default();
    match run(&mut f, &started(None, Some(500)), &spawned) {
        HookOutcome::Failed(why) => assert!(why.contains("local copy"), "{why}"),
        other => panic!("{other:?}"),
    }
}

// ---- exclude_dirs with a tilde (I-13) ----

#[test]
fn a_session_in_a_tilde_excluded_folder_is_not_recorded() {
    let mut f = fixture();
    f.cfg.exclude_dirs = vec![PathBuf::from("~/private")];
    let inside = f.home.join("private/proj");
    let mut agent = started(None, Some(500));
    if let Observation::Started { cwd, .. } = &mut agent.obs {
        *cwd = inside.clone();
    }
    let spawned = Spawned::default();
    assert_eq!(
        run(&mut f, &agent, &spawned),
        HookOutcome::Ignored(format!("{} is excluded", inside.display()))
    );
    assert_eq!(stored(&mut f, "t1"), None);

    // A sibling whose name only starts the same is recorded.
    if let Observation::Started { cwd, .. } = &mut agent.obs {
        *cwd = f.home.join("private-notes");
    }
    assert_eq!(run(&mut f, &agent, &spawned), HookOutcome::Recorded);
}

// ---- per-terminal opt-out: herdr-wake off <terminal> ----

#[test]
fn a_terminal_that_is_switched_off_is_not_recorded() {
    let mut f = fixture();
    record::set_off(&mut f.core, &TerminalId::from("t1"), true).unwrap();
    let spawned = Spawned::default();
    match run(&mut f, &started(None, Some(500)), &spawned) {
        HookOutcome::Ignored(why) => assert!(why.contains("switched off"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(stored(&mut f, "t1"), None);
    assert!(spawned.borrow().is_empty());

    // Switched on again, the next start is recorded.
    record::set_off(&mut f.core, &TerminalId::from("t1"), false).unwrap();
    assert_eq!(
        run(&mut f, &started(None, Some(500)), &spawned),
        HookOutcome::Recorded
    );
}

#[test]
fn a_private_group_is_named_like_the_user_and_has_no_other_member() {
    use herdr_wake::trust::private_group_rule;
    assert!(private_group_rule("ann", "ann", &[]));
    assert!(private_group_rule("ann", "ann", &["ann".to_string()]));
    assert!(!private_group_rule("ann", "staff", &[]));
    assert!(!private_group_rule("ann", "ann", &["bob".to_string()]));
}
