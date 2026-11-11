//! Claude Code adapter, driven through the `Agent` interface with the fakes
//! and payload fixtures under `tests/fixtures/claude/`.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::Fixture;
use herdr_wake::agents::claude::{project_dir_name, Claude};
use herdr_wake::agents::{Agent, EditKind, Observation, Plan};
use herdr_wake::platform::{Platform, ProcInfo};
use herdr_wake::record::{AgentKind, Record};

const ID: &str = "0b5a2c1e-1111-4222-8333-444455556666";
const NEW_ID: &str = "7c9d3e2f-aaaa-4bbb-8ccc-ddddeeeeffff";

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The process tree a hook sees: claude (pid 500) runs `sh -c` (pid 600),
/// which runs herdr-wake with `HERDR_WAKE_PPID=600`.
fn with_claude(f: &mut Fixture, claude_argv: &[&str], claude_env: Option<&[(&str, &str)]>) {
    let mut claude = ProcInfo::new(500, 400, 1_700_000_000_500, strings(claude_argv));
    claude.env = claude_env.map(|kv| {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>()
    });
    f.platform.add_process(claude);
    f.platform.add_process(ProcInfo::new(
        600,
        500,
        1_700_000_000_600,
        strings(&["/bin/sh", "-c", "'/opt/hw/herdr-wake' hook claude start"]),
    ));
    f.env.insert("HERDR_WAKE_PPID".into(), "600".into());
}

fn started(id: &str, pid: Option<u32>) -> Observation {
    Observation::Started {
        session_id: id.to_string(),
        session_file: Some(PathBuf::from(format!(
            "/home/u/.claude/projects/-home-u-proj/{id}.jsonl"
        ))),
        cwd: PathBuf::from("/home/u/proj"),
        terminal: None,
        agent_pid: pid,
        argv: None,
    }
}

#[test]
fn startup_gives_started_with_the_claude_parent_as_agent_pid() {
    let mut f = Fixture::new();
    with_claude(&mut f, &["claude", "--model", "opus"], None);
    let obs = Claude.observe("start", &fixture("start_startup.json"), &mut f.ctx());
    assert_eq!(obs, started(ID, Some(500)));
}

#[test]
fn every_start_source_gives_started() {
    for (file, id) in [
        ("start_startup.json", ID),
        ("start_resume.json", ID),
        ("start_compact.json", ID),
        ("start_clear.json", NEW_ID),
        ("start_fork.json", NEW_ID),
    ] {
        let mut f = Fixture::new();
        with_claude(&mut f, &["claude"], None);
        let obs = Claude.observe("start", &fixture(file), &mut f.ctx());
        assert_eq!(obs, started(id, Some(500)), "{file}");
    }
}

#[test]
fn unknown_source_is_ignored() {
    let mut f = Fixture::new();
    with_claude(&mut f, &["claude"], None);
    let payload = br#"{"session_id":"0b5a2c1e-1111-4222-8333-444455556666","cwd":"/home/u/proj","hook_event_name":"SessionStart","source":"teleport"}"#;
    assert!(matches!(
        Claude.observe("start", payload, &mut f.ctx()),
        Observation::Ignore(_)
    ));
}

#[test]
fn sub_agent_start_and_end_are_ignored() {
    let mut f = Fixture::new();
    with_claude(&mut f, &["claude"], None);
    for (event, file) in [
        ("start", "start_subagent.json"),
        ("end", "end_subagent.json"),
    ] {
        assert!(
            matches!(
                Claude.observe(event, &fixture(file), &mut f.ctx()),
                Observation::Ignore(_)
            ),
            "{file}"
        );
    }
}

#[test]
fn print_and_bare_runs_are_ignored() {
    for argv in [
        &["claude", "-p", "summarize this"][..],
        &["claude", "--print", "summarize this"],
        &["/usr/local/bin/claude", "--model", "opus", "--print"],
        &["claude", "-cp"],
        &["claude", "--bare"],
    ] {
        let mut f = Fixture::new();
        with_claude(&mut f, argv, None);
        let obs = Claude.observe("start", &fixture("start_startup.json"), &mut f.ctx());
        assert!(matches!(obs, Observation::Ignore(_)), "{argv:?}: {obs:?}");
    }
}

