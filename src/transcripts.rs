//! Reading the agents' own session files, for the sessions the history
//! journal does not know (they ran before herdr-wake was installed, or
//! outside Herdr): Claude Code and Codex transcripts, Pi's session files and
//! OpenCode's database (see `opencode_store`).
//!
//! Titles and the first prompt come from two bounded pieces of a file, the
//! first and the last 128 KB: those facts sit at the start of a transcript
//! (folder, first permission mode, first prompt) or near its end (titles).
//! The details of `facts` (how the conversation ended, cost, branch, models,
//! pull requests) need the whole file; it is read once, line by line, with a
//! cheap byte search deciding which lines are parsed at all. What was learned
//! about a file is kept in a cache in the state folder, keyed by the file's
//! size and modification time, so a later run costs one `stat` per file. The
//! cache holds only the files seen in the latest run, and is thrown away
//! whenever `CACHE_VERSION` changes.
//!
//! Sub-agents are left out (Claude's sidechain rows and `subagents` folders,
//! Codex's child threads), and so are runs that are not interactive
//! (Claude's `sdk-*` entrypoints, `codex exec`).
//!
//! Everything read here is data from a file that a tool, or a person with
//! write access, can change: titles are cut to a short length and only ever
//! shown through `trust::printable`; a session id must be a UUID, which is
//! also a valid `trust::valid_session_id`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::codex::{self, Origin};
use crate::facts::{self, flag, str_of, Conversation, Facts};
use crate::record::AgentKind;
use crate::trust;

/// The cache layout version. Change it whenever `Meta` gets a new field or
/// a parsing rule changes; the old cache is then ignored.
pub const CACHE_VERSION: u32 = 4;

/// How much of the start and of the end of a transcript is read.
const CHUNK: u64 = 128 * 1024;

/// Longest title or prompt kept.
const TEXT_MAX: usize = 200;

/// Longest first line of a Codex rollout read.
const MAX_FIRST_LINE: u64 = 1 << 20;

/// Deepest folder level searched under Codex's `sessions` folder.
const MAX_SESSIONS_DEPTH: usize = 4;

/// What was learned about one transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub agent: AgentKind,
    pub session_id: String,
    /// The folder the session ran in, from the transcript.
    pub cwd: Option<PathBuf>,
    /// Claude's `customTitle`, or Codex's thread name.
    pub custom_title: Option<String>,
    /// Claude's `aiTitle`.
    pub ai_title: Option<String>,
    /// The first real user prompt.
    pub prompt: Option<String>,
    /// Claude: the first recorded `permissionMode`.
    pub mode: Option<String>,
    /// What the whole file says: ending, cost, branch, models, pull requests.
    #[serde(default)]
    pub facts: Facts,
}

impl Meta {
    /// The title: the custom title, else the AI title, else the first prompt.
    pub fn title(&self) -> Option<&str> {
        [&self.custom_title, &self.ai_title, &self.prompt]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .find(|t| !t.is_empty())
    }

    /// True when the transcript holds no conversation (no title and no
    /// prompt): a session that was opened and left.
    pub fn is_empty(&self) -> bool {
        self.title().is_none()
    }
}

/// A transcript with its facts.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub meta: Meta,
    pub path: PathBuf,
    /// Last modification, in milliseconds since the epoch.
    pub mtime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime_ms: u64,
    /// `None`: the file is not a session worth listing (sub-agent, not
    /// interactive, unreadable).
    meta: Option<Meta>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    files: BTreeMap<String, CacheEntry>,
}

/// The stat-keyed cache of what was read from transcripts.
#[derive(Debug, Default)]
pub struct Cache {
    files: BTreeMap<String, CacheEntry>,
    seen: BTreeSet<String>,
    dirty: bool,
}

impl Cache {
    /// An empty cache.
    pub fn new() -> Cache {
        Cache::default()
    }

