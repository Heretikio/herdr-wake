//! Claude Code adapter (Task 6). Source: `docs/research/agents-claude-codex.md`,
//! the Claude Code section and sections 7 and 8.
//!
//! Hooks: `SessionStart` runs `'<bin>' hook claude start` and `SessionEnd`
//! runs `'<bin>' hook claude end`, both from `<config dir>/settings.json`.
//! The hook command sets `HERDR_WAKE_PPID` to its parent pid; the walk up to
//! the claude process starts there.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{Agent, Ctx, Edit, EditKind, Observation, Plan};
use crate::config::Config;
use crate::platform::ProcInfo;
use crate::record::{AgentKind, Record};

/// The env key the hook command sets to its own parent pid.
pub const PPID_ENV: &str = "HERDR_WAKE_PPID";

/// The `SessionStart` matcher: every source, anchored (legacy herdr
/// `src/integration/claude_settings.rs:19`).
pub const START_MATCHER: &str = "^(startup|resume|clear|compact|fork)$";

/// How many processes up from `HERDR_WAKE_PPID` to look for claude (the
/// hook's shell, claude itself, and room for one wrapper).
const MAX_WALK: usize = 4;

/// The `SessionStart` sources that start or re-key a session.
const SOURCES: [&str; 5] = ["startup", "resume", "clear", "compact", "fork"];

#[derive(Debug, Default)]
pub struct Claude;

impl Agent for Claude {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn observe(&self, event: &str, payload: &[u8], ctx: &mut Ctx) -> Observation {
        let Ok(p) = serde_json::from_slice::<Payload>(payload) else {
            return Observation::Ignore("unreadable payload");
        };
        if p.agent_id.as_deref().is_some_and(|a| !a.is_empty()) {
            return Observation::Ignore("sub-agent");
        }
        if !valid_session_id(&p.session_id) {
            return Observation::Ignore("no usable session id");
        }
        match event {
            "start" => observe_start(p, ctx),
            "end" => observe_end(p),
            _ => Observation::Ignore("unknown event"),
        }
    }

    fn restorable_flags(&self, argv: &[String], cfg: &Config) -> Vec<String> {
        let args = argv.get(1..).unwrap_or_default();
        parse(args)
            .iter()
            .filter(|u| u.restorable(cfg))
            .flat_map(Use::emit)
            .collect()
    }

    fn withheld_flags(&self, argv: &[String], cfg: &Config) -> Vec<String> {
        let args = argv.get(1..).unwrap_or_default();
        parse(args)
            .iter()
            .filter(|u| u.secret() && u.allowed(cfg))
            .map(|u| u.given.clone())
            .collect()
    }

    fn resume_plan(&self, rec: &Record, ctx: &mut Ctx) -> Plan {
        if !rec.cwd.is_dir() {
            return Plan::Permanent("folder missing".into());
        }
        let config = config_dir(&rec.env, ctx.home);
        if !transcript_exists(&config, &rec.cwd, &rec.session_id) {
            return Plan::Permanent("session not found".into());
        }
        let mut argv = vec![rec.bin.to_string_lossy().into_owned()];
        for mut u in parse(&rec.flags) {
            if u.restorable(ctx.cfg) {
                u.resolve_paths(&rec.cwd);
                argv.extend(u.emit());
            }
        }
        argv.push("--resume".into());
        argv.push(rec.session_id.clone());
        let mut env = rec.env.clone();
        env.insert("HERDR_WAKE_SUPERVISED".into(), "1".into());
        Plan::Run {
            argv,
            env,
            cwd: rec.cwd.clone(),
            before: Vec::new(),
            confirm_within_ms: None,
        }
    }

    fn present(&self, ctx: &Ctx) -> bool {
        super::installed(self.kind(), &config_dir(ctx.env, ctx.home), ctx.env)
    }

