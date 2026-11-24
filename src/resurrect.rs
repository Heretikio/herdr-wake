//! `herdr-wake resurrect`: list the agent sessions that were closed (or lost)
//! and bring one back, in its folder, with its flags, through the checks
//! `herdr-wake shell` makes at wake time.
//!
//! **What is listed.** The history journal (`history`) first: it knows the
//! exact flags a session ran with. Then sessions that only left a transcript
//! (Claude Code and Codex; see `transcripts`), with flags rebuilt from the
//! first recorded permission mode and marked as such. A session that is
//! running now is never listed: a `Running` record on a live herdr-core
//! terminal (only when core answers; nothing here starts it), a Claude
//! `sessions/<pid>.json` whose process is alive, or a running agent whose
//! command line names the session.
//!
//! **Bringing one back.** `prepare` checks everything `wake::launch_decision`
//! checks at wake time (session id, folder safety, `restore_bypass`, the
//! program, the transcript) on a record rebuilt from the row, and returns the
//! command. `--print` prints it. Otherwise `herdr-wake` writes a short-lived
//! ticket in the state folder, asks `hc` for a new terminal that runs
//! `herdr-wake resurrect --run <key>`, and that process (`run_ticket`) checks
//! again, runs the agent in the foreground like `herdr-wake shell` does and
//! then replaces itself with the real shell, also after any error. The agent
//! is not marked as supervised: its own hook records it in the new terminal
//! as for any session you start, and the watcher notices when you quit it.
//!
//! Resurrect replays the recorded flags, permission bypass included, because
//! a person starts it. `restore_bypass = false` still drops them and the
//! folder check still applies.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::{self, Ctx, Plan};
use crate::config::Config;
use crate::core::{Core, TerminalId};
use crate::history::{self, Ending, Entry};
use crate::platform::Platform;
use crate::record::{self, AgentKind, State};
use crate::transcripts::{self, Cache};
use crate::wake::{self, Decision};
use crate::{paths, trust};

/// Where a row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The history journal: exact flags.
    Wake,
    /// A transcript only: flags rebuilt from it.
    Transcript,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Wake => "wake",
            Source::Transcript => "transcript",
        }
    }
}

/// Whether the agent's own session file is still there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transcript {
    Present,
    /// Deleted (Claude does this after `cleanupPeriodDays`): the agent
    /// cannot resume it.
    Gone,
    /// Not a file herdr-wake can look for (OpenCode keeps sessions in a
    /// database).
    Unknown,
}

/// One session that can be listed.
#[derive(Debug, Clone)]
pub struct Row {
    /// What a resume needs. For a transcript row, `bin` is empty until
    /// `prepare` finds the program.
    pub entry: Entry,
    pub title: Option<String>,
    /// When the session was last active, in milliseconds.
    pub last_ms: u64,
    pub source: Source,
    pub transcript: Transcript,
    /// True when the flags were rebuilt from a transcript.
    pub flags_from_transcript: bool,
    /// True when the flags include a permission bypass.
    pub bypass: bool,
    /// True when the session is running now.
    pub open: bool,
}

impl Row {
    pub fn agent(&self) -> AgentKind {
        self.entry.agent
    }

    pub fn id(&self) -> &str {
        &self.entry.session_id
    }

    /// How it ended, as shown: a journal entry that was never seen to end is
    /// `unknown`.
    pub fn ending(&self) -> Ending {
        match self.entry.ending {
            Ending::Open => Ending::Unknown,
            e => e,
        }
    }
}

/// True when launching `agent` with `flags` would restore a permission
/// bypass: the agent's own flag policy keeps something with `restore_bypass`
/// on that it drops with it off.
pub fn has_bypass(agent: AgentKind, flags: &[String], cfg: &Config) -> bool {
    if !cfg.restore_bypass {
        // Nothing of the kind is replayed, so nothing is marked.
        return false;
    }
    let mut argv = vec![agent.as_str().to_string()];
    argv.extend(flags.iter().cloned());
    let adapter = agents::get(agent);
    let keep = |restore_bypass| {
        let cfg = Config {
            restore_bypass,
            ..Config::default()
        };
        adapter.restorable_flags(&argv, &cfg)
    };
    keep(true) != keep(false)
}

/// The launch flags a Claude session's first recorded permission mode
/// stands for: `bypassPermissions` is `--dangerously-skip-permissions`,
/// `plan`, `auto` and `acceptEdits` are `--permission-mode <mode>`, anything
/// else nothing.
pub fn claude_mode_flags(mode: Option<&str>) -> Vec<String> {
    match mode {
        Some("bypassPermissions") => vec!["--dangerously-skip-permissions".to_string()],
        Some(m @ ("plan" | "auto" | "acceptEdits")) => {
            vec!["--permission-mode".to_string(), m.to_string()]
        }
        _ => Vec::new(),
    }
}

