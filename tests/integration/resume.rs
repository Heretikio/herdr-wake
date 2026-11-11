//! Record, stop and start the core, and see what comes back.

use std::time::Duration;

use herdr_wake::record::State;

use crate::harness::{Harness, FIRST_ALONE_MS, STAGGER_MS};

/// Types `echo` into the terminal's shell and waits for its output: proof
/// that a working shell holds the terminal.
fn assert_shell_works(h: &Harness, t: &str, tag: &str) {
    h.send(t, &format!("echo {tag}-$((40+2))"));
    h.wait(&format!("the shell in {t} to answer"), || {
        h.read(t).contains(&format!("{tag}-42")).then_some(())
    });
}

/// (a) One agent: record it, restart the core, and expect the same session
/// running again in the same terminal and folder with the same flags; then
/// quit it and expect a working shell and an ended record.
fn resumes(kind: &str, launch_flags: &str, want_flags: &[&str], resume_words: &[&str]) {
    let Some(h) = Harness::new() else { return };
    h.start();
    let dir = h.work.join(format!("proj {kind}")); // a folder with a space
    let t = h.terminal_in(&dir);
    let id = format!("{kind}-session-0001");
    let before = h.launch(&t, &format!("STUB_ID={id} {kind} {launch_flags}"));
    assert_eq!(before.session_id, id);
    assert_eq!(before.cwd, dir);
    assert_eq!(
        before.bin,
        h.home.join("bin").join(kind),
        "the launcher on PATH, not bash"
    );
    assert!(
        before.shell_proc.is_some(),
        "started from a shell: it has one to outlive"
    );

    h.restart();

    let s = h.wait(&format!("{kind} to be resumed in {t}"), || {
        h.resumes(&t).into_iter().next()
    });
    assert_eq!(s.kind, kind);
    assert_eq!(s.id, id, "the same session");
    assert_eq!(s.cwd, dir, "the same folder");
    assert!(s.supervised, "the wrapper marks the agent it starts");
    for flag in want_flags {
        assert!(
            s.argv.iter().any(|a| a == flag),
            "{flag} missing from {:?}",
            s.argv
        );
    }
    for word in resume_words {
        assert!(
            s.argv.iter().any(|a| a == word),
            "{word} missing from {:?}",
            s.argv
        );
    }

    // The resumed agent re-stamped the record with its own process.
    let rec = h.wait("the record to name the resumed agent", || {
        h.record(&t)
            .filter(|r| r.agent_proc.is_some_and(|p| p.pid == s.pid))
    });
    assert_eq!(rec.state, State::Running);
    assert_eq!(rec.session_id, id);
    assert_eq!(rec.attempts, 0, "a confirmed resume starts the count over");
    assert_eq!(
        rec.shell_proc, None,
        "the wrapper is its parent, not a shell"
    );

    // Core sees the agent as the terminal's foreground program.
    h.wait("core to report the agent in the foreground", || {
        (h.get(&t).foreground_pid == Some(s.pid)).then_some(())
    });
    assert_eq!(h.resumes(&t).len(), 1, "started exactly once");

    // Quit it: the wrapper hands the terminal to the real shell.
    h.quit(&t);
    h.wait("the record to be ended after the quit", || {
        h.record(&t).filter(|r| r.state == State::Ended)
    });
    h.wait_for_shell(&t);
    assert_shell_works(&h, &t, "ALIVE");
    // The shell starts in the session's folder (the screen wraps long
    // lines, so compare without whitespace).
    h.send(&t, "pwd");
    let want: String = dir.display().to_string().split_whitespace().collect();
    h.wait("the shell to print the session's folder", || {
        let flat: String = h.read(&t).split_whitespace().collect();
        flat.contains(&want).then_some(())
    });

    // A second restart resumes nothing.
    h.restart();
    h.wait_for_shell(&t);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(h.resumes(&t).len(), 1);
}

#[test]
fn a_claude_resumes_after_a_restart_with_its_flags_and_config_dir() {
    let Some(h) = Harness::new() else { return };
    // The selected environment is restored too: CLAUDE_CONFIG_DIR.
    h.start();
    let t = h.terminal();
    let cc = h.home.join("cc");
    let line = format!(
        "CLAUDE_CONFIG_DIR={} STUB_ID=cc-session claude --model opus",
        cc.display()
    );
    h.launch(&t, &line);
    h.restart();
    let s = h.wait("claude to be resumed", || h.resumes(&t).into_iter().next());
    assert_eq!(s.config_dir, cc.display().to_string());
    assert_eq!(s.argv, ["--model", "opus", "--resume", "cc-session"]);
}

