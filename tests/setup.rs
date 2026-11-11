//! Setup: comment-preserving edits, install and uninstall order, the saved
//! real shell, and the remote install command lists. Every test runs in a
//! temporary home with the fakes.

mod common;

use std::path::{Path, PathBuf};

use herdr_wake::agents::{Edit, EditKind};
use herdr_wake::setup::edits::{self, EditEnv};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/setup");

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap()
}

fn edit_env(home: &Path) -> EditEnv {
    EditEnv {
        backups: home.join("backups"),
        trash: Some(home.join(".Trash")),
        now_ms: 1_700_000_000_000,
        backed_up: Default::default(),
    }
}

fn claude_hooks(path: &Path, bin: &str) -> Vec<Edit> {
    vec![
        Edit {
            path: path.to_path_buf(),
            kind: EditKind::JsonHook {
                event: "SessionStart".into(),
                matcher: Some("^(startup|resume|clear|compact|fork)$".into()),
                command: format!("'{bin}' hook claude start"),
                timeout: 10,
            },
        },
        Edit {
            path: path.to_path_buf(),
            kind: EditKind::JsonHook {
                event: "SessionEnd".into(),
                matcher: None,
                command: format!("'{bin}' hook claude end"),
                timeout: 5,
            },
        },
    ]
}

/// The plain JSON value of a JSONC text.
fn parse(text: &str) -> serde_json::Value {
    fn conv(v: jsonc_parser::JsonValue) -> serde_json::Value {
        use jsonc_parser::JsonValue as J;
        match v {
            J::String(s) => serde_json::Value::String(s.into_owned()),
            J::Number(n) => serde_json::from_str(n).unwrap(),
            J::Boolean(b) => serde_json::Value::Bool(b),
            J::Null => serde_json::Value::Null,
            J::Array(a) => serde_json::Value::Array(a.take_inner().into_iter().map(conv).collect()),
            J::Object(o) => serde_json::Value::Object(
                o.take_inner()
                    .into_iter()
                    .map(|(k, v)| (k.into_owned(), conv(v)))
                    .collect(),
            ),
        }
    }
    let opts = jsonc_parser::ParseOptions::default();
    conv(jsonc_parser::parse_to_value(text, &opts).unwrap().unwrap())
}

