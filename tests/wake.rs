//! The wake engine: `decide` over fakes, and the whole `herdr-wake shell`
//! flow through `shell_main_with` (fake platform, shared fake core, scripted
//! child exits, injected sleeper) and `shell_main` (real socket client).

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use common::{agent_bin, sample_record, Fixture};
use herdr_wake::agents::{Agent, Ctx, Edit, Observation, Plan};
use herdr_wake::config::Config;
use herdr_wake::core::fake::FakeCore;
use herdr_wake::core::{Core, CoreError, Hello, Terminal, TerminalId};
use herdr_wake::platform::fake::FakePlatform;
use herdr_wake::platform::{ExitInfo, Platform, ProcInfo};
use herdr_wake::record::{self, AgentKind, ProcRef, Record, State};
use herdr_wake::wake::{decide, shell_main, shell_main_with, shell_quote, Decision, Seams};

// ---------------------------------------------------------------- helpers

/// An agent whose resume plan is fixed by the test. A `Run` plan's cwd is
/// replaced by the record's cwd.
struct StubAgent {
    plan: Plan,
    panic: bool,
}

impl StubAgent {
    fn run(argv: &[&str]) -> StubAgent {
        StubAgent {
            plan: Plan::Run {
                argv: argv.iter().map(|s| s.to_string()).collect(),
                env: BTreeMap::new(),
                cwd: PathBuf::new(),
                before: Vec::new(),
                confirm_within_ms: None,
            },
            panic: false,
        }
    }

    fn permanent(reason: &str) -> StubAgent {
        StubAgent {
            plan: Plan::Permanent(reason.to_string()),
            panic: false,
        }
    }
}