// ------------------------------------------------------------ open sessions

/// The sessions that are running now, as far as herdr-wake can tell.
#[derive(Debug, Default)]
pub struct OpenSet {
    ids: BTreeSet<(AgentKind, String)>,
    argv: Vec<(AgentKind, Vec<String>)>,
}

impl OpenSet {
    /// True when the session is among the running ones: named by a
    /// running record or session file, or by the command line of a running
    /// agent of the same kind (a word equal to the id, or for ids of 16
    /// characters or more, containing it: Pi names a file after its id).
    pub fn contains(&self, agent: AgentKind, id: &str) -> bool {
        self.ids.contains(&(agent, id.to_string()))
            || self.argv.iter().any(|(kind, words)| {
                *kind == agent
                    && words
                        .iter()
                        .any(|w| w == id || (id.len() >= 16 && w.contains(id)))
            })
    }
}

/// The fields of Claude's `<config dir>/sessions/<pid>.json` that tell
/// whether a session is running.
#[derive(Deserialize)]
struct ClaudeSession {
    #[serde(rename = "sessionId")]
    session_id: String,
    pid: u32,
}

/// Finds the running sessions: `Running` records on live terminals and the
/// agents in their foreground (only when core answers), and Claude's own
/// session files whose process is alive and is claude.
pub fn open_sessions(ctx: &mut Ctx, claude_dirs: &BTreeSet<PathBuf>) -> OpenSet {
    let mut open = OpenSet::default();
    if let Ok(terminals) = ctx.core.list(false) {
        for t in terminals {
            if let Some(rec) = record::of_terminal(&t).filter(|r| r.state == State::Running) {
                open.ids.insert((rec.agent, rec.session_id));
            }
            let process = t.foreground_pid.and_then(|pid| ctx.platform.process(pid));
            if let Some(p) = process {
                if let Some(kind) = agents::identify(&p) {
                    open.argv.push((kind, p.argv));
                }
            }
        }
    }
    for dir in claude_dirs {
        let Ok(files) = std::fs::read_dir(dir.join("sessions")) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(s) = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<ClaudeSession>(&b).ok())
            else {
                continue;
            };
            let alive = ctx
                .platform
                .process(s.pid)
                .is_some_and(|p| agents::claude::looks_like_claude(&p));
            if alive {
                open.ids.insert((AgentKind::Claude, s.session_id));
            }
        }
    }
    open
}

// ------------------------------------------------------------------ rows

/// The Claude config folder a journal entry's session used, found the way
/// `resume_plan` finds it.
fn entry_claude_dir(e: &Entry, home: &Path) -> PathBuf {
    agents::claude::config_dir_in(&e.env, home, &e.cwd)
}

/// The Codex home folder a journal entry's session used, found the way
/// `resume_plan` finds it (the caller's `env` stands in when the entry has
/// none).
fn entry_codex_dir(e: &Entry, env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    agents::codex::home_in(&e.env, env, home, &e.cwd)
}

/// Where a journal entry's session file is, and whether it is there.
fn entry_transcript(
    e: &Entry,
    env: &BTreeMap<String, String>,
    home: &Path,
) -> (Option<PathBuf>, Transcript) {
    let recorded = e.session_file.clone().filter(|f| f.is_file());
    let found = match e.agent {
        AgentKind::Claude => recorded.or_else(|| {
            transcripts::claude_transcript(&entry_claude_dir(e, home), &e.cwd, &e.session_id)
        }),
        AgentKind::Codex => recorded.or_else(|| {
            crate::agents::codex::find_rollout(&entry_codex_dir(e, env, home), &e.session_id)
        }),
        AgentKind::Pi => recorded,
        AgentKind::OpenCode => return (None, Transcript::Unknown),
    };
    match found {
        Some(p) => (Some(p), Transcript::Present),
        None => (None, Transcript::Gone),
    }
}