#[test]
fn json_hook_round_trip_leaves_user_settings_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let original = fixture("claude-settings.json");
    std::fs::write(&path, &original).unwrap();
    let env = edit_env(dir.path());
    let hooks = claude_hooks(&path, "/opt/hw/herdr-wake");

    for e in &hooks {
        edits::apply(e, &env, false).unwrap();
    }
    let installed = std::fs::read_to_string(&path).unwrap();
    assert!(installed.contains("// keep this list short"), "{installed}");
    assert!(installed.contains("/* my own formatter hook */"));
    let v = parse(&installed);
    assert_eq!(
        v["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "~/bin/fmt-hook"
    );
    assert_eq!(
        v["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "'/opt/hw/herdr-wake' hook claude start"
    );
    assert_eq!(
        v["hooks"]["SessionStart"][0]["matcher"],
        "^(startup|resume|clear|compact|fork)$"
    );
    assert_eq!(v["hooks"]["SessionStart"][0]["hooks"][0]["timeout"], 10);
    assert_eq!(v["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 5);
    assert!(v["hooks"]["SessionEnd"][0].get("matcher").is_none());

    for e in hooks.iter().rev() {
        edits::revert(e, &env, false).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn json_hook_apply_twice_yields_one_entry_and_no_second_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, fixture("claude-settings.json")).unwrap();
    let env = edit_env(dir.path());
    let hooks = claude_hooks(&path, "/opt/hw/herdr-wake");
    for e in &hooks {
        edits::apply(e, &env, false).unwrap();
    }
    let once = std::fs::read_to_string(&path).unwrap();
    for e in &hooks {
        let actions = edits::apply(e, &env, false).unwrap();
        assert!(actions.is_empty(), "{actions:?}");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), once);
    let v = parse(&once);
    assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    assert_eq!(v["hooks"]["SessionEnd"].as_array().unwrap().len(), 1);
}

#[test]
fn json_hook_backs_up_the_original_before_the_first_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let original = fixture("claude-settings.json");
    std::fs::write(&path, &original).unwrap();
    let env = edit_env(dir.path());
    let hook = &claude_hooks(&path, "/opt/hw/herdr-wake")[0];
    edits::apply(hook, &env, false).unwrap();

    let backups: Vec<PathBuf> = std::fs::read_dir(&env.backups)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    let name = backups[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(name.starts_with("1700000000000-"), "{name}");
    assert!(name.ends_with("settings.json"), "{name}");
    assert!(!name[14..].contains('/'));
    assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn json_hook_write_keeps_the_original_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{}\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let env = edit_env(dir.path());
    edits::apply(&claude_hooks(&path, "/b/herdr-wake")[0], &env, false).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640);
}

#[test]
fn json_hook_creates_a_missing_file_with_minimal_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("codex").join("hooks.json");
    let env = edit_env(dir.path());
    let edit = Edit {
        path: path.clone(),
        kind: EditKind::JsonHook {
            event: "SessionStart".into(),
            matcher: None,
            command: "'/b/herdr-wake' hook codex start".into(),
            timeout: 10,
        },
    };
    edits::apply(&edit, &env, false).unwrap();
    let v = parse(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(
        v,
        serde_json::json!({"hooks": {"SessionStart": [{"hooks": [
            {"type": "command", "command": "'/b/herdr-wake' hook codex start", "timeout": 10}
        ]}]}})
    );
    assert!(!env.backups.exists(), "nothing to back up for a new file");
}

#[test]
fn json_hook_appends_after_user_entries_and_revert_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.json");
    let original = "{\n  \"hooks\": {\n    \"SessionStart\": [\n      {\"hooks\": [{\"type\": \"command\", \"command\": \"mine\"}]}\n    ]\n  }\n}\n";
    std::fs::write(&path, original).unwrap();
    let env = edit_env(dir.path());
    let edit = Edit {
        path: path.clone(),
        kind: EditKind::JsonHook {
            event: "SessionStart".into(),
            matcher: None,
            command: "'/b/herdr-wake' hook codex start".into(),
            timeout: 10,
        },
    };
    edits::apply(&edit, &env, false).unwrap();
    let v = parse(&std::fs::read_to_string(&path).unwrap());
    let entries = v["hooks"]["SessionStart"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["hooks"][0]["command"], "mine");
    assert_eq!(
        entries[1]["hooks"][0]["command"],
        "'/b/herdr-wake' hook codex start"
    );
    edits::revert(&edit, &env, false).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn json_hook_revert_removes_our_command_from_a_shared_group_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let text = "{\"hooks\": {\"SessionEnd\": [{\"hooks\": [\n  {\"type\": \"command\", \"command\": \"mine\"},\n  {\"type\": \"command\", \"command\": \"'/b/herdr-wake' hook claude end\", \"timeout\": 5}\n]}]}}\n";
    std::fs::write(&path, text).unwrap();
    let env = edit_env(dir.path());
    let edit = &claude_hooks(&path, "/b/herdr-wake")[1];
    edits::revert(edit, &env, false).unwrap();
    let v = parse(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(
        v,
        serde_json::json!({"hooks": {"SessionEnd": [{"hooks": [
            {"type": "command", "command": "mine"}
        ]}]}})
    );
}

#[test]
fn json_hook_rejects_duplicate_keys_and_leaves_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let text = "{\"a\": 1, \"a\": 2}\n";
    std::fs::write(&path, text).unwrap();
    let env = edit_env(dir.path());
    let err = edits::apply(&claude_hooks(&path, "/b/herdr-wake")[0], &env, false).unwrap_err();
    assert!(format!("{err:#}").contains("duplicate key"), "{err:#}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn json_hook_rejects_a_hooks_value_that_is_not_an_object() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let text = "{\"hooks\": []}\n";
    std::fs::write(&path, text).unwrap();
    let env = edit_env(dir.path());
    assert!(edits::apply(&claude_hooks(&path, "/b/herdr-wake")[0], &env, false).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn jsonc_list_entry_round_trip_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli.json");
    let original = fixture("opencode-cli.json");
    std::fs::write(&path, &original).unwrap();
    let env = edit_env(dir.path());
    let edit = Edit {
        path: path.clone(),
        kind: EditKind::JsoncListEntry {
            key: "plugins".into(),
            value: "./herdr-wake".into(),
        },
    };
    edits::apply(&edit, &env, false).unwrap();
    assert!(edits::apply(&edit, &env, false).unwrap().is_empty());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("// mine"));
    assert_eq!(
        parse(&text)["plugins"],
        serde_json::json!(["./my-plugin", "./herdr-wake"])
    );
    edits::revert(&edit, &env, false).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn jsonc_list_entry_creates_file_and_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli.json");
    let env = edit_env(dir.path());
    let edit = Edit {
        path: path.clone(),
        kind: EditKind::JsoncListEntry {
            key: "plugins".into(),
            value: "./herdr-wake".into(),
        },
    };
    edits::apply(&edit, &env, false).unwrap();
    assert_eq!(
        parse(&std::fs::read_to_string(&path).unwrap()),
        serde_json::json!({"plugins": ["./herdr-wake"]})
    );
    edits::revert(&edit, &env, false).unwrap();
    assert_eq!(
        parse(&std::fs::read_to_string(&path).unwrap()),
        serde_json::json!({})
    );
}

#[cfg(unix)]
#[test]
fn write_file_apply_then_revert_moves_it_to_the_trash() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("pi")
        .join("extensions")
        .join("herdr-wake.ts");
    let env = edit_env(dir.path());
    let edit = Edit {
        path: path.clone(),
        kind: EditKind::WriteFile {
            body: "// herdr-wake v1\n".into(),
            mode: 0o644,
        },
    };
    edits::apply(&edit, &env, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "// herdr-wake v1\n"
    );
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644);
    assert!(edits::apply(&edit, &env, false).unwrap().is_empty());

    edits::revert(&edit, &env, false).unwrap();
    assert!(!path.exists());
    let trashed = dir.path().join(".Trash").join("herdr-wake.ts");
    assert_eq!(
        std::fs::read_to_string(trashed).unwrap(),
        "// herdr-wake v1\n"
    );
}

#[test]
fn write_file_revert_without_trash_keeps_a_backup_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tui.js");
    let mut env = edit_env(dir.path());
    env.trash = None;
    let edit = Edit {
        path: path.clone(),
        kind: EditKind::WriteFile {
            body: "export {}\n".into(),
            mode: 0o644,
        },
    };
    edits::apply(&edit, &env, false).unwrap();
    edits::revert(&edit, &env, false).unwrap();
    assert!(!path.exists());
    let copies: Vec<String> = std::fs::read_dir(&env.backups)
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert_eq!(copies, vec!["export {}\n".to_string()]);
}

