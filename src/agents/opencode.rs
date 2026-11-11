//! OpenCode adapter (Task 8).
//!
//! # Hook contract
//!
//! The TUI plugin `assets/opencode/tui.js` runs inside the terminal's own TUI
//! process (never in v2's shared background service, which cannot tell
//! terminals apart). It exports one module with `setup(api)` (v2) and
//! `tui(api)` (v1) entry points, polls the route every 500 ms, and runs
//! `herdr-wake hook opencode <event>` with one JSON object on stdin, only
//! inside a herdr-core terminal (`HC_TERMINAL_ID` set):
//!
//! | Event | When | Payload |
//! |---|---|---|
//! | `start` | the root session shown changes (a child session maps to its root) | `{"id": "<root session id>", "cwd": "<abs folder>", "argv": ["opencode", <launch args>...], "pid": <TUI pid>, "major": 1\|2}` |
//! | `end` | the TUI exits with code 0, or after a SIGHUP/SIGTERM | `{"id": "<recorded session id>", "signal": <true when SIGHUP or SIGTERM was seen>}` |
//!
//! `argv[0]` only stands for the program; the flags after it are what
//! `restorable_flags` reads. `pid` and `major` may be missing; `major` is
//! informational, because `Record` has no field for it: `resume_plan` decides
//! v1 or v2 from the config folder instead (see below).
//!
//! # Config folder and major version
//!
//! `CFG` is `$OPENCODE_CONFIG_DIR`, else `${XDG_CONFIG_HOME:-<home>/.config}/opencode`.
//! v2 keeps its settings in `CFG/cli.json`; v1 has none, so an existing
//! `cli.json` means v2.
//!
//! # Resume
//!
//! `cd <cwd> && <bin> <flags..> --session <id>`. v2 treats an unknown id as
//! "create it" and silently opens a new chat, and competing TUIs at boot each
//! start a background service (herdrdev/herdr#5921), so a v2 plan first runs
//! `<bin> service start` (which returns once the service is ready) and asks
//! the wake engine to confirm the same id comes back within 15 s.
//!
//! Limit: a v2 session that no longer exists is not caught before the
//! start. `resume_plan` cannot run commands, and a probe with `opencode
//! session list` was left out because it could not be tested against a real
//! OpenCode. The only signal is the confirmation check afterwards
//! (`wake::confirm_check`): the TUI reports a new session id, the log gets
//! an "unconfirmed" line and the boot summary counts the terminal as
//! unconfirmed. The agent is left running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{Agent, Ctx, Edit, EditKind, Observation, Plan};
use crate::config::Config;
use crate::record::{AgentKind, Record};

/// The plugin source with its placeholders; see `render_asset`.
pub const ASSET: &str = include_str!("../../assets/opencode/tui.js");

/// The plugin as installed: `ASSET` with the binary path and this build's
/// version filled in.
pub fn render_asset(bin: &Path) -> String {
    super::pi::render(ASSET, bin)
}

/// How long a v2 resume has to report the same session id.
pub const CONFIRM_MS: u64 = 15_000;

const SUPERVISED: &str = "HERDR_WAKE_SUPERVISED";

#[derive(Debug, Default)]
pub struct OpenCode;

