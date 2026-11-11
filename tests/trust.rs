//! `trust`: the checks that decide whether a record may be acted on.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use herdr_wake::trust::{current_uid, folder_is_safe};

fn chmod(p: &Path, mode: u32) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A private (0700) temporary folder: on Linux it sits in sticky `/tmp`, on
/// macOS in the user's own `/var/folders/.../T`.
fn tmp() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    chmod(d.path(), 0o700);
    d
}

fn dir(parent: &Path, name: &str, mode: u32) -> std::path::PathBuf {
    let p = parent.join(name);
    std::fs::create_dir(&p).unwrap();
    chmod(&p, mode);
    p
}

#[test]
fn a_private_folder_inside_the_temp_folder_is_safe() {
    let t = tmp();
    assert_eq!(folder_is_safe(t.path(), current_uid()), Ok(()));
    let p = dir(t.path(), "proj", 0o755);
    assert_eq!(folder_is_safe(&p, current_uid()), Ok(()));
    let p = dir(t.path(), "private", 0o700);
    assert_eq!(folder_is_safe(&p, current_uid()), Ok(()));
}

#[test]
fn a_world_writable_folder_is_refused() {
    let t = tmp();
    for mode in [0o777, 0o1777, 0o757] {
        let p = dir(t.path(), &format!("m{mode:o}"), mode);
        let why = folder_is_safe(&p, current_uid()).unwrap_err();
        assert!(why.contains("writable"), "{mode:o}: {why}");
    }
}

#[test]
fn a_group_writable_folder_passes_only_for_a_private_group() {
    use std::os::unix::fs::MetadataExt;
    let t = tmp();
    let p = dir(t.path(), "m775", 0o775);
    let gid = std::fs::metadata(&p).unwrap().gid();
    let res = folder_is_safe(&p, current_uid());
    if herdr_wake::trust::group_is_private(gid, current_uid()) {
        assert_eq!(res, Ok(()));
    } else {
        assert!(res.unwrap_err().contains("group"));
    }
}

#[test]
fn a_file_a_symlink_and_a_missing_path_are_refused() {
    let t = tmp();
    let real = dir(t.path(), "real", 0o755);
    let file = t.path().join("file");
    std::fs::write(&file, "x").unwrap();
    assert!(folder_is_safe(&file, current_uid())
        .unwrap_err()
        .contains("not a folder"));
    let link = t.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(folder_is_safe(&link, current_uid())
        .unwrap_err()
        .contains("symlink"));
    assert!(folder_is_safe(&t.path().join("gone"), current_uid()).is_err());
    assert!(folder_is_safe(Path::new("relative/path"), current_uid())
        .unwrap_err()
        .contains("absolute"));
}

#[test]
fn a_folder_owned_by_someone_else_is_refused() {
    let t = tmp();
    let p = dir(t.path(), "proj", 0o755);
    let why = folder_is_safe(&p, current_uid() + 1).unwrap_err();
    assert!(why.contains("not owned by you"), "{why}");
}

#[test]
fn a_world_writable_parent_without_the_sticky_bit_is_refused() {
    let t = tmp();
    let open = dir(t.path(), "open", 0o777);
    let p = dir(&open, "proj", 0o755);
    let why = folder_is_safe(&p, current_uid()).unwrap_err();
    assert!(why.contains("world-writable"), "{why}");
    assert!(why.contains("open"), "{why}");
    // Deeper down is refused too.
    let deep = dir(&p, "deep", 0o755);
    assert!(folder_is_safe(&deep, current_uid()).is_err());
    // With the sticky bit (like /tmp) others cannot replace the folder.
    chmod(&open, 0o1777);
    assert_eq!(folder_is_safe(&p, current_uid()), Ok(()));
}

#[test]
fn a_symlinked_parent_is_judged_by_where_it_leads() {
    let t = tmp();
    let open = dir(t.path(), "open", 0o777);
    dir(&open, "proj", 0o755);
    let link = t.path().join("link");
    std::os::unix::fs::symlink(&open, &link).unwrap();
    assert!(folder_is_safe(&link.join("proj"), current_uid()).is_err());
    chmod(&open, 0o755);
    assert_eq!(folder_is_safe(&link.join("proj"), current_uid()), Ok(()));
}

// ------------------------------------------------- session ids (M-5 b)