/// Every session known to herdr-wake that could be listed (open ones
/// flagged), newest first: the journal, then transcripts the journal lacks.
pub fn collect(ctx: &mut Ctx) -> Vec<Row> {
    let mut cache = Cache::load(&paths::transcript_cache_file(ctx.env, ctx.home));
    let journal = history::read_all(&paths::history_dir(ctx.env, ctx.home));
    let mut rows: Vec<Row> = Vec::new();
    let mut seen: BTreeSet<(AgentKind, String)> = BTreeSet::new();
    let mut names_by_dir: BTreeMap<PathBuf, BTreeMap<String, String>> = BTreeMap::new();

    let mut claude_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut codex_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    claude_dirs.insert(transcripts::claude_dir(ctx.env, ctx.home));
    codex_dirs.insert(transcripts::codex_dir(ctx.env, ctx.home));

    for e in journal {
        seen.insert((e.agent, e.session_id.clone()));
        let (path, transcript) = entry_transcript(&e, ctx.env, ctx.home);
        let found = path
            .as_deref()
            .and_then(|p| transcripts::meta_of(e.agent, p, &mut cache));
        let mut title = found
            .as_ref()
            .and_then(|f| f.meta.title())
            .map(str::to_string);
        if e.agent == AgentKind::Codex {
            let dir = entry_codex_dir(&e, ctx.env, ctx.home);
            let names = names_by_dir
                .entry(dir.clone())
                .or_insert_with(|| transcripts::codex_names(&dir));
            title = names.get(&e.session_id).cloned().or(title);
        }
        match e.agent {
            AgentKind::Claude => {
                claude_dirs.insert(entry_claude_dir(&e, ctx.home));
            }
            AgentKind::Codex => {
                codex_dirs.insert(entry_codex_dir(&e, ctx.env, ctx.home));
            }
            _ => {}
        }
        rows.push(Row {
            last_ms: e.last_ms().max(found.map_or(0, |f| f.mtime_ms)),
            bypass: has_bypass(e.agent, &e.flags, ctx.cfg),
            entry: e,
            title,
            source: Source::Wake,
            transcript,
            flags_from_transcript: false,
            open: false,
        });
    }

    let default_claude = ctx.home.join(".claude");
    let default_codex = ctx.home.join(".codex");
    for dir in &claude_dirs {
        for f in transcripts::scan_claude(dir, &mut cache) {
            let key = (f.meta.agent, f.meta.session_id.clone());
            let Some(cwd) = f.meta.cwd.clone() else {
                continue;
            };
            if !seen.insert(key) {
                continue;
            }
            let flags = claude_mode_flags(f.meta.mode.as_deref());
            let mut env = BTreeMap::new();
            if *dir != default_claude {
                env.insert(
                    "CLAUDE_CONFIG_DIR".to_string(),
                    dir.to_string_lossy().into_owned(),
                );
            }
            rows.push(transcript_row(&f, cwd, flags, env, ctx.cfg));
        }
    }
    for dir in &codex_dirs {
        for f in transcripts::scan_codex(dir, &mut cache) {
            let key = (f.meta.agent, f.meta.session_id.clone());
            let Some(cwd) = f.meta.cwd.clone() else {
                continue;
            };
            if !seen.insert(key) {
                continue;
            }
            let mut env = BTreeMap::new();
            if *dir != default_codex {
                env.insert("CODEX_HOME".to_string(), dir.to_string_lossy().into_owned());
            }
            rows.push(transcript_row(&f, cwd, Vec::new(), env, ctx.cfg));
        }
    }
    let _ = cache.save(&paths::transcript_cache_file(ctx.env, ctx.home));

    let open = open_sessions(ctx, &claude_dirs);
    for r in &mut rows {
        r.open = open.contains(r.agent(), r.id());
    }
    rows.sort_by(|a, b| {
        b.last_ms
            .cmp(&a.last_ms)
            .then_with(|| a.entry.session_id.cmp(&b.entry.session_id))
    });
    rows
}

fn transcript_row(
    f: &transcripts::Found,
    cwd: PathBuf,
    flags: Vec<String>,
    env: BTreeMap<String, String>,
    cfg: &Config,
) -> Row {
    let agent = f.meta.agent;
    Row {
        bypass: has_bypass(agent, &flags, cfg),
        entry: Entry {
            v: history::VERSION,
            agent,
            session_id: f.meta.session_id.clone(),
            session_file: Some(f.path.clone()),
            cwd,
            flags,
            env,
            bin: PathBuf::new(),
            terminal: None,
            started_ms: f.mtime_ms,
            ended_ms: Some(f.mtime_ms),
            ending: Ending::Unknown,
        },
        title: f.meta.title().map(str::to_string),
        last_ms: f.mtime_ms,
        source: Source::Transcript,
        transcript: Transcript::Present,
        flags_from_transcript: true,
        open: false,
    }
}

// --------------------------------------------------------------- filtering

/// What `herdr-wake resurrect` was asked to list.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Filter {
    /// Words that must all appear (case-insensitive) in the title, folder,
    /// session id or agent name.
    pub query: Option<String>,
    /// Only sessions whose folder is this folder or inside it.
    pub scope: Option<PathBuf>,
    pub agent: Option<AgentKind>,
}

