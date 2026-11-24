//! The command line: help, version, and the rules for `hook` and `shell`
//! that hold whatever the arguments are. Nothing here touches herdr-core.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use herdr_wake::cli::render_setup;
use herdr_wake::platform::{Action, RUN_PREFIX};
use herdr_wake::setup::Report;

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_herdr-wake"));
    c.env_clear().stdin(Stdio::null());
    c
}

#[test]
fn help_exits_zero_and_lists_the_user_commands_only() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for cmd in [
        "install",
        "uninstall",
        "status",
        "doctor",
        "forget",
        "off",
        "on",
        "resume",
        "adopt",
        "resurrect",
    ] {
        assert!(
            text.contains(&format!("\n  {cmd} ")),
            "{cmd} missing:\n{text}"
        );
    }
    for hidden in ["\n  hook", "\n  watch", "\n  shell", "\n  boot"] {
        assert!(!text.contains(hidden), "{hidden} listed:\n{text}");
    }
}

#[test]
fn version_prints_the_crate_version() {
    let out = bin().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        format!("herdr-wake {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn install_help_says_what_before_login_does_on_linux() {
    let out = bin().args(["install", "--help"]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("Linux: has no effect"), "{text}");
    assert!(text.contains("--dry-run") && text.contains("--host"));
}

#[test]
fn hook_never_prints_or_fails_on_bad_arguments() {
    for args in [
        vec!["hook"],
        vec!["hook", "claude"],
        vec!["hook", "not-an-agent", "start"],
        vec!["hook", "--help"],
        vec!["hook", "claude", "start", "extra", "--bogus"],
    ] {
        let home = tempfile::tempdir().unwrap();
        let out = bin().args(&args).env("HOME", home.path()).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?} printed to stdout");
        assert!(out.stderr.is_empty(), "{args:?} printed to stderr");
    }
}

#[test]
fn shell_with_extra_arguments_and_no_core_still_execs_a_shell() {
    // No HC_TERMINAL_ID and no core: the wrapper must end in the real shell.
    // `real_shell` makes that shell print a marker and exit.
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join(".config/herdr-wake");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        "real_shell = [\"/bin/sh\", \"-c\", \"echo REAL-SHELL\"]\n",
    )
    .unwrap();
    let out = bin()
        .args(["shell", "--bogus", "more"])
        .env("HOME", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "REAL-SHELL");
}

#[test]
fn unknown_command_exits_two() {
    let out = bin().arg("bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn setup_output_puts_commands_to_run_last_and_loud() {
    let report = Report {
        actions: vec![
            Action::new("write the plist", Some(PathBuf::from("/x/y.plist"))),
            Action::new(
                format!("{RUN_PREFIX}sudo launchctl bootstrap system /x"),
                None,
            ),
            Action::new("back up /a/b", Some(PathBuf::from("/bk/b"))),
        ],
        warnings: vec!["careful".to_string()],
    };
    let text = render_setup("install", false, &report);
    let run_at = text.find("RUN THESE YOURSELF").expect("heading");
    assert!(text.find("write the plist (/x/y.plist)").unwrap() < run_at);
    assert!(text.find("back up /a/b (/bk/b)").unwrap() < run_at);
    assert!(text.find("warning: careful").unwrap() < run_at);
    assert!(text[run_at..].contains("\n  sudo launchctl bootstrap system /x\n"));
    assert!(!text.contains(RUN_PREFIX));
    assert!(text.contains("herdr-core starts now"));
}

#[test]
fn dry_run_output_says_nothing_changed() {
    let report = Report {
        actions: vec![Action::new("set herdr-core's shell", None)],
        warnings: Vec::new(),
    };
    let text = render_setup("uninstall", true, &report);
    assert!(text.starts_with("herdr-wake uninstall: dry run, nothing was changed"));
    assert!(!text.contains("boot job"));
}

// ---- M-4: never as root ----

#[test]
fn commands_that_write_refuse_to_run_as_root() {
    use herdr_wake::cli::refuse_root;
    for cmd in [
        "install",
        "uninstall",
        "adopt",
        "resume",
        "forget",
        "on",
        "off",
        "resurrect",
    ] {
        let err = refuse_root(cmd, 0).unwrap_err();
        assert!(err.contains(cmd), "{err}");
        assert!(err.contains("as your own user"), "{err}");
        assert!(err.contains("--before-login"), "{err}");
        assert!(err.contains("sudo commands"), "{err}");
        assert_eq!(refuse_root(cmd, 501), Ok(()));
    }
}

// ---- config.toml that cannot be parsed: doctor reports, it does not die ----

#[test]
fn doctor_reports_a_broken_config_as_a_failed_check() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config/herdr-wake");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), "restore_bypass = fales\n").unwrap();
    let out = bin()
        .arg("doctor")
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stdout.contains("FAIL  config:"), "{stdout}{stderr}");
    assert!(stdout.contains("config.toml"), "{stdout}");
    assert!(
        stdout.contains("core shell"),
        "the other checks still run: {stdout}"
    );
}