impl Agent for StubAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }
    fn observe(&self, _: &str, _: &[u8], _: &mut Ctx) -> Observation {
        Observation::Ignore("stub")
    }
    /// Keeps every flag: the policy is the real adapters' business.
    fn restorable_flags(&self, argv: &[String], _: &Config) -> Vec<String> {
        argv.iter().skip(1).cloned().collect()
    }
    fn resume_plan(&self, rec: &Record, _: &mut Ctx) -> Plan {
        assert!(!self.panic, "stub agent panic");
        match &self.plan {
            Plan::Run {
                argv,
                env,
                before,
                confirm_within_ms,
                ..
            } => Plan::Run {
                argv: argv.clone(),
                env: env.clone(),
                cwd: rec.cwd.clone(),
                before: before.clone(),
                confirm_within_ms: *confirm_within_ms,
            },
            p => p.clone(),
        }
    }
    fn integration(&self, _: &Path, _: &mut Ctx) -> Vec<Edit> {
        Vec::new()
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A record whose agent and shell are long gone (an earlier boot), with a
/// program that really exists.
fn recovered_record(cwd: &Path) -> Record {
    let mut r = sample_record();
    r.bin = agent_bin(AgentKind::Claude);
    r.cwd = cwd.to_path_buf();
    r.boot_id = "boot-0".to_string();
    r
}

// ---------------------------------------------------------------- decide

fn decide_with(f: &mut Fixture, rec: Option<&Record>, agent: &dyn Agent) -> Decision {
    decide(rec, Some(agent), &mut f.ctx())
}

#[test]
fn no_record_is_a_shell() {
    let mut f = Fixture::new();
    let a = StubAgent::run(&["claude"]);
    assert_eq!(decide_with(&mut f, None, &a), Decision::Shell);
}

#[test]
fn ended_and_failed_records_are_a_shell() {
    let mut f = Fixture::new();
    let a = StubAgent::run(&["claude"]);
    let mut r = recovered_record(Path::new("/home/u/proj"));
    r.state = State::Ended;
    assert_eq!(decide_with(&mut f, Some(&r), &a), Decision::Shell);
    r.state = State::Failed { reason: "x".into() };
    assert_eq!(decide_with(&mut f, Some(&r), &a), Decision::Shell);
}

#[test]
fn disabled_is_a_shell() {
    let mut f = Fixture::new();
    f.cfg.enabled = false;
    let a = StubAgent::run(&["claude"]);
    let r = recovered_record(Path::new("/home/u/proj"));
    assert_eq!(decide_with(&mut f, Some(&r), &a), Decision::Shell);
}

#[test]
fn excluded_folders_and_their_children_are_a_shell() {
    let mut f = Fixture::new();
    f.cfg.exclude_dirs = vec![PathBuf::from("/home/u/secret")];
    let a = StubAgent::run(&["claude"]);
    let r = recovered_record(Path::new("/home/u/secret/deep/er"));
    assert_eq!(decide_with(&mut f, Some(&r), &a), Decision::Shell);
    let r = recovered_record(Path::new("/home/u/secret"));
    assert_eq!(decide_with(&mut f, Some(&r), &a), Decision::Shell);
    // A sibling whose name only starts the same is not excluded.
    let r = recovered_record(Path::new("/home/u/secrets"));
    assert!(matches!(
        decide_with(&mut f, Some(&r), &a),
        Decision::Resume(_)
    ));
}

#[test]
fn excluded_folder_with_tilde_means_home() {
    let mut f = Fixture::new();
    f.cfg.exclude_dirs = vec![PathBuf::from("~/scratch")];
    let a = StubAgent::run(&["claude"]);
    let r = recovered_record(Path::new("/home/u/scratch/x"));
    assert_eq!(decide_with(&mut f, Some(&r), &a), Decision::Shell);
}

#[test]
fn same_boot_with_the_agent_still_alive_is_a_shell() {
    let mut f = Fixture::new();
    let a = StubAgent::run(&["claude"]);
    let r = sample_record(); // boot-1, agent 4242
    let p = r.agent_proc.unwrap();
    f.platform
        .add_process(ProcInfo::new(p.pid, 1, p.start, strings(&["claude"])));
    assert_eq!(decide_with(&mut f, Some(&r), &a), Decision::Shell);
}

#[test]
fn same_boot_with_the_agent_gone_resumes() {
    let mut f = Fixture::new();
    let a = StubAgent::run(&["claude"]);
    let mut r = sample_record(); // boot-1, agent not in the process table
    r.bin = agent_bin(AgentKind::Claude);
    assert!(matches!(
        decide_with(&mut f, Some(&r), &a),
        Decision::Resume(_)
    ));
}

#[test]
fn a_running_record_resumes_with_the_agents_plan() {
    let mut f = Fixture::new();
    let a = StubAgent::run(&["/usr/local/bin/claude", "--resume", "S1"]);
    let r = recovered_record(Path::new("/home/u/proj"));
    match decide_with(&mut f, Some(&r), &a) {
        Decision::Resume(Plan::Run { argv, cwd, .. }) => {
            assert_eq!(argv, strings(&["/usr/local/bin/claude", "--resume", "S1"]));
            assert_eq!(cwd, PathBuf::from("/home/u/proj"));
        }
        d => panic!("expected Resume, got {d:?}"),
    }
}

#[test]
fn a_permanent_plan_gives_up_with_the_quoted_manual_command() {
    // Review Focus 3: a folder or session that no longer exists.
    let mut f = Fixture::new();
    let a = StubAgent::permanent("session not found");
    let mut r = recovered_record(Path::new("/home/u/my proj"));
    r.flags = strings(&["--model", "opus"]);
    r.session_id = "abc-123".into();
    r.bin = PathBuf::from("/usr/local/bin/claude");
    match decide_with(&mut f, Some(&r), &a) {
        Decision::GiveUp { reason, manual } => {
            assert_eq!(reason, "session not found");
            assert_eq!(
                manual.as_deref(),
                Some("cd '/home/u/my proj' && /usr/local/bin/claude --model opus --resume abc-123")
            );
        }
        d => panic!("expected GiveUp, got {d:?}"),
    }
}

#[test]
fn manual_hint_per_agent_kind() {
    let a = StubAgent::permanent("gone");
    let cases = [
        (AgentKind::Codex, "cd /p && codex resume --yolo S"),
        (AgentKind::Pi, "cd /p && pi --yolo --session /p/s.jsonl"),
        (AgentKind::OpenCode, "cd /p && opencode --yolo --session S"),
    ];
    for (kind, want) in cases {
        let mut f = Fixture::new();
        let mut r = recovered_record(Path::new("/p"));
        r.agent = kind;
        r.bin = PathBuf::from(kind.as_str());
        r.flags = strings(&["--yolo"]);
        r.session_id = "S".into();
        r.session_file = Some(PathBuf::from("/p/s.jsonl"));
        match decide_with(&mut f, Some(&r), &a) {
            Decision::GiveUp { manual, .. } => assert_eq!(manual.as_deref(), Some(want)),
            d => panic!("expected GiveUp, got {d:?}"),
        }
    }
}

#[test]
fn attempts_used_up_gives_up_with_the_plans_command() {
    let mut f = Fixture::new();
    let a = StubAgent::run(&["/bin/claude", "--resume", "it's"]);
    // Attempts count per boot (see docs/LIMITS.md, "Records"), so the record is
    // stamped with the current boot; its agent process is gone.
    let mut r = recovered_record(Path::new("/p"));
    r.boot_id = f.platform.boot_id();
    r.attempts = 3;
    match decide_with(&mut f, Some(&r), &a) {
        Decision::GiveUp { reason, manual } => {
            assert!(reason.contains('3'), "{reason}");
            assert_eq!(
                manual.as_deref(),
                Some(r#"cd /p && /bin/claude --resume 'it'\''s'"#)
            );
        }
        d => panic!("expected GiveUp, got {d:?}"),
    }
    r.attempts = 2;
    assert!(matches!(
        decide_with(&mut f, Some(&r), &a),
        Decision::Resume(_)
    ));
}

#[test]
fn attempts_used_up_on_an_earlier_boot_start_again() {
    let mut f = Fixture::new();
    let a = StubAgent::run(&["/bin/claude", "--resume", "S"]);
    let mut r = recovered_record(Path::new("/p"));
    r.attempts = 3;
    assert_ne!(r.boot_id, f.platform.boot_id());
    assert!(matches!(
        decide_with(&mut f, Some(&r), &a),
        Decision::Resume(_)
    ));
    // A record that failed stays failed until `herdr-wake resume`.
    r.state = State::Failed { reason: "x".into() };
    assert_eq!(decide_with(&mut f, Some(&r), &a), Decision::Shell);
}

#[test]
fn no_adapter_gives_up() {
    let mut f = Fixture::new();
    let r = recovered_record(Path::new("/p"));
    assert!(matches!(
        decide(Some(&r), None, &mut f.ctx()),
        Decision::GiveUp { .. }
    ));
}

#[test]
fn shell_quote_leaves_plain_words_bare() {
    assert_eq!(
        shell_quote("a-b_c.d/e:f=g@h%i+j,k"),
        "a-b_c.d/e:f=g@h%i+j,k"
    );
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("a b"), "'a b'");
    assert_eq!(shell_quote("$HOME"), "'$HOME'");
    assert_eq!(shell_quote("x'y"), r#"'x'\''y'"#);
}

// ---------------------------------------------------------------- shell_main_with

/// A `Core` over one `FakeCore` shared with the test, so the test can read
/// the record after the wrapper finishes. Each `connect` makes a new handle.
#[derive(Clone)]
struct Shared(Rc<RefCell<FakeCore>>);

impl Core for Shared {
    fn hello(&mut self) -> Result<Hello, CoreError> {
        self.0.borrow_mut().hello()
    }
    fn get(&mut self, t: &TerminalId) -> Result<Terminal, CoreError> {
        self.0.borrow_mut().get(t)
    }
    fn label(
        &mut self,
        t: &TerminalId,
        changes: &BTreeMap<String, Option<String>>,
    ) -> Result<Terminal, CoreError> {
        self.0.borrow_mut().label(t, changes)
    }
    fn list(&mut self, all: bool) -> Result<Vec<Terminal>, CoreError> {
        self.0.borrow_mut().list(all)
    }
}

/// A platform that delegates to `FakePlatform` but runs `during_run` while
/// the "child" runs, to change the world under the wrapper.
struct HookPlatform<'a> {
    inner: &'a FakePlatform,
    during_run: Box<dyn Fn() + 'a>,
}

impl Platform for HookPlatform<'_> {
    fn process(&self, pid: u32) -> Option<ProcInfo> {
        self.inner.process(pid)
    }
    fn alive(&self, p: &ProcRef) -> bool {
        self.inner.alive(p)
    }
    fn boot_id(&self) -> String {
        self.inner.boot_id()
    }
    fn login_shell_argv(&self, shell: &Path) -> Vec<String> {
        self.inner.login_shell_argv(shell)
    }
    fn exec(&self, argv: &[String], env: &BTreeMap<String, String>, cwd: &Path) -> std::io::Error {
        self.inner.exec(argv, env, cwd)
    }
    fn run_foreground(
        &self,
        argv: &[String],
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> std::io::Result<ExitInfo> {
        (self.during_run)();
        self.inner.run_foreground(argv, env, cwd)
    }
    fn shutting_down(&self) -> bool {
        self.inner.shutting_down()
    }
    fn core_pid(&self) -> Option<u32> {
        self.inner.core_pid()
    }
    fn boot_service(&self) -> &dyn herdr_wake::platform::BootService {
        self.inner.boot_service()
    }
    fn now_ms(&self) -> u64 {
        self.inner.now_ms()
    }
    fn mono_ms(&self) -> u64 {
        self.inner.mono_ms()
    }
}

const TID: &str = "t1";

/// Everything one `shell_main_with` run needs.
struct World {
    platform: FakePlatform,
    core: Rc<RefCell<FakeCore>>,
    env: BTreeMap<String, String>,
    home: tempfile::TempDir,
    work: tempfile::TempDir,
    sleeps: RefCell<Vec<u64>>,
    said: RefCell<Vec<String>>,
    connects: RefCell<usize>,
    confirms: RefCell<Vec<herdr_wake::wake::ConfirmJob>>,
}

impl World {
    fn new() -> World {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let mut env = BTreeMap::new();
        env.insert("HOME".into(), home.path().display().to_string());
        env.insert("PATH".into(), "/usr/bin:/bin".into());
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("SHELL".into(), "/bin/zsh".into());
        env.insert("HC_TERMINAL_ID".into(), TID.into());
        env.insert("HC_TERMINAL_SOCKET".into(), "/nonexistent/c.sock".into());
        let mut core = FakeCore::new();
        core.insert(Terminal::new(TID));
        World {
            platform: FakePlatform::new(),
            core: Rc::new(RefCell::new(core)),
            env,
            home,
            work,
            sleeps: RefCell::new(Vec::new()),
            said: RefCell::new(Vec::new()),
            connects: RefCell::new(0),
            confirms: RefCell::new(Vec::new()),
        }
    }

    fn cwd(&self) -> PathBuf {
        self.work.path().to_path_buf()
    }

    /// Stores `rec` the way herdr-wake's hook does: the label plus the
    /// local copy that makes it trusted.
    fn put(&self, rec: &Record) {
        self.put_label_only(rec);
        common::trust_records(&self.core.borrow(), &self.env, self.home.path());
    }

    /// Stores `rec` as something else would: the label alone.
    fn put_label_only(&self, rec: &Record) {
        let mut c = self.core.borrow_mut();
        assert!(record::save(&mut *c, &TerminalId::from(TID), rec).unwrap());
    }

    fn log(&self) -> String {
        std::fs::read_to_string(herdr_wake::paths::log_file(&self.env, self.home.path()))
            .unwrap_or_default()
    }

    fn put_recovered(&self) -> Record {
        let r = recovered_record(&self.cwd());
        self.put(&r);
        r
    }

    fn stored(&self) -> Option<Record> {
        record::load(&mut *self.core.borrow_mut(), &TerminalId::from(TID)).unwrap()
    }

    fn write_config(&self, text: &str) {
        let dir = self.home.path().join(".config/herdr-wake");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), text).unwrap();
    }

    fn run_on(
        &self,
        platform: &dyn Platform,
        agent: &dyn Fn() -> Box<dyn Agent>,
    ) -> std::io::Error {
        let connect = || -> Result<Box<dyn Core>, CoreError> {
            *self.connects.borrow_mut() += 1;
            Ok(Box::new(Shared(self.core.clone())))
        };
        let sleep = |ms: u64| {
            self.sleeps.borrow_mut().push(ms);
            self.platform.advance(ms);
        };
        let agent_for = |_k: AgentKind| agent();
        let say = |s: &str| self.said.borrow_mut().push(s.to_string());
        let confirm = |job: herdr_wake::wake::ConfirmJob| self.confirms.borrow_mut().push(job);
        let seams = Seams {
            connect: &connect,
            sleep: &sleep,
            agent: &agent_for,
            say: &say,
            confirm: &confirm,
        };
        shell_main_with(platform, &self.env, self.home.path(), &seams)
    }

    fn run(&self, agent: &dyn Fn() -> Box<dyn Agent>) -> std::io::Error {
        self.run_on(&self.platform, agent)
    }

    fn run_stub(&self) -> std::io::Error {
        self.run(&|| Box::new(StubAgent::run(&["/bin/claude", "--resume", "S"])))
    }

    fn first_exec(&self) -> herdr_wake::platform::fake::ExecCall {
        self.platform.execs().first().cloned().expect("no exec")
    }
}