impl Filter {
    fn matches(&self, r: &Row) -> bool {
        if self.agent.is_some_and(|a| a != r.agent()) {
            return false;
        }
        if let Some(scope) = &self.scope {
            if !inside(&r.entry.cwd, scope) {
                return false;
            }
        }
        if let Some(q) = &self.query {
            let hay = format!(
                "{}\n{}\n{}\n{}",
                r.title.as_deref().unwrap_or(""),
                r.entry.cwd.display(),
                r.id(),
                r.agent()
            )
            .to_lowercase();
            if !q
                .to_lowercase()
                .split_whitespace()
                .all(|word| hay.contains(word))
            {
                return false;
            }
        }
        true
    }

    /// True when no filter is set.
    pub fn is_empty(&self) -> bool {
        self.query.is_none() && self.scope.is_none() && self.agent.is_none()
    }
}

/// True when `path` is `scope` or inside it, as written or once symlinks
/// are resolved.
fn inside(path: &Path, scope: &Path) -> bool {
    if path.starts_with(scope) {
        return true;
    }
    match (path.canonicalize(), scope.canonicalize()) {
        (Ok(p), Ok(s)) => p.starts_with(s),
        (Err(_), Ok(s)) => path.starts_with(s),
        _ => false,
    }
}

/// The rows to list: not open, matching `filter`, newest first, at most
/// `limit` of them (0 for all). The second value is how many matched.
pub fn select<'a>(rows: &'a [Row], filter: &Filter, limit: usize) -> (Vec<&'a Row>, usize) {
    let all: Vec<&Row> = rows
        .iter()
        .filter(|r| !r.open && filter.matches(r))
        .collect();
    let total = all.len();
    let shown = if limit == 0 {
        all
    } else {
        all.into_iter().take(limit).collect()
    };
    (shown, total)
}

// ---------------------------------------------------------------- display

/// `now`, `5m`, `3h`, `2d`, `4mo` or `1y`: how long ago `then_ms` was.
pub fn age(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    match secs {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        86_400..=2_591_999 => format!("{}d", secs / 86_400),
        2_592_000..=31_103_999 => format!("{}mo", secs / 2_592_000),
        _ => format!("{}y", secs / 31_104_000),
    }
}

