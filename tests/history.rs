//! The history journal: file layout and private-file rules, the ending
//! rules, the cap, and what the hook writes into it.

mod common;

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use common::{sample_record, Fixture};
use herdr_wake::agents::{Agent, Ctx, Edit, Observation, Plan};
use herdr_wake::config::Config;
use herdr_wake::core::{Terminal, TerminalId};
use herdr_wake::history::{self, Ending, Entry};
use herdr_wake::hook::{run_with, HookOutcome};
use herdr_wake::paths;
use herdr_wake::platform::{Platform, ProcInfo};
use herdr_wake::record::{self, AgentKind, Record};

const SID: &str = "11111111-2222-4333-8444-555555555555";
const OTHER: &str = "99999999-2222-4333-8444-555555555555";

fn dir() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("history");
    (t, d)
}

fn rec(id: &str) -> Record {
    let mut r = sample_record();
    r.session_id = id.to_string();
    r
}

fn tid() -> TerminalId {
    TerminalId::from("t1")
}

#[test]
fn started_writes_a_private_file_named_after_agent_and_id() {
    let (_t, d) = dir();
    history::started(&d, 500, &tid(), &rec(SID), 1000).unwrap();

    let file = history::file(&d, AgentKind::Claude, SID);
    assert_eq!(
        file.file_name().unwrap().to_string_lossy(),
        format!("claude-{SID}.json")
    );
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&d), 0o700);
    assert_eq!(mode(&file), 0o600);

    let e = history::load(&d, AgentKind::Claude, SID).unwrap();
    assert_eq!(e.session_id, SID);
    assert_eq!(e.cwd, PathBuf::from("/home/u/proj"));
    assert_eq!(e.flags, vec!["--model".to_string(), "opus".to_string()]);
    assert_eq!(e.terminal.as_deref(), Some("t1"));
    assert_eq!(
        (e.started_ms, e.ended_ms, e.ending),
        (1000, None, Ending::Open)
    );
    // No stray temporary files.
    let names: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().collect();
    assert_eq!(names.len(), 1);
}

#[test]
fn the_environment_is_scrubbed_and_no_secret_is_written() {
    let (_t, d) = dir();
    let mut r = rec(SID);
    r.env
        .insert("ANTHROPIC_API_KEY".into(), "not-a-real-key".into());
    r.env
        .insert("GITHUB_TOKEN".into(), "not-a-real-token".into());
    r.env
        .insert("CLAUDE_CONFIG_DIR".into(), "/home/u/.cc".into());
    history::started(&d, 500, &tid(), &r, 1).unwrap();

    let text = std::fs::read_to_string(history::file(&d, AgentKind::Claude, SID)).unwrap();
    assert!(!text.contains("not-a-real"), "{text}");
    assert!(
        !text.contains("API_KEY") && !text.contains("GITHUB"),
        "{text}"
    );
    let e = history::load(&d, AgentKind::Claude, SID).unwrap();
    assert!(e.env.contains_key("CLAUDE_CONFIG_DIR"));
    assert!(e.env.contains_key("PATH"));
}

#[test]
fn a_restart_keeps_the_first_start_time_and_reopens_the_entry() {
    let (_t, d) = dir();
    history::started(&d, 500, &tid(), &rec(SID), 1000).unwrap();
    history::ended(&d, 500, AgentKind::Claude, SID, None, Ending::Quit, 2000).unwrap();
    let e = history::load(&d, AgentKind::Claude, SID).unwrap();
    assert_eq!((e.ended_ms, e.ending), (Some(2000), Ending::Quit));

    let mut again = rec(SID);
    again.flags = vec!["--model".into(), "sonnet".into()];
    history::started(&d, 500, &TerminalId::from("t5"), &again, 3000).unwrap();
    let e = history::load(&d, AgentKind::Claude, SID).unwrap();
    assert_eq!(e.started_ms, 1000);
    assert_eq!((e.ended_ms, e.ending), (None, Ending::Open));
    assert_eq!(e.flags, again.flags);
    assert_eq!(e.terminal.as_deref(), Some("t5"));
}

