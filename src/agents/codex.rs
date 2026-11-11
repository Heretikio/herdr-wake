//! Codex CLI adapter (Task 7).
//!
//! **Learning the session.** Only `SessionStart` is installed (turn hooks
//! cause timeout-clamp warnings, HD #5246). Since `daemon_auto_start`, Codex
//! runs hooks inside a shared app-server whose environment belongs to
//! whichever terminal started it, so the hook's `HC_TERMINAL_ID` can name the
//! wrong terminal. `observe` therefore ignores it and binds the session to a
//! herdr-core terminal by its foreground process (core reports the process
//! group leader, which `herdr-wake shell` makes the agent):
//!
//! 1. A terminal whose foreground process is `codex` and whose argv names the
//!    session id (resumed runs). Exactly one such terminal wins.
//! 2. Otherwise a terminal whose foreground process is `codex` and whose
//!    folder equals the payload `cwd`. Exactly one such terminal wins.
//!
//! Zero candidates give `Ignore("unbound")`, several `Ignore("ambiguous")`.
//! Core refreshes a terminal's foreground process only about twice a second
//! (`foreground.rs` in herdr-core), so a hook that fires right after Codex
//! starts can find no candidate; the hook command retries an unbound result
//! for about 2 s (`hook::retry_unbound`).
//!
//! Limitation: the research also bounds the cwd match by process start time
//! (start at most the session's creation plus a few seconds). `ProcInfo.start`
//! is an opaque per-platform stamp and there is no state database to read a
//! creation time from (no SQLite dependency), so no time window is applied:
//! uniqueness of the cwd match is the only tiebreak. The folder compared is
//! the terminal's folder as core reports it, not the process's own folder.
//!
//! **Skipping.** `CODEX_THREAD_ID` set and different from the session id
//! means a Codex nested in another Codex's tool shell. When the session's
//! rollout (`$CODEX_HOME/sessions/**/rollout-<ts>-<uuid>.jsonl`) exists and
//! is plain JSONL, its first line (`session_meta`) must have `source` `cli`
//! (or another plain string that is not `exec`, `vscode` or `mcp`) and
//! `thread_source` `user` or absent. Compressed rollouts (`.jsonl.zst`) and
//! rollouts not yet written skip this check.
//!
//! **Resuming.** `codex resume <flags> <id>` in the recorded folder, with
//! `CODEX_HOME` taken from the record. The rollout's existence is the
//! session-exists check. Hook trust is never written: the user reviews the
//! hook in Codex's `/hooks`, and the command string never changes so the
//! trust hash stays valid across herdr-wake versions.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use super::{Agent, Ctx, Edit, EditKind, Observation, Plan};
use crate::config::Config;
use crate::core::TerminalId;
use crate::platform::ProcInfo;
use crate::record::{AgentKind, Record};

/// The `Ignore` reason when no terminal runs the session (yet): the hook
/// tries again shortly, see `hook::retry_unbound`.
pub const UNBOUND: &str = "unbound";

/// The Codex hook event herdr-wake installs.
pub const HOOK_EVENT: &str = "SessionStart";

/// The hook's timeout in seconds.
pub const HOOK_TIMEOUT_S: u32 = 10;

/// Longest rollout first line read for the source check.
const MAX_FIRST_LINE: u64 = 1 << 20;

/// Deepest folder level searched under `sessions/` (`YYYY/MM/DD/file`).
const MAX_SESSIONS_DEPTH: usize = 4;

#[derive(Debug, Default)]
pub struct Codex;

/// The hook command line: `'<bin>' hook codex start`, with `bin`
/// single-quoted for the shell. It must never change between versions,
/// because Codex hook trust is bound to its hash.
pub fn hook_command(bin: &Path) -> String {
    let quoted = bin.to_string_lossy().replace('\'', r"'\''");
    format!("'{quoted}' hook codex start")
}

/// `$CODEX_HOME` from `env` when set and non-empty, else `<home>/.codex`.
pub fn codex_home(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    match env.get("CODEX_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(".codex"),
    }
}

