//! `status`, `doctor` and `adopt` against the isolated home.

use herdr_wake::record::State;

use crate::harness::Harness;

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}):\n{text}"))
}

/// (g) Sane output with the core up and with it down, and neither command
/// starts a stopped core.
#[test]
fn status_and_doctor_report_and_never_start_the_core() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let t = h.terminal();
    h.launch(&t, "STUB_ID=st-1 claude --model opus");

    // Core up: one row, live.
    let text = h.hw_ok(&["status"]);
    assert!(text.contains("TERMINAL"), "{text}");
    let row = text
        .lines()
        .find(|l| l.starts_with(&format!("{t} ")))
        .unwrap_or_else(|| panic!("no row for {t}:\n{text}"));
    for word in ["claude", "running", "yes", "st-1"] {
        assert!(row.contains(word), "{word} missing from: {row}");
    }
    assert!(text.contains("1 session: 1 running"), "{text}");
    let v = json(&h.hw_ok(&["status", "--json"]));
    assert_eq!(v["core_running"], true);
    assert_eq!(v["sessions"][0]["terminal"], t.as_str());
    assert_eq!(v["sessions"][0]["agent"], "claude");
    assert_eq!(v["sessions"][0]["session_id"], "st-1");
    assert_eq!(v["sessions"][0]["live"], true);

    let out = h.hw(&["doctor"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("core: running (pid "), "{text}");
    assert!(text.contains("ok    core shell: core starts"), "{text}");
    assert!(
        text.contains("ok    real shell: terminals end in /bin/sh"),
        "{text}"
    );
    assert!(
        text.contains("ok    dead terminals: every kept terminal has a running program"),
        "{text}"
    );
    assert!(out.stderr.is_empty());

    // Core down.
    h.stop();
    let left = h.wait_until_no_processes();
    assert!(left.is_empty(), "the stop left processes: {left:#?}");

    let text = h.hw_ok(&["status"]);
    assert!(text.contains("herdr-core is not running"), "{text}");
    assert!(text.contains("did not start it"), "{text}");
    let v = json(&h.hw_ok(&["status", "--json"]));
    assert_eq!(v["core_running"], false);
    assert_eq!(v["sessions"].as_array().map(Vec::len), Some(0));

    let out = h.hw(&["doctor"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("herdr-core is not running (doctor did not start it)"),
        "{text}"
    );
    assert!(text.contains("core version: skipped"), "{text}");
    let out = h.hw(&["doctor", "--json"]);
    let v = json(&String::from_utf8_lossy(&out.stdout));
    let findings = v["findings"].as_array().expect("findings");
    assert!(findings.len() > 5);
    assert!(findings
        .iter()
        .all(|f| f["check"].is_string() && f["detail"].is_string() && f["level"].is_string()));
    let failed = findings.iter().filter(|f| f["level"] == "fail").count();
    assert_eq!(v["failed"], failed);
    assert_eq!(out.status.code(), Some(i32::from(failed > 0)));

    // `forget` and `resume` say why they cannot act, and start nothing too.
    for cmd in ["forget", "resume"] {
        let out = h.hw(&[cmd, &t]);
        assert_eq!(out.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("herdr-core is not running"),
            "{cmd}"
        );
    }

    assert!(
        h.processes().is_empty(),
        "a command started a process for the stopped home: {:#?}",
        h.processes()
    );
    assert!(
        h.core().is_err(),
        "the core answers again: something started it"
    );
}

/// A broken `shell` setting leaves kept terminals without a program, and
/// doctor says so with the line from core's holder log.
#[test]
fn doctor_flags_terminals_that_core_cannot_start() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let t = h.terminal();
    h.stop();
    h.write_core_config("shell = [\"/nonexistent/herdr-wake\", \"shell\"]\n");
    h.start();
    let out = h.hw(&["doctor"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("FAIL  core shell:"), "{text}");
    assert!(
        text.contains(&format!(
            "FAIL  dead terminals: kept terminal {t} has had no program"
        )),
        "{text}"
    );
    assert!(text.contains("holder.log"), "{text}");
}

/// `adopt` records a Claude that was already running without a record,
/// from Claude's own `sessions/<pid>.json`, and says what it cannot adopt.
#[test]
fn adopt_records_a_running_claude_and_reports_the_rest() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let t1 = h.terminal();
    let t2 = h.terminal();
    // No hooks installed yet: the stubs start without calling herdr-wake.
    std::fs::write(h.home.join("stub/nohook"), "").unwrap();
    h.send(&t1, "STUB_ID=adopt-me claude --model opus");
    h.send(&t2, "codex");
    let stubs = h.wait("both stubs to start", || {
        Some(h.starts()).filter(|s| s.len() == 2)
    });
    let claude = stubs.iter().find(|s| s.kind == "claude").unwrap();
    assert_eq!(h.record(&t1), None);
    // Core refreshes the foreground program about twice a second.
    h.wait("core to see the agents in the foreground", || {
        (h.get(&t1).foreground_pid == Some(claude.pid) && h.foreground(&t2).contains("codex"))
            .then_some(())
    });

    let out = h.hw_ok(&["adopt"]);
    assert!(
        out.contains(&format!("{t1}: recorded claude session adopt-me")),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "{t2}: codex needs a restart of the agent to be recorded"
        )),
        "{out}"
    );
    assert!(
        out.contains("1 of 2 running agents newly recorded"),
        "{out}"
    );
    let rec = h.record(&t1).expect("adopted record");
    assert_eq!(rec.state, State::Running);
    assert_eq!(rec.session_id, "adopt-me");
    assert_eq!(rec.flags, ["--model", "opus"]);
    assert_eq!(rec.agent_proc.map(|p| p.pid), Some(claude.pid));
    assert_eq!(h.record(&t2), None);

    // A second adopt changes nothing.
    let out = h.hw_ok(&["adopt"]);
    assert!(
        out.contains(&format!("{t1}: claude is already recorded")),
        "{out}"
    );

    // The adopted session resumes like any other.
    std::fs::remove_file(h.home.join("stub/nohook")).unwrap();
    h.restart();
    let s = h.wait("the adopted session to resume", || {
        h.resumes(&t1).into_iter().next()
    });
    assert_eq!(s.id, "adopt-me");
    assert_eq!(s.argv, ["--model", "opus", "--resume", "adopt-me"]);
}