// ---- resurrect ----

/// `herdr-wake resurrect` in a clean home with no herdr-core.
fn resurrect(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    bin()
        .arg("resurrect")
        .args(args)
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home)
        .output()
        .unwrap()
}

#[test]
fn resurrect_help_lists_its_options_and_hides_the_internal_one() {
    let out = bin().args(["resurrect", "--help"]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    for word in [
        "--here", "--folder", "--agent", "--limit", "--json", "--print", "--fork",
    ] {
        assert!(text.contains(word), "{word} missing:\n{text}");
    }
    assert!(!text.contains("--run"), "{text}");
    assert!(!text.contains('\u{2014}'));
}

#[test]
fn resurrect_with_nothing_recorded_says_so_and_does_not_need_core() {
    let home = tempfile::tempdir().unwrap();
    let out = resurrect(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "No closed sessions found.\n"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("herdr-core is not running"));

    let out = resurrect(home.path(), &["--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sessions"], serde_json::json!([]));

    let out = resurrect(home.path(), &["two", "words"]);
    assert_eq!(out.status.code(), Some(2), "one search argument only");
}

#[test]
fn resurrect_refuses_what_makes_no_sense() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let err = |args: &[&str]| {
        let out = resurrect(h, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(err(&["3"]).contains("no earlier listing"));
    assert!(err(&["--print"]).contains("need a number"));
    assert!(err(&["--fork"]).contains("need a number"));
    assert!(err(&["--agent", "emacs"]).contains("unknown agent"));
    assert!(err(&["no-such-session-id", "--print"]).contains("no session with the id"));
    let out = resurrect(h, &["--here", "--folder", "/x"]);
    assert_eq!(out.status.code(), Some(2));
    let out = resurrect(h, &["--id", "123", "something"]);
    assert_eq!(out.status.code(), Some(2), "--id and a target do not mix");
    assert!(err(&["--id", "123", "--print"]).contains("no session with the id"));
}

#[test]
fn resurrect_print_gives_the_command_for_a_journaled_session() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    // A program named claude that the safety check accepts.
    let bindir = home.path().join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    let claude = bindir.join("claude");
    std::fs::write(&claude, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let id = "0b5a2c1e-1111-4222-8333-444455556666";
    // Its transcript and its journal entry.
    let projects = home
        .path()
        .join(".claude/projects")
        .join(herdr_wake::agents::claude::project_dir_name(work.path()));
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join(format!("{id}.jsonl")),
        serde_json::json!({"type":"user","cwd":work.path(),"message":{"role":"user","content":"hi"}})
            .to_string()
            + "\n",
    )
    .unwrap();
    let mut rec = herdr_wake::record::Record::new(
        herdr_wake::record::AgentKind::Claude,
        id,
        work.path().to_path_buf(),
        claude.clone(),
        "b",
    );
    rec.flags = vec!["--model".into(), "opus".into()];
    let dir = home.path().join(".local/state/herdr-wake/history");
    herdr_wake::history::started(&dir, 500, &"t1".into(), &rec, 1000).unwrap();
    herdr_wake::history::ended(
        &dir,
        500,
        rec.agent,
        id,
        None,
        herdr_wake::history::Ending::Quit,
        2000,
    )
    .unwrap();

    let out = bin()
        .args(["resurrect", "--print", id])
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        printed.trim_end(),
        format!(
            "cd {} && {} --model opus --resume {id}",
            work.path().display(),
            claude.display()
        )
    );

    // Opening a terminal needs a running core, and says so.
    let out = bin()
        .args(["resurrect", id])
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("herdr-core is not running"));
}

#[test]
fn resurrect_run_with_no_ticket_says_why_and_ends_in_a_shell() {
    let home = tempfile::tempdir().unwrap();
    let out = bin()
        .args(["resurrect", "--run", "nope"])
        .env("HOME", home.path())
        .env("SHELL", "/bin/sh")
        .current_dir(home.path())
        .output()
        .unwrap();
    // stdin is closed, so the shell exits at once.
    assert_eq!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot bring the session back"), "{err}");
}
