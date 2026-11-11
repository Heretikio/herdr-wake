//! Reports: `status` rows and table text, `doctor` findings in pass and
//! fail form, driven through `FakeCore`, `FakePlatform` and `FakeProbes`.

mod common;

use std::path::PathBuf;

use herdr_wake::core::{CoreError, Terminal, TerminalId};
use herdr_wake::platform::ProcInfo;
use herdr_wake::record::{AgentKind, ProcRef, Record, State, LABEL};
use herdr_wake::report::{self, StatusRow};

use common::{sample_record, Fixture};

fn put(f: &mut Fixture, t: &str, rec: &Record) {
    let mut term = Terminal::new(t);
    term.labels
        .insert(LABEL.to_string(), serde_json::to_string(rec).unwrap());
    f.core.insert(term);
}

fn rec(agent: AgentKind, id: &str, state: State) -> Record {
    let mut r = sample_record();
    r.agent = agent;
    r.session_id = id.to_string();
    r.state = state;
    r
}

// ---- status ----

#[test]
fn status_lists_terminals_with_a_record_and_marks_live_agents() {
    let mut f = Fixture::temp();
    // t1: running, agent alive on this boot.
    let mut r1 = rec(AgentKind::Claude, "s-1", State::Running);
    r1.agent_proc = Some(ProcRef { pid: 10, start: 5 });
    f.platform
        .add_process(ProcInfo::new(10, 1, 5, vec!["claude".into()]));
    put(&mut f, "t1", &r1);
    // t2: running record, but the agent is gone (waiting to resume).
    let mut r2 = rec(AgentKind::Codex, "s-2", State::Running);
    r2.agent_proc = Some(ProcRef { pid: 11, start: 5 });
    r2.attempts = 1;
    put(&mut f, "t2", &r2);
    // t3: ended.
    put(&mut f, "t3", &rec(AgentKind::Pi, "s-3", State::Ended));
    // t4: no record at all; t5: a label this build cannot read.
    f.core.insert(Terminal::new("t4"));
    let mut t5 = Terminal::new("t5");
    t5.labels.insert(LABEL.into(), "{not json".into());
    f.core.insert(t5);

    f.trust();
    let rows = report::status(&mut f.ctx()).unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.terminal.as_str()).collect();
    assert_eq!(ids, ["t1", "t2", "t3"]);
    assert!(rows[0].running);
    assert_eq!(rows[0].state, "running");
    assert_eq!(rows[0].agent, Some(AgentKind::Claude));
    assert!(!rows[1].running);
    assert_eq!(rows[1].attempts, 1);
    assert_eq!(rows[2].state, "ended");
    assert_eq!(rows[2].cwd, PathBuf::from("/home/u/proj"));
}

#[test]
fn status_does_not_count_an_agent_from_another_boot_as_running() {
    let mut f = Fixture::new();
    let mut r = rec(AgentKind::Claude, "s-1", State::Running);
    r.boot_id = "boot-0".into();
    r.agent_proc = Some(ProcRef { pid: 10, start: 5 });
    f.platform
        .add_process(ProcInfo::new(10, 1, 5, vec!["claude".into()]));
    put(&mut f, "t1", &r);
    let rows = report::status(&mut f.ctx()).unwrap();
    assert!(!rows[0].running);
}

#[test]
fn status_sorts_terminals_by_number() {
    let mut f = Fixture::new();
    for t in ["t10", "t2", "t1"] {
        put(&mut f, t, &rec(AgentKind::Claude, t, State::Ended));
    }
    let rows = report::status(&mut f.ctx()).unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.terminal.as_str()).collect();
    assert_eq!(ids, ["t1", "t2", "t10"]);
}

#[test]
fn status_passes_core_errors_through() {
    let mut f = Fixture::new();
    f.core.fail_with(Some(CoreError::NotRunning));
    assert_eq!(
        report::status(&mut f.ctx()).unwrap_err(),
        CoreError::NotRunning
    );
}

fn row(t: &str, agent: AgentKind, state: &str, running: bool, cwd: &str) -> StatusRow {
    StatusRow {
        terminal: TerminalId::from(t),
        agent: Some(agent),
        session_id: format!("{t}-session"),
        cwd: PathBuf::from(cwd),
        state: state.to_string(),
        attempts: 0,
        running,
    }
}

#[test]
fn render_status_prints_an_aligned_table_and_a_summary() {
    let rows = vec![
        row("t1", AgentKind::Claude, "running", true, "/w/a"),
        row("t12", AgentKind::OpenCode, "running", true, "/w/b"),
        row("t3", AgentKind::Pi, "ended", false, "/w/c"),
    ];
    let text = report::render_status(&rows, true);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "TERMINAL  AGENT     STATE    LIVE  TRIES  SESSION      CWD"
    );
    assert_eq!(
        lines[1],
        "t1        claude    running  yes   0      t1-session   /w/a"
    );
    assert_eq!(
        lines[2],
        "t12       opencode  running  yes   0      t12-session  /w/b"
    );
    assert_eq!(
        lines[3],
        "t3        pi        ended    no    0      t3-session   /w/c"
    );
    assert_eq!(lines.last().unwrap(), &"3 sessions: 2 running, 1 ended");
}