#[test]
fn session_id_shapes() {
    use herdr_wake::trust::valid_session_id;
    for good in [
        "0b5a2c1e-1111-4222-8333-444455556666",
        "ses_01HXYZ",
        "a",
        "claude-t1-4242",
        "2026.11.10_x",
        &"a".repeat(128),
    ] {
        assert!(valid_session_id(good), "{good}");
    }
    for bad in [
        "",
        "--dangerously-skip-permissions",
        "-x",
        ".hidden",
        "_x",
        "../../etc/passwd",
        "a/b",
        "a b",
        "a\nb",
        "a\u{1b}[31m",
        "id;rm",
        "caf\u{e9}",
        &"a".repeat(129),
    ] {
        assert!(!valid_session_id(bad), "{bad:?}");
    }
}

// ------------------------------------------------- the program (M-5 c)

use herdr_wake::record::AgentKind;
use herdr_wake::trust::bin_is_safe;

fn program(dir: &Path, name: &str, mode: u32) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, "#!/bin/sh\n").unwrap();
    chmod(&p, mode);
    p
}

#[test]
fn the_agents_own_program_is_safe() {
    let t = tmp();
    for (kind, name) in [
        (AgentKind::Claude, "claude"),
        (AgentKind::Codex, "codex"),
        (AgentKind::Pi, "pi"),
        (AgentKind::OpenCode, "opencode"),
    ] {
        let p = program(t.path(), name, 0o755);
        assert_eq!(bin_is_safe(kind, &p, current_uid()), Ok(()), "{name}");
    }
}

#[test]
fn a_program_under_another_name_is_refused() {
    let t = tmp();
    let p = program(t.path(), "evil-program", 0o755);
    let why = bin_is_safe(AgentKind::Claude, &p, current_uid()).unwrap_err();
    assert!(why.contains("not claude"), "{why}");
    // One agent's program is not another's.
    let p = program(t.path(), "codex", 0o755);
    assert!(bin_is_safe(AgentKind::Claude, &p, current_uid()).is_err());
    // An interpreter is never the agent.
    assert!(bin_is_safe(AgentKind::Pi, Path::new("/bin/sh"), current_uid()).is_err());
}

#[test]
fn launcher_shapes_the_adapters_know_are_accepted() {
    let t = tmp();
    // Claude's native build: ~/.local/share/claude/versions/<version>.
    let versions = t.path().join("claude/versions");
    std::fs::create_dir_all(&versions).unwrap();
    let native = program(&versions, "2.1.322", 0o755);
    assert_eq!(
        bin_is_safe(AgentKind::Claude, &native, current_uid()),
        Ok(())
    );
    assert!(bin_is_safe(AgentKind::Codex, &native, current_uid()).is_err());
    // The stable name is a symlink to it.
    let link = t.path().join("claude-link");
    std::os::unix::fs::symlink(&native, &link).unwrap();
    assert_eq!(bin_is_safe(AgentKind::Claude, &link, current_uid()), Ok(()));
    // An npm launcher: `codex` is a symlink to a script with another name.
    let script = program(t.path(), "cli-entry.js", 0o755);
    let bin = t.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(&script, bin.join("codex")).unwrap();
    assert_eq!(
        bin_is_safe(AgentKind::Codex, &bin.join("codex"), current_uid()),
        Ok(())
    );
    let js = program(t.path(), "codex.js", 0o755);
    assert_eq!(bin_is_safe(AgentKind::Codex, &js, current_uid()), Ok(()));
}

#[test]
fn a_program_that_is_missing_relative_or_not_executable_is_refused() {
    let t = tmp();
    assert!(
        bin_is_safe(AgentKind::Claude, Path::new("claude"), current_uid())
            .unwrap_err()
            .contains("absolute")
    );
    assert!(bin_is_safe(AgentKind::Claude, &t.path().join("claude"), current_uid()).is_err());
    let p = program(t.path(), "claude", 0o644);
    assert!(bin_is_safe(AgentKind::Claude, &p, current_uid())
        .unwrap_err()
        .contains("executable"));
    let d = t.path().join("d");
    std::fs::create_dir_all(d.join("codex")).unwrap();
    assert!(
        bin_is_safe(AgentKind::Codex, &d.join("codex"), current_uid())
            .unwrap_err()
            .contains("regular file")
    );
}

#[test]
fn a_program_others_can_write_or_someone_else_owns_is_refused() {
    let t = tmp();
    for mode in [0o775, 0o757, 0o777] {
        let p = program(t.path(), "claude", mode);
        let why = bin_is_safe(AgentKind::Claude, &p, current_uid()).unwrap_err();
        assert!(why.contains("writable"), "{mode:o}: {why}");
    }
    let p = program(t.path(), "claude", 0o755);
    let why = bin_is_safe(AgentKind::Claude, &p, current_uid() + 1).unwrap_err();
    assert!(why.contains("owned"), "{why}");
}