/// `path` with the home folder written as `~`.
pub fn place(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn one_line(s: &str) -> String {
    trust::printable(&s.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// `s` cut to `n` characters, with `...` at the end.
fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let keep: String = s.chars().take(n.saturating_sub(3)).collect();
    format!("{keep}...")
}

/// `s` cut to `n` characters, with `...` at the start.
fn cut_left(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len <= n {
        return s.to_string();
    }
    let keep: String = s.chars().skip(len - n.saturating_sub(3)).collect();
    format!("...{keep}")
}

/// The notes column: where the row came from, and a deleted transcript.
fn notes(r: &Row) -> String {
    match r.transcript {
        Transcript::Gone => format!("{}, transcript gone", r.source.as_str()),
        _ => r.source.as_str().to_string(),
    }
}

/// The numbered table, newest first. `shown` and `total` are the number of
/// rows listed and the number that matched.
pub fn render_list(
    rows: &[&Row],
    total: usize,
    now_ms: u64,
    home: &Path,
    filtered: bool,
) -> String {
    if rows.is_empty() {
        return if filtered {
            "No closed sessions match.\n".to_string()
        } else {
            "No closed sessions found.\n".to_string()
        };
    }
    let cells: Vec<[String; 8]> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            [
                (i + 1).to_string(),
                age(now_ms, r.last_ms),
                r.agent().to_string(),
                cut_left(&one_line(&place(&r.entry.cwd, home)), 30),
                cut(
                    &r.title
                        .as_deref()
                        .map_or_else(|| "(no title)".to_string(), one_line),
                    44,
                ),
                r.ending().as_str().to_string(),
                if r.bypass { "!" } else { "" }.to_string(),
                notes(r),
            ]
        })
        .collect();
    let head = [
        "#", "AGE", "AGENT", "PLACE", "TITLE", "ENDED", "!", "SOURCE",
    ];
    let widths: Vec<usize> = (0..8)
        .map(|c| {
            cells
                .iter()
                .map(|row| row[c].chars().count())
                .chain([head[c].chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |row: &[String]| -> String {
        let mut out = String::new();
        for (c, cell) in row.iter().enumerate() {
            if c + 1 == row.len() {
                out.push_str(cell);
            } else if c == 0 {
                out.push_str(&format!("{cell:>w$}  ", w = widths[c]));
            } else {
                out.push_str(&format!("{cell:<w$}  ", w = widths[c]));
            }
        }
        out.trim_end().to_string() + "\n"
    };
    let mut out = line(&head.map(str::to_string));
    for row in &cells {
        out.push_str(&line(row));
    }
    if total > rows.len() {
        out.push_str(&format!(
            "{} of {total} shown (--limit N shows more)\n",
            rows.len()
        ));
    }
    if rows.iter().any(|r| r.bypass) {
        out.push_str("! restores a permission bypass\n");
    }
    out.push_str(
        "herdr-wake resurrect <number> opens one in a new terminal (--print shows the command)\n",
    );
    out
}

/// The listing as JSON: `{"sessions": [...]}`, in the order of the table.
pub fn list_json(rows: &[&Row], now_ms: u64) -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            serde_json::json!({
                "number": i + 1,
                "agent": r.agent().as_str(),
                "session_id": r.id(),
                "cwd": r.entry.cwd,
                "title": r.title.as_deref().map(one_line),
                "ending": r.ending().as_str(),
                "last_ms": r.last_ms,
                "age": age(now_ms, r.last_ms),
                "flags": r.entry.flags,
                "bypass": r.bypass,
                "flags_from_transcript": r.flags_from_transcript,
                "source": r.source.as_str(),
                "transcript_gone": r.transcript == Transcript::Gone,
            })
        })
        .collect();
    serde_json::json!({ "sessions": sessions })
}

// ------------------------------------------------------- the last listing

#[derive(Serialize, Deserialize)]
struct Last {
    filter: Filter,
    sessions: Vec<(AgentKind, String)>,
}

fn last_file(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    paths::resurrect_dir(env, home).join("last.json")
}

/// Stores the listing just shown, so a number can refer to it.
pub fn remember(
    env: &BTreeMap<String, String>,
    home: &Path,
    filter: &Filter,
    rows: &[&Row],
) -> std::io::Result<()> {
    let last = Last {
        filter: filter.clone(),
        sessions: rows
            .iter()
            .map(|r| (r.agent(), r.id().to_string()))
            .collect(),
    };
    let body = serde_json::to_vec(&last).map_err(std::io::Error::other)?;
    crate::private::write(&last_file(env, home), &body)
}

/// Picks the row a request names, among `rows` (open ones included):
/// `target` is a number of the last listing, or a session id. A number
/// refers to the last listing; when this request sets filters of its own
/// (`filter`), they must be the ones that listing used.
pub fn resolve<'a>(
    rows: &'a [Row],
    target: &str,
    filter: &Filter,
    env: &BTreeMap<String, String>,
    home: &Path,
) -> Result<&'a Row, String> {
    if let Ok(n) = target.parse::<usize>() {
        let last: Last = std::fs::read(last_file(env, home))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or("there is no earlier listing: run `herdr-wake resurrect` first, then use a number from it")?;
        if !filter.is_empty()
            && (filter.scope != last.filter.scope || filter.agent != last.filter.agent)
        {
            return Err(
                "the last listing was made with other filters, so its numbers do not apply: list again with these filters first"
                    .to_string(),
            );
        }
        let (agent, id) = n
            .checked_sub(1)
            .and_then(|i| last.sessions.get(i))
            .ok_or_else(|| format!("the last listing has no number {n}"))?;
        return rows
            .iter()
            .find(|r| r.agent() == *agent && r.id() == id)
            .ok_or_else(|| {
                format!(
                    "session {} is no longer known: list again",
                    trust::printable(id)
                )
            });
    }
    resolve_id(rows, target, filter)
}

/// The row of the session whose id is `id` (`--id` names an id that is all
/// digits, which `resolve` would read as a number).
pub fn resolve_id<'a>(rows: &'a [Row], id: &str, filter: &Filter) -> Result<&'a Row, String> {
    let target = id;
    let matches: Vec<&Row> = rows
        .iter()
        .filter(|r| r.id() == target && filter.agent.is_none_or(|a| a == r.agent()))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "no session with the id {} (`herdr-wake resurrect` lists them)",
            trust::printable(target)
        )),
        _ => Err("that id belongs to sessions of more than one agent: add --agent".to_string()),
    }
}

// --------------------------------------------------------------- launching

/// A checked launch of a session.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The entry the launch was made from (program found).
    pub entry: Entry,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub before: Vec<Vec<String>>,
}

/// The agent-folder variables a printed command must carry when they are
/// not what the person's own shell has.
const FOLDER_VARS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "PI_CODING_AGENT_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
    "OPENCODE_CONFIG_DIR",
    "OPENCODE_DB",
];