#[test]
fn render_status_counts_waiting_and_failed_sessions() {
    let rows = vec![
        row("t1", AgentKind::Claude, "running", false, "/w"),
        row(
            "t2",
            AgentKind::Codex,
            "failed: session not found",
            false,
            "/w",
        ),
    ];
    let text = report::render_status(&rows, true);
    assert!(text.ends_with("2 sessions: 1 waiting to resume, 1 failed\n"));
}

#[test]
fn render_status_with_no_rows_says_so() {
    assert_eq!(report::render_status(&[], true), "0 sessions\n");
}

#[test]
fn render_status_when_core_is_down_says_status_did_not_start_it() {
    let text = report::render_status(&[], false);
    assert!(text.contains("herdr-core is not running"), "{text}");
    assert!(text.contains("did not start it"), "{text}");
    assert!(!text.contains("TERMINAL"));
}

// ---- doctor ----

use herdr_wake::config::Config;
use herdr_wake::platform::BootState;
use herdr_wake::report::{FakeProbes, Finding, Level, Os};

const BIN: &str = "/opt/hw/herdr-wake";
const CORE_CONFIG: &str = "/home/u/.config/herdr-core/config.toml";
const BOOT_ENV: &str = "/home/u/.config/herdr-wake/boot.env";
const HOLDER_LOG: &str = "/home/u/.local/state/herdr-core/default/holder.log";
const AUTOLOGIN: [&str; 4] = [
    "defaults",
    "read",
    "/Library/Preferences/com.apple.loginwindow",
    "autoLoginUser",
];
const IS_ENABLED: [&str; 4] = ["systemctl", "--user", "is-enabled", "herdr-wake"];

/// A machine where every check passes: herdr-wake is core's shell, core
/// runs, the boot service is installed, no agent is installed.
fn healthy() -> (Fixture, FakeProbes) {
    let mut f = Fixture::new();
    f.cfg = Config {
        real_shell: Some(vec!["/bin/zsh".into()]),
        ..Config::default()
    };
    f.env.insert("USER".into(), "u".into());
    f.env.insert("SHELL".into(), "/bin/zsh".into());
    f.platform.set_core_pid(Some(100));
    f.platform.boot().set_state(BootState::AtLogin);
    let p = FakeProbes::new();
    p.file(CORE_CONFIG, &format!("shell = [\"{BIN}\", \"shell\"]\n"));
    p.executable(BIN);
    p.executable("/bin/zsh");
    p.file(
        BOOT_ENV,
        "PATH=/usr/local/bin:/usr/bin:/bin\nLANG=en_US.UTF-8\n",
    );
    p.command(&["fdesetup", "isactive"], 1, "false\n");
    p.command(&AUTOLOGIN, 1, "");
    p.file("/proc/1/comm", "systemd\n");
    p.file("/var/lib/systemd/linger/u", "");
    p.command(&IS_ENABLED, 0, "enabled\n");
    (f, p)
}

fn run(f: &mut Fixture, p: &FakeProbes, os: Os) -> Vec<Finding> {
    report::doctor_on(os, &mut f.ctx(), p)
}

fn find<'a>(all: &'a [Finding], check: &str) -> &'a Finding {
    all.iter()
        .find(|x| x.check == check)
        .unwrap_or_else(|| panic!("no finding {check:?} in {all:#?}"))
}

fn check(f: &mut Fixture, p: &FakeProbes, os: Os, name: &str) -> Finding {
    find(&run(f, p, os), name).clone()
}

#[test]
fn healthy_machine_passes_every_check_on_both_systems() {
    for os in [Os::MacOs, Os::Linux] {
        let (mut f, p) = healthy();
        let all = run(&mut f, &p, os);
        let bad: Vec<&Finding> = all.iter().filter(|x| x.level != Level::Ok).collect();
        assert!(bad.is_empty(), "{os:?}: {bad:#?}");
    }
}

#[test]
fn doctor_never_runs_hc() {
    let (mut f, p) = healthy();
    run(&mut f, &p, Os::MacOs);
    run(&mut f, &p, Os::Linux);
    for argv in p.commands_run() {
        let prog = argv[0].rsplit('/').next().unwrap();
        assert!(prog != "hc" && prog != "herdr-core", "{argv:?}");
    }
}

// core shell