    fn integration(&self, bin: &Path, ctx: &mut Ctx) -> Vec<Edit> {
        let path = config_dir(ctx.env, ctx.home).join("settings.json");
        let quoted = shell_quote(&bin.to_string_lossy());
        vec![
            Edit {
                path: path.clone(),
                kind: EditKind::JsonHook {
                    event: "SessionStart".into(),
                    matcher: Some(START_MATCHER.into()),
                    command: format!("{quoted} hook claude start"),
                    timeout: 10,
                },
            },
            Edit {
                path,
                kind: EditKind::JsonHook {
                    event: "SessionEnd".into(),
                    matcher: None,
                    command: format!("{quoted} hook claude end"),
                    timeout: 5,
                },
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Hook payloads

/// The fields of the SessionStart and SessionEnd payloads this adapter uses.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Payload {
    session_id: String,
    transcript_path: String,
    cwd: String,
    hook_event_name: String,
    source: String,
    reason: String,
    agent_id: Option<String>,
}

/// Claude ids are UUIDs; anything else (and anything that could climb out of
/// a folder when joined to a path) is refused.
pub(crate) fn valid_session_id(id: &str) -> bool {
    crate::trust::valid_session_id(id)
}

fn observe_start(p: Payload, ctx: &mut Ctx) -> Observation {
    if !p.hook_event_name.is_empty() && p.hook_event_name != "SessionStart" {
        return Observation::Ignore("not a SessionStart payload");
    }
    if !SOURCES.contains(&p.source.as_str()) {
        return Observation::Ignore("unknown SessionStart source");
    }
    let cwd = PathBuf::from(&p.cwd);
    if !cwd.is_absolute() {
        return Observation::Ignore("no absolute cwd");
    }
    let claude = find_claude(ctx);
    if let Some(c) = &claude {
        let uses = parse(c.argv.get(1..).unwrap_or_default());
        if uses.iter().any(|u| u.is("--print")) {
            return Observation::Ignore("print mode");
        }
        if uses.iter().any(|u| u.is("--bare")) {
            return Observation::Ignore("bare mode");
        }
        let child = c
            .env
            .as_ref()
            .and_then(|e| e.get("CLAUDE_CODE_CHILD_SESSION"));
        if child.is_some_and(|v| v == "1") {
            return Observation::Ignore("nested child session");
        }
    }
    Observation::Started {
        session_id: p.session_id,
        session_file: (!p.transcript_path.is_empty()).then(|| PathBuf::from(p.transcript_path)),
        cwd,
        terminal: None,
        agent_pid: claude.map(|c| c.pid),
        argv: None,
    }
}

/// SessionEnd reasons (CC hooks docs): `prompt_input_exit` and `logout` are a
/// user quit; `clear` and `resume` are an id change that the next
/// SessionStart reports; `other` and anything unknown are left to the watcher.
fn observe_end(p: Payload) -> Observation {
    if !p.hook_event_name.is_empty() && p.hook_event_name != "SessionEnd" {
        return Observation::Ignore("not a SessionEnd payload");
    }
    let deliberate = match p.reason.as_str() {
        "prompt_input_exit" | "logout" => Some(true),
        "clear" | "resume" => return Observation::Ignore("session id change"),
        _ => None,
    };
    Observation::Ended {
        session_id: p.session_id,
        deliberate,
    }
}

/// The claude process among the hook's ancestors, starting at
/// `HERDR_WAKE_PPID`. `None` when the key is absent or no ancestor within
/// `MAX_WALK` looks like claude.
fn find_claude(ctx: &Ctx) -> Option<ProcInfo> {
    let mut pid: u32 = ctx.env.get(PPID_ENV)?.trim().parse().ok()?;
    for _ in 0..MAX_WALK {
        if pid <= 1 {
            return None;
        }
        let p = ctx.platform.process(pid)?;
        if looks_like_claude(&p) {
            return Some(p);
        }
        pid = p.ppid;
    }
    None
}

/// True for the native build (`argv[0]` or exe named `claude`, or an exe
/// under `claude/versions/`) and the npm build (`node .../claude-code/cli.js`).
pub(crate) fn looks_like_claude(p: &ProcInfo) -> bool {
    let named = |s: &str| {
        Path::new(s)
            .file_name()
            .is_some_and(|n| n == "claude" || n == "claude.exe")
    };
    let exe = p.exe.to_string_lossy();
    p.argv.first().is_some_and(|a| named(a))
        || named(&exe)
        || exe.contains("/claude/versions/")
        || p.argv.iter().take(2).any(|a| a.contains("claude-code/cli"))
}

// ---------------------------------------------------------------------------
// Resume

/// `CLAUDE_CONFIG_DIR` from `env` when set, else `<home>/.claude`.
pub(crate) fn config_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    match env.get("CLAUDE_CONFIG_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => home.join(".claude"),
    }
}

/// Claude's project folder name for `cwd`: every character that is not an
/// ASCII letter or digit becomes `-`, once per UTF-16 unit, as Claude's own
/// `replace(/[^a-zA-Z0-9]/g, "-")` does.
pub fn project_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .flat_map(|c| {
            let (ch, n) = if c.is_ascii_alphanumeric() {
                (c, 1)
            } else {
                ('-', c.len_utf16())
            };
            std::iter::repeat_n(ch, n)
        })
        .collect()
}

/// `<config>/projects/<project dir name>/<id>.jsonl` exists, or failing
/// that, `<config>/projects/*/<id>.jsonl` (Claude shortens and hashes very
/// long folder names, so the computed name can miss).
fn transcript_exists(config: &Path, cwd: &Path, id: &str) -> bool {
    let projects = config.join("projects");
    let file = format!("{id}.jsonl");
    if projects.join(project_dir_name(cwd)).join(&file).is_file() {
        return true;
    }
    let Ok(dirs) = std::fs::read_dir(&projects) else {
        return false;
    };
    dirs.flatten().any(|d| d.path().join(&file).is_file())
}

/// `s` in single quotes, safe for `sh -c`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ---------------------------------------------------------------------------
// Flag policy: the three tables from research section 7, as data.

/// How many values a flag takes, with commander's rules (Claude's parser):
/// `One` takes the next word whatever it is; `Optional` takes the next word
/// unless it starts with `-`; `Variadic` takes the next word, then every
/// following word up to the next one that starts with `-`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arity {
    Bool,
    One,
    Optional,
    Variadic,
}