/// The rollout file of session `id` under `<codex_home>/sessions`, plain
/// (`.jsonl`) preferred over compressed (`.jsonl.zst`), or `None`.
pub fn find_rollout(codex_home: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() {
        return None;
    }
    let mut compressed = None;
    let mut stack = vec![(codex_home.join("sessions"), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if ft.is_dir() {
                if depth + 1 < MAX_SESSIONS_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let name = entry.file_name();
            match rollout_name_matches(&name.to_string_lossy(), id) {
                Some(false) => return Some(path),
                Some(true) => compressed = compressed.or(Some(path)),
                None => {}
            }
        }
    }
    compressed
}

/// `Some(compressed)` when `name` is the rollout file name of thread `id`
/// (`rollout-YYYY-MM-DDTHH-MM-SS-<thread>[_<rollout>].jsonl[.zst]`).
fn rollout_name_matches(name: &str, id: &str) -> Option<bool> {
    let (stem, compressed) = match name.strip_suffix(".jsonl.zst") {
        Some(s) => (s, true),
        None => (name.strip_suffix(".jsonl")?, false),
    };
    let core = stem.strip_prefix("rollout-")?;
    if core.get(19..20)? != "-" {
        return None;
    }
    let ids = core.get(20..)?;
    let thread = ids.split_once('_').map_or(ids, |(t, _)| t);
    thread.eq_ignore_ascii_case(id).then_some(compressed)
}

/// What a rollout's first line says about who started the thread.
#[derive(Debug, PartialEq, Eq)]
enum Origin {
    /// A user's thread, or nothing to tell otherwise.
    User,
    /// `codex exec`, the IDE extension or an MCP client.
    NonInteractive,
    /// A sub-agent, review, guardian or other thread Codex started itself.
    SubAgent,
}

fn rollout_origin(path: &Path) -> Origin {
    if path.extension().is_some_and(|e| e == "zst") {
        return Origin::User;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return Origin::User;
    };
    let mut line = String::new();
    if BufReader::new(file.take(MAX_FIRST_LINE))
        .read_line(&mut line)
        .is_err()
    {
        return Origin::User;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return Origin::User;
    };
    if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
        return Origin::User;
    }
    let Some(meta) = v.get("payload") else {
        return Origin::User;
    };
    match meta.get("source") {
        Some(serde_json::Value::String(s)) if matches!(s.as_str(), "exec" | "vscode" | "mcp") => {
            return Origin::NonInteractive
        }
        Some(serde_json::Value::Object(_)) => return Origin::SubAgent,
        _ => {}
    }
    match meta.get("thread_source") {
        Some(serde_json::Value::String(s)) if s != "user" => Origin::SubAgent,
        _ => Origin::User,
    }
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// The arguments after the program in a Codex argv: `argv[1..]`, or
/// `argv[2..]` for the npm launcher run by its interpreter
/// (`node /path/to/codex ...`).
fn program_args(argv: &[String]) -> &[String] {
    let wrapped = argv
        .first()
        .is_some_and(|a| matches!(basename(a), "node" | "bun"))
        && argv
            .get(1)
            .is_some_and(|a| matches!(basename(a), "codex" | "codex.js"));
    if wrapped {
        &argv[2..]
    } else {
        argv.get(1..).unwrap_or(&[])
    }
}

/// The Codex arguments of `p`, or `None` when `p` is not Codex.
pub(crate) fn codex_args(p: &ProcInfo) -> Option<&[String]> {
    let argv0 = p.argv.first().map(|a| basename(a)).unwrap_or("");
    let exe = p
        .exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if argv0 == "codex" || exe == "codex" {
        return Some(p.argv.get(1..).unwrap_or(&[]));
    }
    let args = program_args(&p.argv);
    (args.len() + 2 == p.argv.len()).then_some(args)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Picks the one terminal that runs this session, as described in the
/// module docs.
fn bind(id: &str, cwd: &Path, ctx: &mut Ctx) -> Result<(TerminalId, u32), &'static str> {
    let terminals = ctx.core.list(false).map_err(|_| "core unavailable")?;
    let mut by_argv = Vec::new();
    let mut by_cwd = Vec::new();
    for t in terminals {
        let Some(pid) = t.foreground_pid else {
            continue;
        };
        let Some(p) = ctx.platform.process(pid) else {
            continue;
        };
        let Some(args) = codex_args(&p) else {
            continue;
        };
        if args.iter().any(|a| a.eq_ignore_ascii_case(id)) {
            by_argv.push((t.address, pid));
        } else if t.cwd.as_deref().is_some_and(|c| same_dir(c, cwd)) {
            by_cwd.push((t.address, pid));
        }
    }
    let pool = if by_argv.is_empty() { by_cwd } else { by_argv };
    match pool.len() {
        0 => Err(UNBOUND),
        1 => Ok(pool.into_iter().next().expect("one candidate")),
        _ => Err("ambiguous"),
    }
}

#[derive(serde::Deserialize)]
struct StartPayload {
    session_id: String,
    cwd: PathBuf,
    #[serde(default)]
    transcript_path: Option<PathBuf>,
}

/// How a flag is shaped on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arity {
    Bool,
    /// One value, next token or attached (`--x=v`, `-xv`).
    Value,
    /// One or more values (clap `num_args = 1..`), dropped anyway.
    Many,
}