#[test]
fn core_shell_pointing_at_a_missing_binary_fails_with_the_exact_fix() {
    let (mut f, p) = healthy();
    p.remove(BIN);
    let x = check(&mut f, &p, Os::MacOs, "core shell");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains(BIN), "{}", x.detail);
    assert!(x.detail.contains("kept terminal"), "{}", x.detail);
    let fix = x.fix.unwrap();
    assert!(fix.contains(CORE_CONFIG), "{fix}");
    assert!(fix.contains(r#"shell = ["/bin/zsh"]"#), "{fix}");
    assert!(fix.contains("60 s"), "{fix}");
}

#[test]
fn missing_shell_binary_without_a_saved_real_shell_says_to_delete_the_line() {
    let (mut f, p) = healthy();
    f.cfg.real_shell = None;
    p.remove(BIN);
    let x = check(&mut f, &p, Os::Linux, "core shell");
    assert_eq!(x.level, Level::Fail);
    assert!(x.fix.unwrap().contains("delete the `shell` line"));
}

#[test]
fn core_shell_set_to_herdr_wake_passes() {
    let (mut f, p) = healthy();
    let x = check(&mut f, &p, Os::MacOs, "core shell");
    assert_eq!(x.level, Level::Ok);
    assert!(x.detail.contains(BIN));
}

#[test]
fn missing_core_config_fails_and_points_at_install() {
    let (mut f, p) = healthy();
    p.remove(CORE_CONFIG);
    let x = check(&mut f, &p, Os::MacOs, "core shell");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains("not found"), "{}", x.detail);
    assert!(x.fix.unwrap().contains("herdr-wake install"));
}

#[test]
fn core_shell_that_is_not_herdr_wake_fails() {
    let (mut f, p) = healthy();
    p.file(CORE_CONFIG, "shell = [\"/bin/zsh\", \"-l\"]\n");
    let x = check(&mut f, &p, Os::MacOs, "core shell");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains("plain shells"), "{}", x.detail);
}

#[test]
fn core_shell_unset_fails() {
    let (mut f, p) = healthy();
    p.file(CORE_CONFIG, "# no shell here\n");
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "core shell").level,
        Level::Fail
    );
}

#[test]
fn core_shell_with_a_relative_path_fails() {
    let (mut f, p) = healthy();
    p.file(CORE_CONFIG, "shell = [\"herdr-wake\", \"shell\"]\n");
    let x = check(&mut f, &p, Os::MacOs, "core shell");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains("absolute"), "{}", x.detail);
}

#[test]
fn core_shell_that_is_not_executable_fails() {
    let (mut f, p) = healthy();
    p.remove(BIN);
    p.file(BIN, "not a program");
    let x = check(&mut f, &p, Os::MacOs, "core shell");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains("not executable"), "{}", x.detail);
}

#[test]
fn unparseable_core_config_fails() {
    let (mut f, p) = healthy();
    p.file(CORE_CONFIG, "shell = [\n");
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "core shell").level,
        Level::Fail
    );
}

// real shell

#[test]
fn saved_real_shell_that_is_gone_warns() {
    let (mut f, p) = healthy();
    p.remove("/bin/zsh");
    let x = check(&mut f, &p, Os::MacOs, "real shell");
    assert_eq!(x.level, Level::Warn);
    assert!(x.detail.contains("/bin/zsh"));
}

#[test]
fn no_saved_real_shell_uses_shell_from_the_environment() {
    let (mut f, p) = healthy();
    f.cfg.real_shell = None;
    let x = check(&mut f, &p, Os::MacOs, "real shell");
    assert_eq!(x.level, Level::Ok);
    assert!(x.detail.contains("/bin/zsh"), "{}", x.detail);
}

#[test]
fn real_shell_found_on_path_passes() {
    let (mut f, p) = healthy();
    f.cfg.real_shell = Some(vec!["fish".into()]);
    f.env.insert("PATH".into(), "/usr/bin:/opt/fish/bin".into());
    p.executable("/opt/fish/bin/fish");
    let x = check(&mut f, &p, Os::MacOs, "real shell");
    assert_eq!(x.level, Level::Ok, "{x:?}");
}

// core and its version

#[test]
fn core_not_running_warns_and_skips_the_core_checks() {
    let (mut f, p) = healthy();
    f.platform.set_core_pid(None);
    f.core.fail_with(Some(CoreError::NotRunning));
    let all = run(&mut f, &p, Os::MacOs);
    let core = find(&all, "core");
    assert_eq!(core.level, Level::Warn);
    assert!(core.detail.contains("not running"));
    assert!(find(&all, "core version").detail.contains("skipped"));
    assert!(find(&all, "dead terminals").detail.contains("skipped"));
}

#[test]
fn core_running_reports_its_pid() {
    let (mut f, p) = healthy();
    let x = check(&mut f, &p, Os::MacOs, "core");
    assert_eq!(x.level, Level::Ok);
    assert!(x.detail.contains("100"));
}

#[test]
fn core_that_does_not_answer_fails() {
    let (mut f, p) = healthy();
    f.core.fail_with(Some(CoreError::Timeout));
    assert_eq!(check(&mut f, &p, Os::MacOs, "core").level, Level::Fail);
}

#[test]
fn core_process_without_a_reachable_socket_warns() {
    let (mut f, p) = healthy();
    f.core.fail_with(Some(CoreError::NotRunning));
    let x = check(&mut f, &p, Os::MacOs, "core");
    assert_eq!(x.level, Level::Warn);
    assert!(x.detail.contains("socket"), "{}", x.detail);
}

#[test]
fn core_older_than_0_1_16_fails() {
    let (mut f, p) = healthy();
    f.core.hello.version = "0.1.15".into();
    let x = check(&mut f, &p, Os::MacOs, "core version");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains("0.1.16"));
}

