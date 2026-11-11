//! L-6 and M-4: every file herdr-wake writes under its own config and state
//! folders is 0600 in a 0700 folder, whatever the umask, and is never
//! written through a symlink.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{sample_record, Fixture};
use herdr_wake::config::Config;
use herdr_wake::core::TerminalId;
use herdr_wake::record::AgentKind;
use herdr_wake::{boot, gate, paths, record, wake};

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

fn chmod(p: &Path, mode: u32) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn the_gate_file_is_private() {
    let f = Fixture::temp();
    let dir = paths::gate_dir(&f.env, &f.home);
    gate::enter(&dir, AgentKind::Claude, &f.cfg, "k", 1000);
    assert_eq!(mode(&dir.join("k.json")), 0o600);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&paths::state_dir(&f.env, &f.home)), 0o700);
}

#[test]
fn a_gate_file_left_loose_by_an_older_build_is_tightened() {
    let f = Fixture::temp();
    let dir = paths::gate_dir(&f.env, &f.home);
    std::fs::create_dir_all(&dir).unwrap();
    chmod(&dir, 0o755);
    std::fs::write(dir.join("k.json"), "{}").unwrap();
    chmod(&dir.join("k.json"), 0o644);
    gate::enter(&dir, AgentKind::Claude, &f.cfg, "k", 1000);
    assert_eq!(mode(&dir.join("k.json")), 0o600);
    assert_eq!(mode(&dir), 0o700);
}

#[test]
fn the_gate_never_writes_through_a_symlink() {
    let f = Fixture::temp();
    let victim = f.home.join("victim");
    std::fs::write(&victim, "precious").unwrap();
    let dir = paths::gate_dir(&f.env, &f.home);
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&victim, dir.join("k.json")).unwrap();
    // The gate leans toward starting: no wait, and nothing written.
    assert_eq!(gate::enter(&dir, AgentKind::Claude, &f.cfg, "k", 1000), 0);
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
}

#[test]
fn the_boot_summary_is_private() {
    let f = Fixture::temp();
    let path = paths::last_boot_file(&f.env, &f.home);
    let s = boot::BootSummary {
        boot_id: "b".into(),
        started_ms: 1,
        finished_ms: 2,
        duration_ms: 1,
        core_started: true,
        resumed: 0,
        failed: 0,
        skipped: 0,
        unconfirmed: 0,
        details: vec![],
    };
    boot::write_summary(&path, &s).unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
}

#[test]
fn claims_are_private() {
    let f = Fixture::temp();
    let dir = paths::claims_dir(&f.env, &f.home).join("k");
    assert!(record::claim(&dir, "s-1", &TerminalId::from("t1")));
    assert!(!record::claim(&dir, "s-1", &TerminalId::from("t2")));
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(mode(&files[0]), 0o600);
    assert_eq!(mode(&dir), 0o700);
}

#[test]
fn the_unconfirmed_marker_and_the_log_are_private() {
    let f = Fixture::temp();
    let job = wake::ConfirmJob {
        terminal: TerminalId::from("t1"),
        session_id: "s".into(),
        within_ms: 1,
        boot_key: "k".into(),
    };
    wake::mark_unconfirmed(&f.env, &f.home, &job, "why", 5);
    let marker = wake::unconfirmed_file(&f.env, &f.home, "k", &job.terminal);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "why");
    assert_eq!(mode(&marker), 0o600);
    let log = paths::log_file(&f.env, &f.home);
    assert!(std::fs::read_to_string(&log)
        .unwrap()
        .contains("unconfirmed"));
    assert_eq!(mode(&log), 0o600);
}

#[test]
fn a_log_left_loose_is_tightened_and_a_symlinked_log_is_not_followed() {
    let f = Fixture::temp();
    let log = paths::log_file(&f.env, &f.home);
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "old\n").unwrap();
    chmod(&log, 0o644);
    let job = wake::ConfirmJob {
        terminal: TerminalId::from("t1"),
        session_id: "s".into(),
        within_ms: 1,
        boot_key: "k".into(),
    };
    wake::mark_unconfirmed(&f.env, &f.home, &job, "why", 5);
    assert_eq!(mode(&log), 0o600);
    assert!(std::fs::read_to_string(&log).unwrap().starts_with("old\n"));

    let victim = f.home.join("victim");
    std::fs::write(&victim, "precious").unwrap();
    std::fs::remove_file(&log).unwrap();
    std::os::unix::fs::symlink(&victim, &log).unwrap();
    wake::mark_unconfirmed(&f.env, &f.home, &job, "why", 5);
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
}

#[test]
fn config_save_is_private_and_tightens_an_existing_file() {
    let f = Fixture::temp();
    let path = paths::config_file(&f.env, &f.home);
    Config::default().save(&path).unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);

    chmod(&path, 0o644);
    chmod(path.parent().unwrap(), 0o755);
    Config::default().save(&path).unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
}

#[test]
fn config_save_does_not_follow_a_planted_symlink() {
    // The audit's demonstration: `config.toml.tmp` as a symlink made save
    // overwrite the link's target and move the link into place.
    let f = Fixture::temp();
    let path = paths::config_file(&f.env, &f.home);
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let victim = f.home.join("victim");
    std::fs::write(&victim, "precious").unwrap();
    std::os::unix::fs::symlink(&victim, dir.join("config.toml.tmp")).unwrap();

    Config::default().save(&path).unwrap();

    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    assert!(std::fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_file());

    // And a symlink at config.toml itself is refused outright.
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&victim, &path).unwrap();
    assert!(Config::default().save(&path).is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
}

#[test]
fn the_record_copy_is_private() {
    let f = Fixture::temp();
    let dir = paths::records_dir(&f.env, &f.home);
    let t = TerminalId::from("t1");
    record::write_copy(&dir, &t, 0, &sample_record()).unwrap();
    assert_eq!(mode(&record::copy_file(&dir, &t)), 0o600);
    assert_eq!(mode(&dir), 0o700);
}
