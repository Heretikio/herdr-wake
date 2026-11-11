mod common;

use std::path::{Path, PathBuf};

use common::Fixture;
use herdr_wake::agents::opencode::{self, OpenCode};
use herdr_wake::agents::{Agent, EditKind, Observation, Plan};
use herdr_wake::config::Config;
use herdr_wake::record::{AgentKind, Record};

const V2_ID: &str = "ses_7a1f3c9e2ffeAbCdEf0123456789";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/opencode")
            .join(name),
    )
    .unwrap()
}

fn observe(event: &str, payload: &[u8]) -> Observation {
    let mut f = Fixture::new();
    OpenCode.observe(event, payload, &mut f.ctx())
}

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

fn flags(argv: &str, cfg: &Config) -> Vec<String> {
    OpenCode.restorable_flags(&args(argv), cfg)
}

// ---- observe ----

#[test]
fn kind_is_opencode() {
    assert_eq!(OpenCode.kind(), AgentKind::OpenCode);
}

#[test]
fn start_v2_is_started_with_the_pid_and_no_file() {
    assert_eq!(
        observe("start", &fixture("start-v2.json")),
        Observation::Started {
            session_id: V2_ID.to_string(),
            session_file: None,
            cwd: PathBuf::from("/home/u/proj"),
            terminal: None,
            agent_pid: Some(61234),
            argv: Some(vec!["opencode".to_string(), "--auto".to_string()]),
        }
    );
}

#[test]
fn start_v1_is_started_too() {
    assert_eq!(
        observe("start", &fixture("start-v1.json")),
        Observation::Started {
            session_id: "ses_4b2e8d1c0aaeZyXwVu9876543210".to_string(),
            session_file: None,
            cwd: PathBuf::from("/home/u/proj"),
            terminal: None,
            agent_pid: Some(61300),
            argv: Some(vec![
                "opencode".to_string(),
                "-m".to_string(),
                "anthropic/claude-sonnet-4".to_string()
            ]),
        }
    );
}

#[test]
fn start_without_pid_or_major_still_counts() {
    match observe("start", br#"{"id":"ses_1","cwd":"/p"}"#) {
        Observation::Started { agent_pid, .. } => assert_eq!(agent_pid, None),
        other => panic!("{other:?}"),
    }
}

#[test]
fn exit_without_signal_is_a_deliberate_end() {
    assert_eq!(
        observe("end", &fixture("end-quit.json")),
        Observation::Ended {
            session_id: V2_ID.to_string(),
            deliberate: Some(true),
        }
    );
}

#[test]
fn exit_after_a_signal_is_ignored() {
    assert!(matches!(
        observe("end", &fixture("end-signal.json")),
        Observation::Ignore(_)
    ));
}