#[test]
fn core_at_exactly_0_1_16_passes() {
    let (mut f, p) = healthy();
    f.core.hello.version = "0.1.16".into();
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "core version").level,
        Level::Ok
    );
    f.core.hello.version = "0.2.0-beta.1".into();
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "core version").level,
        Level::Ok
    );
}

// dead terminals (a broken shell is invisible in the API)

#[test]
fn kept_live_terminal_without_a_process_fails_and_quotes_holder_log() {
    let (mut f, p) = healthy();
    let mut t = Terminal::new("t1");
    t.pid = None;
    f.core.insert(t);
    let mut ok = Terminal::new("t2");
    ok.pid = Some(4000);
    f.core.insert(ok);
    p.file(
        HOLDER_LOG,
        "holder: started\nholder: t1: cannot start \"/nonexistent/herdr-wake\": No such file or directory\n\n",
    );
    let x = check(&mut f, &p, Os::MacOs, "dead terminals");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains("t1"), "{}", x.detail);
    assert!(!x.detail.contains("t2"), "{}", x.detail);
    assert!(
        x.detail
            .contains("holder: t1: cannot start \"/nonexistent/herdr-wake\""),
        "{}",
        x.detail
    );
    assert!(x.fix.unwrap().contains("60 s"));
    // It watched for more than 5 s before deciding.
    assert!(p.slept_ms() > 5000, "{}", p.slept_ms());
}

#[test]
fn unkept_or_running_terminals_are_not_dead() {
    let (mut f, p) = healthy();
    let mut unkept = Terminal::new("t1");
    unkept.keep = false;
    f.core.insert(unkept);
    let mut running = Terminal::new("t2");
    running.pid = Some(42);
    f.core.insert(running);
    let x = check(&mut f, &p, Os::MacOs, "dead terminals");
    assert_eq!(x.level, Level::Ok);
    assert_eq!(p.slept_ms(), 0);
}

#[test]
fn dead_terminal_without_holder_log_still_fails() {
    let (mut f, p) = healthy();
    f.core.insert(Terminal::new("t3"));
    let x = check(&mut f, &p, Os::Linux, "dead terminals");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains(HOLDER_LOG), "{}", x.detail);
}

// boot service

#[test]
fn boot_service_states_map_to_levels() {
    for (state, level) in [
        (BootState::NotInstalled, Level::Warn),
        (BootState::Disabled, Level::Warn),
        (BootState::AtLogin, Level::Ok),
        (BootState::BeforeLogin, Level::Ok),
    ] {
        let (mut f, p) = healthy();
        f.platform.boot().set_state(state);
        let x = check(&mut f, &p, Os::Linux, "boot service");
        assert_eq!(x.level, level, "{state:?}");
        if level == Level::Warn {
            assert!(x.fix.is_some());
        }
    }
}

// boot.env

#[test]
fn missing_boot_env_warns() {
    let (mut f, p) = healthy();
    p.remove(BOOT_ENV);
    let x = check(&mut f, &p, Os::MacOs, "boot env");
    assert_eq!(x.level, Level::Warn);
    assert!(x.detail.contains(BOOT_ENV));
}

#[test]
fn boot_env_path_missing_an_agent_folder_warns() {
    let (mut f, p) = healthy();
    let mut r = sample_record();
    r.bin = PathBuf::from("/home/u/.local/bin/claude");
    put(&mut f, "t1", &r);
    let x = check(&mut f, &p, Os::MacOs, "boot env");
    assert_eq!(x.level, Level::Warn);
    assert!(x.detail.contains("/home/u/.local/bin"), "{}", x.detail);
}

#[test]
fn boot_env_path_with_every_agent_folder_passes() {
    let (mut f, p) = healthy();
    put(&mut f, "t1", &sample_record()); // bin /usr/local/bin/claude
    p.file(BOOT_ENV, "# captured\nPATH=\"/usr/local/bin:/usr/bin\"\n");
    assert_eq!(check(&mut f, &p, Os::MacOs, "boot env").level, Level::Ok);
}

// macOS

#[test]
fn macos_filevault_on_is_reported() {
    let (mut f, p) = healthy();
    p.command(&["fdesetup", "isactive"], 0, "true\n");
    let x = check(&mut f, &p, Os::MacOs, "filevault");
    assert_eq!(x.level, Level::Ok);
    assert!(x.detail.contains("on"));
}

#[test]
fn macos_filevault_unknown_warns() {
    let (mut f, p) = healthy();
    p.forget_command(&["fdesetup", "isactive"]);
    assert_eq!(check(&mut f, &p, Os::MacOs, "filevault").level, Level::Warn);
}

#[test]
fn macos_wake_time_at_login_without_auto_login() {
    let (mut f, p) = healthy();
    let all = run(&mut f, &p, Os::MacOs);
    assert!(find(&all, "auto-login").detail.contains("off"));
    let x = find(&all, "wake time");
    assert_eq!(x.level, Level::Ok);
    assert!(x.detail.contains("at login"), "{}", x.detail);
}