#[test]
fn a_flag_value_that_looks_like_print_is_not_print_mode() {
    let mut f = Fixture::new();
    with_claude(
        &mut f,
        &["claude", "--append-system-prompt", "-p", "fix it"],
        None,
    );
    let obs = Claude.observe("start", &fixture("start_startup.json"), &mut f.ctx());
    assert_eq!(obs, started(ID, Some(500)));
}

#[test]
fn nested_child_session_is_ignored_when_env_is_readable() {
    let mut f = Fixture::new();
    with_claude(
        &mut f,
        &["claude"],
        Some(&[("CLAUDE_CODE_CHILD_SESSION", "1"), ("PATH", "/usr/bin")]),
    );
    let obs = Claude.observe("start", &fixture("start_startup.json"), &mut f.ctx());
    assert!(matches!(obs, Observation::Ignore(_)), "{obs:?}");

    let mut f = Fixture::new();
    with_claude(&mut f, &["claude"], Some(&[("PATH", "/usr/bin")]));
    let obs = Claude.observe("start", &fixture("start_startup.json"), &mut f.ctx());
    assert_eq!(obs, started(ID, Some(500)));
}

#[test]
fn without_hook_ppid_there_is_no_agent_pid() {
    let mut f = Fixture::new();
    with_claude(&mut f, &["claude", "-p"], None);
    f.env.remove("HERDR_WAKE_PPID");
    let obs = Claude.observe("start", &fixture("start_startup.json"), &mut f.ctx());
    assert_eq!(obs, started(ID, None));
}

#[test]
fn no_claude_among_the_ancestors_gives_no_agent_pid() {
    let mut f = Fixture::new();
    with_claude(&mut f, &["/usr/bin/python3", "wrapper.py"], None);
    let obs = Claude.observe("start", &fixture("start_startup.json"), &mut f.ctx());
    assert_eq!(obs, started(ID, None));
}

