//! (h) `install` and `uninstall` end to end in the throwaway home.
//!
//! The boot service cannot be faked in a real binary, so the real runs set
//! the test-only switch `HERDR_WAKE_SKIP_BOOT_SERVICE=1`: everything except
//! the launchd or systemd job is installed and removed for real. The dry
//! runs go through the whole path, boot service included, and must change
//! nothing.

use crate::harness::{Harness, BIN};

const SETTINGS: &str = r#"{
  // my own settings
  "model": "opus",
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "echo mine" }] },
    ],
  },
}
"#;

/// A home that looks like a machine with all four agents and a user shell
/// setting in core's config. No core is started.
fn lived_in_home() -> Option<Harness> {
    let h = Harness::new()?;
    h.write_core_config("# my shell\nshell = [\"/bin/sh\"] # keep\nnotes = false\n");
    std::fs::remove_dir_all(h.home.join(".config/herdr-wake")).unwrap();
    for dir in [".claude", ".codex", ".pi/agent", ".config/opencode"] {
        std::fs::create_dir_all(h.home.join(dir)).unwrap();
    }
    std::fs::write(h.home.join(".claude/settings.json"), SETTINGS).unwrap();
    Some(h)
}

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn dry_runs_change_no_file() {
    let Some(h) = lived_in_home() else { return };
    let before = h.snapshot();

    let out = h.hw(&["install", "--dry-run"]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("dry run, nothing was changed"), "{said}");
    assert!(said.contains("set herdr-core's shell to"), "{said}");
    assert!(said.contains("SessionStart hook"), "{said}");
    assert_eq!(h.snapshot(), before, "install --dry-run changed a file");

    let out = h.hw(&["uninstall", "--dry-run"]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("dry run, nothing was changed"), "{said}");
    assert_eq!(h.snapshot(), before, "uninstall --dry-run changed a file");

    // The same after a real install: a dry-run uninstall leaves it all.
    let skip = [("HERDR_WAKE_SKIP_BOOT_SERVICE", "1")];
    assert!(h.hw_env(&["install"], &skip).status.success());
    let installed = h.snapshot();
    let out = h.hw(&["uninstall", "--dry-run"]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("set herdr-core's shell to [\"/bin/sh\"]"),
        "{said}"
    );
    assert_eq!(h.snapshot(), installed);
}

#[test]
fn install_then_uninstall_restores_the_home() {
    let Some(h) = lived_in_home() else { return };
    let core_before = std::fs::read_to_string(h.core_config()).unwrap();
    let skip = [("HERDR_WAKE_SKIP_BOOT_SERVICE", "1"), ("SHELL", "/bin/sh")];

    let out = h.hw_env(&["install"], &skip);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("test-only switch"), "{said}");

    // Core's shell is this binary by its absolute path; the old one is saved.
    let core = std::fs::read_to_string(h.core_config()).unwrap();
    assert!(
        core.contains(&format!("shell = [\"{BIN}\", \"shell\"] # keep")),
        "{core}"
    );
    assert!(core.contains("# my shell") && core.contains("notes = false"));
    let cfg = std::fs::read_to_string(h.home.join(".config/herdr-wake/config.toml")).unwrap();
    assert!(cfg.contains("real_shell = [\"/bin/sh\"]"), "{cfg}");
    let boot_env = std::fs::read_to_string(h.home.join(".config/herdr-wake/boot.env")).unwrap();
    assert!(
        boot_env.contains("PATH=") && boot_env.contains("SHELL=/bin/sh"),
        "{boot_env}"
    );

    // Every agent's integration is in place, next to the user's own entries.
    let settings = std::fs::read_to_string(h.home.join(".claude/settings.json")).unwrap();
    assert!(settings.contains("// my own settings") && settings.contains("echo mine"));
    assert!(
        settings.contains(&format!("'{BIN}' hook claude start")),
        "{settings}"
    );
    assert!(settings.contains(&format!("'{BIN}' hook claude end")));
    let codex = std::fs::read_to_string(h.home.join(".codex/hooks.json")).unwrap();
    assert!(
        codex.contains(&format!("'{BIN}' hook codex start")),
        "{codex}"
    );
    assert!(h.home.join(".pi/agent/extensions/herdr-wake.ts").is_file());
    assert!(h.home.join(".config/opencode/herdr-wake-tui.js").is_file());

    // The switch kept the boot service out of it.
    assert!(!h.home.join("Library").exists());
    assert!(!h.home.join(".config/systemd").exists());

    // Doctor agrees with what was installed (core is down, which is fine).
    let out = h.hw_env(&["doctor"], &skip);
    let doc = text(&out);
    for line in [
        "ok    core shell:",
        "ok    real shell:",
        "ok    claude hook:",
        "ok    codex hook:",
        "ok    pi extension:",
        "ok    opencode plugin:",
    ] {
        assert!(doc.contains(line), "{line} missing:\n{doc}");
    }

    // Installing again changes nothing.
    let installed = h.snapshot();
    let out = h.hw_env(&["install"], &skip);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(h.snapshot(), installed, "a second install changed a file");

    let out = h.hw_env(&["uninstall"], &skip);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("up to 60 s"), "{said}");

    // The user's files are as they were, byte for byte.
    assert_eq!(
        std::fs::read_to_string(h.core_config()).unwrap(),
        core_before
    );
    assert_eq!(
        std::fs::read_to_string(h.home.join(".claude/settings.json")).unwrap(),
        SETTINGS
    );
    // What install created is gone (to the Trash or the backups).
    for gone in [
        ".codex/hooks.json",
        ".pi/agent/extensions/herdr-wake.ts",
        ".config/opencode/herdr-wake-tui.js",
        ".config/opencode/tui.jsonc",
        ".config/herdr-wake",
    ] {
        assert!(!h.home.join(gone).exists(), "{gone} is still there");
    }
    // Backups stay, and the binary is never deleted.
    assert!(h.home.join(".local/state/herdr-wake/backups").is_dir());
    assert!(std::path::Path::new(BIN).is_file());
    assert!(h.processes().is_empty(), "setup started something");
}