impl Launch {
    /// `cd <folder> && [VAR=value ...] <command>`, shell-quoted. Agent
    /// folder variables are included only when they differ from `current`.
    pub fn command(&self, current: &BTreeMap<String, String>) -> String {
        let vars: Vec<String> = FOLDER_VARS
            .iter()
            .filter_map(|k| {
                let v = self.env.get(*k)?;
                (current.get(*k) != Some(v)).then(|| format!("{k}={}", wake::shell_quote(v)))
            })
            .collect();
        let plain = wake::manual_command(&self.cwd, &self.argv);
        match plain.split_once(" && ") {
            Some((cd, cmd)) if !vars.is_empty() => format!("{cd} && {} {cmd}", vars.join(" ")),
            _ => plain,
        }
    }
}

/// Checks that `row` can be brought back and builds the command, through the
/// same checks as a wake (`wake::launch_decision`). `fork` is for Claude
/// only. `Err` is one sentence for the person.
pub fn prepare(row: &Row, ctx: &mut Ctx, fork: bool) -> Result<Launch, String> {
    let agent_kind = row.agent();
    if fork && agent_kind != AgentKind::Claude {
        return Err(format!(
            "--fork works only for Claude Code sessions, not {agent_kind}"
        ));
    }
    if !trust::valid_session_id(row.id()) {
        return Err("the session id is not valid".to_string());
    }
    if row.open && !(fork && agent_kind == AgentKind::Claude) {
        return Err(if agent_kind == AgentKind::Claude {
            "that session is running now; --fork resumes it as a copy".to_string()
        } else {
            "that session is running now; it cannot be brought back twice".to_string()
        });
    }
    let mut entry = row.entry.clone();
    if row.source == Source::Transcript {
        // Nothing recorded the program or the environment: take them from
        // this shell, which is where the person is.
        let program = crate::config::program_for(ctx.cfg, agent_kind.as_str(), ctx.home);
        entry.bin = match (program, agents::on_path(agent_kind.as_str(), ctx.env)) {
            (Some(p), _) => p,
            (None, Some(found)) => found,
            (None, None) => {
                return Err(format!(
                    "cannot find {agent_kind} on PATH, so there is nothing to run"
                ))
            }
        };
        let mut env = record::scrub_env(ctx.env);
        env.extend(entry.env.clone());
        entry.env = env;
    }
    let rec = entry.to_record();
    let adapter = agents::get(agent_kind);
    match wake::launch_decision(&rec, Some(&*adapter), ctx, false) {
        Decision::Resume(Plan::Run {
            mut argv,
            env,
            cwd,
            before,
            ..
        }) => {
            if fork {
                argv.push("--fork-session".to_string());
            }
            Ok(Launch {
                entry,
                argv,
                env,
                cwd,
                before,
            })
        }
        Decision::GiveUp { reason, .. } => Err(format!("cannot bring it back: {reason}")),
        _ => Err("cannot bring it back".to_string()),
    }
}

// ------------------------------------------------------------------ tickets

const TICKET_VERSION: u32 = 1;
/// A ticket older than this is not used.
const TICKET_TTL_MS: u64 = 10 * 60 * 1000;