#[test]
fn npm_and_native_builds_are_recognised() {
    for (argv, exe) in [
        (
            &[
                "node",
                "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            ][..],
            "/usr/bin/node",
        ),
        (&["2.1.322"], "/home/u/.local/share/claude/versions/2.1.322"),
        (
            &["/home/u/.local/bin/claude", "--model", "opus"],
            "/x/2.1.322",
        ),
    ] {
        let mut f = Fixture::new();
        with_claude(&mut f, argv, None);
        let mut p = f.platform.process(500).unwrap();
        p.exe = PathBuf::from(exe);
        f.platform.add_process(p);
        let obs = Claude.observe("start", &fixture("start_startup.json"), &mut f.ctx());
        assert_eq!(obs, started(ID, Some(500)), "{argv:?}");
    }
}

#[test]
fn malformed_payloads_and_unknown_events_are_ignored() {
    let mut f = Fixture::new();
    with_claude(&mut f, &["claude"], None);
    for payload in [
        &b"not json"[..],
        br#"{"cwd":"/home/u/proj","source":"startup"}"#,
        br#"{"session_id":"0b5a2c1e-1111-4222-8333-444455556666","source":"startup"}"#,
        br#"{"session_id":"../../etc","cwd":"/home/u/proj","source":"startup"}"#,
        br#"{"session_id":"0b5a2c1e-1111-4222-8333-444455556666","cwd":"relative","source":"startup"}"#,
    ] {
        let obs = Claude.observe("start", payload, &mut f.ctx());
        assert!(matches!(obs, Observation::Ignore(_)), "{obs:?}");
    }
    assert!(matches!(
        Claude.observe("bogus", &fixture("start_startup.json"), &mut f.ctx()),
        Observation::Ignore(_)
    ));
}

// ---------------------------------------------------------------------------
// SessionEnd

#[test]
fn end_reasons_map_to_quit_id_change_or_unknown() {
    let mut f = Fixture::new();
    let ended = |deliberate| Observation::Ended {
        session_id: ID.to_string(),
        deliberate,
    };
    for (file, want) in [
        ("end_prompt_input_exit.json", Some(ended(Some(true)))),
        ("end_logout.json", Some(ended(Some(true)))),
        ("end_other.json", Some(ended(None))),
        ("end_clear.json", None),
        ("end_resume.json", None),
    ] {
        let obs = Claude.observe("end", &fixture(file), &mut f.ctx());
        match want {
            Some(w) => assert_eq!(obs, w, "{file}"),
            None => assert!(matches!(obs, Observation::Ignore(_)), "{file}: {obs:?}"),
        }
    }
}

#[test]
fn an_unknown_end_reason_is_left_to_the_watcher() {
    let mut f = Fixture::new();
    let payload = br#"{"session_id":"0b5a2c1e-1111-4222-8333-444455556666","cwd":"/home/u/proj","hook_event_name":"SessionEnd","reason":"bypass_permissions_disabled"}"#;
    assert_eq!(
        Claude.observe("end", payload, &mut f.ctx()),
        Observation::Ended {
            session_id: ID.to_string(),
            deliberate: None
        }
    );
}

// ---------------------------------------------------------------------------
// Flags

#[test]
fn flags_table() {
    #[rustfmt::skip]
    let cases: &[(&[&str], bool, &[&str])] = &[
        // (argv, restore_bypass, restored flags)
        (&["claude"], true, &[]),
        (&["claude", "--model", "opus", "fix the bug"], true, &["--model", "opus"]),
        (&["claude", "fix the bug", "--model=opus"], true, &["--model=opus"]),
        (&["claude", "--dangerously-skip-permissions", "--model", "opus"], true,
            &["--dangerously-skip-permissions", "--model", "opus"]),
        (&["claude", "--dangerously-skip-permissions", "--model", "opus"], false, &["--model", "opus"]),
        (&["claude", "--allow-dangerously-skip-permissions"], true, &["--allow-dangerously-skip-permissions"]),
        (&["claude", "--allow-dangerously-skip-permissions"], false, &[]),
        (&["claude", "--permission-mode", "bypassPermissions"], true, &["--permission-mode", "bypassPermissions"]),
        (&["claude", "--permission-mode", "bypassPermissions"], false, &[]),
        (&["claude", "--permission-mode=acceptEdits"], false, &[]),
        (&["claude", "--permission-mode=plan"], false, &["--permission-mode=plan"]),
        (&["claude", "-p", "hi", "--model", "opus"], true, &["--model", "opus"]),
        (&["claude", "--print", "--output-format", "json", "hi"], true, &[]),
        (&["claude", "--add-dir", "a", "/b c", "--model", "opus"], true,
            &["--add-dir", "a", "/b c", "--model", "opus"]),
        (&["claude", "--add-dir", "/a", "--add-dir", "/b"], true, &["--add-dir", "/a", "--add-dir", "/b"]),
        (&["claude", "--add-dir=/a", "fix it"], true, &["--add-dir=/a"]),
        (&["claude", "--allowedTools", "Bash(git *)", "Edit", "--disallowed-tools=WebFetch", "go"], true,
            &["--allowedTools", "Bash(git *)", "Edit", "--disallowed-tools=WebFetch"]),
        (&["claude", "--tools", "Read,Edit", "--betas", "b1", "b2", "--chrome", "--restricted"], true,
            &["--tools", "Read,Edit", "--betas", "b1", "b2", "--chrome", "--restricted"]),
        (&["claude", "--mcp-config", "m.json", "{\"mcpServers\":{}}", "--strict-mcp-config"], true,
            &["--strict-mcp-config"]), // inline JSON is never stored (M-3)
        (&["claude", "--settings={\"model\":\"opus\"}", "--setting-sources", "user,project"], true,
            &["--setting-sources", "user,project"]),
        (&["claude", "--resume", "abc", "--continue", "--session-id", "x", "--fork-session",
            "-n", "work", "--effort", "high"], true, &["--effort", "high"]),
        (&["claude", "-r", "--model", "opus"], true, &["--model", "opus"]),
        (&["claude", "--debug", "api", "--debug-file", "/tmp/d", "--ide", "--tmux", "-w", "feat",
            "--from-pr", "12", "--teleport", "--bg", "--file", "f1:a", "f2:b"], true, &[]),
        (&["claude", "--remote-control", "--bare", "--safe-mode", "--autocompact", "200k", "--verbose"], true, &[]),
        (&["claude", "-nwork", "--fallback-model", "sonnet", "-cp"], true, &["--fallback-model", "sonnet"]),
        (&["claude", "--system-prompt", "-p", "--agent", "reviewer"], true,
            &["--system-prompt", "-p", "--agent", "reviewer"]),
        (&["claude", "--model"], true, &[]),
        (&["claude", "--", "--model", "opus"], true, &[]),
        (&["claude", "--plugin-dir", "./p1", "--plugin-dir=p2", "--append-system-prompt", "be brief"], true,
            &["--plugin-dir", "./p1", "--plugin-dir=p2", "--append-system-prompt", "be brief"]),
    ];
    assert!(cases.len() >= 15);
    for (argv, bypass, want) in cases {
        let cfg = herdr_wake::config::Config {
            restore_bypass: *bypass,
            ..Default::default()
        };
        let got = Claude.restorable_flags(&strings(argv), &cfg);
        assert_eq!(got, strings(want), "argv {argv:?}, restore_bypass {bypass}");
    }
}

// ---------------------------------------------------------------------------
// Resume

#[test]
fn project_dir_name_replaces_every_non_alphanumeric() {
    assert_eq!(
        project_dir_name(Path::new("/home/u/my.proj_1")),
        "-home-u-my-proj-1"
    );
    assert_eq!(
        project_dir_name(Path::new("/home/u/caf\u{e9}")),
        "-home-u-caf-"
    );
    assert_eq!(project_dir_name(Path::new("/a/\u{1f600}b")), "-a---b");
}

/// A record whose cwd is `<home>/work/my.proj` (created), with no transcript.
fn resume_setup() -> (Fixture, tempfile::TempDir, Record) {
    let (f, dir) = Fixture::with_temp_home();
    let cwd = f.home.join("work/my.proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rec = Record::new(
        AgentKind::Claude,
        ID,
        cwd,
        PathBuf::from("/usr/local/bin/claude"),
        "boot-1",
    );
    rec.env.insert("PATH".into(), "/usr/bin:/bin".into());
    (f, dir, rec)
}

fn write_transcript(config: &Path, folder: &str) {
    let dir = config.join("projects").join(folder);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{ID}.jsonl")), "{}\n").unwrap();
}

#[test]
fn resume_runs_claude_with_flags_then_resume_id() {
    let (mut f, _dir, mut rec) = resume_setup();
    write_transcript(&f.home.join(".claude"), &project_dir_name(&rec.cwd));
    rec.flags = strings(&["--model", "opus", "--dangerously-skip-permissions"]);
    let plan = Claude.resume_plan(&rec, &mut f.ctx());
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    env.insert("HERDR_WAKE_SUPERVISED".to_string(), "1".to_string());
    assert_eq!(
        plan,
        Plan::Run {
            argv: strings(&[
                "/usr/local/bin/claude",
                "--model",
                "opus",
                "--dangerously-skip-permissions",
                "--resume",
                ID
            ]),
            env,
            cwd: rec.cwd.clone(),
            before: Vec::new(),
            confirm_within_ms: None,
        }
    );
}

#[test]
fn resume_with_a_missing_folder_is_permanent() {
    let (mut f, _dir, mut rec) = resume_setup();
    write_transcript(&f.home.join(".claude"), &project_dir_name(&rec.cwd));
    rec.cwd = f.home.join("gone");
    assert_eq!(
        Claude.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("folder missing".into())
    );
}

#[test]
fn resume_with_no_transcript_is_permanent() {
    let (mut f, _dir, rec) = resume_setup();
    assert_eq!(
        Claude.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("session not found".into())
    );
    // A transcript for another session does not count.
    let other = f
        .home
        .join(".claude/projects")
        .join(project_dir_name(&rec.cwd));
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join(format!("{NEW_ID}.jsonl")), "{}\n").unwrap();
    assert_eq!(
        Claude.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("session not found".into())
    );
}

