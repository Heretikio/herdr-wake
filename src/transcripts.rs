//! Reading the agents' own transcripts, for the sessions the history journal
//! does not know (they ran before herdr-wake was installed, or outside
//! Herdr). Claude Code and Codex only; Pi and OpenCode are not read yet.
//!
//! Every file is read in two bounded pieces, the first and the last 128 KB,
//! never whole: the facts wanted sit at the start of a transcript (folder,
//! first permission mode, first prompt) or near its end (titles). What was
//! learned about a file is kept in a cache in the state folder, keyed by the
//! file's size and modification time, so a later run costs one `stat` per
//! file. The cache holds only the files seen in the latest run, and is
//! thrown away whenever `CACHE_VERSION` changes.
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
use crate::record::AgentKind;
use crate::trust;

/// The cache layout version. Change it whenever `Meta` gets a new field or
/// a parsing rule changes; the old cache is then ignored.
pub const CACHE_VERSION: u32 = 1;

/// How much of the start and of the end of a transcript is read.
const CHUNK: u64 = 128 * 1024;

/// Longest title or prompt kept.
const TEXT_MAX: usize = 200;

/// Longest first line of a Codex rollout read.
const MAX_FIRST_LINE: u64 = 1 << 20;

/// Deepest folder level searched under Codex's `sessions` folder.
const MAX_SESSIONS_DEPTH: usize = 4;

/// What was learned about one transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub meta: Meta,
    pub path: PathBuf,
    /// Last modification, in milliseconds since the epoch.
    pub mtime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        self.seen.insert(key.clone());
        if let Some(e) = self.files.get(&key) {
            if (e.size, e.mtime_ms) == (size, mtime_ms) {
                return e.meta.clone().map(|m| (m, mtime_ms));
            }
        }
        let meta = parse(path, size);
        self.files.insert(
            key,
            CacheEntry {
                size,
                mtime_ms,
                meta: meta.clone(),
            },
        );
        self.dirty = true;
        meta.map(|m| (m, mtime_ms))
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

/// `text` on one line, whitespace collapsed, at most `TEXT_MAX` characters.
fn tidy(text: &str) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    one.chars().take(TEXT_MAX).collect()
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

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool) == Some(true)
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
fn rollout_thread(name: &str) -> Option<&str> {
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
/// cache or by reading it: `None` for Pi and OpenCode (not read), for a
/// file that is missing or not a listable session.
pub fn meta_of(kind: AgentKind, path: &Path, cache: &mut Cache) -> Option<Found> {
    let parse = match kind {
        AgentKind::Claude => parse_claude as fn(&Path, u64) -> Option<Meta>,
        AgentKind::Codex => parse_codex,
        AgentKind::Pi | AgentKind::OpenCode => return None,
    };
    let (meta, mtime_ms) = cache.lookup(path, parse)?;
    Some(Found {
        meta,
        path: path.to_path_buf(),
        mtime_ms,
    })
}