#[test]
fn a_claude_resumes_after_a_restart() {
    resumes(
        "claude",
        "--dangerously-skip-permissions --model opus",
        &["--dangerously-skip-permissions", "--model", "opus"],
        &["--resume", "claude-session-0001"],
    );
}

#[test]
fn a_codex_resumes_after_a_restart() {
    resumes(
        "codex",
        "--yolo -m gpt-5",
        &["--yolo", "-m", "gpt-5"],
        &["resume", "codex-session-0001"],
    );
}

#[test]
fn a_pi_resumes_after_a_restart() {
    resumes(
        "pi",
        "--approve --model sonnet",
        &["--approve", "--model", "sonnet"],
        &["--session"],
    );
}

#[test]
fn a_opencode_resumes_after_a_restart() {
    resumes(
        "opencode",
        "--auto -m anthropic/sonnet",
        &["--auto", "-m", "anthropic/sonnet"],
        &["--session", "opencode-session-0001"],
    );
}

/// (b) Quit before the stop: the terminal comes back as a plain shell.
fn quit_before_stop_stays_gone(kind: &str) {
    let Some(h) = Harness::new() else { return };
    h.start();
    let t = h.terminal();
    h.launch(&t, kind);
    h.quit(&t);
    // Claude, Pi and OpenCode say so themselves; for Codex the watcher
    // notices that the shell outlived the agent.
    h.wait(&format!("the {kind} record to be ended"), || {
        h.record(&t).filter(|r| r.state == State::Ended)
    });

    h.restart();
    h.wait_for_shell(&t);
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        h.resumes(&t).is_empty(),
        "nothing may start: {:?}",
        h.resumes(&t)
    );
    assert_shell_works(&h, &t, "PLAIN");
    assert!(
        !h.read(&t).contains("herdr-wake:"),
        "no message for a session that was quit"
    );
}

#[test]
fn a_claude_quit_before_the_stop_comes_back_as_a_plain_shell() {
    quit_before_stop_stays_gone("claude");
}

#[test]
fn a_codex_quit_before_the_stop_comes_back_as_a_plain_shell() {
    quit_before_stop_stays_gone("codex");
}

/// (c) The session file is gone: no start, no retries, one line with the
/// reason and the command to run by hand.
#[test]
fn a_missing_session_file_gives_the_manual_command_and_no_retries() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let t = h.terminal();
    let rec = h.launch(&t, "STUB_ID=lost-session claude --model opus");
    let transcript = rec.session_file.expect("the stub reported its transcript");
    std::fs::remove_file(&transcript).unwrap();

    h.restart();
    let failed = h.wait("the record to be marked failed", || {
        h.record(&t)
            .filter(|r| matches!(r.state, State::Failed { .. }))
    });
    assert_eq!(
        failed.state,
        State::Failed {
            reason: "session not found".into()
        }
    );
    assert_eq!(failed.attempts, 0, "a permanent failure uses no attempts");
    h.wait_for_shell(&t);
    let screen = h.wait("the message to show in the terminal", || {
        Some(h.read(&t)).filter(|s| s.contains("To retry:"))
    });
    // The terminal wraps long lines, so compare without whitespace.
    let flat: String = screen.split_whitespace().collect();
    let want: String = format!(
        "herdr-wake: could not resume claude session lost-session: session not found. To retry: cd {} && {} --model opus --resume lost-session",
        h.work.display(),
        h.home.join("bin/claude").display()
    )
    .split_whitespace()
    .collect();
    assert!(flat.contains(&want), "screen:\n{screen}");
    assert_eq!(
        screen.matches("could not resume").count(),
        1,
        "printed once"
    );
    assert!(h.resumes(&t).is_empty(), "the agent was never started");
    assert_shell_works(&h, &t, "AFTER");

    // It stays failed across another restart, and says nothing again.
    h.restart();
    h.wait_for_shell(&t);
    assert!(h.resumes(&t).is_empty());
    assert!(!h.read(&t).contains("could not resume"));
}