#[test]
fn edits_dry_run_changes_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let original = fixture("claude-settings.json");
    std::fs::write(&path, &original).unwrap();
    let env = edit_env(dir.path());
    let hook = &claude_hooks(&path, "/b/herdr-wake")[0];
    let actions = edits::apply(hook, &env, true).unwrap();
    assert!(!actions.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(!env.backups.exists());

    let file = Edit {
        path: dir.path().join("new.ts"),
        kind: EditKind::WriteFile {
            body: "x".into(),
            mode: 0o644,
        },
    };
    assert!(!edits::apply(&file, &env, true).unwrap().is_empty());
    assert!(!dir.path().join("new.ts").exists());
}

// --- install and uninstall -------------------------------------------------

use common::Fixture;
use herdr_wake::agents::{Agent, Ctx, Observation, Plan};
use herdr_wake::config::Config;
use herdr_wake::paths;
use herdr_wake::platform::{Action, BootService, BootState};
use herdr_wake::record::{AgentKind, Record};
use herdr_wake::setup::{self, Options};

/// One edit of each kind, under the context's home.
struct EveryKind;

impl EveryKind {
    fn settings(home: &Path) -> PathBuf {
        home.join(".claude").join("settings.json")
    }
    fn cli_json(home: &Path) -> PathBuf {
        home.join(".config").join("opencode").join("cli.json")
    }
    fn extension(home: &Path) -> PathBuf {
        home.join(".pi")
            .join("agent")
            .join("extensions")
            .join("herdr-wake.ts")
    }
}

impl Agent for EveryKind {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }
    fn observe(&self, _event: &str, _payload: &[u8], _ctx: &mut Ctx) -> Observation {
        Observation::Ignore("test agent")
    }
    fn restorable_flags(&self, _argv: &[String], _cfg: &Config) -> Vec<String> {
        Vec::new()
    }
    fn resume_plan(&self, _rec: &Record, _ctx: &mut Ctx) -> Plan {
        Plan::Permanent("test agent".into())
    }
    fn integration(&self, bin: &Path, ctx: &mut Ctx) -> Vec<Edit> {
        let mut v = claude_hooks(&EveryKind::settings(ctx.home), &bin.display().to_string());
        v.push(Edit {
            path: EveryKind::extension(ctx.home),
            kind: EditKind::WriteFile {
                body: "// herdr-wake v0.1.0\n".into(),
                mode: 0o644,
            },
        });
        v.push(Edit {
            path: EveryKind::cli_json(ctx.home),
            kind: EditKind::JsoncListEntry {
                key: "plugins".into(),
                value: "./herdr-wake".into(),
            },
        });
        v
    }
}

fn agents() -> Vec<Box<dyn Agent>> {
    vec![Box::new(EveryKind)]
}

struct Home {
    f: Fixture,
    _dir: tempfile::TempDir,
    bin: PathBuf,
}

impl Home {
    /// A temp home with the settings, cli.json and core config fixtures in
    /// place, a stand-in binary, and a login-like environment.
    fn new() -> Home {
        let (mut f, dir) = Fixture::with_temp_home();
        let home = f.home.clone();
        for (k, v) in [
            ("PATH", "/opt/homebrew/bin:/usr/bin:/bin"),
            ("LANG", "en_US.UTF-8"),
            ("LC_CTYPE", "en_US.UTF-8"),
            ("SHELL", "/bin/zsh"),
            ("USER", "u"),
            ("LOGNAME", "u"),
            ("TMPDIR", "/var/folders/xy/T/"),
            ("ANTHROPIC_API_KEY", "not-a-real-key"),
            ("TERM", "xterm-256color"),
        ] {
            f.env.insert(k.into(), v.into());
        }
        let settings = EveryKind::settings(&home);
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, fixture("claude-settings.json")).unwrap();
        let cli = EveryKind::cli_json(&home);
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&cli, fixture("opencode-cli.json")).unwrap();
        let core = Home::core_path(&home);
        std::fs::create_dir_all(core.parent().unwrap()).unwrap();
        std::fs::write(&core, fixture("core-config.toml")).unwrap();
        let bin = home.join(".local").join("bin").join("herdr-wake");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        Home { f, _dir: dir, bin }
    }

    fn core_path(home: &Path) -> PathBuf {
        home.join(".config").join("herdr-core").join("config.toml")
    }

    fn core(&self) -> PathBuf {
        Home::core_path(&self.f.home)
    }

    fn opts(&self) -> Options {
        Options {
            before_login: false,
            dry_run: false,
            bin: self.bin.clone(),
        }
    }

    fn install(&mut self, dry_run: bool) -> anyhow::Result<setup::Report> {
        let mut opts = self.opts();
        opts.dry_run = dry_run;
        let agents = agents();
        setup::install_with(&opts, &agents, &mut self.f.ctx())
    }

    fn uninstall(&mut self, dry_run: bool) -> anyhow::Result<setup::Report> {
        let agents = agents();
        let bin = self.bin.clone();
        setup::uninstall_with(dry_run, &bin, &agents, &mut self.f.ctx())
    }

    fn saved_config(&self) -> Config {
        Config::load(&paths::config_file(&self.f.env, &self.f.home)).unwrap()
    }

    fn read(&self, p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    /// Every file under the home with its bytes, for before/after checks.
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push((p.clone(), std::fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.f.home, &mut out);
        out.sort();
        out
    }
}

