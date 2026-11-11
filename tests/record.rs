//! Record store: load, save, end, fail, attempts, forget, claims, env scrub
//! and sequence numbers, driven through `FakeCore` and temporary folders.

mod common;

use std::collections::BTreeMap;

use herdr_wake::core::fake::FakeCore;
use herdr_wake::core::{CoreError, Terminal, TerminalId};
use herdr_wake::record::{self, ProcRef, State, LABEL};

use common::sample_record;

fn core_with(t: &str) -> (FakeCore, TerminalId) {
    let mut core = FakeCore::new();
    core.insert(Terminal::new(t));
    (core, TerminalId::from(t))
}

fn raw_label(core: &FakeCore, t: &TerminalId) -> Option<String> {
    core.terminal(t).unwrap().labels.get(LABEL).cloned()
}

fn set_raw_label(core: &mut FakeCore, t: &TerminalId, value: &str) {
    core.terminals
        .get_mut(t)
        .unwrap()
        .labels
        .insert(LABEL.to_string(), value.to_string());
}

// ---- load and save ----

#[test]
fn save_then_load_round_trips_the_record() {
    let (mut core, t) = core_with("t1");
    let rec = sample_record();
    assert!(record::save(&mut core, &t, &rec).unwrap());
    assert_eq!(record::load(&mut core, &t).unwrap(), Some(rec));
}

#[test]
fn the_label_is_compact_json_under_the_wake_key() {
    let (mut core, t) = core_with("t1");
    record::save(&mut core, &t, &sample_record()).unwrap();
    let raw = raw_label(&core, &t).unwrap();
    assert!(raw.starts_with("{\"v\":1,"), "{raw}");
    assert!(!raw.contains('\n') && !raw.contains(": "), "{raw}");
}

#[test]
fn load_without_a_label_is_none() {
    let (mut core, t) = core_with("t1");
    assert_eq!(record::load(&mut core, &t).unwrap(), None);
}

#[test]
fn unknown_version_loads_as_none_without_error() {
    let (mut core, t) = core_with("t1");
    let mut json = serde_json::to_value(sample_record()).unwrap();
    json["v"] = serde_json::json!(2);
    set_raw_label(&mut core, &t, &json.to_string());
    assert_eq!(record::load(&mut core, &t).unwrap(), None);
}

#[test]
fn malformed_json_loads_as_none_without_error() {
    let (mut core, t) = core_with("t1");
    set_raw_label(&mut core, &t, "{not json");
    assert_eq!(record::load(&mut core, &t).unwrap(), None);
    set_raw_label(&mut core, &t, "{\"v\":1}");
    assert_eq!(record::load(&mut core, &t).unwrap(), None);
}

#[test]
fn load_passes_core_errors_through() {
    let (mut core, t) = core_with("t1");
    core.fail_with(Some(CoreError::Timeout));
    assert_eq!(record::load(&mut core, &t), Err(CoreError::Timeout));
    core.fail_with(None);
    assert_eq!(
        record::load(&mut core, &TerminalId::from("nope")),
        Err(CoreError::NotFound)
    );
}

#[test]
fn save_with_an_older_seq_is_rejected_and_writes_nothing() {
    let (mut core, t) = core_with("t1");
    let newer = sample_record(); // seq 7
    record::save(&mut core, &t, &newer).unwrap();
    let before = raw_label(&core, &t);

    let mut stale = sample_record();
    stale.seq = 6;
    stale.session_id = "old".to_string();
    assert!(!record::save(&mut core, &t, &stale).unwrap());
    assert_eq!(raw_label(&core, &t), before);
}

#[test]
fn save_with_an_equal_or_newer_seq_is_written() {
    let (mut core, t) = core_with("t1");
    record::save(&mut core, &t, &sample_record()).unwrap();

    let mut same = sample_record();
    same.attempts = 1;
    assert!(record::save(&mut core, &t, &same).unwrap());
    assert_eq!(record::load(&mut core, &t).unwrap().unwrap().attempts, 1);

    let mut newer = sample_record();
    newer.seq = 8;
    newer.session_id = "new".to_string();
    assert!(record::save(&mut core, &t, &newer).unwrap());
    assert_eq!(record::load(&mut core, &t).unwrap(), Some(newer));
}