#[test]
fn malformed_payloads_and_unknown_events_are_ignored() {
    for (event, payload) in [
        ("start", &b"{"[..]),
        ("start", br#"{"id":"","cwd":"/p"}"#),
        ("start", br#"{"id":"ses_1"}"#),
        ("start", br#"{"id":"ses_1","cwd":"rel"}"#),
        ("end", br#"{"signal":false}"#),
        ("end", br#"{"id":"","signal":false}"#),
        ("idle", &fixture("start-v2.json")[..]),
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
        ("opencode", ""),
        ("opencode --auto", "--auto"),
        ("opencode --yolo", "--auto"),
        ("opencode --dangerously-skip-permissions", "--auto"),
        (
            "opencode --yolo --auto --dangerously-skip-permissions",
            "--auto",
        ),
        ("opencode --auto=true", "--auto"),
        ("opencode --auto=false", ""),
        ("opencode --auto false --standalone", "--standalone"),
        ("opencode --standalone", "--standalone"),
        (
            "opencode --server http://127.0.0.1:4096",
            "--server http://127.0.0.1:4096",
        ),
        ("opencode --server=http://h:1", "--server=http://h:1"),
        (
            "opencode -m anthropic/sonnet --agent build",
            "-m anthropic/sonnet --agent build",
        ),
        (
            "opencode --model=openai/gpt --agent=plan",
            "--model=openai/gpt --agent=plan",
        ),
        // never
        ("opencode -s ses_1 --session ses_2 -c --continue --fork", ""),
        ("opencode --prompt hello --model m", "--model m"),
        ("opencode /home/u/other --auto", "--auto"),
        // needs validation: not restored
        ("opencode --port 4096 --hostname 0.0.0.0 --mini", ""),
        ("opencode --mdns --mdns-domain x.local --cors http://a", ""),
        // a word starting with - is a flag, not a value (yargs and Effect CLI)
        ("opencode --prompt --auto", "--auto"),
        (
            "opencode --session --standalone --agent a",
            "--standalone --agent a",
        ),
        ("opencode --server --auto", "--auto"),
        // a value flag at the end has no value
        ("opencode --model", ""),
        // unknown flags are dropped
        ("opencode --print-logs --log-level DEBUG --auto", "--auto"),
    ];
    for (argv, want) in cases {
        assert_eq!(flags(argv, &cfg), args(want), "argv: {argv}");
    }
}

#[test]
fn auto_is_dropped_when_bypass_restore_is_off() {
    let cfg = Config {
        restore_bypass: false,
        ..Config::default()
    };
    assert_eq!(
        flags("opencode --yolo --standalone", &cfg),
        args("--standalone")
    );
    assert_eq!(flags("opencode --auto", &cfg), args(""));
}

// ---- resume_plan ----

fn record(cwd: &Path) -> Record {
    let mut rec = Record::new(
        AgentKind::OpenCode,
        V2_ID,
        cwd.to_path_buf(),
        PathBuf::from("/usr/local/bin/opencode"),
        "boot-1",
    );
    rec.flags = args("--yolo --standalone");
    rec.env.insert(
        "XDG_DATA_HOME".to_string(),
        "/home/u/.local/share".to_string(),
    );
    rec
}

fn supervised(rec: &Record) -> std::collections::BTreeMap<String, String> {
    let mut env = rec.env.clone();
    env.insert("HERDR_WAKE_SUPERVISED".to_string(), "1".to_string());
    env
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[test]
fn resume_v1_runs_opencode_with_the_session_id() {
    let (mut f, dir) = Fixture::with_temp_home();
    let rec = record(dir.path());
    assert_eq!(
        OpenCode.resume_plan(&rec, &mut f.ctx()),
        Plan::Run {
            argv: args("/usr/local/bin/opencode --auto --standalone --session")
                .into_iter()
                .chain([V2_ID.to_string()])
                .collect(),
            env: supervised(&rec),
            cwd: dir.path().to_path_buf(),
            before: vec![],
            confirm_within_ms: None,
        }
    );
}

#[test]
fn resume_v2_starts_the_service_first_and_asks_for_confirmation() {
    let (mut f, dir) = Fixture::with_temp_home();
    write(&dir.path().join(".config/opencode/cli.json"), "{}\n");
    let rec = record(dir.path());
    assert_eq!(
        OpenCode.resume_plan(&rec, &mut f.ctx()),
        Plan::Run {
            argv: args("/usr/local/bin/opencode --auto --standalone --session")
                .into_iter()
                .chain([V2_ID.to_string()])
                .collect(),
            env: supervised(&rec),
            cwd: dir.path().to_path_buf(),
            before: vec![args("/usr/local/bin/opencode service start")],
            confirm_within_ms: Some(15000),
        }
    );
}

#[test]
fn resume_finds_v2_through_the_records_opencode_config_dir() {
    let (mut f, dir) = Fixture::with_temp_home();
    let cfg_dir = dir.path().join("oc-config");
    write(&cfg_dir.join("cli.json"), "{}\n");
    let mut rec = record(dir.path());
    rec.env.insert(
        "OPENCODE_CONFIG_DIR".to_string(),
        cfg_dir.display().to_string(),
    );
    match OpenCode.resume_plan(&rec, &mut f.ctx()) {
        Plan::Run {
            confirm_within_ms, ..
        } => assert_eq!(confirm_within_ms, Some(15000)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn resume_finds_v2_through_xdg_config_home() {
    let (mut f, dir) = Fixture::with_temp_home();
    let xdg = dir.path().join("xdg");
    write(&xdg.join("opencode/cli.json"), "{}\n");
    f.env
        .insert("XDG_CONFIG_HOME".to_string(), xdg.display().to_string());
    let rec = record(dir.path());
    match OpenCode.resume_plan(&rec, &mut f.ctx()) {
        Plan::Run { before, .. } => {
            assert_eq!(before, vec![args("/usr/local/bin/opencode service start")])
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn resume_drops_auto_when_bypass_restore_is_now_off() {
    let (mut f, dir) = Fixture::with_temp_home();
    f.cfg.restore_bypass = false;
    let rec = record(dir.path());
    match OpenCode.resume_plan(&rec, &mut f.ctx()) {
        Plan::Run { argv, .. } => assert_eq!(
            argv,
            args("/usr/local/bin/opencode --standalone --session")
                .into_iter()
                .chain([V2_ID.to_string()])
                .collect::<Vec<_>>()
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn resume_with_missing_folder_is_permanent() {
    let (mut f, dir) = Fixture::with_temp_home();
    let rec = record(&dir.path().join("gone"));
    assert_eq!(
        OpenCode.resume_plan(&rec, &mut f.ctx()),
        Plan::Permanent("folder missing".to_string())
    );
}

// ---- integration ----

#[test]
fn integration_v1_writes_the_plugin_file_and_lists_it_in_tui_jsonc() {
    let (mut f, dir) = Fixture::with_temp_home();
    let bin = Path::new("/opt/hw/herdr-wake");
    let edits = OpenCode.integration(bin, &mut f.ctx());
    let cfg = dir.path().join(".config/opencode");
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].path, cfg.join("herdr-wake-tui.js"));
    assert_eq!(
        edits[0].kind,
        EditKind::WriteFile {
            body: opencode::render_asset(bin),
            mode: 0o644,
        }
    );
    assert_eq!(edits[1].path, cfg.join("tui.jsonc"));
    assert_eq!(
        edits[1].kind,
        EditKind::JsoncListEntry {
            key: "plugin".to_string(),
            value: "./herdr-wake-tui.js".to_string(),
        }
    );
}

#[test]
fn integration_v2_writes_the_plugin_folder_and_lists_it_in_cli_json() {
    let (mut f, dir) = Fixture::with_temp_home();
    let cfg = dir.path().join(".config/opencode");
    write(&cfg.join("cli.json"), "{\n  // mine\n}\n");
    let bin = Path::new("/opt/hw/herdr-wake");
    let edits = OpenCode.integration(bin, &mut f.ctx());
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].path, cfg.join("herdr-wake/tui.js"));
    assert_eq!(
        edits[0].kind,
        EditKind::WriteFile {
            body: opencode::render_asset(bin),
            mode: 0o644,
        }
    );
    assert_eq!(edits[1].path, cfg.join("cli.json"));
    assert_eq!(
        edits[1].kind,
        EditKind::JsoncListEntry {
            key: "plugins".to_string(),
            value: "./herdr-wake".to_string(),
        }
    );
}

#[test]
fn integration_uses_opencode_config_dir() {
    let (mut f, dir) = Fixture::with_temp_home();
    let cfg = dir.path().join("elsewhere");
    f.env
        .insert("OPENCODE_CONFIG_DIR".to_string(), cfg.display().to_string());
    let edits = OpenCode.integration(Path::new("/b"), &mut f.ctx());
    assert_eq!(edits[0].path, cfg.join("herdr-wake-tui.js"));
    assert_eq!(edits[1].path, cfg.join("tui.jsonc"));
}

#[test]
fn integration_ignores_a_relative_xdg_config_home() {
    let (mut f, dir) = Fixture::with_temp_home();
    f.env
        .insert("XDG_CONFIG_HOME".to_string(), "relative".to_string());
    let edits = OpenCode.integration(Path::new("/b"), &mut f.ctx());
    assert_eq!(
        edits[0].path,
        dir.path().join(".config/opencode/herdr-wake-tui.js")
    );
}

// ---- asset ----

#[test]
fn raw_asset_has_the_marker_on_its_first_line_and_the_placeholder() {
    let first = opencode::ASSET.lines().next().unwrap();
    assert!(first.contains("herdr-wake v"), "{first}");
    assert_eq!(opencode::ASSET.matches("\"__HERDR_WAKE_BIN__\"").count(), 1);
}

#[test]
fn rendered_asset_names_the_binary_and_the_version() {
    let body = opencode::render_asset(Path::new("/home/me/bin/herdr-wake"));
    let first = body.lines().next().unwrap();
    assert!(
        first.contains(&format!("herdr-wake v{}", env!("CARGO_PKG_VERSION"))),
        "{first}"
    );
    assert!(!body.contains("__HERDR_WAKE_BIN__"));
    assert!(!body.contains("__HERDR_WAKE_VERSION__"));
    assert!(body.contains(r#"const BIN = "/home/me/bin/herdr-wake";"#));
}

#[test]
fn asset_follows_the_contract() {
    let a = opencode::ASSET;
    assert!(!a.contains('\u{2014}'), "em dash in asset");
    assert!(a.contains("HC_TERMINAL_ID"));
    assert!(a.contains("setup("), "v2 entry point");
    assert!(a.contains("tui("), "v1 entry point");
    // v1 rejects a module that also has a server export
    assert!(!a.contains("server:") && !a.contains("server("));
    assert!(!a.contains("export const server") && !a.contains("export function server"));
    assert!(a.contains("500"));
    assert!(a.contains("unref"));
    assert!(a.contains("prependListener"));
    assert!(a.contains(r#"process.on("exit""#));
    assert!(a.contains("spawnSync"));
    assert!(a.contains("detached: true"));
    assert!(a.contains(r#"["hook", "opencode", "start"]"#));
    assert!(a.contains(r#"["hook", "opencode", "end"]"#));
}