/// (d) An agent that exits 1 at once is tried exactly 3 times, then marked
/// failed; `herdr-wake resume` arms it again and `forget` drops it.
#[test]
fn a_failing_agent_is_tried_three_times_then_failed_and_resume_rearms_it() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let t = h.terminal();
    h.launch(&t, "STUB_ID=crashy claude");
    std::fs::write(h.home.join("stub").join(format!("fail-{t}")), "").unwrap();

    h.restart();
    let failed = h.wait("the record to be marked failed", || {
        h.record(&t)
            .filter(|r| matches!(r.state, State::Failed { .. }))
    });
    assert_eq!(h.resumes(&t).len(), 3, "exactly three attempts");
    assert_eq!(failed.attempts, 3);
    let State::Failed { reason } = &failed.state else {
        unreachable!()
    };
    assert!(reason.contains("exited with code 1"), "{reason}");
    h.wait_for_shell(&t);
    let screen = h.read(&t);
    assert_eq!(screen.matches("could not resume").count(), 1);
    assert_shell_works(&h, &t, "STILL");

    // Failed stays failed across restarts.
    h.restart();
    h.wait_for_shell(&t);
    assert_eq!(h.resumes(&t).len(), 3);

    // `resume` arms it again and prints the command; it does not start it.
    std::fs::remove_file(h.home.join("stub").join(format!("fail-{t}"))).unwrap();
    let out = h.hw_ok(&["resume", &t]);
    assert!(out.contains("set to resume again"), "{out}");
    assert!(out.contains("--resume crashy"), "{out}");
    let armed = h.record(&t).unwrap();
    assert_eq!((armed.state.clone(), armed.attempts), (State::Running, 0));
    assert_eq!(h.resumes(&t).len(), 3, "resume itself starts nothing");
    h.restart();
    h.wait("the armed session to resume", || {
        (h.resumes(&t).len() == 4).then_some(())
    });

    // `forget` removes the record, and then nothing resumes.
    let out = h.hw_ok(&["forget", &t]);
    assert!(out.contains("record removed"), "{out}");
    assert_eq!(h.record(&t), None);
    h.restart();
    h.wait_for_shell(&t);
    assert_eq!(h.resumes(&t).len(), 4);
}

/// (e) Six terminals with records start one after another, never together.
#[test]
fn six_agents_start_staggered() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let terminals: Vec<String> = (0..6).map(|_| h.terminal()).collect();
    for t in &terminals {
        h.launch(t, "claude");
    }
    h.restart();
    let mut starts = h.wait("all six to be resumed", || {
        let s: Vec<_> = h.starts().into_iter().filter(|s| s.resumed).collect();
        (s.len() == 6).then_some(s)
    });
    starts.sort_by_key(|s| s.ms);
    let mut seen: Vec<&str> = starts.iter().map(|s| s.terminal.as_str()).collect();
    seen.sort();
    let mut all: Vec<&str> = terminals.iter().map(String::as_str).collect();
    all.sort();
    assert_eq!(seen, all, "each terminal resumed once");

    let gaps: Vec<u64> = starts.windows(2).map(|w| w[1].ms - w[0].ms).collect();
    // The stub stamps its start a little after the wrapper released it, so
    // allow some slack below the configured stagger.
    let floor = STAGGER_MS * 3 / 4;
    assert!(
        gaps.iter().all(|g| *g >= floor),
        "starts closer than the stagger: gaps {gaps:?} ms, floor {floor} ms"
    );
    // The first agent's hook confirmed the gate, so the second did not wait
    // out the whole first-alone time.
    assert!(
        gaps[0] < FIRST_ALONE_MS,
        "the second start waited {} ms: the gate was never confirmed",
        gaps[0]
    );
}

/// (f) Bypass flags come back by default (see the claude test above) and
/// are dropped with `restore_bypass = false`.
#[test]
fn bypass_flags_are_dropped_when_restore_bypass_is_off() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let t1 = h.terminal();
    let t2 = h.terminal();
    h.launch(
        &t1,
        "STUB_ID=b1 claude --dangerously-skip-permissions --model opus",
    );
    h.launch(&t2, "STUB_ID=b2 codex --yolo -m gpt-5");
    h.stop();
    h.write_config("restore_bypass = false\n");
    h.start();
    let c = h.wait("claude to be resumed", || h.resumes(&t1).into_iter().next());
    assert_eq!(c.argv, ["--model", "opus", "--resume", "b1"]);
    let x = h.wait("codex to be resumed", || h.resumes(&t2).into_iter().next());
    assert_eq!(x.argv, ["resume", "-m", "gpt-5", "b2"]);
}

