//! `herdr-wake boot`: start skipping, the `hc` lookup order, the network
//! wait, settling, counts, the timeout, `last-boot.json` and `boot.env`
//! parsing, with fakes, an injected runner and sleeper, and a temp home.

mod common;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use herdr_wake::boot::{self, BootParts, BootSummary};
use herdr_wake::core::fake::FakeCore;
use herdr_wake::core::{Core, CoreError, Terminal};
use herdr_wake::platform::{Platform, ProcInfo};
use herdr_wake::record::{ProcRef, Record, State, LABEL};
use herdr_wake::report::FakeProbes;

use common::{sample_record, Fixture};

/// A machine at boot: core down until the runner "starts" it.
struct Rig {
    f: Fixture,
    _home: tempfile::TempDir,
    core: RefCell<FakeCore>,
    up: Cell<bool>,
    probes: FakeProbes,
    exe: Option<PathBuf>,
    network_after: Cell<u32>,
    runner_result: RefCell<Option<io::Result<i32>>>,
    ran: RefCell<Vec<Vec<String>>>,
    sleeps: Cell<u64>,
}

impl Rig {
    fn new() -> Rig {
        let (mut f, home) = Fixture::with_temp_home();
        f.env.insert("PATH".into(), "/usr/bin:/bin".into());
        let probes = FakeProbes::new();
        probes.executable("/opt/hw/hc");
        Rig {
            f,
            _home: home,
            core: RefCell::new(FakeCore::new()),
            up: Cell::new(false),
            probes,
            exe: Some(PathBuf::from("/opt/hw/herdr-wake")),
            network_after: Cell::new(0),
            runner_result: RefCell::new(None),
            ran: RefCell::new(Vec::new()),
            sleeps: Cell::new(0),
        }
    }

    fn put(&self, t: &str, rec: &Record) {
        let mut term = Terminal::new(t);
        term.labels
            .insert(LABEL.into(), serde_json::to_string(rec).unwrap());
        self.core.borrow_mut().insert(term);
        // As herdr-wake's own hook would have left it: with its local copy.
        common::trust_records(&self.core.borrow(), &self.f.env, &self.f.home);
    }

    /// A record whose label was written by something other than herdr-wake.
    fn put_label_only(&self, t: &str, rec: &Record) {
        let mut term = Terminal::new(t);
        term.labels
            .insert(LABEL.into(), serde_json::to_string(rec).unwrap());
        self.core.borrow_mut().insert(term);
    }

    /// A running record whose agent `pid` is alive on this boot when `live`.
    fn running(&self, t: &str, pid: u32, live: bool) -> Record {
        let mut r = sample_record();
        r.agent_proc = Some(ProcRef { pid, start: 9 });
        if live {
            self.f
                .platform
                .add_process(ProcInfo::new(pid, 1, 9, vec!["claude".into()]));
        }
        self.put(t, &r);
        r
    }

    fn run_with(&self, on_sleep: &dyn Fn(u64)) -> BootSummary {
        let connect = || -> Result<Box<dyn Core>, CoreError> {
            if self.up.get() {
                Ok(Box::new(self.core.borrow().clone()))
            } else {
                Err(CoreError::NotRunning)
            }
        };
        let checks = Cell::new(0u32);
        let network_up = || {
            checks.set(checks.get() + 1);
            checks.get() > self.network_after.get()
        };
        let parts = BootParts {
            platform: &self.f.platform,
            env: &self.f.env,
            home: &self.f.home,
            cfg: &self.f.cfg,
            exe: self.exe.clone(),
            probes: &self.probes,
            network_up: &network_up,
            connect: &connect,
        };
        let runner = |argv: &[String]| -> io::Result<i32> {
            self.ran.borrow_mut().push(argv.to_vec());
            match self.runner_result.borrow_mut().take() {
                Some(r) => r,
                None => {
                    self.up.set(true);
                    Ok(0)
                }
            }
        };
        let sleeper = |ms: u64| {
            self.sleeps.set(self.sleeps.get() + 1);
            self.f.platform.advance(ms);
            on_sleep(self.sleeps.get());
        };
        boot::run(&parts, &runner, &sleeper)
    }

