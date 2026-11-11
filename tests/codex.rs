//! Codex adapter: hook payloads, terminal binding, flag policy, resume plans
//! and the hooks.json entry, through the `Agent` interface and the fakes.

mod common;

use std::path::{Path, PathBuf};

use common::Fixture;
use herdr_wake::agents::codex::Codex;
use herdr_wake::agents::{Agent, EditKind, Observation, Plan};
use herdr_wake::config::Config;
use herdr_wake::core::Terminal;
use herdr_wake::platform::ProcInfo;
use herdr_wake::record::{AgentKind, Record};

const ID: &str = "019a6f2e-7c41-7d30-9f0e-3b2a1c4d5e6f";
const OTHER_ID: &str = "019a6f30-1111-7222-8333-444455556666";

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// Writes a rollout for `id` under `<codex_home>/sessions/2026/11/10`.
fn put_rollout(codex_home: &Path, id: &str, body: &str, compressed: bool) -> PathBuf {
    let dir = codex_home.join("sessions/2026/11/10");
    std::fs::create_dir_all(&dir).unwrap();
    let ext = if compressed { "jsonl.zst" } else { "jsonl" };
    let path = dir.join(format!("rollout-2026-11-10T09-15-02-{id}.{ext}"));
    std::fs::write(&path, body).unwrap();
    path
}

/// A live terminal `addr` in `cwd` whose foreground process is `pid` with `argv`.
fn add_terminal(f: &mut Fixture, addr: &str, cwd: &str, pid: u32, argv: &[&str]) {
    let mut t = Terminal::new(addr);
    t.cwd = Some(PathBuf::from(cwd));
    t.pid = Some(pid - 1);
    t.foreground_pid = Some(pid);
    f.core.insert(t);
    f.platform
        .add_process(ProcInfo::new(pid, pid - 1, 1_000 + pid as u64, s(argv)));
}

fn started(terminal: &str, pid: u32) -> (Option<String>, Option<u32>) {
    (Some(terminal.to_string()), Some(pid))
}

fn binding(o: &Observation) -> (Option<String>, Option<u32>) {
    match o {
        Observation::Started {
            terminal,
            agent_pid,
            ..
        } => (terminal.as_ref().map(|t| t.0.clone()), *agent_pid),
        other => panic!("expected Started, got {other:?}"),
    }
}

// ---------------------------------------------------------------- observe

#[test]
fn kind_is_codex() {
    assert_eq!(Codex.kind(), AgentKind::Codex);
}