#[test]
fn shell_main_without_a_terminal_id_execs_the_login_shell_and_never_connects() {
    let w = World::new();
    let mut w = w;
    w.env.remove("HC_TERMINAL_ID");
    w.run_stub();
    assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));
    assert_eq!(*w.connects.borrow(), 0);
    assert!(w.platform.runs().is_empty());
    assert!(w.said.borrow().is_empty());
}

#[test]
fn shell_main_with_no_record_execs_the_shell_silently() {
    let w = World::new();
    w.run_stub();
    let e = w.first_exec();
    assert_eq!(e.argv, strings(&["/bin/zsh", "-l"]));
    assert!(w.platform.runs().is_empty());
    assert!(w.said.borrow().is_empty());
    // The shell gets the wrapper's own environment.
    assert_eq!(
        e.env.get("TERM").map(String::as_str),
        Some("xterm-256color")
    );
    assert_eq!(e.env.get("HC_TERMINAL_ID").map(String::as_str), Some(TID));
}

#[test]
fn every_exec_fallback_is_tried_in_order() {
    let w = World::new();
    w.write_config("real_shell = [\"/opt/fish/bin/fish\", \"--login\"]\n");
    let err = w.run_stub();
    assert_eq!(err.to_string(), "fake exec");
    let argvs: Vec<Vec<String>> = w.platform.execs().into_iter().map(|e| e.argv).collect();
    assert_eq!(argvs[0], strings(&["/opt/fish/bin/fish", "--login"]));
    assert_eq!(argvs[1], strings(&["/bin/zsh", "-l"]));
    assert!(argvs[2..].iter().all(|a| a == &strings(&["/bin/sh"])));
}