/// (i) A saved real shell that does not exist: the wrapper still ends in a
/// working shell (core would retry a dead shell forever).
#[test]
fn a_missing_real_shell_falls_back_to_a_working_shell() {
    let Some(h) = Harness::new() else { return };
    h.write_config("real_shell = [\"/nonexistent/bin/fancy-shell\", \"-l\"]\n");
    h.start();
    let out = h.hc_ok(&["run", "--bg", "--keep", "--json"]);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let t = v["address"].as_str().unwrap().to_string();
    h.wait("a fallback shell to hold the terminal", || {
        let fg = h.foreground(&t);
        (fg.starts_with("/bin/sh") || fg.starts_with("sh")).then_some(())
    });
    assert_shell_works(&h, &t, "FALLBACK");

    // The same holds for a recovered terminal with a record to resume.
    h.launch(&t, "STUB_ID=fb claude");
    h.restart();
    let s = h.wait("claude to be resumed", || h.resumes(&t).into_iter().next());
    assert_eq!(s.id, "fb");
    h.quit(&t);
    h.wait("the record to be ended", || {
        h.record(&t).filter(|r| r.state == State::Ended)
    });
    assert_shell_works(&h, &t, "AGAIN");
}

/// A terminal that never had an agent is left alone, and one whose folder
/// is excluded is not resumed.
#[test]
fn plain_terminals_and_excluded_folders_are_left_alone() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let plain = h.terminal();
    let skipped = h.terminal_in(&h.work.join("secret"));
    h.launch(&skipped, "STUB_ID=ex claude");
    h.stop();
    h.write_config(&format!(
        "exclude_dirs = [\"{}\"]\n",
        h.work.join("secret").display()
    ));
    h.start();
    h.wait_for_shell(&plain);
    h.wait_for_shell(&skipped);
    std::thread::sleep(Duration::from_secs(1));
    assert!(h.starts().iter().all(|s| !s.resumed));
    assert_eq!(h.record(&plain), None);
    assert_shell_works(&h, &plain, "PLAIN");
}

/// A resumed agent that is killed (not quit) keeps its record, and a session
/// survives any number of restarts in one boot: every confirmed resume
/// starts the attempt count over.
#[test]
fn a_killed_agent_stays_resumable_across_many_restarts() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let t = h.terminal();
    h.launch(&t, "STUB_ID=tough claude");
    for round in 1..=5 {
        h.restart();
        let s = h.wait(&format!("resume number {round}"), || {
            h.resumes(&t).into_iter().nth(round - 1)
        });
        let rec = h.wait("the record to name the resumed agent", || {
            h.record(&t)
                .filter(|r| r.agent_proc.is_some_and(|p| p.pid == s.pid))
        });
        assert_eq!(rec.attempts, 0, "round {round}");
        if round == 3 {
            // Killed, as a crash or `kill` would: not a quit.
            let status = std::process::Command::new("kill")
                .args(["-TERM", &s.pid.to_string()])
                .status()
                .unwrap();
            assert!(status.success());
            h.wait_for_shell(&t);
            assert_eq!(h.record(&t).unwrap().state, State::Running);
            assert_shell_works(&h, &t, "KILLED");
        }
    }
    assert_eq!(h.resumes(&t).len(), 5);
}

/// An agent that is the terminal's own program (`hc run -- claude`) has no
/// shell to outlive. After a recovery the terminal is a wrapper terminal:
/// the agent comes back, and quitting it leaves a shell where there was none.
#[test]
fn an_agent_that_was_the_terminals_program_resumes_under_the_wrapper() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let out = h.hc_ok(&[
        "run", "--bg", "--keep", "--json", "--", "claude", "--model", "opus",
    ]);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let t = v["address"].as_str().unwrap().to_string();
    let rec = h.wait("the record of the terminal's own program", || {
        h.record(&t).filter(|r| r.agent_proc.is_some())
    });
    assert_eq!(
        rec.shell_proc, None,
        "core's holder is not a shell to outlive"
    );
    assert_eq!(rec.flags, ["--model", "opus"]);

    h.restart();
    let s = h.wait("the agent to come back", || {
        h.resumes(&t).into_iter().next()
    });
    assert_eq!(s.id, rec.session_id);
    assert_eq!(
        s.argv,
        ["--model", "opus", "--resume", rec.session_id.as_str()]
    );
    h.quit(&t);
    h.wait("the record to be ended", || {
        h.record(&t).filter(|r| r.state == State::Ended)
    });
    h.wait_for_shell(&t);
    assert_shell_works(&h, &t, "NOWASHELL");
}

