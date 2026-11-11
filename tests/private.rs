//! `private`: the writer for herdr-wake's own config and state files.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use herdr_wake::private;

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

fn chmod(p: &Path, mode: u32) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn write_creates_a_0600_file_in_0700_folders() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("state/sub/a.json");
    private::write(&file, b"one").unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"one");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&t.path().join("state/sub")), 0o700);
    assert_eq!(mode(&t.path().join("state")), 0o700);
}

#[test]
fn write_replaces_and_tightens_what_was_there() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("state");
    std::fs::create_dir(&dir).unwrap();
    chmod(&dir, 0o755);
    let file = dir.join("a.json");
    std::fs::write(&file, "old").unwrap();
    chmod(&file, 0o644);

    private::write(&file, b"new").unwrap();

    assert_eq!(std::fs::read(&file).unwrap(), b"new");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&dir), 0o700);
    // No temporary file is left behind.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn write_refuses_a_symlink_and_leaves_its_target_alone() {
    let t = tempfile::tempdir().unwrap();
    let victim = t.path().join("victim");
    std::fs::write(&victim, "precious").unwrap();
    let dir = t.path().join("state");
    std::fs::create_dir(&dir).unwrap();
    let link = dir.join("config.toml");
    std::os::unix::fs::symlink(&victim, &link).unwrap();

    let err = private::write(&link, b"x").unwrap_err();

    assert!(err.to_string().contains("symlink"), "{err}");
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn a_symlink_where_the_folder_should_be_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let elsewhere = t.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let link = t.path().join("state");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    assert!(private::write(&link.join("a"), b"x").is_err());
    assert!(private::append(&link.join("log"), "x\n").is_err());
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn append_creates_0600_tightens_and_never_follows_a_symlink() {
    let t = tempfile::tempdir().unwrap();
    let log = t.path().join("state/x.log");
    private::append(&log, "one\n").unwrap();
    assert_eq!(mode(&log), 0o600);
    chmod(&log, 0o644);
    private::append(&log, "two\n").unwrap();
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "one\ntwo\n");
    assert_eq!(mode(&log), 0o600);

    let victim = t.path().join("victim");
    std::fs::write(&victim, "precious").unwrap();
    std::fs::remove_file(&log).unwrap();
    std::os::unix::fs::symlink(&victim, &log).unwrap();
    assert!(private::append(&log, "evil\n").is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
}

#[test]
fn open_rw_and_create_new_are_private_too() {
    let t = tempfile::tempdir().unwrap();
    let gate = t.path().join("gate/k.json");
    drop(private::open_rw(&gate).unwrap());
    assert_eq!(mode(&gate), 0o600);
    assert_eq!(mode(&t.path().join("gate")), 0o700);

    let claim = t.path().join("claims/c");
    private::create_new(&claim, b"t1").unwrap();
    assert_eq!(mode(&claim), 0o600);
    assert!(private::create_new(&claim, b"t2").is_err());
    assert_eq!(std::fs::read(&claim).unwrap(), b"t1");
}