#[derive(Serialize, Deserialize)]
struct Ticket {
    v: u32,
    created_ms: u64,
    fork: bool,
    entry: Entry,
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn ticket_file(env: &BTreeMap<String, String>, home: &Path, key: &str) -> PathBuf {
    paths::resurrect_dir(env, home).join(format!("run-{key}.json"))
}

/// Writes the ticket `--run <key>` will read, and removes tickets that are
/// long out of date. Returns the key.
pub fn write_ticket(
    env: &BTreeMap<String, String>,
    home: &Path,
    now_ms: u64,
    launch: &Launch,
    fork: bool,
) -> std::io::Result<String> {
    let key = format!("{}-{now_ms}", std::process::id());
    let dir = paths::resurrect_dir(env, home);
    if let Ok(files) = std::fs::read_dir(&dir) {
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            let stale = f
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_millis() > u128::from(TICKET_TTL_MS) * 6);
            if name.starts_with("run-") && stale {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
    let ticket = Ticket {
        v: TICKET_VERSION,
        created_ms: now_ms,
        fork,
        entry: launch.entry.clone(),
    };
    let body = serde_json::to_vec(&ticket).map_err(std::io::Error::other)?;
    crate::private::write(&ticket_file(env, home, &key), &body)?;
    Ok(key)
}

/// Reads and removes the ticket for `key`: a plain file of ours, not too old.
fn take_ticket(
    env: &BTreeMap<String, String>,
    home: &Path,
    now_ms: u64,
    key: &str,
) -> Result<Ticket, String> {
    if !valid_key(key) {
        return Err("the launch key is not valid".to_string());
    }
    let file = ticket_file(env, home, key);
    let md = std::fs::symlink_metadata(&file)
        .map_err(|_| "there is no launch ticket for that key (it is used once)".to_string())?;
    if !md.is_file() || md.len() > 1024 * 1024 {
        return Err("the launch ticket is not a plain file".to_string());
    }
    let bytes = std::fs::read(&file).map_err(|e| format!("cannot read the launch ticket: {e}"))?;
    let _ = std::fs::remove_file(&file);
    let ticket: Ticket = serde_json::from_slice(&bytes)
        .map_err(|_| "the launch ticket cannot be read".to_string())?;
    if ticket.v != TICKET_VERSION {
        return Err("the launch ticket is from another version".to_string());
    }
    if now_ms.saturating_sub(ticket.created_ms) > TICKET_TTL_MS {
        return Err("the launch ticket is out of date".to_string());
    }
    Ok(ticket)
}

// ------------------------------------------------- the new terminal's side

/// What `herdr-wake resurrect --run <key>` does, inside the terminal `hc`
/// opened: checks the ticket's session again, runs the agent in the
/// foreground, then replaces itself with the real shell in the session's
/// folder. Any problem is one line on the terminal (the reason and the
/// command to try by hand) and then the shell. Returns only when the final
/// exec failed.
pub fn run_ticket(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    home: &Path,
    key: &str,
    say: &dyn Fn(&str),
) -> std::io::Error {
    let cfg = match Config::load(&paths::config_file(env, home)) {
        Ok(cfg) => cfg,
        Err(e) => {
            say(&format!(
                "herdr-wake: cannot bring the session back: config.toml cannot be parsed: {e:#}"
            ));
            return wake::exec_shell(platform, env, home, &Config::default(), None);
        }
    };
    let ticket = match take_ticket(env, home, platform.now_ms(), key) {
        Ok(t) => t,
        Err(why) => {
            say(&format!("herdr-wake: cannot bring the session back: {why}"));
            return wake::exec_shell(platform, env, home, &cfg, None);
        }
    };
    let folder = ticket.entry.cwd.clone();
    let row = Row {
        title: None,
        last_ms: 0,
        source: Source::Wake,
        transcript: Transcript::Present,
        flags_from_transcript: false,
        bypass: false,
        open: false,
        entry: ticket.entry,
    };
    let mut core = crate::report::NoCore;
    let launch = {
        let mut ctx = Ctx {
            platform,
            core: &mut core,
            env,
            home,
            cfg: &cfg,
        };
        prepare(&row, &mut ctx, ticket.fork)
    };
    match launch {
        Err(why) => {
            let rec = row.entry.to_record();
            say(&wake::give_up_line(&rec, &why, None));
        }
        Ok(launch) => {
            let mut plan_env = launch.env.clone();
            // The agent's own hook records it and the watcher notices when
            // it is quit, as for any session a person starts.
            plan_env.remove(wake::SUPERVISED_ENV);
            let child_env = wake::launch_env(platform, env, &cfg, plan_env);
            for cmd in &launch.before {
                wake::run_before(cmd, &child_env, &launch.cwd);
            }
            if let Err(e) = platform.run_foreground(&launch.argv, &child_env, &launch.cwd) {
                let rec = row.entry.to_record();
                say(&wake::give_up_line(
                    &rec,
                    &format!("it could not start: {e}"),
                    Some(&launch.command(env)),
                ));
            }
        }
    }
    wake::exec_shell(platform, env, home, &cfg, Some(&folder))
}

/// The real `herdr-wake resurrect --run`: never returns normally.
pub fn run_entry(platform: &dyn Platform, env: BTreeMap<String, String>, key: &str) -> ! {
    let home = env
        .get("HOME")
        .filter(|h| !h.is_empty())
        .map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let say = |line: &str| {
        let _ = writeln!(std::io::stderr(), "{line}");
    };
    let err = run_ticket(platform, &env, &home, key, &say);
    let _ = writeln!(std::io::stderr(), "herdr-wake: cannot start a shell: {err}");
    std::process::exit(1)
}

// ------------------------------------------------------ opening a terminal

/// The two outside programs resurrect asks for help, for tests to replace.
pub trait Tools {
    /// The output of `herdr-app ls`, when it works (the desktop app is open).
    fn herdr_app_ls(&self) -> Option<String>;
    /// Runs `hc` with `args`. `Ok` is what it printed, `Err` why it failed.
    fn hc(&self, args: &[String]) -> Result<String, String>;
}

/// `Tools` on the real machine: `herdr-app` and `hc` as found on `PATH`.
pub struct SystemTools {
    pub env: BTreeMap<String, String>,
}

impl Tools for SystemTools {
    fn herdr_app_ls(&self) -> Option<String> {
        let app = agents::on_path("herdr-app", &self.env)?;
        let app = app.to_string_lossy().into_owned();
        let out = crate::report::Probes::run(
            &crate::report::SystemProbes,
            &[app.as_str(), "ls"],
            std::time::Duration::from_secs(3),
        )?;
        (out.code == Some(0)).then_some(out.stdout)
    }

    fn hc(&self, args: &[String]) -> Result<String, String> {
        let hc = agents::on_path("hc", &self.env)
            .ok_or("cannot find hc on PATH (herdr-core's command)")?;
        let out = std::process::Command::new(hc)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("cannot run hc: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            Err(format!("hc failed: {}", trust::printable(first)))
        }
    }
}

/// The space the new terminal belongs to, by Herdr's `herdr.space` label.
/// From inside a Herdr terminal (`HC_TERMINAL_ID`): that terminal's own
/// space. Otherwise, when the desktop app is open (`herdr-app ls`): the local
/// space whose folder, or whose terminals' folders, hold `folder`, the
/// deepest such folder winning and the active space winning a tie. A folder
/// of `/` or of the home folder holds only itself.
pub fn find_space(
    tools: &dyn Tools,
    core: &mut dyn Core,
    env: &BTreeMap<String, String>,
    home: &Path,
    folder: &Path,
) -> Option<String> {
    if let Some(id) = env.get("HC_TERMINAL_ID").filter(|v| !v.is_empty()) {
        if let Ok(t) = core.get(&TerminalId::from(id.as_str())) {
            if let Some(space) = t.labels.get("herdr.space").filter(|s| !s.is_empty()) {
                return Some(space.clone());
            }
        }
    }
    let ls: serde_json::Value = serde_json::from_str(&tools.herdr_app_ls()?).ok()?;
    let cwds: BTreeMap<String, PathBuf> = core
        .list(false)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| Some((t.address.as_str().to_string(), t.cwd?)))
        .collect();
    let want = folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf());
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let holds = |c: &Path| {
        let c = c.canonicalize().unwrap_or_else(|_| c.to_path_buf());
        if c == Path::new("/") || c == home {
            return want == c;
        }
        want.starts_with(&c)
    };
    let mut best: Option<(usize, bool, String)> = None;
    for space in ls.get("spaces")?.as_array()? {
        if !space.get("machine").is_none_or(serde_json::Value::is_null) {
            continue;
        }
        let Some(id) = space.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let active = space.get("active").and_then(|v| v.as_bool()) == Some(true);
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(f) = space.get("folder").and_then(|v| v.as_str()) {
            candidates.push(PathBuf::from(f));
        }
        for tab in space
            .get("tabs")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            for term in tab
                .get("terminals")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(c) = term
                    .get("terminal")
                    .and_then(|v| v.as_str())
                    .and_then(|t| cwds.get(t))
                {
                    candidates.push(c.clone());
                }
            }
        }
        for c in candidates.iter().filter(|c| holds(c)) {
            let depth = c.components().count();
            if best
                .as_ref()
                .is_none_or(|(d, a, _)| (depth, active) > (*d, *a))
            {
                best = Some((depth, active, id.to_string()));
            }
        }
    }
    best.map(|(_, _, id)| id)
}