/// What happens to a flag on resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Permission bypass: kept only when `restore_bypass` is on.
    Bypass,
    Keep,
    Never,
}

/// The Codex flag table (research section 7). Anything not listed is
/// dropped, as are positionals (prompt, subcommand, session id).
const FLAGS: &[(&[&str], Arity, Class)] = &[
    (
        &[
            "--dangerously-bypass-approvals-and-sandbox",
            "--yolo",
            "--approve-for-me",
            "--not-so-yolo",
        ],
        Arity::Bool,
        Class::Bypass,
    ),
    (
        &[
            "-m",
            "--model",
            "-p",
            "--profile",
            "-s",
            "--sandbox",
            "-a",
            "--ask-for-approval",
            "-c",
            "--config",
            "--enable",
            "--disable",
            "--add-dir",
            "--local-provider",
        ],
        Arity::Value,
        Class::Keep,
    ),
    (
        &[
            "--search",
            "--oss",
            "--no-alt-screen",
            "--no-daemon",
            "--strict-config",
        ],
        Arity::Bool,
        Class::Keep,
    ),
    (
        &["-C", "--cd", "--remote", "--remote-auth-token-env"],
        Arity::Value,
        Class::Never,
    ),
    (&["-i", "--image"], Arity::Many, Class::Never),
    (
        &[
            "--dangerously-bypass-hook-trust",
            "--last",
            "--all",
            "--include-non-interactive",
            "--worktree",
            "--remote-control",
        ],
        Arity::Bool,
        Class::Never,
    ),
];

/// The key of a `-c key=value` override, without quotes or spaces.
fn config_key(value: &str) -> &str {
    value
        .split_once('=')
        .map_or(value, |(k, _)| k)
        .trim()
        .trim_matches(['"', '\''])
}

/// True when the `Keep` flag `name` with `value` loosens Codex's sandbox or
/// approvals, so `restore_bypass = false` must drop it:
///
/// - `-s/--sandbox` with anything but `read-only` or `workspace-write`
///   (`danger-full-access`, and values this build does not know);
/// - `-a/--ask-for-approval` with anything but `untrusted`, `on-request` or
///   `on-failure` (`never`, and unknown values);
/// - `-c/--config` whose key, or any dotted part of it, is
///   `approval_policy`, `sandbox_mode`, starts with `sandbox_`, or is
///   `profile`;
/// - `-p/--profile` always: a profile can set any of the above.
fn loosens(name: &str, value: &str) -> bool {
    match name {
        "-s" | "--sandbox" => !matches!(value, "read-only" | "workspace-write"),
        "-a" | "--ask-for-approval" => !matches!(value, "untrusted" | "on-request" | "on-failure"),
        "-p" | "--profile" => true,
        "-c" | "--config" => config_key(value).split('.').any(|part| {
            let part = part.trim_matches(['"', '\'']);
            part == "approval_policy"
                || part == "sandbox_mode"
                || part == "profile"
                || part.starts_with("sandbox_")
        }),
        _ => false,
    }
}