#[test]
fn resume_looks_in_the_recorded_config_dir() {
    let (mut f, _dir, mut rec) = resume_setup();
    let config = f.home.join("profiles/work");
    rec.env
        .insert("CLAUDE_CONFIG_DIR".into(), config.display().to_string());
    // Only under the default dir: the recorded one has no transcript.
    write_transcript(&f.home.join(".claude"), &project_dir_name(&rec.cwd));
    assert_eq!(
        Claude.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("session not found".into())
    );
    write_transcript(&config, &project_dir_name(&rec.cwd));
    match Claude.resume_plan(&rec, &mut f.ctx()) {
        Plan::Run { env, .. } => {
            assert_eq!(env["CLAUDE_CONFIG_DIR"], config.display().to_string())
        }
        p => panic!("{p:?}"),
    }
}

#[test]
fn resume_finds_a_transcript_under_a_shortened_project_folder() {
    let (mut f, _dir, rec) = resume_setup();
    write_transcript(&f.home.join(".claude"), "-home-u-very-long-name-1a2b3c");
    assert!(matches!(
        Claude.resume_plan(&rec, &mut f.ctx()),
        Plan::Run { .. }
    ));
}

#[test]
fn resume_resolves_relative_paths_against_the_record_cwd() {
    let (mut f, _dir, mut rec) = resume_setup();
    write_transcript(&f.home.join(".claude"), &project_dir_name(&rec.cwd));
    rec.flags = strings(&[
        "--add-dir",
        "sub",
        "/abs",
        "~/x",
        "--settings",
        "s.json",
        "--mcp-config",
        "m.json",
        "m2.json",
        "--plugin-dir=plug",
        "--model",
        "opus",
    ]);
    let cwd = rec.cwd.display().to_string();
    let Plan::Run { argv, .. } = Claude.resume_plan(&rec, &mut f.ctx()) else {
        panic!("not a Run plan");
    };
    assert_eq!(
        argv,
        vec![
            "/usr/local/bin/claude".to_string(),
            "--add-dir".into(),
            format!("{cwd}/sub"),
            "/abs".into(),
            "~/x".into(),
            "--settings".into(),
            format!("{cwd}/s.json"),
            "--mcp-config".into(),
            format!("{cwd}/m.json"),
            format!("{cwd}/m2.json"),
            format!("--plugin-dir={cwd}/plug"),
            "--model".into(),
            "opus".into(),
            "--resume".into(),
            ID.into(),
        ]
    );
}