    fn run(&self) -> BootSummary {
        self.run_with(&|_| {})
    }

    fn last_boot(&self) -> PathBuf {
        self.f.home.join(".local/state/herdr-wake/last-boot.json")
    }
}

fn has(s: &BootSummary, needle: &str) -> bool {
    s.details.iter().any(|d| d.contains(needle))
}

// ---- starting core ----

#[test]
fn core_found_by_process_lookup_skips_the_start() {
    let r = Rig::new();
    r.f.platform.set_core_pid(Some(77));
    r.up.set(true);
    let s = r.run();
    assert!(r.ran.borrow().is_empty());
    assert!(!s.core_started);
    assert!(has(&s, "already running (pid 77)"), "{:?}", s.details);
}

#[test]
fn core_answering_on_its_socket_skips_the_start() {
    let r = Rig::new();
    r.up.set(true);
    let s = r.run();
    assert!(r.ran.borrow().is_empty());
    assert!(has(&s, "already running (socket answers)"));
}

#[test]
fn core_down_runs_hc_server_start_next_to_the_binary() {
    let r = Rig::new();
    let s = r.run();
    assert_eq!(
        *r.ran.borrow(),
        vec![vec![
            "/opt/hw/hc".to_string(),
            "server".into(),
            "start".into()
        ]]
    );
    assert!(s.core_started);
    assert_eq!(s.boot_id, "boot-1");
}

#[test]
fn hc_override_from_the_environment_comes_first() {
    let mut r = Rig::new();
    r.f.env.insert("HERDR_WAKE_HC".into(), "/custom/hc".into());
    r.probes.executable("/custom/hc");
    r.run();
    assert_eq!(r.ran.borrow()[0][0], "/custom/hc");
}

#[test]
fn missing_hc_is_recorded_and_returns_without_waiting() {
    let mut r = Rig::new();
    r.probes.remove("/opt/hw/hc");
    r.f.env.insert("PATH".into(), "/nowhere".into());
    r.running("t1", 50, false);
    let s = r.run();
    assert!(r.ran.borrow().is_empty());
    assert!(!s.core_started);
    assert!(has(&s, "hc not found"), "{:?}", s.details);
    assert_eq!(r.sleeps.get(), 0);
    assert!(r.last_boot().exists());
}

#[test]
fn hc_that_cannot_be_run_ends_the_boot() {
    let r = Rig::new();
    *r.runner_result.borrow_mut() = Some(Err(io::Error::other("exec format error")));
    let s = r.run();
    assert!(!s.core_started);
    assert!(has(&s, "could not run /opt/hw/hc"));
    assert_eq!(r.sleeps.get(), 0);
}

#[test]
fn hc_that_exits_non_zero_still_waits_for_core() {
    let r = Rig::new();
    *r.runner_result.borrow_mut() = Some(Ok(1));
    r.up.set(false);
    // Core comes up on its own two seconds later.
    let s = r.run_with(&|n| {
        if n == 2 {
            r.up.set(true);
        }
    });
    assert!(!s.core_started);
    assert!(has(&s, "exited with 1"));
    assert_eq!(r.sleeps.get(), 2);
}

// ---- hc lookup order ----

fn env_with_path(path: &str) -> BTreeMap<String, String> {
    let mut e = BTreeMap::new();
    e.insert("PATH".to_string(), path.to_string());
    e
}