#[test]
fn save_respects_the_seq_of_a_record_it_cannot_read() {
    // A newer herdr-wake wrote v2 with a higher seq: an older writer must
    // not clobber it.
    let (mut core, t) = core_with("t1");
    let mut json = serde_json::to_value(sample_record()).unwrap();
    json["v"] = serde_json::json!(2);
    json["seq"] = serde_json::json!(100);
    set_raw_label(&mut core, &t, &json.to_string());
    assert!(!record::save(&mut core, &t, &sample_record()).unwrap());

    // Garbage with no readable seq is overwritten.
    set_raw_label(&mut core, &t, "garbage");
    assert!(record::save(&mut core, &t, &sample_record()).unwrap());
}

// ---- sequence numbers ----

#[test]
fn next_seq_is_the_clock_when_it_is_ahead() {
    assert_eq!(record::next_seq(1_000, None), 1_000);
    assert_eq!(record::next_seq(1_000, Some(500)), 1_000);
}

#[test]
fn next_seq_stays_monotonic_when_the_clock_is_behind() {
    assert_eq!(record::next_seq(1_000, Some(1_000)), 1_001);
    assert_eq!(record::next_seq(5, Some(2_000)), 2_001);
    assert_eq!(record::next_seq(0, Some(u64::MAX)), u64::MAX);
}

// ---- end, fail, attempts ----

fn proc(pid: u32, start: u64) -> ProcRef {
    ProcRef { pid, start }
}

#[test]
fn end_marks_the_matching_record_ended_and_bumps_seq() {
    let (mut core, t) = core_with("t1");
    let rec = sample_record();
    record::save(&mut core, &t, &rec).unwrap();

    assert!(record::end(&mut core, &t, &rec.session_id, rec.agent_proc).unwrap());
    let got = record::load(&mut core, &t).unwrap().unwrap();
    assert_eq!(got.state, State::Ended);
    assert!(got.seq > rec.seq);
    assert_eq!(got.session_id, rec.session_id);
    assert_eq!(got.agent_proc, rec.agent_proc);
}

#[test]
fn end_without_an_agent_proc_checks_only_the_session_id() {
    let (mut core, t) = core_with("t1");
    let rec = sample_record();
    record::save(&mut core, &t, &rec).unwrap();
    assert!(record::end(&mut core, &t, &rec.session_id, None).unwrap());
    assert_eq!(
        record::load(&mut core, &t).unwrap().unwrap().state,
        State::Ended
    );
}

/// Review Focus 2: SessionEnd (`clear`) for the old id arrives after
/// SessionStart for the new id. The newer record must win.
#[test]
fn late_end_for_the_old_session_leaves_the_new_record() {
    let (mut core, t) = core_with("t1");
    let mut old = sample_record();
    old.session_id = "old-session".to_string();
    record::save(&mut core, &t, &old).unwrap();

    let mut new = sample_record();
    new.session_id = "new-session".to_string();
    new.seq = record::next_seq(2_000, Some(old.seq));
    assert!(record::save(&mut core, &t, &new).unwrap());
    let before = raw_label(&core, &t);

    assert!(!record::end(&mut core, &t, "old-session", old.agent_proc).unwrap());
    assert!(!record::end(&mut core, &t, "old-session", None).unwrap());
    assert_eq!(raw_label(&core, &t), before);
    assert_eq!(record::load(&mut core, &t).unwrap(), Some(new));
}

/// Amendment to Task 3: the wrapper's pre-resume copy (old agent pid, old
/// seq) must not end the record the resumed agent re-stamped.
#[test]
fn stale_copy_cannot_end_a_newer_record() {
    let (mut core, t) = core_with("t1");
    let stale = sample_record(); // agent 4242, seq 7
    record::save(&mut core, &t, &stale).unwrap();

    // The resumed agent's hook re-stamps the same session with its own pid.
    let mut fresh = stale.clone();
    fresh.agent_proc = Some(proc(5151, 1_700_000_100_000));
    fresh.seq = record::next_seq(1_700_000_100_000, Some(stale.seq));
    assert!(record::save(&mut core, &t, &fresh).unwrap());
    let before = raw_label(&core, &t);

    // The wrapper ends from its stale copy: no effect.
    assert!(!record::end(&mut core, &t, &stale.session_id, stale.agent_proc).unwrap());
    // Nor can it write the stale copy back.
    let mut ended = stale.clone();
    ended.state = State::Ended;
    assert!(!record::save(&mut core, &t, &ended).unwrap());
    assert_eq!(raw_label(&core, &t), before);
    assert_eq!(record::load(&mut core, &t).unwrap(), Some(fresh.clone()));

    // The current agent process can end it.
    assert!(record::end(&mut core, &t, &fresh.session_id, fresh.agent_proc).unwrap());
    assert_eq!(
        record::load(&mut core, &t).unwrap().unwrap().state,
        State::Ended
    );
}