#[test]
fn the_first_definite_ending_wins_but_unknown_can_be_upgraded() {
    let (_t, d) = dir();
    let r = rec(SID);
    history::started(&d, 500, &tid(), &r, 1000).unwrap();
    let end = |ending, at| history::ended(&d, 500, AgentKind::Claude, SID, None, ending, at);

    end(Ending::Unknown, 2000).unwrap();
    assert_eq!(
        history::load(&d, AgentKind::Claude, SID).unwrap().ending,
        Ending::Unknown
    );
    end(Ending::Quit, 3000).unwrap();
    let e = history::load(&d, AgentKind::Claude, SID).unwrap();
    assert_eq!((e.ending, e.ended_ms), (Ending::Quit, Some(3000)));

    end(Ending::Forgotten, 4000).unwrap();
    let e = history::load(&d, AgentKind::Claude, SID).unwrap();
    assert_eq!((e.ending, e.ended_ms), (Ending::Quit, Some(3000)));
}

#[test]
fn an_end_with_no_entry_is_made_from_the_fallback_record_only() {
    let (_t, d) = dir();
    let r = rec(SID);
    history::ended(&d, 500, AgentKind::Claude, SID, None, Ending::Quit, 5).unwrap();
    assert!(history::read_all(&d).is_empty());

    // A record of another session is not used.
    history::ended(
        &d,
        500,
        AgentKind::Claude,
        OTHER,
        Some((&tid(), &r)),
        Ending::Quit,
        5,
    )
    .unwrap();
    assert!(history::read_all(&d).is_empty());

    history::ended(
        &d,
        500,
        AgentKind::Claude,
        SID,
        Some((&tid(), &r)),
        Ending::Forgotten,
        7,
    )
    .unwrap();
    let e = history::load(&d, AgentKind::Claude, SID).unwrap();
    assert_eq!((e.ending, e.ended_ms), (Ending::Forgotten, Some(7)));
    assert_eq!(e.cwd, r.cwd);
}

#[test]
fn the_cap_removes_the_oldest_entries_and_never_the_new_one() {
    let (_t, d) = dir();
    for (i, id) in ["a1", "a2", "a3", "a4", "a5"].iter().enumerate() {
        history::started(&d, 3, &tid(), &rec(id), 100 + i as u64).unwrap();
    }
    let mut ids: Vec<String> = history::read_all(&d)
        .into_iter()
        .map(|e| e.session_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["a3", "a4", "a5"]);

    // An old session that ends now is the newest by last activity.
    history::ended(&d, 3, AgentKind::Claude, "a3", None, Ending::Quit, 900).unwrap();
    history::started(&d, 3, &tid(), &rec("a6"), 500).unwrap();
    let mut ids: Vec<String> = history::read_all(&d)
        .into_iter()
        .map(|e| e.session_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["a3", "a5", "a6"]);
}

#[test]
fn files_that_are_not_entries_are_neither_counted_nor_pruned() {
    let (_t, d) = dir();
    for (i, id) in ["a1", "a2"].iter().enumerate() {
        history::started(&d, 2, &tid(), &rec(id), 100 + i as u64).unwrap();
    }
    // Things a newer build, or something else, may have put there.
    std::fs::write(d.join("claude-bad.json"), "{not json").unwrap();
    std::fs::write(d.join("claude-big.json"), vec![b' '; 300 * 1024]).unwrap();
    let newer = std::fs::read_to_string(history::file(&d, AgentKind::Claude, "a1"))
        .unwrap()
        .replace("\"v\":1", "\"v\":2")
        .replace("a1", "zz");
    std::fs::write(d.join("claude-zz.json"), newer).unwrap();

    history::started(&d, 2, &tid(), &rec("a3"), 300).unwrap();

    let mut ids: Vec<String> = history::read_all(&d)
        .into_iter()
        .map(|e| e.session_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["a2", "a3"], "only the oldest readable entry went");
    for f in ["claude-bad.json", "claude-big.json", "claude-zz.json"] {
        assert!(d.join(f).exists(), "{f} was removed");
    }
}

#[test]
fn stale_temporary_files_are_removed_after_an_hour() {
    let (_t, d) = dir();
    history::started(&d, 500, &tid(), &rec("a1"), 1).unwrap();
    let old = d.join(".claude-a1.json.tmp-123-0");
    let fresh = d.join(".claude-a1.json.tmp-124-0");
    for f in [&old, &fresh] {
        std::fs::write(f, "partial").unwrap();
    }
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(two_hours_ago)
        .unwrap();

    history::started(&d, 500, &tid(), &rec("a2"), 2).unwrap();

    assert!(!old.exists(), "a stale temporary file stays");
    assert!(fresh.exists(), "a fresh one may be a write in progress");
}