#[test]
fn hc_lookup_order() {
    let home = Path::new("/home/u");
    let exe = Path::new("/opt/hw/herdr-wake");
    let p = FakeProbes::new();
    let env = env_with_path("/a:/b");
    for path in ["/opt/hw/hc", "/home/u/.local/bin/hc", "/a/hc", "/b/hc"] {
        p.executable(path);
    }
    let find = |env: &BTreeMap<String, String>| boot::find_hc(env, home, Some(exe), &p);
    assert_eq!(find(&env).unwrap(), PathBuf::from("/opt/hw/hc"));
    p.remove("/opt/hw/hc");
    assert_eq!(find(&env).unwrap(), PathBuf::from("/home/u/.local/bin/hc"));
    p.remove("/home/u/.local/bin/hc");
    assert_eq!(find(&env).unwrap(), PathBuf::from("/a/hc"));
    p.remove("/a/hc");
    p.file("/a/hc", "not executable");
    assert_eq!(find(&env).unwrap(), PathBuf::from("/b/hc"));
    p.remove("/b/hc");
    assert!(find(&env).unwrap_err().contains("hc not found"));
}

#[test]
fn hc_override_naming_a_missing_file_is_not_passed_over() {
    let p = FakeProbes::new();
    p.executable("/home/u/.local/bin/hc");
    let mut env = env_with_path("/usr/bin");
    env.insert("HERDR_WAKE_HC".into(), "/gone/hc".into());
    let err = boot::find_hc(&env, Path::new("/home/u"), None, &p).unwrap_err();
    assert!(err.contains("/gone/hc"), "{err}");
}

// ---- network wait ----

#[test]
fn waits_for_the_network_before_starting_core() {
    let r = Rig::new();
    r.network_after.set(3);
    let s = r.run();
    assert!(has(&s, "network came up after 3 s"), "{:?}", s.details);
    assert!(s.core_started);
}

#[test]
fn starts_core_anyway_after_30_s_without_network() {
    let r = Rig::new();
    r.network_after.set(u32::MAX);
    let s = r.run();
    assert_eq!(r.sleeps.get(), 30);
    assert!(has(&s, "no network address after 30 s"));
    assert!(s.core_started);
}

#[test]
fn no_network_wait_when_core_already_runs() {
    let r = Rig::new();
    r.up.set(true);
    r.network_after.set(u32::MAX);
    r.run();
    assert_eq!(r.sleeps.get(), 0);
}

// ---- settling and counts ----

#[test]
fn counts_resumed_failed_and_skipped() {
    let mut r = Rig::new();
    r.f.cfg.exclude_dirs = vec![PathBuf::from("/secret")];
    r.running("t1", 50, true); // resumed
    let mut failed = sample_record();
    failed.state = State::Failed {
        reason: "session not found".into(),
    };
    r.put("t2", &failed);
    let mut ended = sample_record();
    ended.state = State::Ended;
    r.put("t3", &ended);
    let mut spent = sample_record();
    spent.attempts = 3;
    r.put("t4", &spent); // out of attempts: failed
    r.core.borrow_mut().insert(Terminal::new("t5")); // no record: ignored
    let mut excluded = sample_record();
    excluded.cwd = PathBuf::from("/secret/x");
    r.put("t6", &excluded); // skipped
    let s = r.run();
    assert_eq!(
        (s.resumed, s.failed, s.skipped),
        (1, 2, 2),
        "{:?}",
        s.details
    );
    assert!(has(&s, "t1 claude: resumed"));
    assert!(has(&s, "t2 claude: failed: session not found"));
    assert!(has(&s, "t4 claude: gave up after 3 attempts"));
    assert!(has(&s, "t6 claude: skipped (folder excluded)"));
    assert_eq!(r.sleeps.get(), 0, "nothing was left to wait for");
    assert_eq!(boot::render_summary(&s), "1 resumed, 2 failed, 2 skipped");
}

#[test]
fn an_agent_from_the_previous_boot_does_not_count_as_resumed() {
    let r = Rig::new();
    r.f.platform.set_boot_id("boot-2");
    r.running("t1", 50, true); // recorded on boot-1
    r.up.set(true);
    // Resumed on poll 4: the agent's hook re-stamps the record on boot-2.
    let s = r.run_with(&|n| {
        if n == 4 {
            let mut rec = r.running("t1", 60, true);
            rec.boot_id = "boot-2".into();
            r.put("t1", &rec);
        }
    });
    assert_eq!(s.resumed, 1);
    assert_eq!(r.sleeps.get(), 4);
}