#[test]
fn end_with_an_agent_proc_does_not_match_a_record_without_one() {
    let (mut core, t) = core_with("t1");
    let mut rec = sample_record();
    rec.agent_proc = None;
    record::save(&mut core, &t, &rec).unwrap();
    assert!(!record::end(&mut core, &t, &rec.session_id, Some(proc(1, 1))).unwrap());
    assert_eq!(
        record::load(&mut core, &t).unwrap().unwrap().state,
        State::Running
    );
}

#[test]
fn end_with_no_record_or_an_unreadable_one_writes_nothing() {
    let (mut core, t) = core_with("t1");
    assert!(!record::end(&mut core, &t, "s", None).unwrap());
    assert_eq!(raw_label(&core, &t), None);

    let unknown = "{\"v\":9,\"session_id\":\"s\",\"seq\":1}";
    set_raw_label(&mut core, &t, unknown);
    assert!(!record::end(&mut core, &t, "s", None).unwrap());
    assert_eq!(raw_label(&core, &t).as_deref(), Some(unknown));
}

#[test]
fn end_passes_core_errors_through() {
    let (mut core, t) = core_with("t1");
    core.fail_with(Some(CoreError::Timeout));
    assert_eq!(
        record::end(&mut core, &t, "s", None),
        Err(CoreError::Timeout)
    );
}

#[test]
fn fail_marks_the_matching_record_failed_with_its_reason() {
    let (mut core, t) = core_with("t1");
    let rec = sample_record();
    record::save(&mut core, &t, &rec).unwrap();

    assert!(!record::fail(&mut core, &t, "other", "folder is gone").unwrap());
    assert_eq!(
        record::load(&mut core, &t).unwrap().unwrap().state,
        State::Running
    );

    assert!(record::fail(&mut core, &t, &rec.session_id, "folder is gone").unwrap());
    let got = record::load(&mut core, &t).unwrap().unwrap();
    assert_eq!(
        got.state,
        State::Failed {
            reason: "folder is gone".to_string()
        }
    );
    assert!(got.seq > rec.seq);
}

#[test]
fn bump_attempts_counts_up_on_the_matching_record() {
    let (mut core, t) = core_with("t1");
    let rec = sample_record();
    record::save(&mut core, &t, &rec).unwrap();

    assert_eq!(
        record::bump_attempts(&mut core, &t, &rec.session_id).unwrap(),
        Some(1)
    );
    assert_eq!(
        record::bump_attempts(&mut core, &t, &rec.session_id).unwrap(),
        Some(2)
    );
    let got = record::load(&mut core, &t).unwrap().unwrap();
    assert_eq!(got.attempts, 2);
    assert_eq!(got.state, State::Running);
    assert!(got.seq > rec.seq);

    assert_eq!(record::bump_attempts(&mut core, &t, "other").unwrap(), None);
    assert_eq!(record::load(&mut core, &t).unwrap().unwrap().attempts, 2);
}

#[test]
fn bump_attempts_saturates() {
    let (mut core, t) = core_with("t1");
    let mut rec = sample_record();
    rec.attempts = u8::MAX;
    record::save(&mut core, &t, &rec).unwrap();
    assert_eq!(
        record::bump_attempts(&mut core, &t, &rec.session_id).unwrap(),
        Some(u8::MAX)
    );
}

// ---- forget ----

