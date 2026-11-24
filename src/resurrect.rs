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
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agents::{self, Ctx, Plan};
use crate::config::Config;
use crate::core::{Core, TerminalId};
use crate::facts::{self, Extra};
use crate::history::{self, Ending, Entry};
use crate::platform::Platform;
use crate::record::{self, AgentKind, State};
use crate::transcripts::{self, Cache};
use crate::wake::{self, Decision};
use crate::{opencode_store, paths, trust};

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

impl Transcript {
    pub fn as_str(self) -> &'static str {
        match self {
            Transcript::Present => "present",
            Transcript::Gone => "gone",
            Transcript::Unknown => "unknown",
        }
    }
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
    /// When the agent's own session file was last written, in
    /// milliseconds; 0 when there is none to look at.
    pub transcript_mtime_ms: u64,
    /// What a full read of the session file found (ending, cost, branch,
    /// models, pull requests, a saved summary) and where the file is.
    pub extra: Extra,
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
    /// Codex thread ids whose rollout file a running Codex holds open.
    codex_threads: BTreeSet<String>,
    /// Whether herdr-core answered.
    pub core_up: bool,
}

/// A session whose own file was written this recently counts as running:
/// an agent writes its transcript as it goes, and nothing here can see an
/// agent that runs outside Herdr.
pub const FRESH_MS: u64 = 120_000;

/// True when a session file last written at `mtime_ms` (0: unknown) counts
/// as being written now, at `now_ms`.
pub fn is_fresh(now_ms: u64, mtime_ms: u64) -> bool {
    mtime_ms > 0 && now_ms.saturating_sub(mtime_ms) < FRESH_MS
}