#[test]
fn a_real_shell_pointing_at_herdr_wake_itself_is_skipped() {
    let w = World::new();
    w.write_config("real_shell = [\"/usr/local/bin/herdr-wake\", \"shell\"]\n");
    w.run_stub();
    assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));
}

#[test]
fn core_errors_end_in_the_shell() {
    for err in [
        CoreError::NotRunning,
        CoreError::Timeout,
        CoreError::NotFound,
        CoreError::Protocol("x".into()),
    ] {
        let w = World::new();
        w.put_recovered();
        w.core.borrow_mut().fail_with(Some(err));
        w.run_stub();
        assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));
        assert!(w.platform.runs().is_empty());
        assert!(w.said.borrow().is_empty());
    }
}

#[test]
fn resume_then_quit_ends_the_record_and_execs_the_shell_in_its_folder() {
    let w = World::new();
    let r = w.put_recovered();
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run_stub();

    let runs = w.platform.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].argv, strings(&["/bin/claude", "--resume", "S"]));
    assert_eq!(runs[0].cwd, w.cwd());
    let env = &runs[0].env;
    assert_eq!(
        env.get("HERDR_WAKE_SUPERVISED").map(String::as_str),
        Some("1")
    );
    assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
    assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));

    let stored = w.stored().unwrap();
    assert_eq!(stored.state, State::Ended);
    assert_eq!(stored.session_id, r.session_id);
    assert_eq!(stored.attempts, 1);

    let e = w.first_exec();
    assert_eq!(e.argv, strings(&["/bin/zsh", "-l"]));
    assert_eq!(e.cwd, w.cwd());
    assert!(w.said.borrow().is_empty());
}

#[test]
fn exit_130_is_a_quit_too() {
    let w = World::new();
    w.put_recovered();
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 130, 1000)));
    w.run_stub();
    assert_eq!(w.stored().unwrap().state, State::Ended);
    assert_eq!(w.platform.runs().len(), 1);
}

#[test]
fn plan_env_is_merged_over_the_wrapper_env() {
    let w = World::new();
    w.put_recovered();
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run(&|| {
        let mut a = StubAgent::run(&["/bin/claude"]);
        if let Plan::Run { env, .. } = &mut a.plan {
            env.insert("PATH".into(), "/agent/bin".into());
            env.insert("CLAUDE_CONFIG_DIR".into(), "/cfg".into());
        }
        Box::new(a)
    });
    let env = &w.platform.runs()[0].env;
    assert_eq!(env.get("PATH").map(String::as_str), Some("/agent/bin"));
    assert_eq!(
        env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
        Some("/cfg")
    );
    assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
}

#[test]
fn killed_by_sigterm_leaves_the_record_running() {
    for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGKILL] {
        let w = World::new();
        w.put_recovered();
        w.platform
            .push_run_result(Ok(ExitInfo::signaled(900, sig, 100)));
        w.run_stub();
        assert_eq!(w.stored().unwrap().state, State::Running, "signal {sig}");
        assert_eq!(w.platform.runs().len(), 1, "signal {sig}: no retry");
        assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));
    }
}

#[test]
fn a_quit_during_shutdown_leaves_the_record_running() {
    let w = World::new();
    w.put_recovered();
    w.platform.set_shutting_down(true);
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run_stub();
    assert_eq!(w.stored().unwrap().state, State::Running);
}

#[test]
fn a_fast_failing_child_is_retried_up_to_attempts_then_failed() {
    let w = World::new();
    let r = w.put_recovered();
    for _ in 0..5 {
        w.platform
            .push_run_result(Ok(ExitInfo::exited(900, 1, 300)));
    }
    w.run_stub();
    assert_eq!(w.platform.runs().len(), 3);
    let stored = w.stored().unwrap();
    assert!(matches!(stored.state, State::Failed { .. }), "{stored:?}");
    // Two 2 s pauses between the three attempts.
    assert_eq!(w.sleeps.borrow().iter().filter(|&&s| s == 2000).count(), 2);
    let said = w.said.borrow();
    assert_eq!(said.len(), 1, "{said:?}");
    let line = &said[0];
    assert!(
        line.starts_with(&format!(
            "herdr-wake: could not resume claude session {}: ",
            r.session_id
        )),
        "{line}"
    );
    assert!(
        line.ends_with(&format!(
            ". To retry: cd {} && /bin/claude --resume S",
            shell_quote(&w.cwd().display().to_string())
        )),
        "{line}"
    );
    assert_eq!(w.first_exec().cwd, w.cwd());
}