#[test]
fn forget_removes_the_label_and_keeps_other_labels() {
    let (mut core, t) = core_with("t1");
    core.terminals
        .get_mut(&t)
        .unwrap()
        .labels
        .insert("note".to_string(), "keep me".to_string());
    record::save(&mut core, &t, &sample_record()).unwrap();

    assert!(record::forget(&mut core, &t).unwrap());
    assert_eq!(record::load(&mut core, &t).unwrap(), None);
    let mut want = BTreeMap::new();
    want.insert("note".to_string(), "keep me".to_string());
    assert_eq!(core.terminal(&t).unwrap().labels, want);
}

#[test]
fn forget_with_no_record_writes_nothing() {
    let (mut core, t) = core_with("t1");
    let calls_before = core.calls;
    assert!(!record::forget(&mut core, &t).unwrap());
    assert_eq!(
        core.calls,
        calls_before + 1,
        "only the re-read, no label call"
    );
}

// ---- claims ----

#[test]
fn second_claim_from_another_terminal_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let claims = dir.path().join("claims");
    let (t1, t2) = (TerminalId::from("t1"), TerminalId::from("t2"));

    assert!(record::claim(&claims, "s-1", &t1));
    assert!(!record::claim(&claims, "s-1", &t2));
    // The holder may claim again; other sessions are independent.
    assert!(record::claim(&claims, "s-1", &t1));
    assert!(record::claim(&claims, "s-2", &t2));
}

#[test]
fn release_frees_the_claim_only_for_its_holder() {
    let dir = tempfile::tempdir().unwrap();
    let (t1, t2) = (TerminalId::from("t1"), TerminalId::from("t2"));
    assert!(record::claim(dir.path(), "s-1", &t1));

    assert!(!record::release_claim(dir.path(), "s-1", &t2));
    assert!(!record::claim(dir.path(), "s-1", &t2));

    assert!(record::release_claim(dir.path(), "s-1", &t1));
    assert!(record::claim(dir.path(), "s-1", &t2));
    assert!(!record::release_claim(dir.path(), "never-claimed", &t1));
}

#[test]
fn claims_with_odd_session_ids_stay_inside_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let claims = dir.path().join("claims");
    let (t1, t2) = (TerminalId::from("t1"), TerminalId::from("t2"));
    for id in ["../escape", "a/b", "..", ".", "", "x\0y"] {
        assert!(record::claim(&claims, id, &t1), "{id:?}");
        assert!(!record::claim(&claims, id, &t2), "{id:?}");
    }
    // "a/b" and "a%2Fb" must not share a claim file.
    assert!(record::claim(&claims, "a%2Fb", &t2));
    let outside: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(outside, vec![std::ffi::OsString::from("claims")]);
}

#[test]
fn racing_claims_have_exactly_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let claims = dir.path().join("claims");
    let winners: usize = std::thread::scope(|s| {
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let claims = &claims;
                s.spawn(move || record::claim(claims, "s-race", &TerminalId::from(format!("t{i}"))))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap() as usize)
            .sum()
    });
    assert_eq!(winners, 1);
}

// ---- env scrub ----

fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn scrub_env_keeps_only_the_allowlist() {
    let env = env_of(&[
        ("PATH", "/usr/bin"),
        ("LANG", "en_US.UTF-8"),
        ("LC_ALL", "C"),
        ("LC_CTYPE", "UTF-8"),
        ("CLAUDE_CONFIG_DIR", "/c"),
        ("CLAUDE_CODE_PROJECT_DIR_NAME", "p"),
        ("CODEX_HOME", "/x"),
        ("PI_CODING_AGENT_DIR", "/pi"),
        ("PI_CODING_AGENT_SESSION_DIR", "/pis"),
        ("PI_OFFLINE", "1"),
        ("OPENCODE_CONFIG_DIR", "/o"),
        ("OPENCODE_DB", "/o/db"),
        ("XDG_CONFIG_HOME", "/xc"),
        ("XDG_DATA_HOME", "/xd"),
        ("XDG_STATE_HOME", "/xs"),
        ("HOME", "/home/u"),
        ("SHELL", "/bin/zsh"),
        ("XDG_RUNTIME_DIR", "/run/user/1"),
        ("XDG_HOME", "/nope"),
        ("LC_", "x"),
        ("HC_TERMINAL_ID", "t1"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ("PATHEXT", ".exe"),
    ]);
    let kept: Vec<String> = record::scrub_env(&env).into_keys().collect();
    assert_eq!(
        kept,
        vec![
            "CLAUDE_CODE_PROJECT_DIR_NAME",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "OPENCODE_CONFIG_DIR",
            "OPENCODE_DB",
            "PATH",
            "PI_CODING_AGENT_DIR",
            "PI_CODING_AGENT_SESSION_DIR",
            "PI_OFFLINE",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
        ]
    );
    assert_eq!(record::scrub_env(&env)["PATH"], "/usr/bin");
}

#[test]
fn scrub_env_drops_secrets() {
    let env = env_of(&[
        ("ANTHROPIC_API_KEY", "sk-test-not-real"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "test-not-real"),
        ("OPENAI_API_KEY", "test-not-real"),
        ("PATH", "/usr/bin"),
    ]);
    assert_eq!(record::scrub_env(&env), env_of(&[("PATH", "/usr/bin")]));
}

#[test]
fn scrub_env_drops_secret_names_even_under_an_allowed_prefix() {
    let env = env_of(&[
        ("LC_API_KEY", "x"),
        ("LC_SESSION_TOKEN", "x"),
        ("LC_DB_PASSWORD", "x"),
        ("LC_CLIENT_SECRET", "x"),
        ("lc_lower_token", "x"),
        ("LC_TIME", "C"),
    ]);
    assert_eq!(record::scrub_env(&env), env_of(&[("LC_TIME", "C")]));
}

// ------------------------------------------------ Task 13: per-boot attempts

#[test]
fn attempts_on_counts_only_the_records_own_boot() {
    let mut rec = sample_record();
    rec.attempts = 3;
    assert_eq!(rec.attempts_on(&rec.boot_id.clone()), 3);
    assert_eq!(rec.attempts_on("another-boot"), 0);
}

#[test]
fn begin_attempt_counts_per_boot_and_clears_the_old_processes() {
    let mut core = FakeCore::new();
    let t = TerminalId::from("t1");
    core.insert(Terminal::new("t1"));
    let mut rec = sample_record();
    rec.attempts = 3;
    assert!(record::save(&mut core, &t, &rec).unwrap());

    // Same boot: one more attempt.
    let same = record::begin_attempt(&mut core, &t, &rec.session_id, &rec.boot_id)
        .unwrap()
        .unwrap();
    assert_eq!(same.attempts, 4);
    assert_eq!((same.agent_proc, same.shell_proc), (None, None));
    assert!(same.seq > rec.seq);

    // A new boot: the count starts over and the record takes the new boot.
    let next = record::begin_attempt(&mut core, &t, &rec.session_id, "boot-2")
        .unwrap()
        .unwrap();
    assert_eq!(next.attempts, 1);
    assert_eq!(next.boot_id, "boot-2");
    assert_eq!(record::load(&mut core, &t).unwrap(), Some(next));

    // Another session's record is left alone.
    assert_eq!(
        record::begin_attempt(&mut core, &t, "other", "boot-2").unwrap(),
        None
    );
}

#[test]
fn rearm_makes_a_failed_record_resumable() {
    let mut core = FakeCore::new();
    let t = TerminalId::from("t1");
    core.insert(Terminal::new("t1"));
    let mut rec = sample_record();
    rec.attempts = 3;
    rec.state = State::Failed { reason: "x".into() };
    assert!(record::save(&mut core, &t, &rec).unwrap());
    assert!(!record::rearm(&mut core, &t, "other").unwrap());
    assert!(record::rearm(&mut core, &t, &rec.session_id).unwrap());
    let now = record::load(&mut core, &t).unwrap().unwrap();
    assert_eq!((now.state, now.attempts), (State::Running, 0));
}

// ------------------------------------------------- local copy (M-5 e)

mod local_copy {
    use super::common::sample_record;
    use herdr_wake::core::TerminalId;
    use herdr_wake::record::{copy_file, copy_matches, remove_copy, write_copy, State};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn t(id: &str) -> TerminalId {
        TerminalId::from(id)
    }

    #[test]
    fn a_written_copy_matches_the_same_record_and_is_private() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("records");
        let rec = sample_record();
        assert!(!copy_matches(&dir, &t("t1"), 5, &rec), "no copy yet");
        write_copy(&dir, &t("t1"), 5, &rec).unwrap();
        assert!(copy_matches(&dir, &t("t1"), 5, &rec));
        let file = copy_file(&dir, &t("t1"));
        assert_eq!(file, dir.join("t1.json"));
        let mode =
            |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn bookkeeping_fields_do_not_break_the_match() {
        let d = tempfile::tempdir().unwrap();
        let mut rec = sample_record();
        write_copy(d.path(), &t("t1"), 5, &rec).unwrap();
        rec.state = State::Failed { reason: "x".into() };
        rec.attempts = 2;
        rec.seq += 10;
        rec.boot_id = "boot-9".into();
        rec.agent_proc = None;
        rec.shell_proc = None;
        assert!(copy_matches(d.path(), &t("t1"), 5, &rec));
    }

    #[test]
    fn any_change_to_what_would_run_breaks_the_match() {
        let d = tempfile::tempdir().unwrap();
        let rec = sample_record();
        write_copy(d.path(), &t("t1"), 5, &rec).unwrap();
        type Change = fn(&mut herdr_wake::record::Record);
        let changes: [Change; 7] = [
            |r| r.agent = herdr_wake::record::AgentKind::Codex,
            |r| r.session_id.push('x'),
            |r| r.session_file = None,
            |r| r.cwd = PathBuf::from("/tmp/evil"),
            |r| r.flags.push("--dangerously-skip-permissions".into()),
            |r| {
                r.env.insert("CLAUDE_CONFIG_DIR".into(), "/tmp/evil".into());
            },
            |r| r.bin = PathBuf::from("/tmp/evil-program"),
        ];
        for (i, change) in changes.iter().enumerate() {
            let mut r = rec.clone();
            change(&mut r);
            assert!(!copy_matches(d.path(), &t("t1"), 5, &r), "change {i}");
        }
        assert!(copy_matches(d.path(), &t("t1"), 5, &rec));
    }

    #[test]
    fn a_reused_terminal_id_does_not_match_the_old_terminals_copy() {
        let d = tempfile::tempdir().unwrap();
        let rec = sample_record();
        write_copy(d.path(), &t("t1"), 5, &rec).unwrap();
        assert!(!copy_matches(d.path(), &t("t1"), 6, &rec));
        assert!(!copy_matches(d.path(), &t("t2"), 5, &rec));
    }

    #[test]
    fn remove_and_odd_terminal_ids() {
        let d = tempfile::tempdir().unwrap();
        let rec = sample_record();
        let odd = t("../../x y");
        write_copy(d.path(), &odd, 1, &rec).unwrap();
        assert_eq!(copy_file(d.path(), &odd).parent(), Some(d.path()));
        assert!(copy_matches(d.path(), &odd, 1, &rec));
        assert!(remove_copy(d.path(), &odd));
        assert!(!remove_copy(d.path(), &odd));
        assert!(!copy_matches(d.path(), &odd, 1, &rec));
        // Garbage in the file is a mismatch, not an error.
        std::fs::write(copy_file(d.path(), &t("t3")), "not json").unwrap();
        assert!(!copy_matches(d.path(), &t("t3"), 1, &rec));
    }
}

// ------------------------------------------------- env values (M-3, M-5 a)

#[test]
fn scrub_env_keeps_only_absolute_path_entries_and_a_plain_db_path() {
    use herdr_wake::record::scrub_env;
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/usr/bin:.::bin:/opt/x/bin".to_string());
    env.insert(
        "OPENCODE_DB".to_string(),
        "postgres://u:pass@h/db".to_string(),
    );
    let out = scrub_env(&env);
    assert_eq!(
        out.get("PATH").map(String::as_str),
        Some("/usr/bin:/opt/x/bin")
    );
    assert!(!out.contains_key("OPENCODE_DB"));

    env.insert("OPENCODE_DB".to_string(), "/home/u/oc.db".to_string());
    env.insert("PATH".to_string(), "relative:.".to_string());
    let out = scrub_env(&env);
    assert_eq!(
        out.get("OPENCODE_DB").map(String::as_str),
        Some("/home/u/oc.db")
    );
    assert!(!out.contains_key("PATH"), "nothing absolute is left");
}