/// `herdr-wake boot` is what the boot service runs: it starts the core,
/// waits for the resumes to settle and leaves a summary that `status` shows.
#[test]
fn boot_starts_the_core_and_reports_what_resumed() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let t1 = h.terminal();
    let t2 = h.terminal();
    let t3 = h.terminal();
    h.launch(&t1, "STUB_ID=boot-1 claude");
    h.launch(&t2, "STUB_ID=boot-2 pi");
    h.launch(&t3, "STUB_ID=boot-3 claude");
    h.quit(&t3);
    h.wait("the quit session to be ended", || {
        h.record(&t3).filter(|r| r.state == State::Ended)
    });
    h.stop();
    assert!(h.wait_until_no_processes().is_empty());

    // The boot job's environment: bare, as launchd or systemd give it.
    // SSH_CONNECTION keeps core's start on the direct path, as for every
    // `hc` call in this suite; HERDR_WAKE_HC names the `hc` to run.
    let hc = h.hc_path().display().to_string();
    let out = h.hw_env(
        &["boot"],
        &[("HERDR_WAKE_HC", hc.as_str()), ("SSH_CONNECTION", "test")],
    );
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert!(
        said.contains("herdr-wake boot: 2 resumed, 0 failed, 1 skipped"),
        "{said}"
    );
    assert!(said.contains("started core:"), "{said}");
    assert!(said.contains(&format!("{t1} claude: resumed")), "{said}");
    assert!(said.contains(&format!("{t2} pi: resumed")), "{said}");
    assert!(said.contains(&format!("{t3} claude: ended")), "{said}");

    assert_eq!(h.resumes(&t1).len(), 1);
    assert_eq!(h.resumes(&t2).len(), 1);
    assert!(h.resumes(&t3).is_empty());
    let status = h.hw_ok(&["status"]);
    assert!(
        status.contains("last boot run (this boot): 2 resumed, 0 failed, 1 skipped"),
        "{status}"
    );

    // A second boot run finds the core up and starts nothing.
    let out = h.hw_env(
        &["boot"],
        &[("HERDR_WAKE_HC", hc.as_str()), ("SSH_CONNECTION", "test")],
    );
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(said.contains("core already running"), "{said}");
    assert_eq!(h.resumes(&t1).len(), 1);
}