    /// Reads the cache file. A missing, malformed or other-version file
    /// gives an empty cache.
    pub fn load(path: &Path) -> Cache {
        let Ok(md) = std::fs::symlink_metadata(path) else {
            return Cache::default();
        };
        if !md.is_file() || md.len() > 64 * 1024 * 1024 {
            return Cache::default();
        }
        let file = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
            .filter(|f| f.version == CACHE_VERSION);
        match file {
            Some(f) => Cache {
                files: f.files,
                seen: BTreeSet::new(),
                dirty: false,
            },
            None => Cache {
                dirty: true,
                ..Cache::default()
            },
        }
    }

    /// Writes the files seen in this run (as a private file), when that is
    /// not what is stored already. Errors are returned for the caller to
    /// ignore: the cache is only a speed-up.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        let before = self.files.len();
        self.files.retain(|k, _| self.seen.contains(k));
        if !self.dirty && self.files.len() == before {
            return Ok(());
        }
        let body = serde_json::to_vec(&CacheFile {
            version: CACHE_VERSION,
            files: self.files.clone(),
        })
        .map_err(std::io::Error::other)?;
        crate::private::write(path, &body)?;
        self.dirty = false;
        Ok(())
    }

    /// What `parse` makes of the file `path`, from the cache when its size
    /// and modification time are unchanged. `None` when the file cannot be
    /// seen or is not a listable session. The modification time comes with
    /// it.
    fn lookup(
        &mut self,
        path: &Path,
        parse: impl FnOnce(&Path, u64) -> Option<Meta>,
    ) -> Option<(Meta, u64)> {
        let md = std::fs::symlink_metadata(path).ok()?;
        if !md.is_file() {
            return None;
        }
        let (size, mtime_ms) = (md.len(), mtime_ms(&md));
        let key = path.to_string_lossy().into_owned();
        let meta = match self.peek(&key, size, mtime_ms) {
            Some(found) => found,
            None => {
                let meta = parse(path, size);
                self.store(key, size, mtime_ms, meta.clone());
                meta
            }
        };
        meta.map(|m| (m, mtime_ms))
    }

    /// What was learned about `key` when it had this size and time: `None`
    /// when nothing, or something about another size or time. Marks `key` as
    /// seen in this run.
    pub(crate) fn peek(&mut self, key: &str, size: u64, mtime_ms: u64) -> Option<Option<Meta>> {
        self.seen.insert(key.to_string());
        let e = self.files.get(key)?;
        ((e.size, e.mtime_ms) == (size, mtime_ms)).then(|| e.meta.clone())
    }

    /// Remembers what was learned about `key` at this size and time.
    pub(crate) fn store(&mut self, key: String, size: u64, mtime_ms: u64, meta: Option<Meta>) {
        self.seen.insert(key.clone());
        self.files.insert(
            key,
            CacheEntry {
                size,
                mtime_ms,
                meta,
            },
        );
        self.dirty = true;
    }
}

fn mtime_ms(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// True for a UUID in its usual written form (Claude and Codex name their
/// sessions this way); any other file name is not a session.
fn is_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// What a title or prompt that looks like a credential is shown as, here
/// and in the cache.
pub const HIDDEN_TITLE: &str = "(title hidden: looks like a credential)";

/// `text` on one line, whitespace collapsed, at most `TEXT_MAX` characters;
/// `HIDDEN_TITLE` instead when it seems to hold a secret value (see
/// `trust::text_holds_secret`), so that never reaches the cache or the
/// screen. The whole text is checked, not only the part that is kept.
pub(crate) fn tidy(text: &str) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = one.chars().take(TEXT_MAX).collect();
    if trust::text_holds_secret(&one) {
        HIDDEN_TITLE.to_string()
    } else {
        short
    }
}

// ------------------------------------------------------------------ Claude

/// Claude Code's config folder: `CLAUDE_CONFIG_DIR` when set, else
/// `<home>/.claude`.
pub fn claude_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    crate::agents::claude::config_dir(env, home)
}

/// Codex's home folder: `CODEX_HOME` when set, else `<home>/.codex`.
pub fn codex_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    codex::codex_home(env, home)
}

/// Lines of a transcript, as bytes.
type Lines = Vec<Vec<u8>>;

