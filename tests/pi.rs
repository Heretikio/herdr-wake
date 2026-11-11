mod common;

use std::path::{Path, PathBuf};

use common::Fixture;
use herdr_wake::agents::pi::{self, Pi};
use herdr_wake::agents::{Agent, EditKind, Observation, Plan};
use herdr_wake::config::Config;
use herdr_wake::record::{AgentKind, Record};

const ID: &str = "019a6f2e-7c4d-7b3a-9e21-5d8f0c4b2a17";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi")
            .join(name),
    )
    .unwrap()
}

fn observe(event: &str, payload: &[u8]) -> Observation {
    let mut f = Fixture::new();
    Pi.observe(event, payload, &mut f.ctx())
}

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

fn flags(argv: &str, cfg: &Config) -> Vec<String> {
    Pi.restorable_flags(&args(argv), cfg)
}

// ---- observe ----

#[test]
fn kind_is_pi() {
    assert_eq!(Pi.kind(), AgentKind::Pi);
}

#[test]
fn start_with_file_is_started_with_the_file_and_pid() {
    let obs = observe("start", &fixture("start-session.json"));
    assert_eq!(
        obs,
        Observation::Started {
            session_id: ID.to_string(),
            session_file: Some(PathBuf::from(
                "/home/u/.pi/agent/sessions/--home-u-proj--/2026-11-10T09-12-44-001Z_019a6f2e-7c4d-7b3a-9e21-5d8f0c4b2a17.jsonl"
            )),
            cwd: PathBuf::from("/home/u/proj"),
            terminal: None,
            agent_pid: Some(51234),
            argv: Some(vec!["pi".to_string(), "--model".to_string(), "sonnet".to_string(), "-a".to_string()]),
        }
    );
}

#[test]
fn start_without_file_is_started_with_no_file() {
    let obs = observe("start", &fixture("start-prompt-no-file.json"));
    assert_eq!(
        obs,
        Observation::Started {
            session_id: ID.to_string(),
            session_file: None,
            cwd: PathBuf::from("/home/u/proj"),
            terminal: None,
            agent_pid: Some(51234),
            argv: Some(vec!["pi".to_string()]),
        }
    );
}

#[test]
fn start_with_empty_file_string_has_no_file() {
    let obs = observe(
        "start",
        br#"{"reason":"new","id":"abc","file":"","cwd":"/p","argv":[],"pid":1}"#,
    );
    match obs {
        Observation::Started { session_file, .. } => assert_eq!(session_file, None),
        other => panic!("{other:?}"),
    }
}