fn shell_of(h: &Home) -> Option<Vec<String>> {
    setup::read_core_shell(&h.core()).unwrap()
}

#[test]
fn install_points_core_at_herdr_wake_and_saves_the_real_shell() {
    let mut h = Home::new();
    h.install(false).unwrap();

    let bin = h.bin.display().to_string();
    assert_eq!(shell_of(&h), Some(vec![bin.clone(), "shell".to_string()]));
    let core = h.read(&h.core());
    assert!(core.starts_with("# herdr-core settings\n"), "{core}");
    assert!(core.contains("socket = \"/private/tmp/hw-1/c.sock\" # short path for tests"));
    assert!(core.contains("[log]\nlevel = \"info\""), "{core}");
    assert_eq!(
        h.saved_config().real_shell,
        Some(vec!["/bin/zsh".to_string(), "-l".to_string()])
    );
    assert_eq!(h.f.platform.boot().state(), BootState::AtLogin);
    let settings = h.read(&EveryKind::settings(&h.f.home));
    assert!(settings.contains(&format!("'{bin}' hook claude start")));
    assert!(EveryKind::extension(&h.f.home).exists());
    assert!(h
        .read(&EveryKind::cli_json(&h.f.home))
        .contains("\"./herdr-wake\""));
}

#[cfg(unix)]
#[test]
fn install_writes_boot_env_with_login_variables_only() {
    use std::os::unix::fs::PermissionsExt;
    let mut h = Home::new();
    h.install(false).unwrap();
    let path = paths::boot_env_file(&h.f.env, &h.f.home);
    let text = h.read(&path);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let parsed = setup::parse_boot_env(&text);
    let keys: Vec<&str> = parsed.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["LANG", "LC_CTYPE", "LOGNAME", "PATH", "SHELL", "TMPDIR", "USER"]
    );
    assert_eq!(parsed["PATH"], "/opt/homebrew/bin:/usr/bin:/bin");
    assert!(!text.contains("not-a-real-key"));
}

#[test]
fn boot_env_quotes_values_that_need_it() {
    let mut env = std::collections::BTreeMap::new();
    env.insert("PATH".to_string(), "/a b/bin:/usr/bin".to_string());
    env.insert("LC_ALL".to_string(), "say \"hi\" $HOME \\".to_string());
    let text = setup::boot_env_text(&env);
    assert_eq!(setup::parse_boot_env(&text), env);
    assert!(text.contains("PATH=\"/a b/bin:/usr/bin\"\n"), "{text}");
}

#[test]
fn install_twice_changes_nothing_the_second_time() {
    let mut h = Home::new();
    h.install(false).unwrap();
    let first = h.snapshot();
    let report = h.install(false).unwrap();
    let second = h.snapshot();
    let changed: Vec<&PathBuf> = second
        .iter()
        .filter(|(p, b)| !first.iter().any(|(q, c)| q == p && c == b))
        .map(|(p, _)| p)
        .collect();
    assert!(
        changed.is_empty(),
        "changed: {changed:?}\n{:?}",
        report.actions
    );
    assert_eq!(
        h.saved_config().real_shell,
        Some(vec!["/bin/zsh".to_string(), "-l".to_string()]),
        "the second install must not save herdr-wake as the real shell"
    );
    let v = parse(&h.read(&EveryKind::settings(&h.f.home)));
    assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    assert_eq!(v["hooks"]["SessionEnd"].as_array().unwrap().len(), 1);
    assert_eq!(
        parse(&h.read(&EveryKind::cli_json(&h.f.home)))["plugins"],
        serde_json::json!(["./my-plugin", "./herdr-wake"])
    );
}

#[test]
fn install_then_uninstall_leaves_user_files_byte_identical() {
    let mut h = Home::new();
    let settings = EveryKind::settings(&h.f.home);
    let cli = EveryKind::cli_json(&h.f.home);
    h.install(false).unwrap();
    let report = h.uninstall(false).unwrap();

    assert_eq!(h.read(&settings), fixture("claude-settings.json"));
    assert_eq!(h.read(&cli), fixture("opencode-cli.json"));
    assert_eq!(h.read(&h.core()), fixture("core-config.toml"));
    assert!(!EveryKind::extension(&h.f.home).exists());
    assert_eq!(h.f.platform.boot().state(), BootState::NotInstalled);
    assert!(h.bin.exists(), "uninstall never deletes the binary");
    assert!(
        report.warnings.iter().any(|w| w.contains("60 s")),
        "{:?}",
        report.warnings
    );
}

fn index_of(actions: &[Action], path: &Path) -> usize {
    actions
        .iter()
        .position(|a| a.path.as_deref() == Some(path))
        .unwrap_or_else(|| panic!("no action on {}: {actions:#?}", path.display()))
}

#[test]
fn uninstall_restores_core_shell_first_and_names_the_binary_last() {
    let mut h = Home::new();
    h.install(false).unwrap();
    let report = h.uninstall(false).unwrap();
    let a = &report.actions;

    let core = index_of(a, &h.core());
    let boot = index_of(a, Path::new("/fake/boot-service"));
    let settings = index_of(a, &EveryKind::settings(&h.f.home));
    let cli = index_of(a, &EveryKind::cli_json(&h.f.home));
    assert!(
        a[..core].iter().all(|x| x.what.starts_with("back up")),
        "{a:#?}"
    );
    assert!(core < boot && boot < settings && boot < cli, "{a:#?}");
    let last = a.last().unwrap();
    assert_eq!(last.path.as_deref(), Some(h.bin.as_path()));
    assert!(last.what.contains("delete"), "{last:?}");
}