/// Whether a flag is carried into the resume command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Always restored.
    Restore,
    /// A permission bypass: restored only when `restore_bypass` is on.
    Bypass,
    /// A bypass unless its value is one of these (`--permission-mode`).
    BypassUnless(&'static [&'static str]),
    /// Never restored.
    Never,
    /// Not validated yet, so not restored.
    NeedsValidation,
}

/// What a flag's values are, for resolving relative paths at resume time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Value {
    Text,
    Path,
    /// A path, unless the value is inline JSON.
    PathOrJson,
}

#[derive(Debug)]
struct Flag {
    names: &'static [&'static str],
    arity: Arity,
    class: Class,
    value: Value,
}

const fn flag(names: &'static [&'static str], arity: Arity, class: Class, value: Value) -> Flag {
    Flag {
        names,
        arity,
        class,
        value,
    }
}

use Arity::{Bool, One, Optional, Variadic};
use Class::{Bypass, BypassUnless, NeedsValidation, Never, Restore};
use Value::{PathOrJson, Text};

/// Permission modes that loosen nothing (dmazlum_herdr-revive README: modes
/// other than these count as a bypass).
const SAFE_MODES: &[&str] = &["default", "manual", "plan", "dontAsk"];

/// Every flag in `claude --help` (2.1.322). Flags not named in section 7 are
/// listed as `NeedsValidation` so their values parse correctly.
static FLAGS: &[Flag] = &[
    // Restore, opt-out (`restore_bypass`).
    flag(&["--dangerously-skip-permissions"], Bool, Bypass, Text),
    flag(
        &["--allow-dangerously-skip-permissions"],
        Bool,
        Bypass,
        Text,
    ),
    flag(&["--permission-mode"], One, BypassUnless(SAFE_MODES), Text),
    // Restore; relative paths resolved against the launch cwd.
    flag(&["--model"], One, Restore, Text),
    flag(&["--effort"], One, Restore, Text),
    flag(&["--fallback-model"], One, Restore, Text),
    flag(&["--agent"], One, Restore, Text),
    flag(&["--agents"], One, Restore, Text),
    flag(&["--add-dir"], Variadic, Restore, Value::Path),
    flag(&["--settings"], One, Bypass, PathOrJson),
    flag(&["--setting-sources"], One, Restore, Text),
    flag(&["--mcp-config"], Variadic, Restore, PathOrJson),
    flag(&["--strict-mcp-config"], Bool, Restore, Text),
    flag(&["--plugin-dir"], One, Restore, Value::Path),
    flag(
        &["--allowed-tools", "--allowedTools"],
        Variadic,
        Bypass,
        Text,
    ),
    flag(
        &["--disallowed-tools", "--disallowedTools"],
        Variadic,
        Restore,
        Text,
    ),
    flag(&["--tools"], Variadic, Bypass, Text),
    flag(&["--append-system-prompt"], One, Restore, Text),
    flag(&["--system-prompt"], One, Restore, Text),
    flag(&["--chrome"], Bool, Restore, Text),
    flag(&["--no-chrome"], Bool, Restore, Text),
    flag(&["--betas"], Variadic, Restore, Text),
    flag(&["--restricted"], Bool, Restore, Text),
    // Never.
    flag(&["-p", "--print"], Bool, Never, Text),
    flag(&["-c", "--continue"], Bool, Never, Text),
    flag(&["-r", "--resume"], Optional, Never, Text),
    flag(&["--session-id"], One, Never, Text),
    flag(&["--fork-session"], Bool, Never, Text),
    flag(&["--from-pr"], Optional, Never, Text),
    flag(&["--teleport"], Optional, Never, Text),
    flag(&["--cloud"], Optional, Never, Text),
    flag(&["--environment"], One, Never, Text),
    flag(&["--bg", "--background"], Bool, Never, Text),
    flag(&["--desktop"], Bool, Never, Text),
    flag(&["-w", "--worktree"], Optional, Never, Text),
    flag(&["--tmux"], Bool, Never, Text),
    flag(&["--file"], Variadic, Never, Text),
    flag(&["-d", "--debug"], Optional, Never, Text),
    flag(&["--debug-file"], One, Never, Text),
    flag(&["--ide"], Bool, Never, Text),
    flag(&["-n", "--name"], One, Never, Text),
    flag(&["-h", "--help"], Bool, Never, Text),
    flag(&["-v", "--version"], Bool, Never, Text),
    // Never: print-only.
    flag(&["--output-format"], One, Never, Text),
    flag(&["--input-format"], One, Never, Text),
    flag(&["--json-schema"], One, Never, Text),
    flag(&["--max-budget-usd"], One, Never, Text),
    flag(&["--include-partial-messages"], Bool, Never, Text),
    flag(&["--include-hook-events"], Bool, Never, Text),
    flag(&["--replay-user-messages"], Bool, Never, Text),
    flag(&["--no-session-persistence"], Bool, Never, Text),
    flag(&["--forward-subagent-text"], Bool, Never, Text),
    flag(&["--permission-prompts"], One, Never, Text),
    flag(&["--permission-prompt-tool"], One, Never, Text),
    // Needs validation.
    flag(&["--remote-control"], Optional, NeedsValidation, Text),
    flag(&["--bare"], Bool, NeedsValidation, Text),
    flag(&["--safe-mode"], Bool, NeedsValidation, Text),
    flag(&["--autocompact"], One, NeedsValidation, Text),
    // Not in section 7 (from `claude --help` 2.1.322): not restored.
    flag(
        &["--remote-control-session-name-prefix"],
        One,
        NeedsValidation,
        Text,
    ),
    flag(&["--ax-screen-reader"], Bool, NeedsValidation, Text),
    flag(&["--brief"], Bool, NeedsValidation, Text),
    flag(&["--disable-slash-commands"], Bool, NeedsValidation, Text),
    flag(
        &["--exclude-dynamic-system-prompt-sections"],
        Bool,
        NeedsValidation,
        Text,
    ),
    flag(&["--plugin-url"], One, NeedsValidation, Text),
    flag(&["--prompt-suggestions"], Optional, NeedsValidation, Text),
    flag(&["--system-prompt-snapshot"], One, NeedsValidation, Text),
    flag(&["--verbose"], Bool, NeedsValidation, Text),
];