#[test]
fn attempts_follow_config() {
    let w = World::new();
    w.write_config("attempts = 1\n");
    w.put_recovered();
    for _ in 0..3 {
        w.platform
            .push_run_result(Ok(ExitInfo::exited(900, 1, 300)));
    }
    w.run_stub();
    assert_eq!(w.platform.runs().len(), 1);
    assert!(matches!(w.stored().unwrap().state, State::Failed { .. }));
}

#[test]
fn a_slow_failure_is_not_retried_and_leaves_the_record() {
    let w = World::new();
    w.put_recovered();
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 1, 60_000)));
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 1, 60_000)));
    w.run_stub();
    assert_eq!(w.platform.runs().len(), 1);
    assert_eq!(w.stored().unwrap().state, State::Running);
    assert!(w.said.borrow().is_empty());
}

#[test]
fn a_permanent_plan_prints_once_marks_failed_and_never_runs() {
    let w = World::new();
    let r = w.put_recovered();
    w.run(&|| Box::new(StubAgent::permanent("folder missing")));
    assert!(w.platform.runs().is_empty());
    assert_eq!(
        w.stored().unwrap().state,
        State::Failed {
            reason: "folder missing".into()
        }
    );
    let said = w.said.borrow();
    assert_eq!(said.len(), 1);
    assert!(said[0].contains(&format!(
        "could not resume claude session {}: folder missing. To retry: cd ",
        r.session_id
    )));
    assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));
}

#[test]
fn a_record_replaced_while_the_child_ran_is_not_ended() {
    // Stale-copy protection: another agent's record lands during the run.
    let w = World::new();
    w.put_recovered();
    let newer = {
        let mut n = recovered_record(&w.cwd());
        n.session_id = "other-session".into();
        n.agent_proc = Some(ProcRef {
            pid: 7777,
            start: 1,
        });
        n.seq = u64::MAX / 2;
        n
    };
    let hp = HookPlatform {
        inner: &w.platform,
        during_run: Box::new(|| w.put(&newer)),
    };
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run_on(&hp, &|| Box::new(StubAgent::run(&["/bin/claude"])));
    assert_eq!(w.stored().unwrap(), newer);
}

#[test]
fn a_record_restamped_by_the_resumed_agent_is_ended_on_quit() {
    let w = World::new();
    w.put_recovered();
    let restamped = {
        let mut n = recovered_record(&w.cwd());
        n.agent_proc = Some(ProcRef { pid: 900, start: 5 });
        n.boot_id = "boot-1".into();
        n.seq = u64::MAX / 2;
        n
    };
    let hp = HookPlatform {
        inner: &w.platform,
        during_run: Box::new(|| w.put(&restamped)),
    };
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run_on(&hp, &|| Box::new(StubAgent::run(&["/bin/claude"])));
    let stored = w.stored().unwrap();
    assert_eq!(stored.state, State::Ended);
    assert_eq!(stored.agent_proc, restamped.agent_proc);
}

#[test]
fn a_session_claimed_by_another_terminal_gets_a_plain_shell() {
    let w = World::new();
    let r = w.put_recovered();
    let key = herdr_wake::gate::boot_key(&w.platform);
    let claims = herdr_wake::paths::claims_dir(&w.env, w.home.path()).join(key);
    assert!(record::claim(
        &claims,
        &r.session_id,
        &TerminalId::from("t9")
    ));
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run_stub();
    assert!(w.platform.runs().is_empty());
    assert_eq!(w.stored().unwrap().state, State::Running);
    assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));
}

#[test]
fn the_claim_is_released_afterwards() {
    let w = World::new();
    let r = w.put_recovered();
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run_stub();
    let key = herdr_wake::gate::boot_key(&w.platform);
    let claims = herdr_wake::paths::claims_dir(&w.env, w.home.path()).join(key);
    assert!(record::claim(
        &claims,
        &r.session_id,
        &TerminalId::from("t9")
    ));
}

#[test]
fn the_second_resume_of_a_kind_waits_at_the_gate() {
    let w = World::new();
    w.put_recovered();
    let key = herdr_wake::gate::boot_key(&w.platform);
    let gate_dir = herdr_wake::paths::gate_dir(&w.env, w.home.path());
    // Another terminal took the first claude slot just now.
    herdr_wake::gate::enter(
        &gate_dir,
        AgentKind::Claude,
        &Config::default(),
        &key,
        w.platform.mono_ms(),
    );
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run_stub();
    assert_eq!(w.sleeps.borrow().iter().sum::<u64>(), 8000);
    assert_eq!(w.platform.runs().len(), 1);
}

#[test]
fn a_panic_inside_the_body_still_ends_in_exec() {
    let w = World::new();
    w.put_recovered();
    w.run(&|| {
        let mut a = StubAgent::run(&["/bin/claude"]);
        a.panic = true;
        Box::new(a)
    });
    assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));
}

#[test]
fn a_failing_run_foreground_still_ends_in_exec() {
    let w = World::new();
    w.put_recovered();
    // No scripted result: run_foreground returns an error.
    w.run_stub();
    assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));
    assert_eq!(w.first_exec().cwd, w.cwd());
}

#[test]
fn a_missing_record_folder_execs_in_the_terminals_folder() {
    let w = World::new();
    let mut r = recovered_record(Path::new("/nonexistent/folder"));
    r.state = State::Ended;
    w.put(&r);
    w.run_stub();
    let cwd = w.first_exec().cwd;
    assert!(cwd.is_dir(), "{cwd:?}");
}

// ---------------------------------------------------------------- shell_main (real socket)

fn real_env(home: &Path, socket: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("HOME".into(), home.display().to_string());
    env.insert("SHELL".into(), "/bin/zsh".into());
    env.insert("HC_TERMINAL_ID".into(), TID.into());
    env.insert("HC_TERMINAL_SOCKET".into(), socket.display().to_string());
    env
}