#[test]
fn uninstall_fails_loudly_and_removes_nothing_when_core_still_names_herdr_wake() {
    let mut h = Home::new();
    h.install(false).unwrap();
    // A saved real shell that is itself herdr-wake (corrupt state).
    let mut cfg = h.saved_config();
    cfg.real_shell = Some(vec![h.bin.display().to_string(), "shell".into()]);
    cfg.save(&paths::config_file(&h.f.env, &h.f.home)).unwrap();

    let err = h.uninstall(false).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("herdr-wake"), "{msg}");
    assert!(msg.contains(&h.core().display().to_string()), "{msg}");
    assert_eq!(h.f.platform.boot().state(), BootState::AtLogin);
    assert!(EveryKind::extension(&h.f.home).exists());
    assert!(h
        .read(&EveryKind::settings(&h.f.home))
        .contains("hook claude start"));
}

#[test]
fn uninstall_restores_a_custom_real_shell() {
    let mut h = Home::new();
    std::fs::write(
        h.core(),
        "shell = [\"/opt/homebrew/bin/fish\", \"--login\"]\n",
    )
    .unwrap();
    h.install(false).unwrap();
    let fish = vec!["/opt/homebrew/bin/fish".to_string(), "--login".to_string()];
    assert_eq!(h.saved_config().real_shell, Some(fish.clone()));
    h.uninstall(false).unwrap();
    assert_eq!(shell_of(&h), Some(fish));
}

#[test]
fn missing_core_config_is_created_and_uninstall_removes_the_shell_key() {
    let mut h = Home::new();
    std::fs::remove_file(h.core()).unwrap();
    h.install(false).unwrap();
    assert_eq!(h.saved_config().real_shell, None);
    assert_eq!(
        shell_of(&h),
        Some(vec![h.bin.display().to_string(), "shell".to_string()])
    );
    h.uninstall(false).unwrap();
    assert_eq!(shell_of(&h), None);
}

#[test]
fn uninstall_leaves_a_core_shell_the_user_changed_since() {
    let mut h = Home::new();
    h.install(false).unwrap();
    std::fs::write(h.core(), "shell = [\"/bin/bash\"]\n").unwrap();
    let report = h.uninstall(false).unwrap();
    assert_eq!(shell_of(&h), Some(vec!["/bin/bash".to_string()]));
    assert!(
        report.warnings.iter().any(|w| w.contains("/bin/bash")),
        "{:?}",
        report.warnings
    );
}

#[test]
fn dry_run_install_and_uninstall_change_no_file() {
    let mut h = Home::new();
    let before = h.snapshot();
    let report = h.install(true).unwrap();
    assert!(!report.actions.is_empty());
    assert_eq!(h.snapshot(), before);
    assert_eq!(h.f.platform.boot().state(), BootState::NotInstalled);

    h.install(false).unwrap();
    let installed = h.snapshot();
    let report = h.uninstall(true).unwrap();
    assert!(!report.actions.is_empty());
    assert_eq!(h.snapshot(), installed);
    assert_eq!(h.f.platform.boot().state(), BootState::AtLogin);
}

#[test]
fn install_refuses_a_relative_or_missing_binary() {
    let mut h = Home::new();
    let before = h.snapshot();
    let agents = agents();
    let mut opts = h.opts();
    opts.bin = PathBuf::from("bin/herdr-wake");
    let err = setup::install_with(&opts, &agents, &mut h.f.ctx()).unwrap_err();
    assert!(format!("{err:#}").contains("absolute"), "{err:#}");
    opts.bin = h.f.home.join("nope").join("herdr-wake");
    assert!(setup::install_with(&opts, &agents, &mut h.f.ctx()).is_err());
    assert_eq!(h.snapshot(), before);
}

#[test]
fn install_with_the_real_adapters_runs() {
    let mut h = Home::new();
    // No agent program of this machine is found or run.
    h.f.env.insert("PATH".into(), "/nonexistent/bin".into());
    let opts = h.opts();
    setup::install(&opts, &mut h.f.ctx()).unwrap();
    assert_eq!(
        shell_of(&h),
        Some(vec![h.bin.display().to_string(), "shell".to_string()])
    );
}

// --- remote ----------------------------------------------------------------