/// The first and the last `CHUNK` bytes of `path` as lines. A line cut by
/// the end of a piece is dropped. The second list is empty for a file that
/// fits in one piece.
fn read_lines(path: &Path, size: u64) -> std::io::Result<(Lines, Lines)> {
    let mut f = std::fs::File::open(path)?;
    let mut head = Vec::new();
    (&mut f).take(CHUNK).read_to_end(&mut head)?;
    let whole = size <= CHUNK;
    if !whole {
        // Drop the line the piece cuts.
        match head.iter().rposition(|&b| b == b'\n') {
            Some(i) => head.truncate(i + 1),
            None => head.clear(),
        }
    }
    let mut tail = Vec::new();
    if !whole {
        // One byte before the piece says whether it starts on a line.
        let start = size.saturating_sub(CHUNK).max(CHUNK);
        f.seek(SeekFrom::Start(start - 1))?;
        let mut piece = Vec::new();
        f.take(CHUNK + 1).read_to_end(&mut piece)?;
        let starts_a_line = piece.first() == Some(&b'\n');
        tail = piece.split_off(1.min(piece.len()));
        if !starts_a_line {
            // The first line is cut (or continues the one dropped above).
            match tail.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    tail.drain(..=i);
                }
                None => tail.clear(),
            }
        }
    }
    let lines = |b: &[u8]| -> Vec<Vec<u8>> {
        b.split(|&c| c == b'\n')
            .filter(|l| !l.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    };
    Ok((lines(&head), lines(&tail)))
}

/// The text of a user message's content: a string, or the first text block
/// of a list (a list that holds a tool result has none).
fn message_text(content: &Value) -> Option<&str> {
    match content {
        Value::String(s) => Some(s),
        Value::Array(blocks) => blocks
            .iter()
            .find(|b| str_of(b, "type") == Some("text"))
            .and_then(|b| str_of(b, "text")),
        _ => None,
    }
}

/// True when `text` is something the person typed, not text the agent put
/// in a user message (reminders, slash commands, interrupts, summaries).
fn is_real_prompt(text: &str) -> bool {
    let t = text.trim_start();
    !t.is_empty()
        && !t.starts_with("<system-reminder>")
        && !t.starts_with("<command-")
        && !t.starts_with("<local-command")
        && !t.starts_with("<bash-")
        && !t.starts_with("Caveat: The messages below were generated")
        && !t.starts_with("[Request interrupted")
        && !t.starts_with("This session is being continued from a previous conversation")
}

/// What `parse_claude` collects.
#[derive(Default)]
struct ClaudeFacts {
    sidechain: bool,
    entrypoint: Option<String>,
    cwd: Option<String>,
    mode: Option<String>,
    prompt: Option<String>,
    custom: Option<String>,
    ai: Option<String>,
}

impl ClaudeFacts {
    /// One row. `head`: the row is from the start of the file, where
    /// "first" facts are looked for; titles are taken from anywhere, the
    /// last one winning.
    fn row(&mut self, v: &Value, head: bool) {
        match str_of(v, "type") {
            Some("custom-title") => {
                self.custom = str_of(v, "customTitle").map(tidy).filter(|t| !t.is_empty());
                return;
            }
            Some("ai-title") => {
                self.ai = str_of(v, "aiTitle").map(tidy).filter(|t| !t.is_empty());
                return;
            }
            _ => {}
        }
        if !head {
            return;
        }
        if self.mode.is_none() {
            self.mode = str_of(v, "permissionMode").map(str::to_string);
        }
        let ty = str_of(v, "type");
        if matches!(ty, Some("user" | "assistant")) {
            if flag(v, "isSidechain") {
                self.sidechain = true;
                return;
            }
            if self.cwd.is_none() {
                self.cwd = str_of(v, "cwd").map(str::to_string);
            }
            if self.entrypoint.is_none() {
                self.entrypoint = str_of(v, "entrypoint").map(str::to_string);
            }
        }
        if ty == Some("user")
            && self.prompt.is_none()
            && !flag(v, "isMeta")
            && !flag(v, "isCompactSummary")
        {
            let from_user = v
                .pointer("/message/role")
                .and_then(Value::as_str)
                .is_none_or(|r| r == "user");
            if let Some(text) = v
                .pointer("/message/content")
                .and_then(message_text)
                .filter(|t| from_user && is_real_prompt(t))
            {
                self.prompt = Some(tidy(text));
            }
        }
    }
}