#[test]
fn macos_wake_time_at_boot_via_auto_login() {
    let (mut f, p) = healthy();
    p.command(&AUTOLOGIN, 0, "u\n");
    let all = run(&mut f, &p, Os::MacOs);
    assert!(find(&all, "auto-login").detail.contains("on"));
    assert!(find(&all, "wake time")
        .detail
        .contains("at boot via auto-login"));
}

#[test]
fn macos_before_login_warns_about_the_keychain() {
    let (mut f, p) = healthy();
    f.platform.boot().set_state(BootState::BeforeLogin);
    p.command(&["fdesetup", "isactive"], 0, "true\n");
    let x = check(&mut f, &p, Os::MacOs, "wake time");
    assert_eq!(x.level, Level::Warn);
    assert!(x.detail.contains("before login (daemon)"), "{}", x.detail);
    assert!(x.detail.contains("Keychain"), "{}", x.detail);
    assert!(x.detail.contains("unlocked"), "{}", x.detail);
}

#[test]
fn macos_without_boot_service_agents_wait_for_core() {
    let (mut f, p) = healthy();
    f.platform.boot().set_state(BootState::NotInstalled);
    assert_eq!(check(&mut f, &p, Os::MacOs, "wake time").level, Level::Warn);
}

#[test]
fn macos_checks_do_not_appear_on_linux_and_back() {
    let (mut f, p) = healthy();
    let linux = run(&mut f, &p, Os::Linux);
    assert!(linux.iter().all(|x| x.check != "filevault"));
    let mac = run(&mut f, &p, Os::MacOs);
    assert!(mac.iter().all(|x| x.check != "linger"));
}

// Linux

#[test]
fn linux_without_linger_warns_and_wakes_at_login() {
    let (mut f, p) = healthy();
    p.remove("/var/lib/systemd/linger/u");
    let all = run(&mut f, &p, Os::Linux);
    let x = find(&all, "linger");
    assert_eq!(x.level, Level::Warn);
    assert!(x.fix.as_deref().unwrap().contains("loginctl enable-linger"));
    assert!(find(&all, "wake time").detail.contains("at login"));
}

#[test]
fn linux_with_linger_wakes_at_boot() {
    let (mut f, p) = healthy();
    let all = run(&mut f, &p, Os::Linux);
    assert_eq!(find(&all, "linger").level, Level::Ok);
    assert!(find(&all, "wake time").detail.contains("at boot"));
}

#[test]
fn linux_unit_disabled_warns() {
    let (mut f, p) = healthy();
    p.command(&IS_ENABLED, 1, "disabled\n");
    let x = check(&mut f, &p, Os::Linux, "systemd unit");
    assert_eq!(x.level, Level::Warn);
    assert!(x.fix.unwrap().contains("systemctl --user enable"));
}

#[test]
fn linux_pid_1_not_systemd_fails() {
    let (mut f, p) = healthy();
    p.file("/proc/1/comm", "init\n");
    let x = check(&mut f, &p, Os::Linux, "init");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains("init"));
}

#[test]
fn linux_wsl_gets_a_note() {
    let (mut f, p) = healthy();
    assert!(run(&mut f, &p, Os::Linux).iter().all(|x| x.check != "wsl"));
    f.env.insert("WSL_DISTRO_NAME".into(), "Ubuntu".into());
    let x = check(&mut f, &p, Os::Linux, "wsl");
    assert_eq!(x.level, Level::Warn);
    assert!(x.detail.contains("Windows"));
}

// Claude

const CLAUDE_SETTINGS: &str = "/home/u/.claude/settings.json";
const CLAUDE_OURS: &str = r#"{
  // user comment
  "model": "opus",
  "hooks": {
    "SessionStart": [
      {"matcher": "^(startup|resume)$", "hooks": [{"type": "command", "command": "echo mine"}]},
      {"hooks": [{"type": "command", "command": "'/opt/hw/herdr-wake' hook claude start", "timeout": 10}]},
    ],
    "SessionEnd": [{"hooks": [{"type": "command", "command": "'/opt/hw/herdr-wake' hook claude end", "timeout": 5}]}]
  }
}"#;

#[test]
fn claude_not_installed_is_ok() {
    let (mut f, p) = healthy();
    let all = run(&mut f, &p, Os::MacOs);
    let x = find(&all, "claude hook");
    assert_eq!(x.level, Level::Ok);
    assert!(x.detail.contains("not installed"));
    assert!(all.iter().all(|x| x.check != "claude hooks allowed"));
}

#[test]
fn claude_hook_present_passes_even_with_comments_and_trailing_commas() {
    let (mut f, p) = healthy();
    p.file(CLAUDE_SETTINGS, CLAUDE_OURS);
    let all = run(&mut f, &p, Os::MacOs);
    assert_eq!(find(&all, "claude hook").level, Level::Ok);
    assert_eq!(find(&all, "claude hooks allowed").level, Level::Ok);
}

#[test]
fn claude_installed_without_our_hook_fails() {
    let (mut f, p) = healthy();
    p.dir("/home/u/.claude");
    let x = check(&mut f, &p, Os::MacOs, "claude hook");
    assert_eq!(x.level, Level::Fail);
    assert!(x.fix.unwrap().contains("herdr-wake install"));
}