#[test]
fn shell_main_with_core_not_running_execs_the_login_shell() {
    let home = tempfile::tempdir().unwrap();
    let p = FakePlatform::new();
    let env = real_env(home.path(), &home.path().join("missing.sock"));
    let start = Instant::now();
    shell_main(&p, &env, home.path());
    assert!(start.elapsed() < Duration::from_millis(500));
    assert_eq!(p.execs()[0].argv, strings(&["/bin/zsh", "-l"]));
    assert!(p.runs().is_empty());
}

#[test]
fn shell_main_with_a_hung_core_execs_the_login_shell_within_the_budget() {
    // Review Focus 1: a core that accepts connections and never replies.
    let home = tempfile::tempdir().unwrap();
    let sock = home.path().join("c.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let held2 = held.clone();
    std::thread::spawn(move || {
        for c in listener.incoming().flatten() {
            held2.lock().unwrap().push(c);
        }
    });
    let p = FakePlatform::new();
    let env = real_env(home.path(), &sock);
    let start = Instant::now();
    shell_main(&p, &env, home.path());
    let took = start.elapsed();
    assert!(took < Duration::from_millis(2500), "took {took:?}");
    assert_eq!(p.execs()[0].argv, strings(&["/bin/zsh", "-l"]));
    assert!(p.runs().is_empty());
}

// ------------------------------------------------- per-boot attempts, confirm

#[test]
fn a_resume_on_a_new_boot_starts_its_attempts_over_and_clears_old_processes() {
    let w = World::new();
    let mut r = recovered_record(&w.cwd());
    r.attempts = 3; // used up on boot-0
    w.put(&r);
    w.platform
        .push_run_result(Ok(ExitInfo::signaled(900, libc::SIGTERM, 60_000)));
    w.run_stub();
    assert_eq!(w.platform.runs().len(), 1, "the agent was started");
    let stored = w.stored().unwrap();
    assert_eq!(stored.attempts, 1);
    assert_eq!(stored.boot_id, w.platform.boot_id());
    assert_eq!(stored.agent_proc, None);
    assert_eq!(stored.shell_proc, None);
    assert_eq!(stored.state, State::Running);
}

#[test]
fn every_resume_asks_for_a_confirmation_check_with_the_plans_deadline() {
    let w = World::new();
    w.put_recovered();
    w.platform
        .push_run_result(Ok(ExitInfo::signaled(900, libc::SIGTERM, 60_000)));
    w.run_stub();
    let jobs = w.confirms.borrow();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].terminal, TerminalId::from(TID));
    assert_eq!(jobs[0].session_id, sample_record().session_id);
    assert_eq!(jobs[0].within_ms, herdr_wake::wake::DEFAULT_CONFIRM_MS);
    assert_eq!(jobs[0].boot_key, herdr_wake::gate::boot_key(&w.platform));
}

#[test]
fn confirm_check_passes_only_when_the_same_session_has_a_live_agent() {
    use herdr_wake::wake::{confirm_check, ConfirmJob};
    let w = World::new();
    let mut rec = recovered_record(&w.cwd());
    rec.agent_proc = None;
    w.put(&rec);
    let job = ConfirmJob {
        terminal: TerminalId::from(TID),
        session_id: rec.session_id.clone(),
        within_ms: 15_000,
        boot_key: "k".into(),
    };
    let check = || confirm_check(&mut *w.core.borrow_mut(), &w.platform, &job);

    // Not re-stamped: unconfirmed, and the message names the deadline.
    let why = check().unwrap_err();
    assert!(why.contains("within 15 s"), "{why}");

    // Re-stamped by a live agent with the same session: confirmed.
    w.platform
        .add_process(ProcInfo::new(900, 1, 77, strings(&["claude"])));
    rec.agent_proc = Some(ProcRef {
        pid: 900,
        start: 77,
    });
    rec.seq += 1;
    w.put(&rec);
    assert_eq!(check(), Ok(()));

    // The agent came up as another session (OpenCode v2 creates a new one
    // for an unknown id): unconfirmed.
    rec.session_id = "another".into();
    rec.seq += 1;
    w.put(&rec);
    let why = check().unwrap_err();
    assert!(why.contains("another"), "{why}");

    // Ended or forgotten records have nothing to confirm.
    rec.state = State::Ended;
    rec.seq += 1;
    w.put(&rec);
    assert_eq!(check(), Ok(()));
    record::forget(&mut *w.core.borrow_mut(), &TerminalId::from(TID)).unwrap();
    assert_eq!(check(), Ok(()));
}

#[test]
fn claim_folders_of_other_boots_are_removed() {
    let w = World::new();
    w.put_recovered();
    let claims = herdr_wake::paths::claims_dir(&w.env, w.home.path());
    let stale = claims.join("old-boot-1");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("x.claim"), "t9").unwrap();
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run_stub();
    assert!(!stale.exists());
    assert!(claims
        .join(herdr_wake::gate::boot_key(&w.platform))
        .is_dir());
}

// ------------------------------------------------- folder safety (M-1)

fn open_folder(parent: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = parent.join("open");
    std::fs::create_dir(&p).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o777)).unwrap();
    p
}

#[test]
fn a_folder_others_can_write_gives_up_and_says_why() {
    let mut f = Fixture::new();
    let a = StubAgent::run(&["claude"]);
    let t = tempfile::tempdir().unwrap();
    let r = recovered_record(&open_folder(t.path()));
    match decide_with(&mut f, Some(&r), &a) {
        Decision::GiveUp { reason, .. } => assert_eq!(
            reason,
            "folder is not safe to resume in: it is writable by others"
        ),
        d => panic!("expected GiveUp, got {d:?}"),
    }
}