#[test]
fn resume_drops_bypass_flags_when_restore_bypass_is_off_at_wake() {
    let (mut f, _dir, mut rec) = resume_setup();
    write_transcript(&f.home.join(".claude"), &project_dir_name(&rec.cwd));
    rec.flags = strings(&[
        "--dangerously-skip-permissions",
        "--permission-mode",
        "bypassPermissions",
        "--model",
        "opus",
    ]);
    f.cfg.restore_bypass = false;
    let Plan::Run { argv, .. } = Claude.resume_plan(&rec, &mut f.ctx()) else {
        panic!("not a Run plan");
    };
    assert_eq!(
        argv,
        strings(&["/usr/local/bin/claude", "--model", "opus", "--resume", ID])
    );
}

// ---------------------------------------------------------------------------
// Integration

#[test]
fn integration_adds_start_and_end_hooks_to_settings_json() {
    let (mut f, _dir) = Fixture::with_temp_home();
    let edits = Claude.integration(Path::new("/opt/hw/herdr-wake"), &mut f.ctx());
    let settings = f.home.join(".claude/settings.json");
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|e| e.path == settings));
    assert_eq!(
        edits[0].kind,
        EditKind::JsonHook {
            event: "SessionStart".into(),
            matcher: Some("^(startup|resume|clear|compact|fork)$".into()),
            command: "'/opt/hw/herdr-wake' hook claude start".into(),
            timeout: 10,
        }
    );
    assert_eq!(
        edits[1].kind,
        EditKind::JsonHook {
            event: "SessionEnd".into(),
            matcher: None,
            command: "'/opt/hw/herdr-wake' hook claude end".into(),
            timeout: 5,
        }
    );
}

#[test]
fn integration_uses_claude_config_dir_and_quotes_the_binary() {
    let (mut f, _dir) = Fixture::with_temp_home();
    let config = f.home.join("profiles/work");
    f.env
        .insert("CLAUDE_CONFIG_DIR".into(), config.display().to_string());
    let edits = Claude.integration(Path::new("/opt/it's/herdr-wake"), &mut f.ctx());
    assert_eq!(edits[0].path, config.join("settings.json"));
    match &edits[0].kind {
        EditKind::JsonHook { command, .. } => {
            assert_eq!(command, r"'/opt/it'\''s/herdr-wake' hook claude start")
        }
        k => panic!("{k:?}"),
    }
}