#[derive(Deserialize)]
struct StartPayload {
    id: String,
    cwd: PathBuf,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    argv: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct EndPayload {
    id: String,
    #[serde(default)]
    signal: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Verdict {
    Restore,
    /// `--auto` and its hidden aliases: restored as `--auto` only when
    /// `restore_bypass` is on.
    Auto,
    /// Never restored, or needs validation first.
    Drop,
}

/// OpenCode's TUI flags (v2 `commands.ts`, v1 `cmd/tui.ts`) with their policy
/// from `docs/research/agents-pi-opencode.md` section 6. `true`: takes a value.
const FLAGS: &[(&[&str], bool, Verdict)] = &[
    (
        &["--auto", "--yolo", "--dangerously-skip-permissions"],
        false,
        Verdict::Auto,
    ),
    (&["--standalone"], false, Verdict::Restore),
    (&["--server"], true, Verdict::Restore),
    (&["--model", "-m"], true, Verdict::Restore),
    (&["--agent"], true, Verdict::Restore),
    // needs validation (v1 network and mini options)
    (&["--port"], true, Verdict::Drop),
    (&["--hostname"], true, Verdict::Drop),
    (&["--mdns-domain"], true, Verdict::Drop),
    (&["--cors"], true, Verdict::Drop),
    (&["--replay-limit"], true, Verdict::Drop),
    (&["--mini"], false, Verdict::Drop),
    (&["--mdns", "--no-mdns"], false, Verdict::Drop),
    (&["--replay", "--no-replay"], false, Verdict::Drop),
    // never
    (&["--session", "-s"], true, Verdict::Drop),
    (&["--prompt"], true, Verdict::Drop),
    (&["--log-level"], true, Verdict::Drop),
    (&["--continue", "-c"], false, Verdict::Drop),
    (&["--fork"], false, Verdict::Drop),
    (&["--print-logs"], false, Verdict::Drop),
];

fn filter_flags(argv: &[String], cfg: &Config) -> Vec<String> {
    filter_flags_noting(argv, cfg, &mut Vec::new())
}

/// `filter_flags`, adding to `withheld` the names of flags left out because
/// their value holds a URL with a user name or password. `--server` is the
/// exception: it keeps its address and loses the user name and password.
fn filter_flags_noting(argv: &[String], cfg: &Config, withheld: &mut Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut auto = false;
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i].as_str();
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with('-') => (n, Some(v)),
            _ => (arg, None),
        };
        let next = argv.get(i + 1).map(String::as_str);
        let Some(&(_, takes_value, verdict)) =
            FLAGS.iter().find(|(names, _, _)| names.contains(&name))
        else {
            // Unknown flags and positional words (the folder argument) are
            // dropped; a word after an unknown flag is dropped as positional.
            i += 1;
            continue;
        };
        if takes_value {
            // A word starting with `-` is the next flag, not a value.
            let (value, used) = match inline {
                Some(v) => (Some(v), 0),
                None => match next.filter(|v| !v.starts_with('-')) {
                    Some(v) => (Some(v), 1),
                    None => (None, 0),
                },
            };
            if let Some(v) = value.filter(|v| verdict == Verdict::Restore && !v.is_empty()) {
                let clean = if name == "--server" {
                    crate::trust::strip_url_userinfo(v)
                } else {
                    v.to_string()
                };
                if crate::trust::has_url_userinfo(&clean) {
                    withheld.push(name.to_string());
                } else if inline.is_some() {
                    out.push(format!("{name}={clean}"));
                } else {
                    out.push(name.to_string());
                    out.push(clean);
                }
            }
            i += 1 + used;
            continue;
        }
        // A boolean: `--flag`, `--flag=true|false` or `--flag true|false`.
        let (on, used) = match inline {
            Some(v) => (v != "false", 0),
            None => match next {
                Some("true") => (true, 1),
                Some("false") => (false, 1),
                _ => (true, 0),
            },
        };
        match verdict {
            Verdict::Auto if on => auto = true,
            Verdict::Restore if on => out.push(name.to_string()),
            _ => {}
        }
        i += 1 + used;
    }
    if auto && cfg.restore_bypass {
        out.insert(0, "--auto".to_string());
    }
    out
}

/// The first non-empty value of `key` in `first`, then `second`.
fn lookup<'a>(
    key: &str,
    first: &'a BTreeMap<String, String>,
    second: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    [first, second]
        .into_iter()
        .filter_map(|m| m.get(key))
        .map(String::as_str)
        .find(|v| !v.is_empty())
}

/// `CFG`: `OPENCODE_CONFIG_DIR`, else `${XDG_CONFIG_HOME:-<home>/.config}/opencode`
/// (an `XDG_CONFIG_HOME` counts only when absolute). `first` wins over `second`.
fn config_dir(
    first: &BTreeMap<String, String>,
    second: &BTreeMap<String, String>,
    home: &Path,
) -> PathBuf {
    if let Some(dir) = lookup("OPENCODE_CONFIG_DIR", first, second) {
        return PathBuf::from(dir);
    }
    let xdg = [first, second]
        .into_iter()
        .filter_map(|m| m.get("XDG_CONFIG_HOME"))
        .find(|v| Path::new(v).is_absolute());
    match xdg {
        Some(v) => PathBuf::from(v).join("opencode"),
        None => home.join(".config/opencode"),
    }
}

/// v2 keeps `cli.json` in its config folder; v1 has none.
fn is_v2(cfg_dir: &Path) -> bool {
    cfg_dir.join("cli.json").is_file()
}

impl Agent for OpenCode {
    fn kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn observe(&self, event: &str, payload: &[u8], _ctx: &mut Ctx) -> Observation {
        match event {
            "start" => {
                let Ok(p) = serde_json::from_slice::<StartPayload>(payload) else {
                    return Observation::Ignore("bad payload");
                };
                if p.id.is_empty() {
                    return Observation::Ignore("no session id");
                }
                if !p.cwd.is_absolute() {
                    return Observation::Ignore("bad cwd");
                }
                Observation::Started {
                    session_id: p.id,
                    session_file: None,
                    cwd: p.cwd,
                    terminal: None,
                    agent_pid: p.pid,
                    argv: p.argv.filter(|a| !a.is_empty()),
                }
            }
            "end" => {
                let Ok(p) = serde_json::from_slice::<EndPayload>(payload) else {
                    return Observation::Ignore("bad payload");
                };
                if p.id.is_empty() {
                    return Observation::Ignore("no session id");
                }
                if p.signal {
                    return Observation::Ignore("signal");
                }
                Observation::Ended {
                    session_id: p.id,
                    deliberate: Some(true),
                }
            }
            _ => Observation::Ignore("unknown event"),
        }
    }