#[test]
fn binds_the_unique_terminal_whose_codex_argv_names_the_session() {
    let mut f = Fixture::new();
    add_terminal(&mut f, "t1", "/home/u/proj", 101, &["codex", "resume", ID]);
    add_terminal(&mut f, "t2", "/home/u/proj", 201, &["codex"]);
    add_terminal(&mut f, "t3", "/home/u/proj", 301, &["zsh", "-l"]);
    let o = Codex.observe(
        "start",
        fixture("session-start-resume.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(binding(&o), started("t1", 101));
    match o {
        Observation::Started {
            session_id, cwd, ..
        } => {
            assert_eq!(session_id, ID);
            assert_eq!(cwd, PathBuf::from("/home/u/proj"));
        }
        _ => unreachable!(),
    }
}

#[test]
fn ignores_the_hook_terminal_env() {
    let mut f = Fixture::new();
    f.env.insert("HC_TERMINAL_ID".into(), "t9".into());
    add_terminal(
        &mut f,
        "t2",
        "/home/u/proj",
        201,
        &["/opt/bin/codex", "-m", "o3"],
    );
    let o = Codex.observe(
        "start",
        fixture("session-start-startup.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(binding(&o), started("t2", 201));
}

#[test]
fn binds_the_unique_codex_terminal_in_the_payload_cwd() {
    let mut f = Fixture::new();
    add_terminal(&mut f, "t1", "/home/u/other", 101, &["codex"]);
    add_terminal(&mut f, "t2", "/home/u/proj", 201, &["codex", "--search"]);
    add_terminal(&mut f, "t3", "/home/u/proj", 301, &["vim"]);
    let o = Codex.observe(
        "start",
        fixture("session-start-startup.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(binding(&o), started("t2", 201));
}

#[test]
fn matches_codex_by_exe_basename_and_node_wrapper() {
    let mut f = Fixture::new();
    let mut t = Terminal::new("t1");
    t.cwd = Some(PathBuf::from("/home/u/proj"));
    t.foreground_pid = Some(101);
    f.core.insert(t);
    let mut p = ProcInfo::new(101, 100, 5, s(&["codex-tui"]));
    p.exe = PathBuf::from("/usr/local/lib/codex/codex");
    f.platform.add_process(p);
    let o = Codex.observe(
        "start",
        fixture("session-start-startup.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(binding(&o), started("t1", 101));

    let mut g = Fixture::new();
    add_terminal(
        &mut g,
        "t4",
        "/home/u/proj",
        401,
        &["node", "/opt/homebrew/bin/codex", "resume", ID],
    );
    let o = Codex.observe(
        "start",
        fixture("session-start-resume.json").as_bytes(),
        &mut g.ctx(),
    );
    assert_eq!(binding(&o), started("t4", 401));
}

#[test]
fn argv_match_beats_cwd_match() {
    let mut f = Fixture::new();
    add_terminal(
        &mut f,
        "t1",
        "/home/u/elsewhere",
        101,
        &["codex", "resume", ID],
    );
    add_terminal(&mut f, "t2", "/home/u/proj", 201, &["codex"]);
    let o = Codex.observe(
        "start",
        fixture("session-start-resume.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(binding(&o), started("t1", 101));
}

#[test]
fn two_codex_terminals_in_the_same_cwd_are_ambiguous() {
    let mut f = Fixture::new();
    add_terminal(&mut f, "t1", "/home/u/proj", 101, &["codex"]);
    add_terminal(
        &mut f,
        "t2",
        "/home/u/proj",
        201,
        &["codex", "resume", OTHER_ID],
    );
    let o = Codex.observe(
        "start",
        fixture("session-start-startup.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(o, Observation::Ignore("ambiguous"));
}

#[test]
fn two_terminals_naming_the_session_are_ambiguous() {
    let mut f = Fixture::new();
    add_terminal(&mut f, "t1", "/home/u/a", 101, &["codex", "resume", ID]);
    add_terminal(&mut f, "t2", "/home/u/b", 201, &["codex", "resume", ID]);
    let o = Codex.observe(
        "start",
        fixture("session-start-resume.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(o, Observation::Ignore("ambiguous"));
}

#[test]
fn no_codex_terminal_is_unbound() {
    let mut f = Fixture::new();
    add_terminal(&mut f, "t1", "/home/u/proj", 101, &["zsh", "-l"]);
    add_terminal(&mut f, "t2", "/home/u/other", 201, &["codex"]);
    let mut t = Terminal::new("t3");
    t.cwd = Some(PathBuf::from("/home/u/proj"));
    f.core.insert(t);
    let o = Codex.observe(
        "start",
        fixture("session-start-startup.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(o, Observation::Ignore("unbound"));
}

#[test]
fn nested_codex_is_ignored() {
    let mut f = Fixture::new();
    add_terminal(&mut f, "t1", "/home/u/proj", 101, &["codex"]);
    f.env.insert("CODEX_THREAD_ID".into(), OTHER_ID.into());
    let o = Codex.observe(
        "start",
        fixture("session-start-startup.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(o, Observation::Ignore("nested"));
}

#[test]
fn thread_id_equal_to_the_session_is_not_nested() {
    let mut f = Fixture::new();
    add_terminal(&mut f, "t1", "/home/u/proj", 101, &["codex"]);
    f.env.insert("CODEX_THREAD_ID".into(), ID.into());
    let o = Codex.observe(
        "start",
        fixture("session-start-startup.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(binding(&o), started("t1", 101));
}

#[test]
fn non_interactive_and_sub_agent_rollouts_are_ignored() {
    for (name, reason) in [
        ("rollout-exec.jsonl", "non-interactive"),
        ("rollout-subagent.jsonl", "sub-agent"),
        ("rollout-guardian.jsonl", "sub-agent"),
    ] {
        let (mut f, _dir) = Fixture::with_temp_home();
        put_rollout(&f.home.join(".codex"), ID, &fixture(name), false);
        add_terminal(&mut f, "t1", "/home/u/proj", 101, &["codex"]);
        let o = Codex.observe(
            "start",
            fixture("session-start-startup.json").as_bytes(),
            &mut f.ctx(),
        );
        assert_eq!(o, Observation::Ignore(reason), "{name}");
    }
}

#[test]
fn a_cli_rollout_is_accepted_and_becomes_the_session_file() {
    let (mut f, _dir) = Fixture::with_temp_home();
    let codex_home = f.home.join("ch");
    f.env
        .insert("CODEX_HOME".into(), codex_home.display().to_string());
    let path = put_rollout(&codex_home, ID, &fixture("rollout-cli.jsonl"), false);
    add_terminal(&mut f, "t1", "/home/u/proj", 101, &["codex"]);
    let o = Codex.observe(
        "start",
        fixture("session-start-startup.json").as_bytes(),
        &mut f.ctx(),
    );
    match o {
        Observation::Started { session_file, .. } => assert_eq!(session_file, Some(path)),
        other => panic!("expected Started, got {other:?}"),
    }
}

#[test]
fn a_compressed_rollout_skips_the_content_check() {
    let (mut f, _dir) = Fixture::with_temp_home();
    put_rollout(&f.home.join(".codex"), ID, "not json", true);
    add_terminal(&mut f, "t1", "/home/u/proj", 101, &["codex"]);
    let o = Codex.observe(
        "start",
        fixture("session-start-startup.json").as_bytes(),
        &mut f.ctx(),
    );
    assert_eq!(binding(&o), started("t1", 101));
}

#[test]
fn other_events_and_bad_payloads_are_ignored() {
    let mut f = Fixture::new();
    add_terminal(&mut f, "t1", "/home/u/proj", 101, &["codex"]);
    let payload = fixture("session-start-startup.json");
    assert!(matches!(
        Codex.observe("end", payload.as_bytes(), &mut f.ctx()),
        Observation::Ignore(_)
    ));
    assert!(matches!(
        Codex.observe("start", b"{not json", &mut f.ctx()),
        Observation::Ignore(_)
    ));
    assert!(matches!(
        Codex.observe("start", br#"{"cwd":"/home/u/proj"}"#, &mut f.ctx()),
        Observation::Ignore(_)
    ));
}

#[test]
fn clear_gives_the_new_session_id() {
    let mut f = Fixture::new();
    add_terminal(&mut f, "t1", "/home/u/proj", 101, &["codex", "resume", ID]);
    let o = Codex.observe(
        "start",
        fixture("session-start-clear.json").as_bytes(),
        &mut f.ctx(),
    );
    match o {
        Observation::Started {
            session_id,
            terminal,
            ..
        } => {
            assert_eq!(session_id, OTHER_ID);
            assert_eq!(terminal.unwrap().0, "t1");
        }
        other => panic!("expected Started, got {other:?}"),
    }
}

#[test]
fn core_down_is_ignored() {
    let mut f = Fixture::new();
    f.core
        .fail_with(Some(herdr_wake::core::CoreError::NotRunning));
    let o = Codex.observe(
        "start",
        fixture("session-start-startup.json").as_bytes(),
        &mut f.ctx(),
    );
    assert!(matches!(o, Observation::Ignore(_)));
}

// ---------------------------------------------------------------- flags

fn flags(argv: &[&str]) -> Vec<String> {
    Codex.restorable_flags(&s(argv), &Config::default())
}

#[test]
fn flag_table() {
    let cases: &[(&[&str], &[&str])] = &[
        // 1. plain start
        (&["codex"], &[]),
        // 2. model, short and long, both value forms
        (&["codex", "-m", "o3"], &["-m", "o3"]),
        (&["codex", "--model=o3"], &["--model=o3"]),
        // 3. profile, sandbox, approval
        (
            &[
                "codex",
                "-p",
                "work",
                "-s",
                "workspace-write",
                "-a",
                "on-request",
            ],
            &["-p", "work", "-s", "workspace-write", "-a", "on-request"],
        ),
        // 4. repeated -c k=v
        (
            &[
                "codex",
                "-c",
                "model_reasoning_effort=high",
                "-c",
                "features.x=true",
            ],
            &["-c", "model_reasoning_effort=high", "-c", "features.x=true"],
        ),
        // 5. enable/disable features
        (
            &["codex", "--enable", "web_search", "--disable", "memories"],
            &["--enable", "web_search", "--disable", "memories"],
        ),
        // 6. booleans kept
        (
            &[
                "codex",
                "--search",
                "--oss",
                "--no-alt-screen",
                "--no-daemon",
                "--strict-config",
            ],
            &[
                "--search",
                "--oss",
                "--no-alt-screen",
                "--no-daemon",
                "--strict-config",
            ],
        ),
        // 7. add-dir and local provider
        (
            &["codex", "--add-dir", "/data", "--local-provider", "ollama"],
            &["--add-dir", "/data", "--local-provider", "ollama"],
        ),
        // 8. bypass flags kept by default
        (
            &[
                "codex",
                "--yolo",
                "--dangerously-bypass-approvals-and-sandbox",
            ],
            &["--yolo", "--dangerously-bypass-approvals-and-sandbox"],
        ),
        (&["codex", "--approve-for-me"], &["--approve-for-me"]),
        // 9. resume subcommand and session id dropped, flags on both sides kept
        (
            &["codex", "-m", "o3", "resume", "--search", ID],
            &["-m", "o3", "--search"],
        ),
        // 10. prompt, images, -C, --remote, hook trust bypass dropped
        (
            &[
                "codex",
                "-i",
                "a.png",
                "b.png",
                "-C",
                "/tmp/x",
                "--remote",
                "ws://h:1",
                "--dangerously-bypass-hook-trust",
                "-m",
                "o3",
                "fix the bug",
            ],
            &["-m", "o3"],
        ),
        // 11. resume picker flags and worktree dropped
        (
            &[
                "codex",
                "resume",
                "--last",
                "--all",
                "--include-non-interactive",
                "--worktree",
            ],
            &[],
        ),
        // 12. fork and exec subcommands dropped with their positionals
        (&["codex", "fork", ID, "--search"], &["--search"]),
        (&["codex", "exec", "--json", "do it"], &[]),
        // 13. attached short values and --cd=
        (&["codex", "-mo3", "--cd=/x", "-ck=v"], &["-mo3", "-ck=v"]),
        // 14. unknown flags and everything after -- dropped
        (
            &["codex", "--frobnicate", "-m", "o3", "--", "-m", "x"],
            &["-m", "o3"],
        ),
        // 15. node wrapper argv
        (
            &["node", "/opt/homebrew/bin/codex", "--search", "resume", ID],
            &["--search"],
        ),
    ];
    for (argv, want) in cases {
        assert_eq!(flags(argv), s(want), "argv {argv:?}");
    }
}

#[test]
fn bypass_flags_dropped_when_restore_bypass_is_off() {
    let cfg = Config {
        restore_bypass: false,
        ..Config::default()
    };
    let got = Codex.restorable_flags(
        &s(&[
            "codex",
            "--yolo",
            "--dangerously-bypass-approvals-and-sandbox",
            "--approve-for-me",
            "--not-so-yolo",
            "-m",
            "o3",
        ]),
        &cfg,
    );
    assert_eq!(got, s(&["-m", "o3"]));
}

// ---------------------------------------------------------------- resume

fn codex_record(cwd: &Path) -> Record {
    let mut r = Record::new(
        AgentKind::Codex,
        ID,
        cwd.to_path_buf(),
        PathBuf::from("/usr/local/bin/codex"),
        "boot-1",
    );
    r.flags = s(&["-m", "o3", "--yolo"]);
    r.env.insert("PATH".into(), "/usr/bin:/bin".into());
    r
}

#[test]
fn resume_plan_folder_missing() {
    let (mut f, dir) = Fixture::with_temp_home();
    put_rollout(
        &f.home.join(".codex"),
        ID,
        &fixture("rollout-cli.jsonl"),
        false,
    );
    let rec = codex_record(&dir.path().join("gone"));
    assert_eq!(
        Codex.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("folder missing".into())
    );
}

#[test]
fn resume_plan_session_not_found() {
    let (mut f, dir) = Fixture::with_temp_home();
    put_rollout(
        &f.home.join(".codex"),
        OTHER_ID,
        &fixture("rollout-cli.jsonl"),
        false,
    );
    let rec = codex_record(dir.path());
    assert_eq!(
        Codex.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("session not found".into())
    );
}

#[test]
fn resume_plan_runs_codex_resume_with_flags_and_id() {
    let (mut f, dir) = Fixture::with_temp_home();
    put_rollout(
        &f.home.join(".codex"),
        ID,
        &fixture("rollout-cli.jsonl"),
        false,
    );
    let rec = codex_record(dir.path());
    let mut env = rec.env.clone();
    env.insert("HERDR_WAKE_SUPERVISED".into(), "1".into());
    assert_eq!(
        Codex.resume_plan(&rec, &mut f.ctx()),
        Plan::Run {
            argv: s(&["/usr/local/bin/codex", "resume", "-m", "o3", "--yolo", ID]),
            env,
            cwd: dir.path().to_path_buf(),
            before: vec![],
            confirm_within_ms: None,
        }
    );
}

#[test]
fn resume_plan_uses_record_codex_home_and_compressed_rollouts() {
    let (mut f, dir) = Fixture::with_temp_home();
    let codex_home = dir.path().join("alt-codex");
    put_rollout(&codex_home, ID, "", true);
    let mut rec = codex_record(dir.path());
    rec.env
        .insert("CODEX_HOME".into(), codex_home.display().to_string());
    assert!(matches!(
        Codex.resume_plan(&rec, &mut f.ctx()),
        Plan::Run { .. }
    ));
    // The same rollout is not found through the default home.
    rec.env.remove("CODEX_HOME");
    assert_eq!(
        Codex.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("session not found".into())
    );
}

#[test]
fn resume_plan_reapplies_the_bypass_setting() {
    let (mut f, dir) = Fixture::with_temp_home();
    f.cfg.restore_bypass = false;
    put_rollout(
        &f.home.join(".codex"),
        ID,
        &fixture("rollout-cli.jsonl"),
        false,
    );
    let rec = codex_record(dir.path());
    match Codex.resume_plan(&rec, &mut f.ctx()) {
        Plan::Run { argv, .. } => {
            assert_eq!(argv, s(&["/usr/local/bin/codex", "resume", "-m", "o3", ID]))
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

// ---------------------------------------------------------------- integration

#[test]
fn integration_is_one_session_start_hook_in_hooks_json() {
    let mut f = Fixture::new();
    let edits = Codex.integration(Path::new("/opt/hw/bin/herdr-wake"), &mut f.ctx());
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].path, PathBuf::from("/home/u/.codex/hooks.json"));
    assert_eq!(
        edits[0].kind,
        EditKind::JsonHook {
            event: "SessionStart".into(),
            matcher: None,
            command: "'/opt/hw/bin/herdr-wake' hook codex start".into(),
            timeout: 10,
        }
    );
}

#[test]
fn integration_follows_codex_home() {
    let mut f = Fixture::new();
    f.env.insert("CODEX_HOME".into(), "/srv/codex".into());
    let edits = Codex.integration(Path::new("/opt/hw/bin/herdr-wake"), &mut f.ctx());
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].path, PathBuf::from("/srv/codex/hooks.json"));
    assert!(edits
        .iter()
        .all(|e| !e.path.to_string_lossy().contains("config.toml")));
}