#[test]
fn claude_without_the_end_hook_warns() {
    let (mut f, p) = healthy();
    let start_only = CLAUDE_OURS.replace("hook claude end", "something else");
    p.file(CLAUDE_SETTINGS, &start_only);
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "claude hook").level,
        Level::Warn
    );
}

#[test]
fn claude_config_dir_from_env_is_used() {
    let (mut f, p) = healthy();
    f.env
        .insert("CLAUDE_CONFIG_DIR".into(), "/work/claude-cfg".into());
    p.file("/work/claude-cfg/settings.json", CLAUDE_OURS);
    assert_eq!(check(&mut f, &p, Os::MacOs, "claude hook").level, Level::Ok);
}

#[test]
fn claude_disable_all_hooks_fails() {
    let (mut f, p) = healthy();
    p.file(
        CLAUDE_SETTINGS,
        &CLAUDE_OURS.replace("\"model\": \"opus\",", "\"disableAllHooks\": true,"),
    );
    let x = check(&mut f, &p, Os::MacOs, "claude hooks allowed");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains("disableAllHooks"));
}

#[test]
fn claude_managed_hooks_only_fails() {
    for (os, managed) in [
        (
            Os::MacOs,
            "/Library/Application Support/ClaudeCode/managed-settings.json",
        ),
        (Os::Linux, "/etc/claude-code/managed-settings.json"),
    ] {
        let (mut f, p) = healthy();
        p.file(CLAUDE_SETTINGS, CLAUDE_OURS);
        p.file(managed, r#"{"allowManagedHooksOnly": true}"#);
        let x = check(&mut f, &p, os, "claude hooks allowed");
        assert_eq!(x.level, Level::Fail, "{os:?}");
        assert!(x.detail.contains("allowManagedHooksOnly"));
    }
}

// Codex

const CODEX_HOOKS: &str = "/home/u/.codex/hooks.json";

#[test]
fn codex_hook_present_passes_and_reminds_about_trust() {
    let (mut f, p) = healthy();
    p.file(
        CODEX_HOOKS,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"'/opt/hw/herdr-wake' hook codex start","timeout":10}]}]}}"#,
    );
    let all = run(&mut f, &p, Os::MacOs);
    assert_eq!(find(&all, "codex hook").level, Level::Ok);
    let trust = find(&all, "codex trust");
    assert_eq!(trust.level, Level::Warn);
    assert!(trust.detail.contains("/hooks"));
    assert!(trust.detail.contains("cannot"), "{}", trust.detail);
}

#[test]
fn codex_hooks_switched_off_fails() {
    let (mut f, p) = healthy();
    p.file(
        CODEX_HOOKS,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"'/opt/hw/herdr-wake' hook codex start"}]}]}}"#,
    );
    p.file("/home/u/.codex/config.toml", "[features]\nhooks = false\n");
    let x = check(&mut f, &p, Os::MacOs, "codex hook");
    assert_eq!(x.level, Level::Fail);
    assert!(x.detail.contains("hooks = false"), "{}", x.detail);
}

#[test]
fn codex_installed_without_our_hook_fails() {
    let (mut f, p) = healthy();
    p.file("/home/u/.codex/config.toml", "model = \"gpt\"\n");
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "codex hook").level,
        Level::Fail
    );
}

#[test]
fn codex_not_installed_is_ok_and_has_no_trust_warning() {
    let (mut f, p) = healthy();
    let all = run(&mut f, &p, Os::MacOs);
    assert!(find(&all, "codex hook").detail.contains("not installed"));
    assert!(all.iter().all(|x| x.check != "codex trust"));
}

// Pi and OpenCode

#[test]
fn pi_extension_present_or_missing() {
    let (mut f, p) = healthy();
    assert!(check(&mut f, &p, Os::MacOs, "pi extension")
        .detail
        .contains("not installed"));
    p.dir("/home/u/.pi/agent");
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "pi extension").level,
        Level::Fail
    );
    p.file(
        "/home/u/.pi/agent/extensions/herdr-wake.ts",
        "// herdr-wake v1",
    );
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "pi extension").level,
        Level::Ok
    );
}

#[test]
fn opencode_plugin_entry_v2_v1_or_missing() {
    let (mut f, p) = healthy();
    assert!(check(&mut f, &p, Os::MacOs, "opencode plugin")
        .detail
        .contains("not installed"));
    p.file("/home/u/.config/opencode/cli.json", r#"{"plugins": []}"#);
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "opencode plugin").level,
        Level::Fail
    );
    p.file(
        "/home/u/.config/opencode/cli.json",
        "{\n  // mine\n  \"plugins\": [\"./other\", \"./herdr-wake\",],\n}",
    );
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "opencode plugin").level,
        Level::Ok
    );
    let (mut f, p) = healthy();
    p.file(
        "/home/u/.config/opencode/tui.json",
        r#"{"plugin": ["/home/u/.config/opencode/herdr-wake-tui.js"]}"#,
    );
    assert_eq!(
        check(&mut f, &p, Os::MacOs, "opencode plugin").level,
        Level::Ok
    );
}

// rendering