fn lookup(name: &str) -> Option<(Arity, Class)> {
    FLAGS
        .iter()
        .find(|(names, _, _)| names.contains(&name))
        .map(|&(_, a, c)| (a, c))
}

impl Agent for Codex {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn observe(&self, event: &str, payload: &[u8], ctx: &mut Ctx) -> Observation {
        if event != "start" {
            return Observation::Ignore("event not handled");
        }
        let Ok(p) = serde_json::from_slice::<StartPayload>(payload) else {
            return Observation::Ignore("bad payload");
        };
        if p.session_id.is_empty() || p.cwd.as_os_str().is_empty() {
            return Observation::Ignore("bad payload");
        }
        if ctx
            .env
            .get("CODEX_THREAD_ID")
            .is_some_and(|t| !t.is_empty() && *t != p.session_id)
        {
            return Observation::Ignore("nested");
        }
        let rollout = p
            .transcript_path
            .as_ref()
            .filter(|t| t.is_file())
            .cloned()
            .or_else(|| find_rollout(&codex_home(ctx.env, ctx.home), &p.session_id));
        if let Some(r) = &rollout {
            match rollout_origin(r) {
                Origin::User => {}
                Origin::NonInteractive => return Observation::Ignore("non-interactive"),
                Origin::SubAgent => return Observation::Ignore("sub-agent"),
            }
        }
        match bind(&p.session_id, &p.cwd, ctx) {
            Ok((terminal, pid)) => Observation::Started {
                session_id: p.session_id,
                session_file: rollout.or(p.transcript_path),
                cwd: p.cwd,
                terminal: Some(terminal),
                agent_pid: Some(pid),
                argv: None,
            },
            Err(reason) => Observation::Ignore(reason),
        }
    }

    fn restorable_flags(&self, argv: &[String], cfg: &Config) -> Vec<String> {
        filter_flags(argv, cfg, &mut Vec::new())
    }

    fn withheld_flags(&self, argv: &[String], cfg: &Config) -> Vec<String> {
        let mut withheld = Vec::new();
        filter_flags(argv, cfg, &mut withheld);
        withheld
    }

    fn resume_plan(&self, rec: &Record, ctx: &mut Ctx) -> Plan {
        resume_plan(self, rec, ctx)
    }

    fn present(&self, ctx: &Ctx) -> bool {
        super::installed(self.kind(), &codex_home(ctx.env, ctx.home), ctx.env)
    }

    fn integration(&self, bin: &Path, ctx: &mut Ctx) -> Vec<Edit> {
        vec![Edit {
            path: codex_home(ctx.env, ctx.home).join("hooks.json"),
            kind: EditKind::JsonHook {
                event: HOOK_EVENT.to_string(),
                matcher: None,
                command: hook_command(bin),
                timeout: HOOK_TIMEOUT_S,
            },
        }]
    }
}

/// True when a kept flag's value could hold a secret, so the flag is never
/// stored or restored: a `-c` override whose key or value looks like a
/// credential, or any value with a URL that carries a user name or password.
fn secret(name: &str, value: &str) -> bool {
    crate::trust::has_url_userinfo(value)
        || (matches!(name, "-c" | "--config") && crate::trust::looks_like_credential(value))
}