fn find(name: &str) -> Option<&'static Flag> {
    FLAGS.iter().find(|f| f.names.contains(&name))
}

/// How a flag and its value were written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `--flag v1 v2` (or a bare `--flag`).
    Spaced,
    /// `--flag=v`.
    Equals,
    /// `-nvalue`.
    Attached,
}

/// One flag occurrence, kept as written so it can be emitted verbatim.
#[derive(Debug, Clone)]
struct Use {
    flag: &'static Flag,
    given: String,
    form: Form,
    values: Vec<String>,
}

impl Use {
    fn is(&self, name: &str) -> bool {
        self.flag.names.contains(&name)
    }

    /// True when a value could hold a secret, so the flag is never stored
    /// or restored: inline JSON for `--settings`, `--mcp-config` and
    /// `--agents` (a file path for the same flag is fine), or a URL with a
    /// user name or password in it for any flag.
    fn secret(&self) -> bool {
        let json = self.flag.value == PathOrJson || self.is("--agents");
        self.values
            .iter()
            .any(|v| (json && crate::trust::is_inline_json(v)) || crate::trust::has_url_userinfo(v))
    }

    fn restorable(&self, cfg: &Config) -> bool {
        !self.secret() && self.allowed(cfg)
    }

    /// The flag policy, values' secrecy aside.
    fn allowed(&self, cfg: &Config) -> bool {
        let needs_value = matches!(self.flag.arity, One | Variadic);
        if needs_value && self.values.is_empty() {
            return false;
        }
        match self.flag.class {
            Restore => true,
            Bypass => cfg.restore_bypass,
            BypassUnless(safe) => {
                cfg.restore_bypass || self.values.iter().all(|v| safe.contains(&v.as_str()))
            }
            Never | NeedsValidation => false,
        }
    }