/// The facts of one Claude transcript, `None` for a sub-agent or a run that
/// is not interactive.
fn parse_claude(path: &Path, size: u64) -> Option<Meta> {
    let id = path.file_stem()?.to_str()?;
    if !is_uuid(id) {
        return None;
    }
    let (head, tail) = read_lines(path, size).ok()?;
    let mut facts = ClaudeFacts::default();
    for line in &head {
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            facts.row(&v, true);
        }
    }
    for line in &tail {
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            facts.row(&v, false);
        }
    }
    if facts.sidechain
        || facts
            .entrypoint
            .as_deref()
            .is_some_and(|e| e.starts_with("sdk"))
    {
        return None;
    }
    Some(Meta {
        agent: AgentKind::Claude,
        session_id: id.to_string(),
        cwd: facts.cwd.map(PathBuf::from).filter(|p| p.is_absolute()),
        custom_title: facts.custom,
        ai_title: facts.ai,
        prompt: facts.prompt,
        mode: facts.mode,
        facts: crate::fullread::claude_facts(path),
    })
}

/// Every Claude transcript under `<config>/projects/*/<id>.jsonl` that is
/// a listable session with a conversation. `subagents` folders are not
/// looked into.
pub fn scan_claude(config: &Path, cache: &mut Cache) -> Vec<Found> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(config.join("projects")) else {
        return out;
    };
    for project in projects.flatten() {
        if !project.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            if let Some((meta, mtime_ms)) = cache.lookup(&path, parse_claude) {
                if !meta.is_empty() {
                    out.push(Found {
                        meta,
                        path,
                        mtime_ms,
                    });
                }
            }
        }
    }
    out
}

/// The transcript of Claude session `id`: `<config>/projects/<folder name
/// for cwd>/<id>.jsonl`, else any `<config>/projects/*/<id>.jsonl` (Claude
/// shortens very long folder names).
pub fn claude_transcript(config: &Path, cwd: &Path, id: &str) -> Option<PathBuf> {
    if !trust::valid_session_id(id) {
        return None;
    }
    let projects = config.join("projects");
    let file = format!("{id}.jsonl");
    let direct = projects
        .join(crate::agents::claude::project_dir_name(cwd))
        .join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|d| d.path().join(&file))
        .find(|p| p.is_file())
}

// ------------------------------------------------------------------- Codex

/// Thread names from `<codex home>/session_index.jsonl`, by thread id; the
/// last line for an id wins.
///
/// At most the last 8 MB of the file are read, and each line is decoded
/// on its own (invalid UTF-8 costs that line only).
pub fn codex_names(codex_home: &Path) -> BTreeMap<String, String> {
    const MAX: u64 = 8 * 1024 * 1024;
    let mut names = BTreeMap::new();
    let Ok(mut f) = std::fs::File::open(codex_home.join("session_index.jsonl")) else {
        return names;
    };
    let Ok(md) = f.metadata() else {
        return names;
    };
    let mut bytes = Vec::new();
    let cut = md.len() > MAX;
    if cut && f.seek(SeekFrom::Start(md.len() - MAX - 1)).is_err() {
        return names;
    }
    if f.take(MAX + 1).read_to_end(&mut bytes).is_err() {
        return names;
    }
    if cut {
        // Drop the line the cut lands in.
        let first_is_line_end = bytes.first() == Some(&b'\n');
        bytes.remove(0);
        if !first_is_line_end {
            match bytes.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    bytes.drain(..=i);
                }
                None => bytes.clear(),
            }
        }
    }
    for raw in bytes.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(raw);
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let (Some(id), Some(name)) = (str_of(&v, "id"), str_of(&v, "thread_name")) {
            let name = tidy(name);
            if !name.is_empty() {
                names.insert(id.to_string(), name);
            }
        }
    }
    names
}