#[test]
fn a_limit_of_zero_writes_no_journal() {
    let (_t, d) = dir();
    history::started(&d, 0, &tid(), &rec(SID), 1).unwrap();
    history::ended(
        &d,
        0,
        AgentKind::Claude,
        SID,
        Some((&tid(), &rec(SID))),
        Ending::Quit,
        2,
    )
    .unwrap();
    assert!(!d.exists());
}

#[test]
fn the_journal_never_writes_through_a_symlink() {
    let (t, d) = dir();
    let elsewhere = t.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();

    // A symlinked journal folder.
    symlink(&elsewhere, &d).unwrap();
    assert!(history::started(&d, 500, &tid(), &rec(SID), 1).is_err());
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    std::fs::remove_file(&d).unwrap();

    // A symlink where an entry's file should be.
    history::started(&d, 500, &tid(), &rec("other"), 1).unwrap();
    let target = elsewhere.join("target.json");
    std::fs::write(&target, "keep").unwrap();
    symlink(&target, history::file(&d, AgentKind::Claude, SID)).unwrap();
    assert!(history::started(&d, 500, &tid(), &rec(SID), 1).is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
}

#[test]
fn unreadable_files_are_skipped() {
    let (t, d) = dir();
    history::started(&d, 500, &tid(), &rec(SID), 1).unwrap();
    let good = history::file(&d, AgentKind::Claude, SID);
    let text = std::fs::read_to_string(&good).unwrap();

    std::fs::write(d.join("claude-bad.json"), "{not json").unwrap();
    // A good entry under the wrong name.
    std::fs::write(d.join("claude-renamed.json"), &text).unwrap();
    // Too big.
    std::fs::write(d.join("claude-big.json"), vec![b' '; 300 * 1024]).unwrap();
    // Another schema version.
    std::fs::write(
        d.join(format!("claude-{OTHER}.json")),
        text.replace("\"v\":1", "\"v\":2").replace(SID, OTHER),
    )
    .unwrap();
    // A symlink to a good file.
    let linked = t.path().join("linked.json");
    std::fs::write(&linked, &text).unwrap();
    symlink(&linked, d.join("claude-linked.json")).unwrap();

    let all = history::read_all(&d);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].session_id, SID);
}

#[test]
fn an_entry_turns_back_into_the_record_it_came_from() {
    let r = rec(SID);
    let e = Entry::from_record(Some(&tid()), &r, 9);
    let back = e.to_record();
    assert_eq!(back.agent, r.agent);
    assert_eq!(back.session_id, r.session_id);
    assert_eq!(back.cwd, r.cwd);
    assert_eq!(back.flags, r.flags);
    assert_eq!(back.env, r.env);
    assert_eq!(back.bin, r.bin);
    assert_eq!(back.session_file, r.session_file);
}

// ------------------------------------------------------------ the hook

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
        argv.iter().skip(1).cloned().collect()
    }
    fn resume_plan(&self, _rec: &Record, _ctx: &mut Ctx) -> Plan {
        Plan::Permanent("test".into())
    }
    fn integration(&self, _bin: &Path, _ctx: &mut Ctx) -> Vec<Edit> {
        Vec::new()
    }
}

fn started(id: &str) -> TestAgent {
    TestAgent {
        obs: Observation::Started {
            session_id: id.to_string(),
            session_file: Some(PathBuf::from("/home/u/.claude/projects/p/s.jsonl")),
            cwd: PathBuf::from("/home/u/proj"),
            terminal: None,
            agent_pid: Some(500),
            argv: None,
        },
    }
}

fn ended(id: &str, deliberate: Option<bool>) -> TestAgent {
    TestAgent {
        obs: Observation::Ended {
            session_id: id.to_string(),
            deliberate,
        },
    }
}

fn fixture() -> Fixture {
    let mut f = Fixture::temp();
    f.core.insert(Terminal::new("t1"));
    f.env.insert("HC_TERMINAL_ID".into(), "t1".into());
    f.env
        .insert("HC_TERMINAL_SOCKET".into(), "/tmp/hc/c.sock".into());
    let mut agent = ProcInfo::new(
        500,
        400,
        50,
        vec!["claude".into(), "--model".into(), "opus".into()],
    );
    agent.exe = PathBuf::from("/opt/claude/bin/claude");
    f.platform.add_process(agent);
    f
}

