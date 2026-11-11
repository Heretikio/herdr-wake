//! L-9: `scripts/install.sh` and `scripts/uninstall.sh`. They stay POSIX sh,
//! install.sh stages with `mktemp`, and uninstall.sh runs the binary it was
//! told about or the default one, not whatever `herdr-wake` is on `PATH`.
//! Everything runs in a temporary home; nothing is downloaded.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name)
}

fn text(name: &str) -> String {
    std::fs::read_to_string(script(name)).unwrap()
}

#[test]
fn both_scripts_parse_as_posix_sh() {
    for name in ["install.sh", "uninstall.sh"] {
        for shell in ["sh", "dash"] {
            let Ok(out) = Command::new(shell).arg("-n").arg(script(name)).output() else {
                eprintln!("{shell} is not installed; skipping its syntax check");
                continue;
            };
            assert!(
                out.status.success(),
                "{shell} -n {name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let body = text(name);
        assert!(body.starts_with("#!/bin/sh\n"), "{name}");
        for bashism in ["[[", "local ", "function ", "<<<", "$'"] {
            assert!(!body.contains(bashism), "{name} uses {bashism}");
        }
    }
}

#[test]
fn install_stages_with_mktemp_and_says_what_the_checksum_is_for() {
    let body = text("install.sh");
    assert!(
        body.contains(r#"mktemp "$bin_dir/.herdr-wake.new.XXXXXX""#),
        "the staged file must come from mktemp in the bin dir"
    );
    assert!(
        !body.contains(".herdr-wake.new.$$"),
        "no predictable staged name"
    );
    let lower = body.to_lowercase();
    assert!(lower.contains("corrupt"), "say what the checksum catches");
    assert!(
        lower.contains("not against a compromised release")
            || lower.contains("not a compromised release"),
        "say what the checksum does not catch"
    );
}

/// A stand-in `herdr-wake` that records that it ran, and with what.
fn stub(path: &Path, tag: &str, marker: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' {tag} \"$*\" >> '{}'\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        Sandbox {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn marker(&self) -> PathBuf {
        self.path("ran.log")
    }

    fn ran(&self) -> String {
        std::fs::read_to_string(self.marker()).unwrap_or_default()
    }

    /// Runs `uninstall.sh --dry-run` with `HOME` in the sandbox and exactly
    /// the given extra environment.
    fn uninstall(&self, path: &str, extra: &[(&str, &str)]) -> Output {
        std::fs::create_dir_all(self.path("home")).unwrap();
        Command::new("sh")
            .arg(script("uninstall.sh"))
            .arg("--dry-run")
            .env_clear()
            .env("HOME", self.path("home"))
            .env("PATH", path)
            .envs(extra.iter().copied())
            .current_dir(self.dir.path())
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

fn out_text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

const SYS: &str = "/usr/bin:/bin";

#[test]
fn uninstall_runs_the_default_install_and_ignores_path() {
    let s = Sandbox::new();
    stub(
        &s.path("home/.local/bin/herdr-wake"),
        "default",
        &s.marker(),
    );
    stub(&s.path("elsewhere/herdr-wake"), "on-path", &s.marker());
    let path = format!("{}:{SYS}", s.path("elsewhere").display());
    let out = s.uninstall(&path, &[]);
    assert!(out.status.success(), "{}", out_text(&out));
    assert_eq!(s.ran(), "default uninstall --dry-run\n");
}

#[test]
fn uninstall_prefers_herdr_wake_bin_over_everything() {
    let s = Sandbox::new();
    stub(
        &s.path("home/.local/bin/herdr-wake"),
        "default",
        &s.marker(),
    );
    stub(&s.path("elsewhere/herdr-wake"), "on-path", &s.marker());
    stub(&s.path("chosen/herdr-wake"), "chosen", &s.marker());
    let path = format!("{}:{SYS}", s.path("elsewhere").display());
    let chosen = s.path("chosen/herdr-wake");
    let out = s.uninstall(&path, &[("HERDR_WAKE_BIN", chosen.to_str().unwrap())]);
    assert!(out.status.success(), "{}", out_text(&out));
    assert_eq!(s.ran(), "chosen uninstall --dry-run\n");

    // A HERDR_WAKE_BIN that is not there is an error, not a PATH lookup.
    std::fs::remove_file(s.marker()).unwrap();
    let missing = s.path("nope/herdr-wake");
    let out = s.uninstall(&path, &[("HERDR_WAKE_BIN", missing.to_str().unwrap())]);
    assert!(!out.status.success());
    assert_eq!(s.ran(), "");
}

#[test]
fn uninstall_falls_back_to_path_only_for_an_absolute_match_and_says_which() {
    let s = Sandbox::new();
    stub(&s.path("elsewhere/herdr-wake"), "on-path", &s.marker());
    let found = s.path("elsewhere/herdr-wake");
    let path = format!("{}:{SYS}", s.path("elsewhere").display());
    let out = s.uninstall(&path, &[]);
    assert!(out.status.success(), "{}", out_text(&out));
    assert_eq!(s.ran(), "on-path uninstall --dry-run\n");
    assert!(
        out_text(&out).contains(&format!("on your PATH at {}", found.display())),
        "{}",
        out_text(&out)
    );
}

#[test]
fn uninstall_never_runs_a_herdr_wake_from_a_relative_path_entry() {
    let s = Sandbox::new();
    // `herdr-wake` in the current folder, reachable through `.` on PATH.
    stub(&s.path("herdr-wake"), "planted", &s.marker());
    let out = s.uninstall(&format!(".:{SYS}"), &[]);
    assert!(!out.status.success(), "{}", out_text(&out));
    assert_eq!(s.ran(), "", "{}", out_text(&out));
}