#[test]
fn remote_install_copies_then_installs_over_ssh() {
    let cmds = setup::remote::remote_install_commands(
        "ares",
        Path::new("/Users/u/.local/bin/herdr-wake"),
        true,
    );
    let as_str: Vec<Vec<&str>> = cmds
        .iter()
        .map(|c| c.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(
        as_str,
        vec![
            vec!["ssh", "--", "ares", "mkdir -p \"$HOME/.local/bin\""],
            vec![
                "scp",
                "--",
                "/Users/u/.local/bin/herdr-wake",
                "ares:.local/bin/herdr-wake.new",
            ],
            vec![
                "ssh",
                "--",
                "ares",
                "chmod 755 \"$HOME/.local/bin/herdr-wake.new\" && mv -f \"$HOME/.local/bin/herdr-wake.new\" \"$HOME/.local/bin/herdr-wake\" && \"$HOME/.local/bin/herdr-wake\" install --before-login",
            ],
        ]
    );
    let plain = setup::remote::remote_install_commands("ares", Path::new("/b/herdr-wake"), false);
    assert!(
        plain[2][3].ends_with("herdr-wake\" install"),
        "{:?}",
        plain[2]
    );
}

#[test]
fn remote_install_rejects_a_host_that_looks_like_an_option() {
    let err =
        setup::remote::run_remote_install("-oProxyCommand=x", Path::new("/b/herdr-wake"), false)
            .unwrap_err();
    assert!(format!("{err:#}").contains("host"), "{err:#}");
}

// --- Task 13: leftovers, the test-only boot switch, remote platform check ----

#[test]
fn uninstall_removes_a_settings_file_install_created_and_its_own_config() {
    let mut h = Home::new();
    let settings = EveryKind::settings(&h.f.home);
    std::fs::remove_file(&settings).unwrap();
    h.install(false).unwrap();
    assert!(settings.exists());
    let cfg_dir = paths::config_dir(&h.f.env, &h.f.home);
    assert!(cfg_dir.join("boot.env").exists());

    let report = h.uninstall(false).unwrap();
    assert!(
        !settings.exists(),
        "install made it, uninstall takes it away"
    );
    assert!(!cfg_dir.exists(), "herdr-wake's own settings are removed");
    assert!(
        !paths::created_files_file(&h.f.env, &h.f.home).exists(),
        "the created-files list is spent"
    );
    // Nothing is destroyed: the files sit in the Trash or the backups.
    let kept = |name: &str| {
        h.snapshot().iter().any(|(p, _)| {
            p.to_string_lossy().contains(name)
                && (p.starts_with(h.f.home.join(".Trash"))
                    || p.starts_with(paths::backups_dir(&h.f.env, &h.f.home)))
        })
    };
    assert!(kept("settings.json") && kept("boot.env") && kept("config.toml"));
    // The binary is still named last.
    assert_eq!(
        report.actions.last().unwrap().path.as_deref(),
        Some(h.bin.as_path())
    );
    // Logs and backups stay.
    assert!(paths::backups_dir(&h.f.env, &h.f.home).is_dir());
}

#[test]
fn uninstall_keeps_an_empty_settings_file_that_was_there_before_install() {
    let mut h = Home::new();
    let settings = EveryKind::settings(&h.f.home);
    std::fs::write(&settings, "{}\n").unwrap();
    h.install(false).unwrap();
    h.uninstall(false).unwrap();
    assert_eq!(h.read(&settings), "{}\n");
}

#[test]
fn the_test_only_switch_leaves_the_boot_service_alone() {
    use herdr_wake::platform::{BootState, Platform};
    let mut h = Home::new();
    h.f.env
        .insert(setup::SKIP_BOOT_SERVICE_ENV.into(), "1".into());
    let report = h.install(false).unwrap();
    assert_eq!(h.f.platform.boot_service().state(), BootState::NotInstalled);
    assert!(report.warnings.iter().any(|w| w.contains("test-only")));
    assert!(shell_of(&h).is_some(), "everything else was installed");

    h.f.platform.boot().set_state(BootState::AtLogin);
    h.uninstall(false).unwrap();
    assert_eq!(h.f.platform.boot_service().state(), BootState::AtLogin);
}

#[test]
fn remote_platform_must_match_this_binary() {
    use setup::remote::check_remote_platform as check;
    assert!(check("Darwin arm64\n", "macos", "aarch64").is_ok());
    assert!(check("Linux aarch64\n", "linux", "aarch64").is_ok());
    assert!(check("Linux x86_64", "linux", "x86_64").is_ok());
    let err = check("Linux x86_64\n", "macos", "aarch64")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("darwin aarch64") && err.contains("linux x86_64"),
        "{err}"
    );
    let err = check("Darwin x86_64\n", "macos", "aarch64")
        .unwrap_err()
        .to_string();
    assert!(err.contains("x86_64"), "{err}");
    assert!(check("", "linux", "x86_64").is_err());
}

// ---- L-6: what install writes under herdr-wake's own folders is private ----

fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

#[test]
fn install_leaves_every_own_file_0600_in_0700_folders() {
    use std::os::unix::fs::PermissionsExt;
    let mut h = Home::new();
    // A file install creates from nothing gets listed in installed-files.json.
    std::fs::remove_file(EveryKind::cli_json(&h.f.home)).unwrap();
    // An older build left the folders and a config file readable by all.
    let config_dir = paths::config_dir(&h.f.env, &h.f.home);
    let state_dir = paths::state_dir(&h.f.env, &h.f.home);
    for d in [&config_dir, &state_dir] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let settings = EveryKind::settings(&h.f.home);
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o644)).unwrap();

    h.install(false).unwrap();

    assert_eq!(mode_of(&config_dir), 0o700);
    assert_eq!(mode_of(&state_dir), 0o700);
    for file in [
        paths::config_file(&h.f.env, &h.f.home),
        paths::boot_env_file(&h.f.env, &h.f.home),
        paths::created_files_file(&h.f.env, &h.f.home),
    ] {
        assert!(file.exists(), "{} was not written", file.display());
        assert_eq!(mode_of(&file), 0o600, "{}", file.display());
    }
    // Backups hold whatever the agents' settings held: private too, even
    // though the file they copy is 0644.
    let backups = paths::backups_dir(&h.f.env, &h.f.home);
    assert_eq!(mode_of(&backups), 0o700);
    let copies: Vec<PathBuf> = std::fs::read_dir(&backups)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert!(!copies.is_empty());
    for c in copies {
        assert_eq!(mode_of(&c), 0o600, "{}", c.display());
    }
    // Other tools' files keep their own mode.
    assert_eq!(mode_of(&settings), 0o644);
}