    fn restorable_flags(&self, argv: &[String], cfg: &Config) -> Vec<String> {
        filter_flags(argv, cfg)
    }

    fn withheld_flags(&self, argv: &[String], cfg: &Config) -> Vec<String> {
        let mut withheld = Vec::new();
        filter_flags_noting(argv, cfg, &mut withheld);
        withheld
    }

    fn resume_plan(&self, rec: &Record, ctx: &mut Ctx) -> Plan {
        if !rec.cwd.is_dir() {
            return Plan::Permanent("folder missing".into());
        }
        let bin = rec.bin.to_string_lossy().into_owned();
        let mut launch = vec![bin.clone()];
        launch.extend(rec.flags.iter().cloned());
        let mut argv = vec![bin.clone()];
        argv.extend(filter_flags(&launch, ctx.cfg));
        argv.push("--session".into());
        argv.push(rec.session_id.clone());
        let mut env = rec.env.clone();
        env.insert(SUPERVISED.into(), "1".into());
        let v2 = is_v2(&config_dir(&rec.env, ctx.env, ctx.home));
        Plan::Run {
            argv,
            env,
            cwd: rec.cwd.clone(),
            before: if v2 {
                vec![vec![bin, "service".into(), "start".into()]]
            } else {
                Vec::new()
            },
            confirm_within_ms: v2.then_some(CONFIRM_MS),
        }
    }

    fn present(&self, ctx: &Ctx) -> bool {
        let none = BTreeMap::new();
        super::installed(self.kind(), &config_dir(ctx.env, &none, ctx.home), ctx.env)
    }

    fn integration(&self, bin: &Path, ctx: &mut Ctx) -> Vec<Edit> {
        let none = BTreeMap::new();
        let cfg = config_dir(ctx.env, &none, ctx.home);
        layout(&cfg, bin, wants_v2(installed_major(ctx.env), &cfg))
    }

    /// The other major version's layout: an install made before OpenCode
    /// was upgraded (or downgraded) left its files there.
    fn former_integration(&self, bin: &Path, ctx: &mut Ctx) -> Vec<Edit> {
        let none = BTreeMap::new();
        let cfg = config_dir(ctx.env, &none, ctx.home);
        layout(&cfg, bin, !wants_v2(installed_major(ctx.env), &cfg))
    }
}

/// How long `opencode --version` may take.
const VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The major version of the `opencode` on `PATH`, from `opencode
/// --version`. `None` when there is none or its output has no version.
fn installed_major(env: &BTreeMap<String, String>) -> Option<u32> {
    use crate::report::Probes;
    let program = super::on_path("opencode", env)?;
    let out = crate::report::SystemProbes
        .run(&[&program.to_string_lossy(), "--version"], VERSION_TIMEOUT)?;
    major_of(&out.stdout)
}

/// The major version in the output of `opencode --version` (`2.0.31`,
/// `opencode 2.0.31`, `v1.4.6`).
pub fn major_of(output: &str) -> Option<u32> {
    output
        .split_whitespace()
        .find_map(crate::report::parse_version)
        .map(|(major, _, _)| major)
}

/// Which layout to use: the installed version decides (2 or newer is v2);
/// only when it cannot be read does an existing `cli.json` (which v2
/// writes and v1 does not have) decide.
pub fn wants_v2(major: Option<u32>, cfg_dir: &Path) -> bool {
    match major {
        Some(m) => m >= 2,
        None => is_v2(cfg_dir),
    }
}

/// The plugin file and the list entry that loads it, for v2 or v1.
fn layout(cfg: &Path, bin: &Path, v2: bool) -> Vec<Edit> {
    let body = render_asset(bin);
    let (file, list, key, value) = if v2 {
        (
            cfg.join("herdr-wake/tui.js"),
            cfg.join("cli.json"),
            "plugins",
            "./herdr-wake",
        )
    } else {
        (
            cfg.join("herdr-wake-tui.js"),
            cfg.join("tui.jsonc"),
            "plugin",
            "./herdr-wake-tui.js",
        )
    };
    vec![
        Edit {
            path: file,
            kind: EditKind::WriteFile { body, mode: 0o644 },
        },
        Edit {
            path: list,
            kind: EditKind::JsoncListEntry {
                key: key.into(),
                value: value.into(),
            },
        },
    ]
}