    /// Makes relative path values absolute under `cwd`. Values starting with
    /// `~` and inline JSON are left alone.
    fn resolve_paths(&mut self, cwd: &Path) {
        let kind = self.flag.value;
        for v in &mut self.values {
            let json = v.trim_start().starts_with(['{', '[']);
            let path = match kind {
                Text => false,
                Value::Path => true,
                PathOrJson => !json,
            };
            if path && !v.is_empty() && !v.starts_with('~') && Path::new(v.as_str()).is_relative() {
                *v = cwd.join(v.as_str()).to_string_lossy().into_owned();
            }
        }
    }

    fn emit(&self) -> Vec<String> {
        match self.form {
            Form::Spaced => std::iter::once(self.given.clone())
                .chain(self.values.iter().cloned())
                .collect(),
            Form::Equals => vec![format!("{}={}", self.given, self.values.join(""))],
            Form::Attached => vec![format!("{}{}", self.given, self.values.join(""))],
        }
    }
}

/// commander's notion of a word that may be an option.
fn maybe_option(s: &str) -> bool {
    s.len() > 1 && s.starts_with('-')
}

/// The known flags in `args` (argv without `argv[0]`), parsed the way
/// commander parses them. Positional words (the prompt), unknown options and
/// everything after `--` are dropped.
fn parse(args: &[String]) -> Vec<Use> {
    let mut out: Vec<Use> = Vec::new();
    let mut queue: VecDeque<String> = args.iter().cloned().collect();
    let mut variadic: Option<usize> = None;
    while let Some(arg) = queue.pop_front() {
        if arg == "--" {
            break;
        }
        if let Some(i) = variadic {
            if !maybe_option(&arg) {
                out[i].values.push(arg);
                continue;
            }
        }
        variadic = None;
        if maybe_option(&arg) {
            if let Some(flag) = find(&arg) {
                let mut values = Vec::new();
                match flag.arity {
                    Bool => {}
                    One | Variadic => values.extend(queue.pop_front()),
                    Optional => {
                        if queue.front().is_some_and(|n| !maybe_option(n)) {
                            values.extend(queue.pop_front());
                        }
                    }
                }
                if flag.arity == Variadic {
                    variadic = Some(out.len());
                }
                out.push(Use {
                    flag,
                    given: arg,
                    form: Form::Spaced,
                    values,
                });
                continue;
            }
        }
        // `-xyz`: a cluster of short flags, or a short flag with its value.
        if !arg.starts_with("--") && arg.starts_with('-') {
            if let Some(c) = arg[1..].chars().next() {
                let short = format!("-{c}");
                let rest = &arg[1 + c.len_utf8()..];
                if let (Some(flag), false) = (find(&short), rest.is_empty()) {
                    if flag.arity == Bool {
                        queue.push_front(format!("-{rest}"));
                        out.push(Use {
                            flag,
                            given: short,
                            form: Form::Spaced,
                            values: Vec::new(),
                        });
                    } else {
                        out.push(Use {
                            flag,
                            given: short,
                            form: Form::Attached,
                            values: vec![rest.to_string()],
                        });
                    }
                    continue;
                }
            }
        }
        // `--flag=value` for a flag that takes a value.
        if arg.starts_with("--") {
            if let Some(eq) = arg.find('=').filter(|&eq| eq > 2) {
                if let Some(flag) = find(&arg[..eq]).filter(|f| f.arity != Bool) {
                    out.push(Use {
                        flag,
                        given: arg[..eq].to_string(),
                        form: Form::Equals,
                        values: vec![arg[eq + 1..].to_string()],
                    });
                    continue;
                }
            }
        }
        // A positional word or an unknown option: dropped.
    }
    out
}