impl OpenSet {
    /// True when the session is among the running ones: named by a
    /// running record or session file, held open by a running Codex, or
    /// named by the command line of a running agent of the same kind (a
    /// word equal to the id, or for ids of 16 characters or more, containing
    /// it: Pi names a file after its id).
    pub fn contains(&self, agent: AgentKind, id: &str) -> bool {
        self.ids.contains(&(agent, id.to_string()))
            || (agent == AgentKind::Codex && self.codex_threads.contains(id))
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
/// agents in their foreground (only when core answers), Claude's own
/// session files whose process is alive and is claude, and the rollout
/// files a running Codex has open.
pub fn open_sessions(ctx: &mut Ctx, tools: &dyn Tools, claude_dirs: &BTreeSet<PathBuf>) -> OpenSet {
    let mut open = OpenSet::default();
    for lock in live_run_locks(ctx.platform, ctx.env, ctx.home) {
        open.ids.insert((lock.agent, lock.id));
    }
    for file in tools.codex_open_files() {
        if let Some(id) = file
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(transcripts::rollout_thread)
        {
            open.codex_threads.insert(id.to_string());
        }
    }
    if let Ok(terminals) = ctx.core.list(false) {
        open.core_up = true;
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

/// What `collect` found.
pub struct Collected {
    /// Every session that could be listed, open ones flagged, newest first.
    pub rows: Vec<Row>,
    /// Whether herdr-core answered (without it, running sessions are found
    /// less well).
    pub core_up: bool,
}

/// When `path`, a file, was last written, in milliseconds; 0 when it
/// cannot be seen.
fn file_mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Every session known to herdr-wake that could be listed (open ones
/// flagged), newest first: the journal, then transcripts the journal lacks.
pub fn collect(ctx: &mut Ctx, tools: &dyn Tools) -> Collected {
    let mut cache = Cache::load(&paths::transcript_cache_file(ctx.env, ctx.home));
    let journal = history::read_all(&paths::history_dir(ctx.env, ctx.home));
    let mut rows: Vec<Row> = Vec::new();
    let mut seen: BTreeSet<(AgentKind, String)> = BTreeSet::new();
    let mut names_by_dir: BTreeMap<PathBuf, BTreeMap<String, String>> = BTreeMap::new();

    let mut claude_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut codex_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    claude_dirs.insert(transcripts::claude_dir(ctx.env, ctx.home));
    codex_dirs.insert(transcripts::codex_dir(ctx.env, ctx.home));
    let mut pi_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    pi_dirs.insert(transcripts::pi_dir(ctx.env, ctx.home));

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
            AgentKind::Pi => {
                pi_dirs.extend(agents::pi::session_dirs(&e.to_record(), ctx.env, ctx.home));
            }
            AgentKind::OpenCode => {}
        }
        let transcript_mtime_ms = path.as_deref().map_or(0, file_mtime_ms);
        let extra = Extra {
            facts: found
                .as_ref()
                .map(|f| f.meta.facts.clone())
                .unwrap_or_default(),
            path: path.clone(),
        };
        rows.push(Row {
            extra,
            transcript_mtime_ms,
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
    let default_pi = transcripts::pi_dir(ctx.env, ctx.home);
    for dir in &pi_dirs {
        for f in transcripts::scan_pi(dir, &mut cache) {
            let key = (f.meta.agent, f.meta.session_id.clone());
            let Some(cwd) = f.meta.cwd.clone() else {
                continue;
            };
            if !seen.insert(key) {
                continue;
            }
            let mut env = BTreeMap::new();
            if *dir != default_pi {
                env.insert(
                    "PI_CODING_AGENT_SESSION_DIR".to_string(),
                    dir.to_string_lossy().into_owned(),
                );
            }
            rows.push(transcript_row(&f, cwd, Vec::new(), env, ctx.cfg));
        }
    }
    // OpenCode keeps its sessions in a database, read through `sqlite3`
    // when there is one.
    let db = opencode_store::db_path(ctx.env, ctx.home);
    let sql = |q: &str| tools.sqlite(&db, q);
    for f in opencode_store::scan(&db, &sql, &mut cache) {
        let key = (f.meta.agent, f.meta.session_id.clone());
        let Some(cwd) = f.meta.cwd.clone() else {
            continue;
        };
        if !seen.insert(key) {
            continue;
        }
        rows.push(transcript_row(
            &f,
            cwd,
            Vec::new(),
            BTreeMap::new(),
            ctx.cfg,
        ));
    }
    let _ = cache.save(&paths::transcript_cache_file(ctx.env, ctx.home));

    let open = open_sessions(ctx, tools, &claude_dirs);
    let now = ctx.platform.now_ms();
    for r in &mut rows {
        r.open = open.contains(r.agent(), r.id()) || is_fresh(now, r.transcript_mtime_ms);
    }
    crate::summary::attach_cached(&mut rows, ctx.env, ctx.home);
    rows.sort_by(|a, b| {
        b.last_ms
            .cmp(&a.last_ms)
            .then_with(|| a.entry.session_id.cmp(&b.entry.session_id))
    });
    Collected {
        rows,
        core_up: open.core_up,
    }
}

fn transcript_row(
    f: &transcripts::Found,
    cwd: PathBuf,
    flags: Vec<String>,
    env: BTreeMap<String, String>,
    cfg: &Config,
) -> Row {
    let agent = f.meta.agent;
    // OpenCode's `path` is its database, not a session file to resume from.
    let own_file = agent != AgentKind::OpenCode;
    Row {
        extra: Extra {
            facts: f.meta.facts.clone(),
            path: own_file.then(|| f.path.clone()),
        },
        bypass: has_bypass(agent, &flags, cfg),
        entry: Entry {
            v: history::VERSION,
            agent,
            session_id: f.meta.session_id.clone(),
            session_file: own_file.then(|| f.path.clone()),
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
        transcript_mtime_ms: f.mtime_ms,
        source: Source::Transcript,
        transcript: if own_file {
            Transcript::Present
        } else {
            Transcript::Unknown
        },
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
                facts::ended_label(r.ending(), r.extra.facts.conversation),
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
                "flags": display_words(&r.entry.flags, &[]),
                "bypass": r.bypass,
                "flags_from_transcript": r.flags_from_transcript,
                "source": r.source.as_str(),
                "transcript_gone": r.transcript == Transcript::Gone,
                "transcript": r.transcript.as_str(),
                "folder_exists": r.entry.cwd.is_dir(),
                "ended_ms": if r.source == Source::Wake { r.entry.ended_ms } else { None },
                "conversation": r.extra.facts.conversation.map(facts::Conversation::as_str),
                "branch": r.extra.facts.branch.as_deref().map(trust::printable),
                "cost_usd": r.extra.facts.cost_usd,
                "duration_ms": r.extra.facts.duration_ms,
                "started_ms": r.extra.facts.started_ms,
                "lines_added": r.extra.facts.lines_added,
                "lines_removed": r.extra.facts.lines_removed,
                "models": r.extra.facts.models.iter().map(|m| trust::printable(m)).collect::<Vec<_>>(),
                "prs": r.extra.facts.prs.iter().map(|p| serde_json::json!({
                    "number": p.number,
                    "url": trust::printable(&p.url),
                    "repo": p.repo.as_deref().map(trust::printable),
                })).collect::<Vec<_>>(),
                "summary": r.extra.facts.summary.as_deref().map(one_line),
                "summary_source": r.extra.facts.summary_source,
            })
        })
        .collect();
    serde_json::json!({ "sessions": sessions })
}

// ------------------------------------------------------- the last listing

/// How long a listing stays valid for numbers.
const LAST_TTL_MS: u64 = 60 * 60 * 1000;

#[derive(Serialize, Deserialize)]
struct Last {
    created_ms: u64,
    filter: Filter,
    sessions: Vec<(AgentKind, String)>,
}

/// The terminal this process is attached to (`/dev/ttys003`), if any.
fn current_tty() -> Option<String> {
    // SAFETY: ttyname returns null or a NUL-terminated string that stays
    // valid until the next call; it is copied out at once.
    unsafe {
        let p = libc::ttyname(0);
        (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
    }
}

/// The file that holds the last listing of this person's terminal: keyed by
/// the herdr-core terminal (`HC_TERMINAL_ID`), else the tty, else one file
/// for everything else, so two terminals never number each other's rows.
fn last_file(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    let name = match env.get("HC_TERMINAL_ID").filter(|v| !v.is_empty()) {
        Some(t) => format!("last-hc-{}.json", record::plain_name(t)),
        None => match current_tty() {
            Some(tty) => format!("last-tty-{}.json", record::plain_name(&tty)),
            None => "last.json".to_string(),
        },
    };
    paths::resurrect_dir(env, home).join(name)
}

/// Stores the listing just shown, so a number can refer to it, and removes
/// listings of other terminals that are a day old.
pub fn remember(
    env: &BTreeMap<String, String>,
    home: &Path,
    now_ms: u64,
    filter: &Filter,
    rows: &[&Row],
) -> std::io::Result<()> {
    if let Ok(files) = std::fs::read_dir(paths::resurrect_dir(env, home)) {
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            let old = f
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_millis() > u128::from(LAST_TTL_MS) * 24);
            if name.starts_with("last") && old {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
    let last = Last {
        created_ms: now_ms,
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
/// refers to the last listing of this terminal, for an hour; when this
/// request sets filters of its own (`filter`), they must be the ones that
/// listing used.
pub fn resolve<'a>(
    rows: &'a [Row],
    target: &str,
    filter: &Filter,
    env: &BTreeMap<String, String>,
    home: &Path,
    now_ms: u64,
) -> Result<&'a Row, String> {
    if let Ok(n) = target.parse::<usize>() {
        let last: Last = std::fs::read(last_file(env, home))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or("there is no earlier listing: run `herdr-wake resurrect` first, then use a number from it")?;
        if now_ms.saturating_sub(last.created_ms) > LAST_TTL_MS {
            return Err(
                "the last listing is more than an hour old, so its numbers may be out of date: run `herdr-wake resurrect` again"
                    .to_string(),
            );
        }
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

/// The values of these flags may be shown: they name a fixed set of modes
/// or a model, not something a person typed in free form.
const SHOWN_VALUES: &[&str] = &[
    "--permission-mode",
    "--model",
    "-s",
    "--sandbox",
    "-a",
    "--ask-for-approval",
];

/// `words` (flags, with their values and perhaps positional words) as they
/// may be shown: flag names only, except the values of `SHOWN_VALUES`. A
/// word that is not a flag or the value of one of those is left out unless
/// it is in `keep` (a subcommand, the session id).
pub fn display_words(words: &[String], keep: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut value_next = false;
    for w in words {
        if std::mem::take(&mut value_next) && !w.starts_with('-') {
            out.push(w.clone());
            continue;
        }
        if let Some(long) = w.strip_prefix("--") {
            match long.split_once('=') {
                Some((name, _)) if SHOWN_VALUES.contains(&format!("--{name}").as_str()) => {
                    out.push(w.clone());
                }
                Some((name, _)) => out.push(format!("--{name}")),
                None => {
                    value_next = SHOWN_VALUES.contains(&w.as_str());
                    out.push(w.clone());
                }
            }
        } else if w.starts_with('-') && w.len() > 1 {
            let name: String = w.chars().take(2).collect();
            if w.chars().count() > 2 {
                // A short flag with its value attached.
                out.push(if SHOWN_VALUES.contains(&name.as_str()) {
                    w.clone()
                } else {
                    name
                });
            } else {
                value_next = SHOWN_VALUES.contains(&w.as_str());
                out.push(w.clone());
            }
        } else if keep.contains(&w.as_str()) {
            out.push(w.clone());
        }
    }
    out
}

impl Launch {
    /// The agent-folder variables the command must carry: the ones that
    /// differ from `current`, the caller's own environment.
    fn vars<'a>(&'a self, current: &BTreeMap<String, String>) -> Vec<(&'static str, &'a str)> {
        FOLDER_VARS
            .iter()
            .filter_map(|k| {
                let v = self.env.get(*k)?;
                (current.get(*k) != Some(v)).then_some((*k, v.as_str()))
            })
            .collect()
    }

    /// `cd <folder> && [VAR=value ...] <command>`, shell-quoted, built from
    /// its parts. Agent folder variables are included only when they differ
    /// from `current`. This is the exact command, flag values included: for
    /// `--print` and the retry line, where it has to run.
    pub fn command(&self, current: &BTreeMap<String, String>) -> String {
        wake::command_line(&self.cwd, &self.vars(current), &self.argv)
    }

    /// The same command for showing: flag names only, except the values in
    /// `SHOWN_VALUES` (see `display_words`).
    pub fn display_command(&self, current: &BTreeMap<String, String>) -> String {
        let mut argv: Vec<String> = self.argv.first().cloned().into_iter().collect();
        let keep = [self.entry.session_id.as_str(), "resume"];
        argv.extend(display_words(self.argv.get(1..).unwrap_or_default(), &keep));
        wake::command_line(&self.cwd, &self.vars(current), &argv)
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
            let launch = Launch {
                entry,
                argv,
                env,
                cwd,
                before,
            };
            // What cannot be shown as it is cannot be checked by eye, so
            // it is neither printed nor run.
            let command = launch.command(ctx.env);
            if trust::printable(&command) != command {
                return Err(
                    "cannot bring it back: its folder or a flag holds control or direction-changing characters, so the command cannot be shown safely"
                        .to_string(),
                );
            }
            Ok(launch)
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

/// The marker that a launch of this session is pending: one file per
/// agent and session id, made with `create_new` so two launches racing for
/// the same session have one winner. It holds the ticket's key.
fn pending_file(
    env: &BTreeMap<String, String>,
    home: &Path,
    agent: AgentKind,
    id: &str,
) -> PathBuf {
    paths::resurrect_dir(env, home).join(format!("pending-{agent}-{}", record::plain_name(id)))
}

/// Writes the ticket `--run <key>` will read, and removes tickets that are
/// long out of date. Returns the key. Fails with `AlreadyExists` while an
/// unexpired ticket for the same session is pending.
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
            let ours = ["run-", "claimed-", "pending-"]
                .iter()
                .any(|p| name.starts_with(p));
            if ours && stale {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
    let e = &launch.entry;
    let pending = pending_file(env, home, e.agent, &e.session_id);
    let mut made = crate::private::create_new(&pending, key.as_bytes());
    if made
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists)
    {
        // A marker whose terminal never came up lapses with its ticket.
        let lapsed = std::fs::symlink_metadata(&pending)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_millis() > u128::from(TICKET_TTL_MS));
        if lapsed {
            let _ = std::fs::remove_file(&pending);
            made = crate::private::create_new(&pending, key.as_bytes());
        }
    }
    made.map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a launch of this session is already pending: wait for its terminal, or for \
                 the ticket to lapse (10 minutes)",
            )
        } else {
            err
        }
    })?;
    let ticket = Ticket {
        v: TICKET_VERSION,
        created_ms: now_ms,
        fork,
        entry: launch.entry.clone(),
    };
    let written = serde_json::to_vec(&ticket)
        .map_err(std::io::Error::other)
        .and_then(|body| crate::private::write(&ticket_file(env, home, &key), &body));
    if let Err(err) = written {
        let _ = std::fs::remove_file(&pending);
        return Err(err);
    }
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
    // Claim it first: a rename is atomic, so of two processes started with
    // the same key exactly one gets the ticket.
    static CLAIMS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let file = ticket_file(env, home, key);
    let claimed = paths::resurrect_dir(env, home).join(format!(
        "claimed-{}-{}.json",
        std::process::id(),
        CLAIMS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::rename(&file, &claimed)
        .map_err(|_| "there is no launch ticket for that key (it is used once)".to_string())?;
    let read = (|| {
        let md = std::fs::symlink_metadata(&claimed)
            .map_err(|e| format!("cannot read the launch ticket: {e}"))?;
        if !md.is_file() || md.len() > 1024 * 1024 {
            return Err("the launch ticket is not a plain file".to_string());
        }
        std::fs::read(&claimed).map_err(|e| format!("cannot read the launch ticket: {e}"))
    })();
    let _ = std::fs::remove_file(&claimed);
    let ticket: Ticket = serde_json::from_slice(&read?)
        .map_err(|_| "the launch ticket cannot be read".to_string())?;
    // The pending mark stays: it goes once the run lock exists (see
    // `release_pending`), so there is no gap between the two.
    if ticket.v != TICKET_VERSION {
        return Err("the launch ticket is from another version".to_string());
    }
    if now_ms.saturating_sub(ticket.created_ms) > TICKET_TTL_MS {
        return Err("the launch ticket is out of date".to_string());
    }
    Ok(ticket)
}

/// Removes the pending mark of the launch `key` of this session, if it is
/// still that launch's.
fn release_pending(
    env: &BTreeMap<String, String>,
    home: &Path,
    agent: AgentKind,
    id: &str,
    key: &str,
) {
    let pending = pending_file(env, home, agent, id);
    if std::fs::read_to_string(&pending).is_ok_and(|k| k == key) {
        let _ = std::fs::remove_file(&pending);
    }
}

// ----------------------------------------------------------- the run lock

/// What `running-<agent>-<id>` holds: who runs the session.
#[derive(Serialize, Deserialize)]
struct RunLock {
    agent: AgentKind,
    id: String,
    pid: u32,
    /// The process's start stamp (`ProcInfo::start`), so a reused pid is
    /// not mistaken for the run; 0 when it could not be read.
    start: u64,
}

fn run_lock_file(
    env: &BTreeMap<String, String>,
    home: &Path,
    agent: AgentKind,
    id: &str,
) -> PathBuf {
    paths::resurrect_dir(env, home).join(format!("running-{agent}-{}", record::plain_name(id)))
}

fn read_run_lock(path: &Path) -> Option<RunLock> {
    let md = std::fs::symlink_metadata(path).ok()?;
    if !md.is_file() || md.len() > 4096 {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// True when the process of `lock` is running: its pid exists and, when the
/// lock recorded a start stamp, has that start.
fn run_lock_is_live(platform: &dyn Platform, lock: &RunLock) -> bool {
    platform
        .process(lock.pid)
        .is_some_and(|p| lock.start == 0 || p.start == lock.start)
}

/// True when a live `--run` holds this session's run lock.
///
/// The launching side asks this after `write_ticket` has taken the pending
/// mark: a `--run` makes its lock before it lets go of the pending mark, so
/// a launch that got the mark and then finds no live lock is the only one.
pub fn run_is_live(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    home: &Path,
    agent: AgentKind,
    id: &str,
) -> bool {
    read_run_lock(&run_lock_file(env, home, agent, id))
        .is_some_and(|l| run_lock_is_live(platform, &l))
}

/// Undoes `write_ticket` for `key`: removes its ticket and its pending mark.
pub fn abandon_ticket(
    env: &BTreeMap<String, String>,
    home: &Path,
    key: &str,
    agent: AgentKind,
    id: &str,
) {
    if valid_key(key) {
        let _ = std::fs::remove_file(ticket_file(env, home, key));
    }
    release_pending(env, home, agent, id, key);
}

/// The run locks whose process is running.
fn live_run_locks(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    home: &Path,
) -> Vec<RunLock> {
    let Ok(files) = std::fs::read_dir(paths::resurrect_dir(env, home)) else {
        return Vec::new();
    };
    files
        .flatten()
        .filter(|f| f.file_name().to_string_lossy().starts_with("running-"))
        .filter_map(|f| read_run_lock(&f.path()))
        .filter(|l| run_lock_is_live(platform, l))
        .collect()
}

/// A held run lock; the file goes when this is dropped.
struct RunGuard {
    path: PathBuf,
    pid: u32,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // Only our own file: a lock that someone replaced after a crash
        // belongs to them.
        if read_run_lock(&self.path).is_some_and(|l| l.pid == self.pid) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Takes the run lock of the session for this process, exclusively, before
/// the agent starts. `Err` says who holds it. A lock whose process is gone
/// (or whose pid now belongs to another process) is stale: it is renamed
/// away, which only one of several racing processes can do, and replaced.
fn acquire_run_lock(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    home: &Path,
    agent: AgentKind,
    id: &str,
) -> Result<RunGuard, String> {
    static STALE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = run_lock_file(env, home, agent, id);
    let pid = std::process::id();
    let start = platform.process(pid).map_or(0, |p| p.start);
    let body = serde_json::to_vec(&RunLock {
        agent,
        id: id.to_string(),
        pid,
        start,
    })
    .map_err(|e| e.to_string())?;
    for _ in 0..2 {
        match crate::private::create_new(&path, &body) {
            Ok(()) => return Ok(RunGuard { path, pid }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Some(held) = read_run_lock(&path).filter(|l| run_lock_is_live(platform, l)) {
                    return Err(format!(
                        "it is already being brought back by another herdr-wake (process {})",
                        held.pid
                    ));
                }
                let away = path.with_file_name(format!(
                    "stale-{pid}-{}",
                    STALE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ));
                if std::fs::rename(&path, &away).is_ok() {
                    let _ = std::fs::remove_file(&away);
                }
            }
            Err(e) => return Err(format!("cannot take the run lock: {e}")),
        }
    }
    Err("another herdr-wake is taking the run lock of that session".to_string())
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
    core: &mut dyn Core,
    tools: &dyn Tools,
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
    let mut row = Row {
        title: None,
        last_ms: 0,
        source: Source::Wake,
        transcript: Transcript::Present,
        flags_from_transcript: false,
        bypass: false,
        open: false,
        transcript_mtime_ms: 0,
        extra: Extra::default(),
        entry: ticket.entry,
    };
    let launch = {
        let mut ctx = Ctx {
            platform,
            core,
            env,
            home,
            cfg: &cfg,
        };
        // The session may have come up since the ticket was made.
        row.open = is_open_now(&mut ctx, tools, &row.entry);
        prepare(&row, &mut ctx, ticket.fork)
    };
    let (agent, id) = (row.entry.agent, row.entry.session_id.clone());
    match launch {
        Err(why) => {
            let rec = row.entry.to_record();
            say(&wake::give_up_line(&rec, &why, None));
        }
        Ok(launch) => {
            // One run per session: the lock exists before the agent starts
            // and lasts as long as it runs, so a second launch in the gap
            // before the agent registers itself finds it. A fork starts a
            // session of its own and takes no lock.
            let lock = if ticket.fork {
                Ok(None)
            } else {
                acquire_run_lock(platform, env, home, agent, &id).map(Some)
            };
            // The lock (or the refusal) is the handover: the pending mark
            // is no longer needed.
            release_pending(env, home, agent, &id, key);
            match lock {
                Err(why) => {
                    let rec = row.entry.to_record();
                    say(&wake::give_up_line(&rec, &why, None));
                }
                Ok(lock) => {
                    let mut plan_env = launch.env.clone();
                    // The agent's own hook records it and the watcher notices
                    // when it is quit, as for any session a person starts.
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
                    // Gone before the shell takes this process over.
                    drop(lock);
                }
            }
        }
    }
    // Every way out of the launch ends the pending mark.
    release_pending(env, home, agent, &id, key);
    wake::exec_shell(platform, env, home, &cfg, Some(&folder))
}

/// True when the session of `entry` is running now, by every sign
/// `collect` looks for.
fn is_open_now(ctx: &mut Ctx, tools: &dyn Tools, entry: &Entry) -> bool {
    let mut claude_dirs = BTreeSet::new();
    claude_dirs.insert(transcripts::claude_dir(ctx.env, ctx.home));
    claude_dirs.insert(entry_claude_dir(entry, ctx.home));
    let open = open_sessions(ctx, tools, &claude_dirs);
    let (path, _) = entry_transcript(entry, ctx.env, ctx.home);
    let mtime = path.as_deref().map_or(0, file_mtime_ms);
    open.contains(entry.agent, &entry.session_id) || is_fresh(ctx.platform.now_ms(), mtime)
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
    let mut core: Box<dyn Core> = match crate::core::socket::connect(
        &crate::core::socket::socket_path(&env, &home),
        Duration::from_secs(2),
    ) {
        Ok(c) => Box::new(c),
        Err(_) => Box::new(crate::report::NoCore),
    };
    let tools = SystemTools::new(env.clone());
    let err = run_ticket(platform, &env, &home, key, core.as_mut(), &tools, &say);
    let _ = writeln!(std::io::stderr(), "herdr-wake: cannot start a shell: {err}");
    std::process::exit(1)
}

// ------------------------------------------------------ opening a terminal

/// The outside programs resurrect asks for help, for tests to replace.
pub trait Tools {
    /// The output of `herdr-app ls`, when it works (the desktop app is open).
    fn herdr_app_ls(&self) -> Option<String>;
    /// Runs `hc` with `args`. `Ok` is what it printed, `Err` why it failed.
    fn hc(&self, args: &[String]) -> Result<String, String>;
    /// The rollout files that a running Codex has open.
    fn codex_open_files(&self) -> Vec<PathBuf>;
    /// Runs `herdr-app do <args>` (the desktop app is open). `Err` says why not.
    fn herdr_app_do(&self, _args: &[String]) -> Result<String, String> {
        Err("herdr-app is not available".to_string())
    }
    /// Runs a query on OpenCode's database (see `opencode_store`). `Err`
    /// says why it could not (no `sqlite3`, no database). The default
    /// reads nothing.
    fn sqlite(&self, _db: &Path, _query: &str) -> Result<String, String> {
        Err("OpenCode's database is not read here".to_string())
    }
}

/// How long `hc run` may take.
const HC_TIMEOUT: Duration = Duration::from_secs(10);
/// How long `herdr-app ls` may take.
const HERDR_APP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `lsof` may take.
const LSOF_TIMEOUT: Duration = Duration::from_secs(5);

/// What a finished command printed.
struct Captured {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs `program` with `args` and stdin closed, at most `timeout`: the
/// process is killed after that, and `None` comes back.
fn run_capture(program: &Path, args: &[String], timeout: Duration) -> Option<Captured> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut out).take(16 * 1024 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut err).take(1024 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    Some(Captured {
        code: status.code(),
        stdout: String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned(),
    })
}

/// `Tools` on the real machine.
pub struct SystemTools {
    pub env: BTreeMap<String, String>,
    /// How long `hc run` may take.
    pub hc_timeout: Duration,
    /// How long `herdr-app ls` may take.
    pub app_timeout: Duration,
}

impl SystemTools {
    /// The tools of the environment `env`, with the usual time limits.
    pub fn new(env: BTreeMap<String, String>) -> SystemTools {
        SystemTools {
            env,
            hc_timeout: HC_TIMEOUT,
            app_timeout: HERDR_APP_TIMEOUT,
        }
    }

    /// `name` as found on `PATH`, when it passes the same check a
    /// configured program does: a regular executable file that you or root
    /// own and that group and others cannot write.
    fn program(&self, name: &str) -> Result<PathBuf, String> {
        let found = agents::on_path(name, &self.env)
            .ok_or_else(|| format!("cannot find {name} on PATH"))?;
        trust::configured_program_is_safe(&found, trust::current_uid())
            .map_err(|why| format!("{name} at {} is not safe to run: {why}", found.display()))?;
        Ok(found)
    }
}

impl Tools for SystemTools {
    fn herdr_app_ls(&self) -> Option<String> {
        let app = self.program("herdr-app").ok()?;
        let out = run_capture(&app, &["ls".to_string()], self.app_timeout)?;
        (out.code == Some(0)).then_some(out.stdout)
    }

    fn herdr_app_do(&self, args: &[String]) -> Result<String, String> {
        let app = self.program("herdr-app")?;
        let mut argv = vec!["do".to_string()];
        argv.extend(args.iter().cloned());
        let out = run_capture(&app, &argv, self.app_timeout)
            .ok_or_else(|| "herdr-app did not finish in time".to_string())?;
        if out.code == Some(0) {
            Ok(out.stdout)
        } else {
            Err(format!(
                "herdr-app failed: {}",
                trust::printable(out.stderr.lines().next().unwrap_or(""))
            ))
        }
    }

    fn hc(&self, args: &[String]) -> Result<String, String> {
        let hc = self.program("hc")?;
        let out = run_capture(&hc, args, self.hc_timeout)
            .ok_or_else(|| format!("hc did not finish within {} s", self.hc_timeout.as_secs()))?;
        if out.code == Some(0) {
            Ok(out.stdout)
        } else {
            let first = out
                .stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            Err(format!("hc failed: {}", trust::printable(first)))
        }
    }

    fn sqlite(&self, db: &Path, query: &str) -> Result<String, String> {
        opencode_store::run_sqlite3(&self.env, db, query)
    }

    fn codex_open_files(&self) -> Vec<PathBuf> {
        if cfg!(target_os = "linux") {
            return proc_open_rollouts(Path::new("/proc"));
        }
        let lsof = Path::new("/usr/sbin/lsof");
        if trust::configured_program_is_safe(lsof, trust::current_uid()).is_err() {
            return Vec::new();
        }
        let args = ["-w", "-Fn", "-c", "/^codex/i"].map(str::to_string);
        run_capture(lsof, &args, LSOF_TIMEOUT)
            .map(|out| parse_lsof(&out.stdout))
            .unwrap_or_default()
    }
}

/// The rollout files in `lsof -Fn` output: the `n` lines naming a
/// `rollout-*.jsonl` file.
pub fn parse_lsof(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(|l| l.strip_prefix('n'))
        .map(PathBuf::from)
        .filter(|p| is_rollout(p))
        .collect()
}

fn is_rollout(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| transcripts::rollout_thread(n).is_some())
}

/// The rollout files open in the `fd` folders of the Codex processes under
/// `proc_root` (`/proc` on Linux): a process counts when its program, or the
/// script an interpreter runs, is named `codex` in any letter case.
pub fn proc_open_rollouts(proc_root: &Path) -> Vec<PathBuf> {
    let Ok(procs) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let named = |w: &str| {
        Path::new(w)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_ascii_lowercase().starts_with("codex"))
    };
    let mut found = Vec::new();
    for p in procs.flatten() {
        if !p
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let Ok(cmdline) = std::fs::read(p.path().join("cmdline")) else {
            continue;
        };
        let words: Vec<String> = cmdline
            .split(|&b| b == 0)
            .filter(|w| !w.is_empty())
            .map(|w| String::from_utf8_lossy(w).into_owned())
            .collect();
        let is_codex = words.first().is_some_and(|w| named(w))
            || (words
                .first()
                .is_some_and(|w| agents::is_interpreter(Path::new(w)))
                && words.get(1).is_some_and(|w| named(w)));
        if !is_codex {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                if is_rollout(&target) {
                    found.push(target);
                }
            }
        }
    }
    found
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

/// True when `id` can be a Herdr space id for a `herdr.space` label: ASCII
/// letters and digits, `-` and `_`, 1 to 64 characters.
pub fn valid_space_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The space the new terminal joins: the one the caller names (`--space`,
/// from the desktop panel) when there is one, otherwise `find_space`'s guess.
pub fn choose_space(
    explicit: Option<&str>,
    tools: &dyn Tools,
    core: &mut dyn Core,
    env: &BTreeMap<String, String>,
    home: &Path,
    folder: &Path,
) -> Option<String> {
    match explicit {
        Some(id) => Some(id.to_string()),
        None => find_space(tools, core, env, home, folder),
    }
}

/// Opens a new herdr-core terminal in the session's folder that runs
/// `herdr-wake resurrect --run <key>`, in the space `find_space` finds.
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
    open_terminal_in(tools, space, folder, exe, key, None)
}

/// As `open_terminal`, in `space` (labelled `herdr.space`) or in none. With a
/// `title` and a space, the tab the app makes for the terminal is renamed to
/// it (best effort: the app may not be open or may be slow to adopt it).
pub fn open_terminal_in(
    tools: &dyn Tools,
    space: Option<String>,
    folder: &Path,
    exe: &Path,
    key: &str,
    title: Option<&str>,
) -> Result<Opened, String> {
    let args = hc_run_args(folder, space.as_deref(), exe, key);
    let out = tools.hc(&args)?;
    let terminal = out
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(trust::printable)
        .unwrap_or_default();
    if let (Some(title), Some(_)) = (title, space.as_deref()) {
        rename_tab_of(tools, &terminal, title);
    }
    Ok(Opened { terminal, space })
}

/// A tab name from a session title: one line, at most 60 characters.
pub fn tab_name(title: &str) -> Option<String> {
    let name = cut(&one_line(title), 60);
    (!name.is_empty()).then_some(name)
}

/// Names the app's tab that holds `terminal` (the first word of what `hc run`
/// printed) after `title`. The app adopts a labelled terminal a moment after
/// `hc run` returns, so this looks a few times.
fn rename_tab_of(tools: &dyn Tools, terminal: &str, title: &str) {
    let Some(want) = terminal.split_whitespace().next() else {
        return;
    };
    let Some(name) = tab_name(title) else {
        return;
    };
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
        let Some(ls) = tools.herdr_app_ls() else {
            return;
        };
        let Ok(ls) = serde_json::from_str::<serde_json::Value>(&ls) else {
            return;
        };
        let tab = ls
            .get("spaces")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .flat_map(|sp| {
                sp.get("tabs")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
            })
            .find(|tab| {
                tab.get("terminals")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .any(|t| t.get("terminal").and_then(|v| v.as_str()) == Some(want))
            })
            .and_then(|tab| tab.get("id").and_then(|v| v.as_str()));
        if let Some(id) = tab {
            let arg = serde_json::json!({ "id": id, "name": name }).to_string();
            let _ = tools.herdr_app_do(&["tab.rename".to_string(), arg]);
            return;
        }
    }
}