#[test]
fn an_unsafe_folder_is_never_run_in() {
    let w = World::new();
    let mut r = recovered_record(&open_folder(w.work.path()));
    r.session_id = "S".into();
    w.put(&r);
    w.run_stub();
    assert!(w.platform.runs().is_empty());
    assert!(w.said.borrow()[0].contains("folder is not safe to resume in"));
    assert!(matches!(w.stored().unwrap().state, State::Failed { .. }));
}

// ------------------------------------------------- the label is not trusted (M-5)

/// An agent whose plan runs the record's program with the record's
/// environment, like the real adapters do.
struct EchoAgent;

impl Agent for EchoAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }
    fn observe(&self, _: &str, _: &[u8], _: &mut Ctx) -> Observation {
        Observation::Ignore("stub")
    }
    fn restorable_flags(&self, argv: &[String], _: &Config) -> Vec<String> {
        argv.iter().skip(1).cloned().collect()
    }
    fn resume_plan(&self, rec: &Record, _: &mut Ctx) -> Plan {
        Plan::Run {
            argv: vec![
                rec.bin.display().to_string(),
                "--resume".into(),
                rec.session_id.clone(),
            ],
            env: rec.env.clone(),
            cwd: rec.cwd.clone(),
            before: Vec::new(),
            confirm_within_ms: None,
        }
    }
    fn integration(&self, _: &Path, _: &mut Ctx) -> Vec<Edit> {
        Vec::new()
    }
}

#[test]
fn a_label_without_herdr_wakes_local_copy_is_never_run() {
    // The audit's hostile label: written straight to the terminal.
    let w = World::new();
    let mut r = recovered_record(&w.cwd());
    r.bin = PathBuf::from("/tmp/evil-program");
    r.session_id = "--dangerously-skip-permissions".into();
    r.env
        .insert("DYLD_INSERT_LIBRARIES".into(), "/tmp/evil.dylib".into());
    w.put_label_only(&r);

    w.run(&|| Box::new(EchoAgent));

    assert!(w.platform.runs().is_empty());
    assert!(
        w.said.borrow().is_empty(),
        "nothing from the label is printed"
    );
    assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));
    assert_ne!(
        w.first_exec().cwd,
        w.cwd(),
        "the label's folder is not used"
    );
    assert_eq!(w.stored().unwrap().state, State::Running, "left as it is");
    let log = w.log();
    assert!(
        log.contains("record does not match the local copy"),
        "{log}"
    );
    assert!(!log.contains("evil"), "{log}");
}

#[test]
fn a_label_changed_after_it_was_recorded_is_never_run() {
    let w = World::new();
    let r = w.put_recovered();
    for change in [
        (|r: &mut Record| r.bin = PathBuf::from("/tmp/evil-program")) as fn(&mut Record),
        |r| r.flags.push("--dangerously-skip-permissions".into()),
        |r| {
            r.env.insert("CLAUDE_CONFIG_DIR".into(), "/tmp/evil".into());
        },
        |r| r.cwd = PathBuf::from("/tmp"),
        |r| r.session_id = "another-session".into(),
    ] {
        let mut hostile = r.clone();
        change(&mut hostile);
        hostile.seq += 1;
        w.put_label_only(&hostile);
        w.run(&|| Box::new(EchoAgent));
        assert!(w.platform.runs().is_empty());
    }
    // The untouched record still resumes.
    let mut same = r.clone();
    same.seq += 100;
    w.put_label_only(&same);
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run(&|| Box::new(EchoAgent));
    assert_eq!(w.platform.runs().len(), 1);
}

#[test]
fn a_reused_terminal_id_does_not_inherit_the_old_terminals_trust() {
    let w = World::new();
    w.put_recovered();
    // Core removed t1 and gave the id to a new terminal.
    w.core
        .borrow_mut()
        .terminals
        .get_mut(&TerminalId::from(TID))
        .unwrap()
        .created_ms = 77;
    w.run(&|| Box::new(EchoAgent));
    assert!(w.platform.runs().is_empty());
}

#[test]
fn the_records_env_is_scrubbed_again_at_wake() {
    let w = World::new();
    let mut r = recovered_record(&w.cwd());
    for (k, v) in [
        ("DYLD_INSERT_LIBRARIES", "/tmp/evil.dylib"),
        ("NODE_OPTIONS", "--require /tmp/x.js"),
        ("ANTHROPIC_BASE_URL", "https://evil.example"),
        ("LD_PRELOAD", "/tmp/x.so"),
        ("CLAUDE_CONFIG_DIR", "/home/u/.cc"),
    ] {
        r.env.insert(k.into(), v.into());
    }
    // Even with a matching local copy (an older herdr-wake wrote both).
    w.put(&r);
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));

    w.run(&|| Box::new(EchoAgent));

    let env = &w.platform.runs()[0].env;
    for gone in [
        "DYLD_INSERT_LIBRARIES",
        "NODE_OPTIONS",
        "ANTHROPIC_BASE_URL",
        "LD_PRELOAD",
    ] {
        assert!(!env.contains_key(gone), "{gone}");
    }
    assert_eq!(
        env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
        Some("/home/u/.cc")
    );
}

#[test]
fn a_session_id_that_reads_as_an_option_gives_up_without_a_command() {
    let mut f = Fixture::new();
    let mut r = recovered_record(Path::new("/p"));
    for bad in ["--dangerously-skip-permissions", "../x", "", "a b"] {
        r.session_id = bad.into();
        assert_eq!(
            decide_with(&mut f, Some(&r), &EchoAgent),
            Decision::GiveUp {
                reason: "the recorded session id is not valid".into(),
                manual: None,
            },
            "{bad:?}"
        );
    }
    // And the line a person sees does not repeat it.
    let w = World::new();
    let mut r = recovered_record(&w.cwd());
    r.session_id = "--dangerously-skip-permissions".into();
    w.put(&r);
    w.run(&|| Box::new(EchoAgent));
    assert!(w.platform.runs().is_empty());
    let said = w.said.borrow();
    assert_eq!(said.len(), 1);
    assert!(!said[0].contains("--dangerously"), "{}", said[0]);
    assert!(!said[0].contains("To retry"), "{}", said[0]);
}

