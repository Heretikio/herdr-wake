//! `install` must not record a versioned path (Homebrew's Cellar, a
//! `versions/` folder) as herdr-core's shell: the next upgrade deletes it
//! and every terminal loses its shell. The pure part, with path fixtures.

use std::path::{Path, PathBuf};

use herdr_wake::setup::{is_versioned_path, pick_stable_path};

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn versioned_paths_are_recognised() {
    for versioned in [
        "/opt/homebrew/Cellar/herdr-wake/0.1.0/bin/herdr-wake",
        "/usr/local/Cellar/herdr-wake/0.1.0_1/bin/herdr-wake",
        "/home/linuxbrew/.linuxbrew/Cellar/herdr-wake/1.2.3/bin/herdr-wake",
        "/home/u/.local/share/herdr-wake/versions/0.1.0/herdr-wake",
        "/home/u/.local/share/herdr-wake/versions/current/herdr-wake",
        "/opt/herdr-wake/0.1.0/herdr-wake",
        "/opt/herdr-wake/v1.2/bin/herdr-wake",
        "/opt/tools/2.1.322",
    ] {
        assert!(is_versioned_path(Path::new(versioned)), "{versioned}");
    }
    for stable in [
        "/opt/homebrew/bin/herdr-wake",
        "/usr/local/bin/herdr-wake",
        "/home/u/.local/bin/herdr-wake",
        "/home/u/.cargo/bin/herdr-wake",
        "/Users/u/git/herdr-wake/target/release/herdr-wake",
        "/opt/python3.11/bin/herdr-wake",
        "/srv/10/herdr-wake",
        "/home/u/v2/herdr-wake",
        "/home/u/1.x/herdr-wake",
    ] {
        assert!(!is_versioned_path(Path::new(stable)), "{stable}");
    }
}

#[test]
fn a_stable_path_is_recorded_as_it_was_run_without_resolving_symlinks() {
    // ~/.local/bin/herdr-wake is a symlink into a dotfiles checkout.
    let exe = p("/home/u/.local/bin/herdr-wake");
    let canonical = p("/home/u/dotfiles/bin/herdr-wake");
    assert_eq!(pick_stable_path(&exe, &canonical, &[]), Ok(exe.clone()));
    // A path with `..` in it is cleaned up through the resolved one.
    let odd = p("/home/u/.local/bin/../bin/herdr-wake");
    let real = p("/home/u/.local/bin/herdr-wake");
    assert_eq!(pick_stable_path(&odd, &real, &[]), Ok(real));
}

#[test]
fn homebrew_on_macos_keeps_the_bin_symlink_it_was_run_through() {
    let exe = p("/opt/homebrew/bin/herdr-wake");
    let canonical = p("/opt/homebrew/Cellar/herdr-wake/0.1.0/bin/herdr-wake");
    assert_eq!(pick_stable_path(&exe, &canonical, &[]), Ok(exe.clone()));
}

#[test]
fn a_versioned_path_is_replaced_by_a_symlink_that_leads_to_the_same_file() {
    // Linux reports the resolved path as the executable.
    let cellar = p("/home/linuxbrew/.linuxbrew/Cellar/herdr-wake/0.1.0/bin/herdr-wake");
    let links = [
        // Another herdr-wake first on PATH: not the same file.
        (
            p("/home/u/.local/bin/herdr-wake"),
            p("/home/u/.local/bin/herdr-wake"),
        ),
        // A link that is itself versioned is no better.
        (
            p("/home/linuxbrew/.linuxbrew/opt/herdr-wake/0.1.0/herdr-wake"),
            cellar.clone(),
        ),
        (
            p("/home/linuxbrew/.linuxbrew/bin/herdr-wake"),
            cellar.clone(),
        ),
        (p("/usr/local/bin/herdr-wake"), cellar.clone()),
    ];
    assert_eq!(
        pick_stable_path(&cellar, &cellar, &links),
        Ok(p("/home/linuxbrew/.linuxbrew/bin/herdr-wake"))
    );
}

#[test]
fn with_no_stable_symlink_install_refuses_and_says_why() {
    let cellar = p("/opt/homebrew/Cellar/herdr-wake/0.1.0/bin/herdr-wake");
    let other = [(
        p("/usr/local/bin/herdr-wake"),
        p("/usr/local/bin/herdr-wake"),
    )];
    let err = pick_stable_path(&cellar, &cellar, &other).unwrap_err();
    assert!(err.contains(&cellar.display().to_string()), "{err}");
    assert!(err.contains("upgrade"), "{err}");
    assert!(err.contains("symlink"), "{err}");
}