#[test]
fn waits_until_every_record_resumes() {
    let r = Rig::new();
    r.running("t1", 50, false);
    r.running("t2", 51, false);
    let s = r.run_with(&|n| {
        if n == 3 {
            r.f.platform
                .add_process(ProcInfo::new(50, 1, 9, vec!["claude".into()]));
        }
        if n == 6 {
            r.f.platform
                .add_process(ProcInfo::new(51, 1, 9, vec!["claude".into()]));
        }
    });
    assert_eq!(s.resumed, 2);
    assert_eq!(r.sleeps.get(), 6);
    assert!(s.finished_ms > s.started_ms);
}

#[test]
fn times_out_after_120_s_and_counts_the_stragglers_as_failed() {
    let r = Rig::new();
    r.up.set(true);
    r.running("t1", 50, true);
    r.running("t2", 51, false);
    let s = r.run();
    assert_eq!(r.sleeps.get(), 120);
    assert_eq!((s.resumed, s.failed), (1, 1));
    assert!(has(&s, "timed out after 120 s with 1 still waiting"));
    assert!(has(&s, "t2 claude: did not come back within 120 s"));
}

#[test]
fn core_that_never_answers_is_reported() {
    let r = Rig::new();
    *r.runner_result.borrow_mut() = Some(Ok(0)); // exits 0 but core stays down
    let s = r.run();
    assert!(s.core_started);
    assert_eq!(r.sleeps.get(), 120);
    assert!(
        has(&s, "core did not answer within 120 s"),
        "{:?}",
        s.details
    );
}

#[test]
fn disabled_herdr_wake_skips_running_records() {
    let mut r = Rig::new();
    r.f.cfg.enabled = false;
    r.running("t1", 50, false);
    let s = r.run();
    assert_eq!((s.resumed, s.failed, s.skipped), (0, 0, 1));
    assert_eq!(r.sleeps.get(), 0);
}

// ---- last-boot.json and rendering ----

#[test]
fn last_boot_json_is_written_atomically_and_round_trips() {
    let r = Rig::new();
    r.running("t1", 50, true);
    let s = r.run();
    let text = std::fs::read_to_string(r.last_boot()).unwrap();
    let back: BootSummary = serde_json::from_str(&text).unwrap();
    assert_eq!(back, s);
    let dir = r.last_boot().parent().unwrap().to_path_buf();
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["herdr-wake.log", "last-boot.json", "records"],
        "no temporary file is left"
    );
}

#[test]
fn render_summary_is_one_line() {
    let s = BootSummary {
        boot_id: "b".into(),
        started_ms: 0,
        finished_ms: 1,
        duration_ms: 1,
        core_started: true,
        resumed: 9,
        failed: 1,
        skipped: 0,
        unconfirmed: 0,
        details: vec!["x".into()],
    };
    assert_eq!(boot::render_summary(&s), "9 resumed, 1 failed");
    let s = BootSummary {
        unconfirmed: 2,
        skipped: 1,
        ..s
    };
    assert_eq!(
        boot::render_summary(&s),
        "9 resumed, 1 failed, 2 unconfirmed, 1 skipped"
    );
}

// ---- boot.env ----