#[test]
fn install_does_not_write_boot_env_through_a_symlink() {
    let mut h = Home::new();
    let victim = h.f.home.join("victim");
    std::fs::write(&victim, "precious").unwrap();
    let boot_env = paths::boot_env_file(&h.f.env, &h.f.home);
    std::fs::create_dir_all(boot_env.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&victim, &boot_env).unwrap();

    assert!(h.install(false).is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
}

// ---- only agents that are installed are set up (real Linux install, 2026-11-10) ----

/// A home for the real adapters: nothing of any agent in it, an empty
/// folder as the whole `PATH`, and a stand-in herdr-wake binary.
struct Bare {
    f: Fixture,
    bin: PathBuf,
    path_dir: PathBuf,
}

impl Bare {
    fn new() -> Bare {
        let mut f = Fixture::temp();
        let path_dir = f.home.join("pathbin");
        std::fs::create_dir_all(&path_dir).unwrap();
        f.env.insert("PATH".into(), path_dir.display().to_string());
        f.env.insert("SHELL".into(), "/bin/sh".into());
        f.env
            .insert(setup::SKIP_BOOT_SERVICE_ENV.into(), "1".into());
        let bin = f.home.join(".local/bin/herdr-wake");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        Bare { f, bin, path_dir }
    }

    /// Puts an executable called `name` on the `PATH`; it prints `version`
    /// for `--version`.
    fn program(&self, name: &str, version: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = self.path_dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn install(&mut self, dry_run: bool) -> setup::Report {
        let opts = Options {
            before_login: false,
            dry_run,
            bin: self.bin.clone(),
        };
        setup::install(&opts, &mut self.f.ctx()).unwrap()
    }

    fn uninstall(&mut self) -> setup::Report {
        let bin = self.bin.clone();
        setup::uninstall_with(false, &bin, &herdr_wake::agents::all(), &mut self.f.ctx()).unwrap()
    }

    fn home(&self, rel: &str) -> PathBuf {
        self.f.home.join(rel)
    }
}

fn whats(r: &setup::Report) -> Vec<&str> {
    r.actions.iter().map(|a| a.what.as_str()).collect()
}

#[test]
fn install_sets_up_no_agent_that_is_not_installed_and_creates_nothing_for_it() {
    let mut b = Bare::new();
    let r = b.install(false);
    for agent in ["claude", "codex", "pi", "opencode"] {
        assert!(
            whats(&r).contains(&format!("skipped {agent}: not installed").as_str()),
            "{agent}: {:?}",
            whats(&r)
        );
    }
    for gone in [".claude", ".codex", ".pi", ".config/opencode"] {
        assert!(!b.home(gone).exists(), "{gone} was created from nothing");
    }
    assert!(!paths::created_files_file(&b.f.env, &b.f.home).exists());
    // Core's shell is still set: herdr-wake itself is installed.
    assert!(
        setup::read_core_shell(&setup::core_config_path(&b.f.env, &b.f.home))
            .unwrap()
            .is_some()
    );
    let text = herdr_wake::cli::render_setup("install", false, &r);
    assert!(text.contains("skipped codex: not installed"), "{text}");
    assert!(
        text.contains("Run `herdr-wake install` again after you install another agent"),
        "{text}"
    );
}

#[test]
fn a_dry_run_reports_the_skipped_agents_too() {
    let mut b = Bare::new();
    let r = b.install(true);
    assert!(whats(&r).contains(&"skipped pi: not installed"));
    assert!(!b.home(".pi").exists());
}

#[test]
fn an_agent_with_a_config_folder_is_set_up() {
    let mut b = Bare::new();
    std::fs::create_dir_all(b.home(".claude")).unwrap();
    std::fs::create_dir_all(b.home(".pi/agent")).unwrap();
    let r = b.install(false);
    assert!(b.home(".claude/settings.json").is_file());
    assert!(b.home(".pi/agent/extensions/herdr-wake.ts").is_file());
    assert!(!whats(&r).contains(&"skipped claude: not installed"));
    assert!(!whats(&r).contains(&"skipped pi: not installed"));
    assert!(whats(&r).contains(&"skipped codex: not installed"));
    assert!(!b.home(".codex").exists());
    assert!(!b.home(".config/opencode").exists());
}

#[test]
fn an_agent_whose_program_is_on_path_is_set_up_and_a_later_install_picks_up_new_ones() {
    let mut b = Bare::new();
    b.program("codex", "codex-cli 0.130.0");
    let r = b.install(false);
    assert!(b.home(".codex/hooks.json").is_file());
    assert!(whats(&r).contains(&"skipped pi: not installed"));

    // Pi is installed later; running install again picks it up.
    b.program("pi", "0.80.0");
    let r = b.install(false);
    assert!(b.home(".pi/agent/extensions/herdr-wake.ts").is_file());
    assert!(!whats(&r).contains(&"skipped pi: not installed"));
    assert!(whats(&r).contains(&"skipped opencode: not installed"));
}

#[test]
fn opencode_v2_is_chosen_by_its_version_and_cli_json_only_decides_without_one() {
    // Version 2: the v2 layout, although there is no cli.json yet.
    let mut b = Bare::new();
    b.program("opencode", "2.0.31");
    b.install(false);
    assert!(b.home(".config/opencode/herdr-wake/tui.js").is_file());
    assert!(b.home(".config/opencode/cli.json").is_file());
    assert!(!b.home(".config/opencode/tui.jsonc").exists());

    // Version 1: the v1 layout, although a cli.json is lying around.
    let mut b = Bare::new();
    b.program("opencode", "1.4.6");
    std::fs::create_dir_all(b.home(".config/opencode")).unwrap();
    std::fs::write(b.home(".config/opencode/cli.json"), "{}\n").unwrap();
    b.install(false);
    assert!(b.home(".config/opencode/herdr-wake-tui.js").is_file());
    assert!(b.home(".config/opencode/tui.jsonc").is_file());
    assert_eq!(
        std::fs::read_to_string(b.home(".config/opencode/cli.json")).unwrap(),
        "{}\n"
    );

    // No readable version (config folder only): cli.json decides.
    let mut b = Bare::new();
    std::fs::create_dir_all(b.home(".config/opencode")).unwrap();
    std::fs::write(b.home(".config/opencode/cli.json"), "{}\n").unwrap();
    b.install(false);
    assert!(b.home(".config/opencode/herdr-wake/tui.js").is_file());
    let mut b = Bare::new();
    std::fs::create_dir_all(b.home(".config/opencode")).unwrap();
    b.install(false);
    assert!(b.home(".config/opencode/herdr-wake-tui.js").is_file());
}

#[test]
fn uninstall_removes_what_an_older_install_created_for_absent_agents() {
    // What the install of 2026-11-10 left on a machine with only Claude:
    // folders and files for three agents that were never installed.
    let mut b = Bare::new();
    for (dir, program) in [
        (".codex", "codex"),
        (".pi/agent", "pi"),
        (".config/opencode", "opencode"),
    ] {
        std::fs::create_dir_all(b.home(dir)).unwrap();
        b.program(program, "1.0.0");
    }
    std::fs::create_dir_all(b.home(".claude")).unwrap();
    std::fs::write(b.home(".claude/settings.json"), "{\"model\": \"opus\"}\n").unwrap();
    b.install(false);
    assert!(b.home(".codex/hooks.json").is_file());
    // The agents are not there (they never were): the programs go, and the
    // folders hold nothing but herdr-wake's files, as the old install left them.
    for program in ["codex", "pi", "opencode"] {
        std::fs::remove_file(b.path_dir.join(program)).unwrap();
    }

    b.uninstall();

    for gone in [".codex", ".pi", ".config/opencode"] {
        assert!(!b.home(gone).exists(), "{gone} is still there");
    }
    assert!(b.home(".config").is_dir(), "the shared config folder stays");
    // Claude was really installed: its folder and the user's setting stay.
    assert_eq!(
        parse(&std::fs::read_to_string(b.home(".claude/settings.json")).unwrap()),
        serde_json::json!({"model": "opus"})
    );
}

#[test]
fn uninstall_keeps_a_folder_that_holds_anything_of_the_users() {
    let mut b = Bare::new();
    std::fs::create_dir_all(b.home(".pi/agent")).unwrap();
    std::fs::write(b.home(".pi/agent/settings.json"), "{}\n").unwrap();
    b.install(false);
    b.uninstall();
    assert!(!b.home(".pi/agent/extensions").exists(), "empty: removed");
    assert!(b.home(".pi/agent/settings.json").is_file());
}

// ---- one backup per file per run ----

#[test]
fn a_file_is_backed_up_once_per_run_however_many_edits_it_gets() {
    let mut h = Home::new();
    let settings = EveryKind::settings(&h.f.home);
    let original = std::fs::read(&settings).unwrap();
    let r = h.install(false).unwrap();
    // EveryKind adds two hooks to settings.json.
    let backups: Vec<&Action> = r
        .actions
        .iter()
        .filter(|a| a.what == format!("back up {}", settings.display()))
        .collect();
    assert_eq!(backups.len(), 1, "{:?}", whats(&r));
    let copies: Vec<PathBuf> = std::fs::read_dir(paths::backups_dir(&h.f.env, &h.f.home))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().contains("settings.json"))
        .collect();
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(
        std::fs::read(&copies[0]).unwrap(),
        original,
        "the copy holds the file as it was before this run"
    );
    // A dry run lists one backup as well.
    let mut h = Home::new();
    let r = h.install(true).unwrap();
    assert_eq!(
        whats(&r)
            .iter()
            .filter(|w| **w == format!("back up {}", settings_of(&h).display()))
            .count(),
        1
    );
}

fn settings_of(h: &Home) -> PathBuf {
    EveryKind::settings(&h.f.home)
}

// ---- I-11: a path with a control character never reaches the boot job ----

#[test]
fn install_refuses_a_binary_or_home_path_with_a_control_character() {
    let mut h = Home::new();
    let odd = h.f.home.join("bin\nExecStartPre=/bin/evil");
    std::fs::create_dir_all(&odd).unwrap();
    let bin = odd.join("herdr-wake");
    std::fs::write(&bin, "#!/bin/sh\n").unwrap();
    let before = h.snapshot();
    let opts = Options {
        before_login: false,
        dry_run: false,
        bin,
    };
    let agents = agents();
    let err = setup::install_with(&opts, &agents, &mut h.f.ctx()).unwrap_err();
    assert!(format!("{err:#}").contains("control character"), "{err:#}");
    assert_eq!(h.snapshot(), before, "nothing was written");
}