/// The thread id of a rollout file name
/// (`rollout-YYYY-MM-DDTHH-MM-SS-<thread>.jsonl`). `None` for other names and
/// for a child thread's file (`..<thread>_<child>.jsonl`).
pub(crate) fn rollout_thread(name: &str) -> Option<&str> {
    let core = name.strip_suffix(".jsonl")?.strip_prefix("rollout-")?;
    if core.get(19..20)? != "-" {
        return None;
    }
    let id = core.get(20..)?;
    (!id.contains('_')).then_some(id)
}

/// True when `text` is context Codex puts into a user message, not a prompt.
fn codex_context(text: &str) -> bool {
    let t = text.trim_start();
    t.is_empty()
        || t.starts_with('<')
        || t.starts_with("# AGENTS.md")
        || t.starts_with("# Context from my IDE")
}

fn codex_user_text(v: &Value) -> Option<&str> {
    let p = v.get("payload")?;
    match (str_of(v, "type"), str_of(p, "type")) {
        (Some("response_item"), Some("message")) if str_of(p, "role") == Some("user") => p
            .get("content")?
            .as_array()?
            .iter()
            .find(|b| str_of(b, "type") == Some("input_text"))
            .and_then(|b| str_of(b, "text")),
        (Some("event_msg"), Some("user_message")) => str_of(p, "message"),
        _ => None,
    }
}

/// The facts of one Codex rollout, `None` for a child thread or a run that
/// is not interactive.
fn parse_codex(path: &Path, _size: u64) -> Option<Meta> {
    let id = rollout_thread(path.file_name()?.to_str()?)?;
    if !is_uuid(id) {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    (&mut reader)
        .take(MAX_FIRST_LINE)
        .read_line(&mut first)
        .ok()?;
    let meta: Value = serde_json::from_str(first.trim()).ok()?;
    if str_of(&meta, "type") != Some("session_meta") {
        return None;
    }
    if codex::meta_origin(&meta) != Origin::User {
        return None;
    }
    let cwd = meta
        .pointer("/payload/cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let mut prompt = None;
    let mut rest = Vec::new();
    reader.take(CHUNK).read_to_end(&mut rest).ok()?;
    for line in rest.split(|&b| b == b'\n') {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if let Some(text) = codex_user_text(&v).filter(|t| !codex_context(t)) {
            prompt = Some(tidy(text));
            break;
        }
    }
    Some(Meta {
        agent: AgentKind::Codex,
        session_id: id.to_string(),
        cwd,
        custom_title: None,
        ai_title: None,
        prompt,
        mode: None,
        facts: crate::fullread::codex_facts(path),
    })
}

/// Every plain Codex rollout under `<codex home>/sessions` that is a
/// user's own interactive thread with a conversation or a name.
pub fn scan_codex(codex_home: &Path, cache: &mut Cache) -> Vec<Found> {
    let names = codex_names(codex_home);
    let mut out = Vec::new();
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
            let name = entry.file_name().to_string_lossy().into_owned();
            if rollout_thread(&name).is_none() {
                continue;
            }
            if let Some((mut meta, mtime_ms)) = cache.lookup(&path, parse_codex) {
                meta.custom_title = names.get(&meta.session_id).cloned();
                if !meta.is_empty() {
                    out.push(Found {
                        meta,
                        path,
                        mtime_ms,
                    });
                }
            }
        }
    }
    out
}

/// The facts of the transcript at `path` of an agent's session, from the
/// cache or by reading it: `None` for OpenCode (its sessions are in a
/// database), for a file that is missing or not a listable session.
pub fn meta_of(kind: AgentKind, path: &Path, cache: &mut Cache) -> Option<Found> {
    let parse = match kind {
        AgentKind::Claude => parse_claude as fn(&Path, u64) -> Option<Meta>,
        AgentKind::Codex => parse_codex,
        AgentKind::Pi => parse_pi,
        AgentKind::OpenCode => return None,
    };
    let (meta, mtime_ms) = cache.lookup(path, parse)?;
    Some(Found {
        meta,
        path: path.to_path_buf(),
        mtime_ms,
    })
}