#[test]
fn parse_env_handles_quotes_blanks_and_comments_without_expansion() {
    let text = r#"
# captured by herdr-wake install

PATH=/Users/u/.local/bin:/opt/homebrew/bin:/usr/bin
LANG="en_US.UTF-8"
  LC_ALL = 'C.UTF-8'
export SHELL=/bin/zsh
TMPDIR="/var/folders/x y/T/"
LITERAL='$HOME and $(whoami)'
ESCAPED="a \"quoted\" \$HOME"
EXPANDED=$HOME/bin
EMPTY=
=novalue
not a pair
1BAD=x
"#;
    let env = herdr_wake::setup::parse_boot_env(text);
    let get = |k: &str| env.get(k).map(String::as_str);
    assert_eq!(
        get("PATH"),
        Some("/Users/u/.local/bin:/opt/homebrew/bin:/usr/bin")
    );
    assert_eq!(get("LANG"), Some("en_US.UTF-8"));
    assert_eq!(get("LC_ALL"), Some("C.UTF-8"));
    assert_eq!(get("SHELL"), Some("/bin/zsh"));
    assert_eq!(get("TMPDIR"), Some("/var/folders/x y/T/"));
    assert_eq!(get("LITERAL"), Some("$HOME and $(whoami)"));
    assert_eq!(get("ESCAPED"), Some("a \"quoted\" $HOME"));
    assert_eq!(get("EXPANDED"), Some("$HOME/bin"));
    assert_eq!(get("EMPTY"), Some(""));
    assert_eq!(env.len(), 9, "{env:?}");
}

#[test]
fn child_env_puts_the_process_on_top_and_merges_path() {
    let boot_env = herdr_wake::setup::parse_boot_env(
        "PATH=/home/u/.local/bin:/usr/bin\nLANG=en_US.UTF-8\nUSER=u\n",
    );
    let mut process = BTreeMap::new();
    process.insert("PATH".to_string(), "/usr/bin:/bin:/usr/sbin".to_string());
    process.insert("LANG".to_string(), "C".to_string());
    process.insert("HOME".to_string(), "/home/u".to_string());
    let env = boot::child_env(&process, &boot_env);
    assert_eq!(env["PATH"], "/home/u/.local/bin:/usr/bin:/bin:/usr/sbin");
    assert_eq!(env["LANG"], "C");
    assert_eq!(env["USER"], "u");
    assert_eq!(env["HOME"], "/home/u");
}

#[test]
fn child_env_without_a_boot_path_keeps_the_process_path() {
    let mut process = BTreeMap::new();
    process.insert("PATH".to_string(), "/usr/bin".to_string());
    let env = boot::child_env(&process, &BTreeMap::new());
    assert_eq!(env["PATH"], "/usr/bin");
}

#[test]
fn network_up_reads_the_interfaces_without_crashing() {
    // The answer depends on the machine; this only exercises the FFI walk.
    let _ = boot::network_up();
}

// ------------------------------------------------- Task 13: unconfirmed

#[test]
fn an_unconfirmed_resume_is_counted_and_not_waited_for() {
    let r = Rig::new();
    r.up.set(true);
    r.running("t1", 50, true);
    r.running("t2", 51, false);
    let key = herdr_wake::gate::boot_key(&r.f.platform);
    let mark = herdr_wake::wake::unconfirmed_file(
        &r.f.env,
        &r.f.home,
        &key,
        &herdr_wake::core::TerminalId::from("t2"),
    );
    std::fs::create_dir_all(mark.parent().unwrap()).unwrap();
    std::fs::write(&mark, "the agent did not report\n").unwrap();
    let s = r.run();
    assert_eq!(r.sleeps.get(), 0, "nothing left to wait for");
    assert_eq!((s.resumed, s.failed, s.unconfirmed), (1, 0, 1));
    assert!(has(&s, "t2 claude: unconfirmed: the agent did not report"));
    assert_eq!(
        boot::render_summary(&s),
        "1 resumed, 0 failed, 1 unconfirmed"
    );
}

#[test]
fn attempts_used_up_on_an_earlier_boot_do_not_count_as_given_up() {
    let r = Rig::new();
    r.up.set(true);
    let mut rec = sample_record();
    rec.boot_id = "an-earlier-boot".into();
    rec.attempts = 3;
    rec.agent_proc = None;
    r.put("t1", &rec);
    let s = r.run();
    assert_eq!(s.failed, 1);
    assert!(has(&s, "t1 claude: did not come back within 120 s"));
    assert!(!has(&s, "gave up after"));
}