/// The restorable flags of `argv` (see `Agent::restorable_flags`). The
/// names of flags left out because of `secret` are added to `withheld`.
fn filter_flags(argv: &[String], cfg: &Config, withheld: &mut Vec<String>) -> Vec<String> {
    {
        let args = program_args(argv);
        let mut out = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            i += 1;
            if a == "--" {
                break;
            }
            // Flag name, and whether its value is attached to the token.
            let (name, attached) = if let Some(long) = a.strip_prefix("--") {
                match long.split_once('=') {
                    Some((n, _)) => (format!("--{n}"), true),
                    None => (a.clone(), false),
                }
            } else if a.starts_with('-') && a.len() > 1 {
                match a.char_indices().nth(2) {
                    Some((at, _)) => (a[..at].to_string(), true),
                    None => (a.clone(), false),
                }
            } else {
                continue; // positional: prompt, subcommand or session id
            };
            let Some((arity, class)) = lookup(&name) else {
                continue;
            };
            let mut tokens = vec![a.clone()];
            // The flag's value, wherever it was written.
            let mut value = attached.then(|| {
                let at = if a.starts_with("--") {
                    name.len() + 1
                } else {
                    name.len()
                };
                a[at..].to_string()
            });
            match (arity, attached) {
                (Arity::Bool, true) => continue,
                (Arity::Value, false) => match args.get(i) {
                    Some(v) => {
                        tokens.push(v.clone());
                        value = Some(v.clone());
                        i += 1;
                    }
                    None => continue,
                },
                (Arity::Many, false) => {
                    while args.get(i).is_some_and(|v| !v.starts_with('-')) {
                        i += 1;
                    }
                }
                _ => {}
            }
            let keep = match class {
                Class::Keep => {
                    cfg.restore_bypass || !loosens(&name, value.as_deref().unwrap_or(""))
                }
                Class::Bypass => cfg.restore_bypass,
                Class::Never => false,
            };
            if keep && secret(&name, value.as_deref().unwrap_or("")) {
                withheld.push(name);
                continue;
            }
            if keep {
                out.extend(tokens);
            }
        }
        out
    }
}

fn resume_plan(codex: &Codex, rec: &Record, ctx: &mut Ctx) -> Plan {
    {
        if !rec.cwd.is_dir() {
            return Plan::Permanent("folder missing".into());
        }
        let home = match rec.env.get("CODEX_HOME") {
            Some(v) if !v.is_empty() => rec.cwd.join(v),
            _ => codex_home(ctx.env, ctx.home),
        };
        if find_rollout(&home, &rec.session_id).is_none() {
            return Plan::Permanent("session not found".into());
        }
        // Re-filter the stored flags so a later `restore_bypass = false` applies.
        let mut stored = vec!["codex".to_string()];
        stored.extend(rec.flags.iter().cloned());
        let mut argv = vec![rec.bin.to_string_lossy().into_owned(), "resume".to_string()];
        argv.extend(codex.restorable_flags(&stored, ctx.cfg));
        argv.push(rec.session_id.clone());
        let mut env = rec.env.clone();
        env.insert("HERDR_WAKE_SUPERVISED".to_string(), "1".to_string());
        Plan::Run {
            argv,
            env,
            cwd: rec.cwd.clone(),
            before: Vec::new(),
            confirm_within_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollout_names() {
        let id = "019a6f2e-7c41-7d30-9f0e-3b2a1c4d5e6f";
        let ts = "rollout-2026-11-10T09-15-02-";
        assert_eq!(
            rollout_name_matches(&format!("{ts}{id}.jsonl"), id),
            Some(false)
        );
        assert_eq!(
            rollout_name_matches(&format!("{ts}{id}.jsonl.zst"), id),
            Some(true)
        );
        assert_eq!(
            rollout_name_matches(
                &format!("{ts}{id}_019a7000-0000-7000-8000-000000000000.jsonl"),
                id
            ),
            Some(false)
        );
        assert_eq!(rollout_name_matches(&format!("{ts}{id}.json"), id), None);
        assert_eq!(rollout_name_matches(&format!("{ts}x{id}.jsonl"), id), None);
        assert_eq!(rollout_name_matches(&format!("{ts}{id}.jsonl"), ""), None);
    }

    #[test]
    fn hook_command_quotes_single_quotes() {
        assert_eq!(
            hook_command(Path::new("/a/it's/herdr-wake")),
            r"'/a/it'\''s/herdr-wake' hook codex start"
        );
    }
}