// ---------------------------------------------------------------------- Pi

/// Pi's session folder: `PI_CODING_AGENT_SESSION_DIR`, else
/// `${PI_CODING_AGENT_DIR:-~/.pi/agent}/sessions`.
pub fn pi_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    crate::agents::pi::sessions_root(env, home)
}

/// What one Pi session file (`<time>_<id>.jsonl`) says. Its first line
/// names the session and its folder; `None` when it does not look like one.
/// The file is read whole, one line at a time: Pi's rows are small.
fn parse_pi(path: &Path, _size: u64) -> Option<Meta> {
    let mut header: Option<(String, PathBuf, Option<u64>)> = None;
    let mut bad = false;
    let mut first = true;
    let mut name: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut conversation: Option<Conversation> = None;
    let mut models = BTreeSet::new();
    let mut cost = 0.0f64;
    let mut duration: Option<u64> = None;
    crate::fullread::each_line(path, |_, line| {
        if bad {
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            bad = first;
            first = false;
            return;
        };
        if std::mem::take(&mut first) {
            match (str_of(&v, "type"), str_of(&v, "id"), str_of(&v, "cwd")) {
                (Some("session"), Some(id), Some(cwd))
                    if trust::valid_session_id(id) && Path::new(cwd).is_absolute() =>
                {
                    header = Some((
                        id.to_string(),
                        PathBuf::from(cwd),
                        str_of(&v, "timestamp").and_then(facts::epoch_ms),
                    ));
                }
                _ => bad = true,
            }
            return;
        }
        match str_of(&v, "type") {
            Some("session_info") => {
                name = str_of(&v, "name").map(tidy).filter(|n| !n.is_empty());
            }
            Some("model_change") => {
                if let Some(m) = str_of(&v, "modelId") {
                    crate::fullread::add_model(&mut models, m);
                }
            }
            Some("message") => {
                if let Some(c) = facts::pi_ending(&v) {
                    conversation = Some(c);
                }
                if prompt.is_none() {
                    if let Some((facts::Role::User, text)) = facts::pi_message(&v) {
                        prompt = Some(tidy(&text));
                    }
                }
                let message = v.get("message");
                if let Some(m) = message.filter(|m| str_of(m, "role") == Some("assistant")) {
                    if let Some(model) = str_of(m, "model") {
                        crate::fullread::add_model(&mut models, model);
                    }
                    cost += facts::number(m.pointer("/usage/cost/total")).unwrap_or(0.0);
                    if let Some(ms) = facts::whole(m.get("durationMs")) {
                        duration = Some(duration.unwrap_or(0).saturating_add(ms));
                    }
                }
            }
            _ => {}
        }
    })
    .ok()?;
    let (id, cwd, started) = header.filter(|_| !bad)?;
    Some(Meta {
        agent: AgentKind::Pi,
        session_id: id,
        cwd: Some(cwd),
        custom_title: name,
        ai_title: None,
        prompt,
        mode: None,
        facts: Facts {
            conversation,
            cost_usd: (cost > 0.0).then_some(cost),
            duration_ms: duration,
            started_ms: started,
            models: models.into_iter().collect(),
            ..Facts::default()
        },
    })
}

/// Every Pi session under `<session folder>/<folder>/<file>.jsonl` that
/// holds a conversation or a name.
pub fn scan_pi(sessions: &Path, cache: &mut Cache) -> Vec<Found> {
    let mut out = Vec::new();
    let Ok(folders) = std::fs::read_dir(sessions) else {
        return out;
    };
    for folder in folders.flatten() {
        if !folder.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(folder.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            if let Some((meta, mtime_ms)) = cache.lookup(&path, parse_pi) {
                if !meta.is_empty() {
                    out.push(Found {
                        meta,
                        path,
                        mtime_ms,
                    });
                }
            }
        }
    }
    out
}