#[test]
fn render_doctor_shows_levels_fixes_and_a_summary() {
    let all = vec![
        Finding {
            level: Level::Ok,
            check: "core",
            detail: "running".into(),
            fix: None,
        },
        Finding {
            level: Level::Warn,
            check: "linger",
            detail: "off".into(),
            fix: Some("loginctl enable-linger".into()),
        },
        Finding {
            level: Level::Fail,
            check: "core shell",
            detail: "missing".into(),
            fix: Some("put it back".into()),
        },
    ];
    let text = report::render_doctor(&all);
    assert_eq!(
        text,
        "ok    core: running\n\
         warn  linger: off\n\
         \x20     fix: loginctl enable-linger\n\
         FAIL  core shell: missing\n\
         \x20     fix: put it back\n\
         1 failed, 1 warning\n"
    );
    let ok = report::render_doctor(&all[..1]);
    assert!(ok.ends_with("all checks passed\n"));
}

// ---- the real probes, on temporary files and harmless commands ----

use herdr_wake::report::{Probes, SystemProbes};
use std::time::{Duration, Instant};

#[test]
fn system_probes_run_capture_stdout_and_exit_code() {
    let p = SystemProbes;
    let out = p
        .run(
            &["/bin/sh", "-c", "echo hi; exit 3"],
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(out.stdout, "hi\n");
    assert_eq!(out.code, Some(3));
    assert!(p
        .run(&["/nonexistent/prog"], Duration::from_secs(1))
        .is_none());
}

#[test]
fn system_probes_run_gives_up_at_the_timeout() {
    let start = Instant::now();
    let out = SystemProbes.run(&["/bin/sleep", "5"], Duration::from_millis(200));
    assert!(out.is_none());
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn system_probes_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    let prog = dir.path().join("prog");
    std::fs::write(&plain, "text").unwrap();
    std::fs::write(&prog, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = SystemProbes;
    assert_eq!(p.read(&plain).as_deref(), Some("text"));
    assert!(p.exists(dir.path()) && p.exists(&plain));
    assert!(!p.exists(&dir.path().join("missing")));
    assert!(p.executable(&prog));
    assert!(!p.executable(&plain));
    assert!(!p.executable(dir.path()));
}

// ---- untrusted records (M-5 e) ----

#[test]
fn status_shows_a_record_without_a_matching_local_copy_as_untrusted() {
    let mut f = Fixture::temp();
    put(&mut f, "t1", &rec(AgentKind::Claude, "s-1", State::Running));
    f.trust();
    // t2's label was written by something else; t3 was changed afterwards.
    put(&mut f, "t2", &rec(AgentKind::Codex, "s-2", State::Running));
    let mut r3 = rec(AgentKind::Pi, "s-3", State::Running);
    put(&mut f, "t3", &r3);
    herdr_wake::record::write_copy(
        &herdr_wake::paths::records_dir(&f.env, &f.home),
        &TerminalId::from("t3"),
        0,
        &r3,
    )
    .unwrap();
    r3.flags.push("--approve".into());
    put(&mut f, "t3", &r3);
    // An ended record needs no copy.
    put(&mut f, "t4", &rec(AgentKind::Claude, "s-4", State::Ended));

    let rows = report::status(&mut f.ctx()).unwrap();
    let states: Vec<&str> = rows.iter().map(|r| r.state.as_str()).collect();
    assert_eq!(states, ["running", "untrusted", "untrusted", "ended"]);

    let text = report::render_status(&rows, true);
    assert!(text.contains("untrusted"), "{text}");
    assert!(
        text.contains("4 sessions: 1 waiting to resume, 2 untrusted, 1 ended"),
        "{text}"
    );
}

// ---- config.toml that cannot be parsed ----

const HW_CONFIG: &str = "/home/u/.config/herdr-wake/config.toml";

#[test]
fn a_config_that_cannot_be_parsed_is_a_failed_check_with_the_parse_error() {
    for os in [Os::MacOs, Os::Linux] {
        let (mut f, p) = healthy();
        p.file(HW_CONFIG, "restore_bypass = fales\n");
        let x = check(&mut f, &p, os, "config");
        assert_eq!(x.level, Level::Fail);
        assert!(x.detail.contains(HW_CONFIG), "{}", x.detail);
        assert!(x.detail.contains("nothing is resumed"), "{}", x.detail);
        // The parser's own words, so the user can find the line.
        assert!(
            x.detail.contains("line 1") || x.detail.contains("fales"),
            "{}",
            x.detail
        );
        assert!(x.fix.unwrap().contains(HW_CONFIG));
    }
}

#[test]
fn a_good_or_missing_config_passes_the_check() {
    let (mut f, p) = healthy();
    assert_eq!(check(&mut f, &p, Os::Linux, "config").level, Level::Ok);
    p.file(HW_CONFIG, "restore_bypass = false\nauto_continue = true\n");
    let x = check(&mut f, &p, Os::Linux, "config");
    assert_eq!(x.level, Level::Ok, "{}", x.detail);
    // A key of the wrong type is a parse failure too.
    p.file(HW_CONFIG, "attempts = \"three\"\n");
    assert_eq!(check(&mut f, &p, Os::Linux, "config").level, Level::Fail);
}

// ---- per-terminal opt-out: herdr-wake off <terminal> ----

#[test]
fn status_shows_switched_off_terminals_as_off() {
    let mut f = Fixture::temp();
    put(&mut f, "t1", &rec(AgentKind::Claude, "s-1", State::Running));
    f.trust();
    // t2: switched off, no record (off forgets it). t3: a plain terminal.
    f.core.insert(Terminal::new("t2"));
    f.core.insert(Terminal::new("t3"));
    herdr_wake::record::set_off(&mut f.core, &TerminalId::from("t2"), true).unwrap();

    let rows = report::status(&mut f.ctx()).unwrap();
    let got: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r.terminal.as_str(), r.state.as_str()))
        .collect();
    assert_eq!(got, [("t1", "running"), ("t2", "off")]);
    assert_eq!(rows[1].agent, None);

    let text = report::render_status(&rows, true);
    let line = text.lines().find(|l| l.starts_with("t2")).unwrap();
    assert!(line.contains("off"), "{text}");
    assert!(text.contains("1 off"), "{text}");
}

