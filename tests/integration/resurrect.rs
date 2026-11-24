//! `herdr-wake resurrect` against a real core: a closed session is listed,
//! then brought back in a new terminal by a real `hc run`.

use herdr_wake::record::State;

use crate::harness::Harness;

/// Makes the stub's session file look old: a session file written in the
/// last two minutes counts as running.
fn age_session_file(h: &Harness, dir: &std::path::Path, id: &str) {
    let file = h
        .home
        .join(".claude/projects")
        .join(herdr_wake::agents::claude::project_dir_name(dir))
        .join(format!("{id}.jsonl"));
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap_or_else(|e| panic!("{}: {e}", file.display()))
        .set_modified(old)
        .unwrap();
}

/// A `PATH` with the stub agents and `hc`, for the commands that look both
/// up.
fn path(h: &Harness) -> String {
    format!(
        "{}:{}:/usr/bin:/bin",
        h.home.join("bin").display(),
        h.hc_path().parent().unwrap().display()
    )
}

fn json(h: &Harness, args: &[&str]) -> serde_json::Value {
    let out = h.hw_env(args, &[("PATH", &path(h))]);
    assert!(
        out.status.success(),
        "{args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("JSON listing")
}

#[test]
fn a_closed_session_is_listed_and_brought_back_in_a_new_terminal() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let dir = h.work.join("proj res");
    let t = h.terminal_in(&dir);
    let id = "claude-res-0001";
    h.launch(
        &t,
        &format!("STUB_ID={id} claude --model opus --dangerously-skip-permissions"),
    );
    // Running: not listed.
    assert!(json(&h, &["resurrect", "--json"])["sessions"]
        .as_array()
        .unwrap()
        .is_empty());

    h.quit(&t);
    h.wait("the record to end", || {
        h.record(&t).filter(|r| r.state == State::Ended)
    });
    h.wait_for_shell(&t);
    age_session_file(&h, &dir, id);

    // Closed: listed, with the flags it ran with and the bypass marked.
    let listing = json(&h, &["resurrect", "--json"]);
    let first = &listing["sessions"][0];
    assert_eq!(first["session_id"], id);
    assert_eq!(first["agent"], "claude");
    assert_eq!(first["ending"], "quit");
    assert_eq!(first["source"], "wake");
    assert_eq!(first["bypass"], true);
    assert_eq!(first["number"], 1);
    assert_eq!(first["cwd"], dir.display().to_string());

    // The command, without opening anything.
    let out = h.hw_env(&["resurrect", "--print", id], &[("PATH", &path(&h))]);
    assert!(out.status.success());
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(printed.starts_with("cd "), "{printed}");
    assert!(
        printed.contains("--dangerously-skip-permissions") && printed.contains("--model opus"),
        "{printed}"
    );
    assert!(
        printed.trim_end().ends_with(&format!("--resume {id}")),
        "{printed}"
    );

    // The JSON listing is for scripts: it does not set the numbers.
    let out = h.hw_env(&["resurrect", "1"], &[("PATH", &path(&h))]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no earlier listing"));
    // The table does.
    let table = h.hw_env(&["resurrect"], &[("PATH", &path(&h))]);
    let text = String::from_utf8_lossy(&table.stdout).into_owned();
    assert!(text.contains("claude") && text.contains("quit"), "{text}");

    // Bring it back by number, through a real `hc run`.
    let before = h.starts().len();
    let out = h.hw_env(&["resurrect", "1"], &[("PATH", &path(&h))]);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{said}");
    assert!(said.contains("Opened"), "{said}");

    let s = h.wait("the session to start in a new terminal", || {
        h.starts()
            .into_iter()
            .skip(before)
            .find(|s| s.resumed && s.id == id)
    });
    assert_ne!(s.terminal, t, "a new terminal");
    assert_eq!(s.cwd, dir, "in the session's folder");
    assert!(!s.supervised, "started like a session you start yourself");
    for word in [
        "--dangerously-skip-permissions",
        "--model",
        "opus",
        "--resume",
    ] {
        assert!(s.argv.iter().any(|a| a == word), "{word} in {:?}", s.argv);
    }
    // Its own hook records it in the new terminal, so it is open now and
    // no longer listed.
    h.wait("the new terminal to hold a running record", || {
        h.record(&s.terminal)
            .filter(|r| r.state == State::Running && r.session_id == id)
    });
    assert!(json(&h, &["resurrect", "--json"])["sessions"]
        .as_array()
        .unwrap()
        .is_empty());

    // Quit it: the shell is left behind and the session is listed again.
    h.quit(&s.terminal);
    h.wait("the record to end again", || {
        h.record(&s.terminal).filter(|r| r.state == State::Ended)
    });
    h.wait_for_shell(&s.terminal);
    age_session_file(&h, &dir, id);
    let again = json(&h, &["resurrect", "--json"]);
    assert_eq!(again["sessions"][0]["session_id"], id);
}

#[test]
fn a_session_that_is_not_safe_to_resume_is_refused() {
    let Some(h) = Harness::new() else { return };
    h.start();
    let dir = h.work.join("proj unsafe");
    let t = h.terminal_in(&dir);
    let id = "claude-res-0002";
    h.launch(&t, &format!("STUB_ID={id} claude"));
    h.quit(&t);
    h.wait("the record to end", || {
        h.record(&t).filter(|r| r.state == State::Ended)
    });
    h.wait_for_shell(&t);
    age_session_file(&h, &dir, id);

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let before = h.starts().len();
    let out = h.hw_env(&["resurrect", id], &[("PATH", &path(&h))]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("not safe"), "{err}");
    assert_eq!(h.starts().len(), before, "nothing was started");
}