fn hook(f: &mut Fixture, agent: &TestAgent) -> HookOutcome {
    let mut ctx = f.ctx();
    run_with(agent, "x", b"{}", &mut ctx, &|_, _, _| {})
}

fn journal(f: &Fixture, id: &str) -> Option<Entry> {
    history::load(&paths::history_dir(&f.env, &f.home), AgentKind::Claude, id)
}

#[test]
fn a_recorded_start_is_journaled_open() {
    let mut f = fixture();
    assert_eq!(hook(&mut f, &started(SID)), HookOutcome::Recorded);

    let e = journal(&f, SID).expect("entry");
    assert_eq!(e.ending, Ending::Open);
    assert_eq!(e.started_ms, f.platform.now_ms());
    assert_eq!(e.terminal.as_deref(), Some("t1"));
    assert_eq!(e.flags, vec!["--model".to_string(), "opus".to_string()]);
    assert_eq!(
        e.session_file,
        Some(PathBuf::from("/home/u/.claude/projects/p/s.jsonl"))
    );
}

#[test]
fn a_quit_ends_the_entry_and_a_new_session_in_the_terminal_replaces_the_old() {
    let mut f = fixture();
    hook(&mut f, &started(SID));
    // /clear: a new session id arrives in the same terminal.
    f.platform.advance(1000);
    hook(&mut f, &started(OTHER));
    let old = journal(&f, SID).unwrap();
    assert_eq!(old.ending, Ending::Replaced);
    assert!(old.ended_ms.is_some());
    assert_eq!(journal(&f, OTHER).unwrap().ending, Ending::Open);

    f.platform.advance(1000);
    assert_eq!(hook(&mut f, &ended(OTHER, Some(true))), HookOutcome::Ended);
    let e = journal(&f, OTHER).unwrap();
    assert_eq!(e.ending, Ending::Quit);
    assert!(e.ended_ms.unwrap() > e.started_ms);
    // The replaced one is untouched by the later end.
    assert_eq!(journal(&f, SID).unwrap().ending, Ending::Replaced);
}

#[test]
fn a_kill_or_an_unknown_end_is_journaled_but_leaves_the_record() {
    for (deliberate, want) in [(Some(false), Ending::Killed), (None, Ending::Unknown)] {
        let mut f = fixture();
        hook(&mut f, &started(SID));
        let out = hook(&mut f, &ended(SID, deliberate));
        assert!(matches!(out, HookOutcome::Ignored(_)), "{out:?}");
        assert_eq!(journal(&f, SID).unwrap().ending, want);
        let stored = record::load(&mut f.core, &tid()).unwrap().unwrap();
        assert_eq!(stored.state, record::State::Running);
    }
}

#[test]
fn an_end_for_a_session_the_journal_never_saw_is_made_from_the_record() {
    let mut f = fixture();
    // A record from before the journal existed.
    let mut r = rec(SID);
    r.seq = 1;
    record::save(&mut f.core, &tid(), &r).unwrap();
    assert!(journal(&f, SID).is_none());

    assert_eq!(hook(&mut f, &ended(SID, Some(true))), HookOutcome::Ended);
    let e = journal(&f, SID).expect("entry made at the end");
    assert_eq!(e.ending, Ending::Quit);
    assert_eq!(e.flags, r.flags);
}

#[test]
fn a_journal_that_cannot_be_written_does_not_fail_the_hook() {
    let mut f = fixture();
    let d = paths::history_dir(&f.env, &f.home);
    std::fs::create_dir_all(d.parent().unwrap()).unwrap();
    std::fs::write(&d, "a file where the folder should be").unwrap();

    assert_eq!(hook(&mut f, &started(SID)), HookOutcome::Recorded);
    let log = std::fs::read_to_string(paths::log_file(&f.env, &f.home)).unwrap();
    assert!(log.contains("history not written"), "{log}");
}

#[test]
fn history_limit_zero_in_the_config_keeps_no_journal() {
    let mut f = fixture();
    f.cfg.history_limit = 0;
    assert_eq!(hook(&mut f, &started(SID)), HookOutcome::Recorded);
    assert!(!paths::history_dir(&f.env, &f.home).exists());
}

#[test]
fn the_default_cap_is_500() {
    assert_eq!(Config::default().history_limit, 500);
    let parsed: Config = toml_edit::de::from_str("history_limit = 20\n").unwrap();
    assert_eq!(parsed.history_limit, 20);
}