// ---- untrusted records (M-5 e) ----

#[test]
fn an_untrusted_record_is_counted_as_failed_and_not_waited_for() {
    let r = Rig::new();
    let mut rec = sample_record();
    rec.agent_proc = None;
    r.put_label_only("t1", &rec);
    let s = r.run();
    assert_eq!((s.resumed, s.failed, s.skipped), (0, 1, 0));
    assert!(
        s.details
            .iter()
            .any(|d| d.contains("t1 claude: untrusted: record does not match the local copy")),
        "{:?}",
        s.details
    );
    assert_eq!(r.sleeps.get(), 0, "nothing to wait for");
}

// ---- the boot run logs for itself (the LaunchDaemon has no stdout file) ----

#[test]
fn the_boot_run_writes_its_summary_to_herdr_wakes_own_log() {
    let r = Rig::new();
    r.running("t1", 50, true);
    let s = r.run();
    let log = std::fs::read_to_string(herdr_wake::paths::log_file(&r.f.env, &r.f.home)).unwrap();
    assert!(
        log.contains(&format!("boot: {}", boot::render_summary(&s))),
        "{log}"
    );
    for d in &s.details {
        assert!(log.contains(d.as_str()), "{d} missing from {log}");
    }
}

// ---- L-10: record text in the boot summary ----

#[test]
fn boot_details_carry_no_control_characters_from_a_record() {
    let r = Rig::new();
    let mut rec = sample_record();
    rec.state = State::Failed {
        reason: "bad\u{1b}[31m\nt9 claude: resumed".into(),
    };
    r.put("t1", &rec);
    let s = r.run();
    let line = s.details.iter().find(|d| d.contains("failed")).unwrap();
    assert_eq!(line, "t1 claude: failed: bad[31mt9 claude: resumed");
}

// ---- exclude_dirs with a tilde (I-13) ----

#[test]
fn a_record_in_a_tilde_excluded_folder_is_skipped_not_waited_for() {
    let mut r = Rig::new();
    r.f.cfg.exclude_dirs = vec![PathBuf::from("~/scratch")];
    let mut rec = sample_record();
    rec.agent_proc = None;
    rec.cwd = r.f.home.join("scratch/x");
    r.put("t1", &rec);
    let s = r.run();
    assert_eq!(
        (s.resumed, s.failed, s.skipped),
        (0, 0, 1),
        "{:?}",
        s.details
    );
    assert!(s
        .details
        .iter()
        .any(|d| d == "t1 claude: skipped (folder excluded)"));
    assert_eq!(r.sleeps.get(), 0, "nothing to wait for");
}

// ---- a clock that steps back during boot (real Linux reboot, 2026-11-10) ----

#[test]
fn a_wall_clock_stepped_back_during_boot_still_gives_a_sane_summary() {
    let r = Rig::new();
    r.network_after.set(3);
    let wall = r.f.platform.now_ms();
    // chronyd steps the clock back 130 s while boot waits for the network.
    let s = r.run_with(&|n| {
        if n == 1 {
            r.f.platform.set_now(wall - 130_000 + 1000);
        }
    });
    assert_eq!(
        s.started_ms, wall,
        "the wall clock, as it read at the start"
    );
    assert_eq!(
        s.duration_ms, 3000,
        "three one-second waits, by the monotonic clock"
    );
    assert_eq!(s.finished_ms, s.started_ms + 3000, "never before the start");
    // Round trip, and an older file without the field still loads.
    let text = std::fs::read_to_string(r.last_boot()).unwrap();
    assert!(text.contains("\"duration_ms\": 3000"), "{text}");
    let old = text.replace("  \"duration_ms\": 3000,\n", "");
    assert_ne!(old, text);
    let back: BootSummary = serde_json::from_str(&old).unwrap();
    assert_eq!(back.duration_ms, 0);
}