#[test]
fn start_without_pid_has_no_agent_pid() {
    let obs = observe("start", br#"{"reason":"resume","id":"abc","cwd":"/p"}"#);
    match obs {
        Observation::Started { agent_pid, .. } => assert_eq!(agent_pid, None),
        other => panic!("{other:?}"),
    }
}

#[test]
fn quit_without_signal_is_a_deliberate_end() {
    assert_eq!(
        observe("end", &fixture("end-quit.json")),
        Observation::Ended {
            session_id: ID.to_string(),
            deliberate: Some(true),
        }
    );
}

#[test]
fn quit_after_a_signal_is_ignored() {
    assert!(matches!(
        observe("end", &fixture("end-signal.json")),
        Observation::Ignore(_)
    ));
}

#[test]
fn malformed_payloads_and_unknown_events_are_ignored() {
    for (event, payload) in [
        ("start", &b"not json"[..]),
        ("start", br#"{"id":"","cwd":"/p"}"#),
        ("start", br#"{"id":"abc"}"#),
        ("start", br#"{"id":"abc","cwd":"relative/dir"}"#),
        ("end", br#"{"signal":false}"#),
        ("end", br#"{"id":"","signal":false}"#),
        ("stop", &fixture("start-session.json")[..]),
    ] {
        assert!(
            matches!(observe(event, payload), Observation::Ignore(_)),
            "{event} {}",
            String::from_utf8_lossy(payload)
        );
    }
}

// ---- restorable_flags ----

#[test]
fn flag_table() {
    let cfg = Config::default();
    let cases: &[(&str, &str)] = &[
        ("pi", ""),
        (
            "pi --provider anthropic --model sonnet",
            "--provider anthropic --model sonnet",
        ),
        (
            "pi --models a,b --thinking high",
            "--models a,b --thinking high",
        ),
        ("pi --session-dir /tmp/s", "--session-dir /tmp/s"),
        (
            "pi --tools read,bash -xt edit",
            "--tools read,bash -xt edit",
        ),
        (
            "pi -t read --exclude-tools write",
            "-t read --exclude-tools write",
        ),
        (
            "pi --no-tools -nbt --no-skills -nc --offline",
            "--no-tools -nbt --no-skills -nc --offline",
        ),
        (
            "pi -nt --no-builtin-tools -ns --no-context-files",
            "-nt --no-builtin-tools -ns --no-context-files",
        ),
        ("pi --tui-mode fullscreen", "--tui-mode fullscreen"),
        ("pi --tui-mode bogus --model m", "--model m"),
        ("pi --tui-mode --model m", "--model m"),
        ("pi -a", "-a"),
        ("pi --approve", "--approve"),
        ("pi -na", "-na"),
        ("pi --no-approve", "--no-approve"),
        // never
        ("pi --session /s.jsonl --session-id x --fork /f.jsonl", ""),
        ("pi -c -r --no-session", ""),
        ("pi -p hello --model m", "--model m"),
        ("pi --mode json --api-key sk-test --name work", ""),
        ("pi -n work --model m", "--model m"),
        ("pi fix the bug please", ""),
        ("pi @notes.md --model m", "--model m"),
        // needs validation: not restored
        ("pi --no-extensions -ne -e ./x.ts --extension ./y.ts", ""),
        ("pi --skill s --prompt-template t --theme th", ""),
        ("pi --system-prompt sp --append-system-prompt ap", ""),
        // a value that looks like a restorable flag stays with its owner
        (
            "pi --system-prompt --model --thinking low",
            "--thinking low",
        ),
        ("pi --name --offline", ""),
        // unknown extension flags and their values are dropped
        ("pi --my-ext-flag value --model m", "--model m"),
        ("pi --my-ext=1 -x --model m", "--model m"),
        // a value flag at the very end has no value
        ("pi --model", ""),
        // everything after -- is a message
        ("pi --model m -- --offline", "--model m"),
    ];
    for (argv, want) in cases {
        assert_eq!(flags(argv, &cfg), args(want), "argv: {argv}");
    }
}

#[test]
fn approve_is_dropped_when_bypass_restore_is_off() {
    let cfg = Config {
        restore_bypass: false,
        ..Config::default()
    };
    assert_eq!(flags("pi -a --model m", &cfg), args("--model m"));
    assert_eq!(flags("pi --approve", &cfg), args(""));
}

#[test]
fn no_approve_is_kept_when_bypass_restore_is_off() {
    // -na and --no-approve deny project trust; dropping them would loosen it.
    let cfg = Config {
        restore_bypass: false,
        ..Config::default()
    };
    assert_eq!(flags("pi -na", &cfg), args("-na"));
    assert_eq!(flags("pi --no-approve", &cfg), args("--no-approve"));
}

#[test]
fn empty_argv_gives_no_flags() {
    assert!(Pi.restorable_flags(&[], &Config::default()).is_empty());
}

// ---- resume_plan ----

fn record(cwd: &Path, file: Option<PathBuf>) -> Record {
    let mut rec = Record::new(
        AgentKind::Pi,
        ID,
        cwd.to_path_buf(),
        PathBuf::from("/usr/local/bin/pi"),
        "boot-1",
    );
    rec.session_file = file;
    rec.flags = args("--model sonnet -a");
    rec.env.insert(
        "PI_CODING_AGENT_SESSION_DIR".to_string(),
        cwd.display().to_string(),
    );
    rec
}

#[test]
fn resume_runs_pi_with_flags_and_the_session_file() {
    let (mut f, dir) = Fixture::with_temp_home();
    let file = dir.path().join("s.jsonl");
    std::fs::write(&file, "{}\n").unwrap();
    let rec = record(dir.path(), Some(file.clone()));

    let plan = Pi.resume_plan(&rec, &mut f.ctx());

    let mut env = rec.env.clone();
    env.insert("HERDR_WAKE_SUPERVISED".to_string(), "1".to_string());
    assert_eq!(
        plan,
        Plan::Run {
            argv: vec![
                "/usr/local/bin/pi".to_string(),
                "--model".to_string(),
                "sonnet".to_string(),
                "-a".to_string(),
                "--session".to_string(),
                file.display().to_string(),
            ],
            env,
            cwd: dir.path().to_path_buf(),
            before: vec![],
            confirm_within_ms: None,
        }
    );
}

#[test]
fn resume_rechecks_flags_against_the_current_config() {
    let (mut f, dir) = Fixture::with_temp_home();
    let file = dir.path().join("s.jsonl");
    std::fs::write(&file, "{}\n").unwrap();
    f.cfg.restore_bypass = false;
    let rec = record(dir.path(), Some(file.clone()));

    match Pi.resume_plan(&rec, &mut f.ctx()) {
        Plan::Run { argv, .. } => assert_eq!(
            argv,
            vec![
                "/usr/local/bin/pi".to_string(),
                "--model".to_string(),
                "sonnet".to_string(),
                "--session".to_string(),
                file.display().to_string(),
            ]
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn resume_with_missing_folder_is_permanent() {
    let (mut f, dir) = Fixture::with_temp_home();
    let file = dir.path().join("s.jsonl");
    std::fs::write(&file, "{}\n").unwrap();
    let rec = record(&dir.path().join("gone"), Some(file));
    assert_eq!(
        Pi.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("folder missing".to_string())
    );
}

#[test]
fn resume_with_no_session_file_is_permanent() {
    let (mut f, dir) = Fixture::with_temp_home();
    let rec = record(dir.path(), None);
    assert_eq!(
        Pi.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("session not found".to_string())
    );
}

#[test]
fn resume_with_missing_session_file_is_permanent() {
    let (mut f, dir) = Fixture::with_temp_home();
    let rec = record(dir.path(), Some(dir.path().join("never-written.jsonl")));
    assert_eq!(
        Pi.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("session not found".to_string())
    );
}

// ---- integration ----

#[test]
fn integration_writes_the_extension_under_the_default_agent_dir() {
    let (mut f, dir) = Fixture::with_temp_home();
    let bin = Path::new("/opt/hw/bin/herdr-wake");
    let edits = Pi.integration(bin, &mut f.ctx());
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].path,
        dir.path().join(".pi/agent/extensions/herdr-wake.ts")
    );
    assert_eq!(
        edits[0].kind,
        EditKind::WriteFile {
            body: pi::render_asset(bin),
            mode: 0o644,
        }
    );
}

#[test]
fn integration_follows_pi_coding_agent_dir() {
    let (mut f, dir) = Fixture::with_temp_home();
    f.env.insert(
        "PI_CODING_AGENT_DIR".to_string(),
        dir.path().join("custom").display().to_string(),
    );
    let edits = Pi.integration(Path::new("/b/herdr-wake"), &mut f.ctx());
    assert_eq!(
        edits[0].path,
        dir.path().join("custom/extensions/herdr-wake.ts")
    );
}

#[test]
fn integration_expands_a_tilde_in_pi_coding_agent_dir() {
    let (mut f, dir) = Fixture::with_temp_home();
    f.env
        .insert("PI_CODING_AGENT_DIR".to_string(), "~/pi-agent".to_string());
    let edits = Pi.integration(Path::new("/b/herdr-wake"), &mut f.ctx());
    assert_eq!(
        edits[0].path,
        dir.path().join("pi-agent/extensions/herdr-wake.ts")
    );
}

#[test]
fn integration_ignores_an_empty_pi_coding_agent_dir() {
    let (mut f, dir) = Fixture::with_temp_home();
    f.env
        .insert("PI_CODING_AGENT_DIR".to_string(), String::new());
    let edits = Pi.integration(Path::new("/b/herdr-wake"), &mut f.ctx());
    assert_eq!(
        edits[0].path,
        dir.path().join(".pi/agent/extensions/herdr-wake.ts")
    );
}

// ---- asset ----

#[test]
fn raw_asset_has_the_marker_on_its_first_line_and_the_placeholder() {
    let first = pi::ASSET.lines().next().unwrap();
    assert!(first.contains("herdr-wake v"), "{first}");
    assert_eq!(pi::ASSET.matches("\"__HERDR_WAKE_BIN__\"").count(), 1);
}

#[test]
fn rendered_asset_names_the_binary_and_the_version() {
    let body = pi::render_asset(Path::new("/Users/me/.local/bin/herdr-wake"));
    let first = body.lines().next().unwrap();
    assert!(
        first.contains(&format!("herdr-wake v{}", env!("CARGO_PKG_VERSION"))),
        "{first}"
    );
    assert!(!body.contains("__HERDR_WAKE_BIN__"));
    assert!(!body.contains("__HERDR_WAKE_VERSION__"));
    assert!(body.contains(r#"const BIN = "/Users/me/.local/bin/herdr-wake";"#));
}

#[test]
fn rendered_asset_escapes_quotes_and_backslashes_in_the_path() {
    let body = pi::render_asset(Path::new(r#"/odd "dir"\x/herdr-wake"#));
    assert!(body.contains(r#"const BIN = "/odd \"dir\"\\x/herdr-wake";"#));
}

#[test]
fn asset_follows_the_contract() {
    let a = pi::ASSET;
    assert!(!a.contains('\u{2014}'), "em dash in asset");
    assert!(a.contains("HC_TERMINAL_ID"));
    assert!(a.contains(r#"ctx?.mode !== "tui""#));
    assert!(a.contains("prependListener"));
    assert!(a.contains("SIGHUP") && a.contains("SIGTERM"));
    assert!(a.contains("spawnSync"));
    assert!(a.contains("detached: true"));
    assert!(a.contains(r#""session_start""#) && a.contains(r#""agent_start""#));
    assert!(a.contains(r#""session_shutdown""#));
    assert!(a.contains(r#"["hook", "pi", "start"]"#));
    assert!(a.contains(r#"["hook", "pi", "end"]"#));
}

// ---- the session file must be Pi's (M-5 d) ----

fn permanent_reason(plan: Plan) -> String {
    match plan {
        Plan::Permanent(why) => why,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_session_file_outside_pis_session_folder_is_refused() {
    let (mut f, dir) = Fixture::with_temp_home();
    let elsewhere = dir.path().join("elsewhere.jsonl");
    std::fs::write(&elsewhere, "{}\n").unwrap();
    let sessions = dir.path().join("sessions");
    std::fs::create_dir(&sessions).unwrap();
    let mut rec = record(dir.path(), Some(elsewhere));
    rec.env.insert(
        "PI_CODING_AGENT_SESSION_DIR".to_string(),
        sessions.display().to_string(),
    );
    assert_eq!(
        permanent_reason(Pi.resume_plan(&rec, &mut f.ctx())),
        "the recorded session file is not usable: it is not in Pi's session folder"
    );
    // The same file inside the folder is fine.
    let inside = sessions.join("s.jsonl");
    std::fs::write(&inside, "{}\n").unwrap();
    rec.session_file = Some(inside);
    assert!(matches!(
        Pi.resume_plan(&rec, &mut f.ctx()),
        Plan::Run { .. }
    ));
}

#[test]
fn the_default_session_folder_is_under_the_agent_dir() {
    let (mut f, dir) = Fixture::with_temp_home();
    let file = dir.path().join(".pi/agent/sessions/proj/s.jsonl");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "{}\n").unwrap();
    let mut rec = record(dir.path(), Some(file));
    rec.env.clear();
    assert!(matches!(
        Pi.resume_plan(&rec, &mut f.ctx()),
        Plan::Run { .. }
    ));
    // PI_CODING_AGENT_DIR moves it.
    rec.env.insert(
        "PI_CODING_AGENT_DIR".to_string(),
        dir.path().join("other").display().to_string(),
    );
    assert!(matches!(
        Pi.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent(_)
    ));
}

#[test]
fn a_recorded_session_dir_flag_names_a_session_folder_too() {
    let (mut f, dir) = Fixture::with_temp_home();
    let custom = dir.path().join("custom");
    std::fs::create_dir(&custom).unwrap();
    let file = custom.join("s.jsonl");
    std::fs::write(&file, "{}\n").unwrap();
    let mut rec = record(dir.path(), Some(file));
    rec.env.clear();
    assert!(matches!(
        Pi.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent(_)
    ));
    rec.flags = args(&format!("--session-dir {}", custom.display()));
    assert!(matches!(
        Pi.resume_plan(&rec, &mut f.ctx()),
        Plan::Run { .. }
    ));
}

#[test]
fn a_symlinked_or_relative_session_file_is_refused() {
    let (mut f, dir) = Fixture::with_temp_home();
    let target = dir.path().join("target.jsonl");
    std::fs::write(&target, "{}\n").unwrap();
    let link = dir.path().join("link.jsonl");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let rec = record(dir.path(), Some(link));
    assert_eq!(
        permanent_reason(Pi.resume_plan(&rec, &mut f.ctx())),
        "the recorded session file is not usable: it is not a regular file"
    );
}