#[test]
fn a_program_that_is_not_the_agents_gives_up_without_a_command() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let evil = t.path().join("evil-program");
    std::fs::write(&evil, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&evil, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut f = Fixture::new();
    let mut r = recovered_record(Path::new("/p"));
    for (bin, why) in [
        (evil.clone(), "it is not claude"),
        (PathBuf::from("claude"), "its path is not absolute"),
        (t.path().join("claude"), "it does not exist"),
    ] {
        r.bin = bin;
        assert_eq!(
            decide_with(&mut f, Some(&r), &EchoAgent),
            Decision::GiveUp {
                reason: format!("the recorded program is not safe to run: {why}"),
                manual: None,
            }
        );
    }
}

// ---- config.toml that cannot be parsed ----

#[test]
fn a_config_that_cannot_be_parsed_resumes_nothing_and_says_so_once_in_the_log() {
    // The defaults include restore_bypass = true: a typo in the file must
    // not silently bring the bypass flags back.
    let w = World::new();
    w.write_config("restore_bypass = fales\n");
    w.put_recovered();
    w.run_stub();
    assert!(w.platform.runs().is_empty());
    assert!(w.said.borrow().is_empty());
    assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));
    assert_eq!(w.stored().unwrap().state, State::Running, "left to resume");
    let log = w.log();
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains("config.toml cannot be parsed"), "{log}");
    assert!(log.contains("nothing is resumed"), "{log}");
    assert_eq!(*w.connects.borrow(), 0, "core is not even asked");
}

// ---- per-terminal opt-out: herdr-wake off <terminal> ----

#[test]
fn a_terminal_that_is_switched_off_wakes_to_a_plain_shell() {
    let w = World::new();
    w.put_recovered();
    record::set_off(&mut *w.core.borrow_mut(), &TerminalId::from(TID), true).unwrap();
    w.run_stub();
    assert!(w.platform.runs().is_empty());
    assert!(w.said.borrow().is_empty());
    assert_eq!(w.first_exec().argv, strings(&["/bin/zsh", "-l"]));

    // Switched on again, the record (still trusted) resumes.
    record::set_off(&mut *w.core.borrow_mut(), &TerminalId::from(TID), false).unwrap();
    w.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));
    w.run_stub();
    assert_eq!(w.platform.runs().len(), 1);
}

// ---- a clock that steps back during boot (real Linux reboot, 2026-11-10) ----

/// chronyd stepped the wall clock back 130 s after the first wrapper had
/// taken its gate slot. The first agent's confirm must still let the second
/// start `stagger_ms` later, not after the full first-alone hold or longer.
#[test]
fn a_wall_clock_stepped_back_after_the_first_slot_does_not_hold_the_second_agent() {
    let mut w = World::new();
    w.put_recovered();
    // The first agent keeps running: its wrapper took the first slot.
    w.platform
        .push_run_result(Ok(ExitInfo::signaled(900, libc_sigterm(), 60_000)));
    w.run_stub();
    assert_eq!(w.platform.runs().len(), 1);
    assert_eq!(w.sleeps.borrow().iter().sum::<u64>(), 0, "first: no wait");

    // The clock is stepped back 130 s.
    w.platform.set_now(w.platform.now_ms() - 130_000);

    // The first agent reports itself (its supervised hook confirms the gate).
    let mut hook_env = w.env.clone();
    hook_env.insert("HERDR_WAKE_SUPERVISED".into(), "1".into());
    let rec = w.stored().unwrap();
    let cfg = Config::default();
    let out = herdr_wake::hook::started(
        &StubAgent::run(&["claude"]),
        herdr_wake::hook::Start {
            session_id: rec.session_id.clone(),
            session_file: None,
            cwd: rec.cwd.clone(),
            terminal: Some(TerminalId::from(TID)),
            agent_pid: None,
            argv: None,
        },
        &mut Ctx {
            platform: &w.platform,
            core: &mut *w.core.borrow_mut(),
            env: &hook_env,
            home: w.home.path(),
            cfg: &cfg,
        },
        &|_, _, _| {},
    );
    assert_eq!(out, herdr_wake::hook::HookOutcome::Recorded);

    // A second terminal resumes another claude session right after.
    let t2 = TerminalId::from("t2");
    w.core.borrow_mut().insert(Terminal::new("t2"));
    let mut second = recovered_record(&w.cwd());
    second.session_id = "second-session".into();
    assert!(record::save(&mut *w.core.borrow_mut(), &t2, &second).unwrap());
    common::trust_records(&w.core.borrow(), &w.env, w.home.path());
    w.env.insert("HC_TERMINAL_ID".into(), "t2".into());
    w.sleeps.borrow_mut().clear();
    w.platform
        .push_run_result(Ok(ExitInfo::signaled(901, libc_sigterm(), 60_000)));
    w.run_stub();

    assert_eq!(w.platform.runs().len(), 2);
    assert_eq!(
        w.sleeps.borrow().iter().sum::<u64>(),
        Config::default().stagger_ms,
        "the second agent starts one stagger after the confirm"
    );
}

fn libc_sigterm() -> i32 {
    15
}

#[test]
fn the_fake_platforms_clocks_are_separate() {
    let p = FakePlatform::new();
    let (wall, mono) = (p.now_ms(), p.mono_ms());
    p.advance(250);
    assert_eq!(p.now_ms(), wall + 250);
    assert_eq!(p.mono_ms(), mono + 250, "time passing moves both");
    p.set_now(wall - 130_000);
    assert_eq!(
        p.mono_ms(),
        mono + 250,
        "a stepped wall clock leaves it alone"
    );
    p.set_mono(7);
    assert_eq!(p.mono_ms(), 7);
    assert_eq!(p.now_ms(), wall - 130_000);
}