// ---- agent versions: warn when older than what herdr-wake is written for ----

fn with_program(p: &FakeProbes, f: &mut Fixture, name: &str, version_output: Option<&str>) {
    let path = format!("/opt/agents/{name}");
    p.executable(&path);
    f.env.insert("PATH".into(), "/opt/agents:/usr/bin".into());
    if let Some(out) = version_output {
        p.command(&[&path, "--version"], 0, out);
    }
}

fn maybe<'a>(all: &'a [Finding], check: &str) -> Option<&'a Finding> {
    all.iter().find(|x| x.check == check)
}

#[test]
fn an_old_pi_is_a_warning_not_a_failure() {
    let (mut f, p) = healthy();
    with_program(&p, &mut f, "pi", Some("0.79.3\n"));
    let all = run(&mut f, &p, Os::Linux);
    let x = find(&all, "pi version");
    assert_eq!(x.level, Level::Warn);
    assert!(x.detail.contains("0.79.3"), "{}", x.detail);
    assert!(x.detail.contains("0.79.4"), "{}", x.detail);
    assert!(x.fix.is_some());
    // The 3 s limit is what the probe was asked for.
    assert!(p
        .commands_run()
        .contains(&vec!["/opt/agents/pi".to_string(), "--version".to_string()]));
}

#[test]
fn a_current_pi_passes_and_an_absent_or_silent_one_says_nothing() {
    for out in ["0.79.4\n", "pi 0.80.1\n", "v1.5.2"] {
        let (mut f, p) = healthy();
        with_program(&p, &mut f, "pi", Some(out));
        let all = run(&mut f, &p, Os::MacOs);
        assert_eq!(find(&all, "pi version").level, Level::Ok, "{out}");
    }
    // Not installed: no line at all.
    let (mut f, p) = healthy();
    assert!(maybe(&run(&mut f, &p, Os::MacOs), "pi version").is_none());
    assert!(maybe(&run(&mut f, &p, Os::MacOs), "opencode version").is_none());
    // Installed, but the version cannot be read: no line either.
    let (mut f, p) = healthy();
    with_program(&p, &mut f, "pi", None);
    assert!(maybe(&run(&mut f, &p, Os::MacOs), "pi version").is_none());
    let (mut f, p) = healthy();
    with_program(&p, &mut f, "pi", Some("no version here\n"));
    assert!(maybe(&run(&mut f, &p, Os::MacOs), "pi version").is_none());
}

#[test]
fn an_old_opencode_v2_is_a_warning_and_v1_has_no_floor() {
    let (mut f, p) = healthy();
    with_program(&p, &mut f, "opencode", Some("2.0.27\n"));
    let all = run(&mut f, &p, Os::Linux);
    let x = find(&all, "opencode version");
    assert_eq!(x.level, Level::Warn);
    assert!(
        x.detail.contains("2.0.27") && x.detail.contains("2.0.28"),
        "{}",
        x.detail
    );

    for out in [
        "2.0.28\n",
        "opencode 2.2.7\n",
        "3.0.0",
        "1.4.6\n",
        "0.9.0\n",
    ] {
        let (mut f, p) = healthy();
        with_program(&p, &mut f, "opencode", Some(out));
        let all = run(&mut f, &p, Os::Linux);
        assert_eq!(find(&all, "opencode version").level, Level::Ok, "{out}");
    }
}

#[test]
fn version_warnings_do_not_make_doctor_fail() {
    let (mut f, p) = healthy();
    with_program(&p, &mut f, "pi", Some("0.70.0\n"));
    p.executable("/opt/agents/opencode");
    p.command(&["/opt/agents/opencode", "--version"], 0, "2.0.1\n");
    let all = run(&mut f, &p, Os::Linux);
    assert!(all.iter().all(|x| x.level != Level::Fail), "{all:#?}");
    assert_eq!(
        all.iter().filter(|x| x.level == Level::Warn).count(),
        2,
        "{all:#?}"
    );
}