/// The `hc run` arguments that open the terminal for ticket `key`.
pub fn hc_run_args(folder: &Path, space: Option<&str>, exe: &Path, key: &str) -> Vec<String> {
    let mut args: Vec<String> = ["run", "--bg", "--no-parent", "--keep", "--cwd"]
        .map(str::to_string)
        .to_vec();
    args.push(folder.to_string_lossy().into_owned());
    if let Some(space) = space {
        args.push("--label".to_string());
        args.push(format!("herdr.space={space}"));
    }
    args.push("--".to_string());
    args.push(exe.to_string_lossy().into_owned());
    args.extend(["resurrect", "--run", key].map(str::to_string));
    args
}

/// What opening the terminal did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// What `hc run` printed (the new terminal), one line.
    pub terminal: String,
    pub space: Option<String>,
}

/// Opens a new herdr-core terminal in the session's folder that runs
/// `herdr-wake resurrect --run <key>`.
pub fn open_terminal(
    tools: &dyn Tools,
    core: &mut dyn Core,
    env: &BTreeMap<String, String>,
    home: &Path,
    folder: &Path,
    exe: &Path,
    key: &str,
) -> Result<Opened, String> {
    let space = find_space(tools, core, env, home, folder);
    let args = hc_run_args(folder, space.as_deref(), exe, key);
    let out = tools.hc(&args)?;
    let terminal = out
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(trust::printable)
        .unwrap_or_default();
    Ok(Opened { terminal, space })
}