/// M-5: a label that something other than herdr-wake wrote or changed is
/// never acted on, until the user accepts it with `herdr-wake resume`.
#[test]
fn a_label_changed_behind_herdr_wakes_back_is_not_resumed_until_accepted() {
    use herdr_wake::core::TerminalId;
    use herdr_wake::record;

    let Some(h) = Harness::new() else { return };
    h.start();
    let t = h.terminal();
    let rec = h.launch(&t, "STUB_ID=claude-tamper-1 claude --model opus");
    let copy = h
        .home
        .join(".local/state/herdr-wake/records")
        .join(format!("{t}.json"));
    assert!(copy.is_file(), "the hook keeps a local copy");

    // Something else rewrites the label over core's socket.
    let mut hostile = rec.clone();
    hostile
        .flags
        .push("--dangerously-skip-permissions".to_string());
    hostile.seq += 1;
    let mut core = h.core().unwrap();
    assert!(record::save(&mut core, &TerminalId::from(t.as_str()), &hostile).unwrap());
    drop(core);

    h.restart();
    h.wait_for_shell(&t);
    assert_shell_works(&h, &t, "untrusted");
    assert!(h.resumes(&t).is_empty(), "{:?}", h.resumes(&t));
    assert!(
        h.log().contains("record does not match the local copy"),
        "{}",
        h.log()
    );
    let status = h.hw_ok(&["status"]);
    assert!(status.contains("untrusted"), "{status}");

    // The user looks at it and accepts it.
    let out = h.hw_ok(&["resume", &t]);
    assert!(out.contains("accepted"), "{out}");
    h.restart();
    let s = h.wait("the accepted record to resume", || {
        h.resumes(&t).into_iter().next()
    });
    assert!(s.argv.iter().any(|a| a == "--dangerously-skip-permissions"));

    // forget drops the label and the copy.
    h.hw_ok(&["forget", &t]);
    assert!(!copy.exists());
}

/// Decision 15: `herdr-wake off <terminal>` and `on <terminal>`, against a
/// real core.
#[test]
fn off_forgets_and_stops_recording_and_resuming_until_on() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let t = h.terminal();
    h.launch(&t, "STUB_ID=claude-off-1 claude");

    let out = h.hw_ok(&["off", &t]);
    assert!(out.contains(&t), "{out}");
    assert_eq!(h.record(&t), None, "off forgets the record");
    assert_eq!(
        h.get(&t).labels.get("wake.off").map(String::as_str),
        Some("1")
    );
    let status = h.hw_ok(&["status"]);
    let line = status.lines().find(|l| l.starts_with(&t)).unwrap_or("");
    assert!(line.contains("off"), "{status}");

    // Nothing comes back after a restart, and a new agent is not recorded.
    h.restart();
    h.wait_for_shell(&t);
    assert!(h.resumes(&t).is_empty());
    let before = h.starts().len();
    h.send(&t, "STUB_ID=claude-off-2 claude");
    h.wait("the second agent to start", || {
        (h.starts().len() > before).then_some(())
    });
    h.wait("the hook to say why it did not record", || {
        h.log().contains("switched off").then_some(())
    });
    assert_eq!(h.record(&t), None);
    h.quit(&t);
    h.wait("the second agent to quit", || {
        h.read(&t).contains("STUB claude bye").then_some(())
    });
    h.wait_for_shell(&t);
    std::fs::remove_file(h.home.join("stub").join(format!("quit-{t}"))).unwrap();

    // Switched on again: the next agent is recorded and resumes.
    let out = h.hw_ok(&["on", &t]);
    assert!(out.contains(&t), "{out}");
    assert_eq!(h.get(&t).labels.get("wake.off"), None);
    h.launch(&t, "STUB_ID=claude-off-3 claude");
    h.restart();
    let s = h.wait("the recorded agent to resume", || {
        h.resumes(&t).into_iter().next()
    });
    assert_eq!(s.id, "claude-off-3");
}
