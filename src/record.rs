//! Session records stored as a JSON label on each herdr-core terminal.
//!
//! The record lives in one label, key `wake`, as compact JSON. Every write
//! is compare-and-set: the writer re-reads the stored record and writes only
//! when its own copy is not older (by `seq`) and, where it names one, still
//! describes the same session and agent process. herdr-core has no
//! conditional label write, so a tiny window between the re-read and the
//! write remains; the re-read closes the stale write-back seen in the
//! prototype, where a wrapper ended a record from its pre-resume copy.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::{Core, CoreError, Terminal, TerminalId};

/// The terminal label key that holds the record.
pub const LABEL: &str = "wake";

/// The terminal label key that switches herdr-wake off for one terminal
/// (`herdr-wake off <terminal>`): while it is set, nothing is recorded
/// there and nothing is resumed there.
pub const OFF_LABEL: &str = "wake.off";

/// True when terminal `t` is switched off (it carries `OFF_LABEL`,
/// whatever its value).
pub fn is_off(t: &Terminal) -> bool {
    t.labels.contains_key(OFF_LABEL)
}

/// Switches terminal `t` off (sets `OFF_LABEL` to `1`) or on again
/// (removes the label).
pub fn set_off(core: &mut dyn Core, t: &TerminalId, off: bool) -> Result<(), CoreError> {
    let mut changes = BTreeMap::new();
    changes.insert(OFF_LABEL.to_string(), off.then(|| "1".to_string()));
    core.label(t, &changes).map(|_| ())
}

/// The record schema version this build writes.
pub const VERSION: u32 = 1;

/// Which coding agent a session belongs to. Serialized, displayed and parsed
/// as the lowercase names `claude`, `codex`, `pi`, `opencode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Pi,
    OpenCode,
}

impl AgentKind {
    /// Every kind, in a fixed order.
    pub const ALL: [AgentKind; 4] = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Pi,
        AgentKind::OpenCode,
    ];

    /// The lowercase name used in records, hook command lines and config.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Pi => "pi",
            AgentKind::OpenCode => "opencode",
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returned by `AgentKind::from_str` for a name that is not an agent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown agent: {0}")]
pub struct UnknownAgent(pub String);

impl FromStr for AgentKind {
    type Err = UnknownAgent;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AgentKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| UnknownAgent(s.to_string()))
    }
}

/// Where a session stands. Serialized as `"running"`, `"ended"` or
/// `{"failed":{"reason":"..."}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Running,
    Ended,
    Failed { reason: String },
}

/// A process identified by pid plus start time, so a reused pid does not
/// match. `start` is an opaque per-platform stamp from `Platform::process`;
/// compare it only with values from the same platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcRef {
    pub pid: u32,
    pub start: u64,
}

/// Everything needed to rebuild the resume command at wake time. Never holds
/// the command itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub v: u32,
    pub agent: AgentKind,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<PathBuf>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub bin: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_proc: Option<ProcRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_proc: Option<ProcRef>,
    pub boot_id: String,
    pub state: State,
    #[serde(default)]
    pub attempts: u8,
    #[serde(default)]
    pub seq: u64,
}

impl Record {
    /// A `Running` record at `VERSION` with no flags, env, processes,
    /// attempts or sequence yet.
    pub fn new(
        agent: AgentKind,
        session_id: impl Into<String>,
        cwd: PathBuf,
        bin: PathBuf,
        boot_id: impl Into<String>,
    ) -> Record {
        Record {
            v: VERSION,
            agent,
            session_id: session_id.into(),
            session_file: None,
            cwd,
            flags: Vec::new(),
            env: BTreeMap::new(),
            bin,
            agent_proc: None,
            shell_proc: None,
            boot_id: boot_id.into(),
            state: State::Running,
            attempts: 0,
            seq: 0,
        }
    }
}

impl Record {
    /// The attempts used on the boot `boot_id`. Attempts are counted per
    /// boot: a record last written on another boot has used none.
    pub fn attempts_on(&self, boot_id: &str) -> u8 {
        if self.boot_id == boot_id {
            self.attempts
        } else {
            0
        }
    }
}

/// The stored label text, or `None` when the terminal has no record label.
fn raw(core: &mut dyn Core, t: &TerminalId) -> Result<Option<String>, CoreError> {
    Ok(core.get(t)?.labels.remove(LABEL))
}

/// Parses label text into a record this build understands. Anything else
/// (malformed JSON, missing fields, another schema version) is `None`.
fn parse(text: &str) -> Option<Record> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("v")?.as_u64()? != u64::from(VERSION) {
        return None;
    }
    serde_json::from_value(value).ok()
}

/// The `seq` of stored label text, read even from a record of another
/// schema version, so an older build never overwrites a newer record.
fn stored_seq(text: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    value.get("seq")?.as_u64()
}

fn write(core: &mut dyn Core, t: &TerminalId, rec: &Record) -> Result<(), CoreError> {
    let text = serde_json::to_string(rec)
        .map_err(|e| CoreError::Protocol(format!("cannot encode record: {e}")))?;
    let mut changes = BTreeMap::new();
    changes.insert(LABEL.to_string(), Some(text));
    core.label(t, &changes)?;
    Ok(())
}

/// The terminal's record. `Ok(None)` when it has none, or when the label
/// holds malformed JSON or an unknown schema version `v`.
pub fn load(core: &mut dyn Core, t: &TerminalId) -> Result<Option<Record>, CoreError> {
    Ok(raw(core, t)?.as_deref().and_then(parse))
}

/// Stores `rec` on the terminal, unless the stored record has a higher
/// `seq`: then it writes nothing and returns `Ok(false)`. An equal `seq` is
/// written. The stored `seq` is honoured even when the rest of the stored
/// record is unreadable (for example a newer schema version).
pub fn save(core: &mut dyn Core, t: &TerminalId, rec: &Record) -> Result<bool, CoreError> {
    if let Some(seq) = raw(core, t)?.as_deref().and_then(stored_seq) {
        if seq > rec.seq {
            return Ok(false);
        }
    }
    write(core, t, rec)?;
    Ok(true)
}

/// The next sequence number for a write: the wall clock in milliseconds, or
/// one past `prev` when the clock is not ahead of it, so sequence numbers
/// only grow even if the clock steps back. Saturates at `u64::MAX`.
pub fn next_seq(now_ms: u64, prev: Option<u64>) -> u64 {
    match prev {
        Some(p) => now_ms.max(p.saturating_add(1)),
        None => now_ms,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Re-reads the record and, when it still belongs to `session_id` (and to
/// `agent_proc`, when given), applies `change`, bumps `seq` and writes it.
/// Returns the written record, or `None` when nothing matched.
fn update(
    core: &mut dyn Core,
    t: &TerminalId,
    session_id: &str,
    agent_proc: Option<ProcRef>,
    change: impl FnOnce(&mut Record),
) -> Result<Option<Record>, CoreError> {
    let Some(mut rec) = load(core, t)? else {
        return Ok(None);
    };
    if rec.session_id != session_id {
        return Ok(None);
    }
    if let Some(p) = agent_proc {
        if rec.agent_proc != Some(p) {
            return Ok(None);
        }
    }
    change(&mut rec);
    rec.seq = next_seq(now_ms(), Some(rec.seq));
    write(core, t, &rec)?;
    Ok(Some(rec))
}

/// Marks the stored record `Ended`, but only when it is still the record of
/// `session_id` and, when `agent_proc` is given, of that agent process. A
/// late end event for an older session, or a stale copy from before a
/// resume, changes nothing. Returns whether it wrote.
pub fn end(
    core: &mut dyn Core,
    t: &TerminalId,
    session_id: &str,
    agent_proc: Option<ProcRef>,
) -> Result<bool, CoreError> {
    Ok(update(core, t, session_id, agent_proc, |r| r.state = State::Ended)?.is_some())
}

/// Marks the stored record `Failed` with `reason`, under the same session id
/// guard as `end`. Returns whether it wrote.
pub fn fail(
    core: &mut dyn Core,
    t: &TerminalId,
    session_id: &str,
    reason: &str,
) -> Result<bool, CoreError> {
    let reason = reason.to_string();
    Ok(update(core, t, session_id, None, |r| {
        r.state = State::Failed { reason }
    })?
    .is_some())
}

/// Adds one to the stored record's `attempts` (saturating), under the same
/// session id guard as `end`. Returns the new count, or `None` when the
/// stored record is not this session's.
pub fn bump_attempts(
    core: &mut dyn Core,
    t: &TerminalId,
    session_id: &str,
) -> Result<Option<u8>, CoreError> {
    Ok(update(core, t, session_id, None, |r| {
        r.attempts = r.attempts.saturating_add(1)
    })?
    .map(|r| r.attempts))
}

/// Starts one resume attempt on the boot `boot_id`, under the same session
/// id guard as `end`: the attempts start over when the record is from
/// another boot, then one is added (saturating) and the record is stamped
/// with `boot_id`. The process references are cleared: they name the agent
/// and shell from before the restart, and stay empty until the resumed
/// agent's hook writes its own. Returns the written record, or `None` when
/// the stored record is not this session's.
pub fn begin_attempt(
    core: &mut dyn Core,
    t: &TerminalId,
    session_id: &str,
    boot_id: &str,
) -> Result<Option<Record>, CoreError> {
    update(core, t, session_id, None, |r| {
        r.attempts = r.attempts_on(boot_id).saturating_add(1);
        r.boot_id = boot_id.to_string();
        r.agent_proc = None;
        r.shell_proc = None;
    })
}

/// Makes the stored record resumable again: `Running` with no attempts used,
/// under the same session id guard as `end`. This is the user's explicit
/// `resume`. Returns whether it wrote.
pub fn rearm(core: &mut dyn Core, t: &TerminalId, session_id: &str) -> Result<bool, CoreError> {
    Ok(update(core, t, session_id, None, |r| {
        r.state = State::Running;
        r.attempts = 0;
    })?
    .is_some())
}

/// Removes the record label, whatever it holds (this is the user's explicit
/// `forget`). Re-reads first and writes nothing when there is no label.
/// Returns whether it removed one.
pub fn forget(core: &mut dyn Core, t: &TerminalId) -> Result<bool, CoreError> {
    if raw(core, t)?.is_none() {
        return Ok(false);
    }
    let mut changes = BTreeMap::new();
    changes.insert(LABEL.to_string(), None);
    core.label(t, &changes)?;
    Ok(true)
}

/// `id` as one plain file name: every byte outside `[A-Za-z0-9_-]` is
/// written as `%XX`, so any id maps to one name inside a folder and two
/// different ids never share it.
fn plain_name(id: &str) -> String {
    let mut name = String::with_capacity(id.len() + 6);
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
            name.push(char::from(b));
        } else {
            name.push_str(&format!("%{b:02X}"));
        }
    }
    name
}

/// The claim file for a session (see `plain_name`).
fn claim_file(dir: &Path, session_id: &str) -> PathBuf {
    dir.join(format!("{}.claim", plain_name(session_id)))
}

/// The record in a terminal's labels, when this build can read it.
pub fn of_terminal(t: &Terminal) -> Option<Record> {
    t.labels.get(LABEL).and_then(|text| parse(text))
}

// --- the local integrity copy -------------------------------------------------
//
// A label can be written by anything that reaches herdr-core's socket, so the
// label alone never decides what runs. Whenever herdr-wake itself stores a
// record (the hook, `adopt`, `resume <terminal>`), it also keeps the parts
// that decide what would run in a private file on this machine. At wake the
// label is acted on only when it says the same as that file.

/// The parts of a record that decide what a resume runs, plus the creation
/// time of the terminal they belong to (core can hand a removed terminal's
/// id to a new terminal).
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LocalCopy {
    created_ms: u64,
    agent: AgentKind,
    session_id: String,
    #[serde(default)]
    session_file: Option<PathBuf>,
    cwd: PathBuf,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    bin: PathBuf,
}

impl LocalCopy {
    fn of(rec: &Record, created_ms: u64) -> LocalCopy {
        LocalCopy {
            created_ms,
            agent: rec.agent,
            session_id: rec.session_id.clone(),
            session_file: rec.session_file.clone(),
            cwd: rec.cwd.clone(),
            flags: rec.flags.clone(),
            env: rec.env.clone(),
            bin: rec.bin.clone(),
        }
    }
}

/// `<dir>/<terminal>.json`, the local copy for terminal `t` (`dir` is
/// `paths::records_dir`).
pub fn copy_file(dir: &Path, t: &TerminalId) -> PathBuf {
    dir.join(format!("{}.json", plain_name(t.as_str())))
}

/// Writes the local copy of `rec` for terminal `t`, created at
/// `created_ms`: a 0600 file in a 0700 folder, replaced atomically.
pub fn write_copy(
    dir: &Path,
    t: &TerminalId,
    created_ms: u64,
    rec: &Record,
) -> std::io::Result<()> {
    let body =
        serde_json::to_vec(&LocalCopy::of(rec, created_ms)).map_err(std::io::Error::other)?;
    crate::private::write(&copy_file(dir, t), &body)
}

/// True when the local copy for `t` exists and equals `rec` on agent,
/// session id, session file, folder, flags, environment and program, for
/// the terminal created at `created_ms`. A missing or unreadable copy is a
/// mismatch.
pub fn copy_matches(dir: &Path, t: &TerminalId, created_ms: u64, rec: &Record) -> bool {
    std::fs::read(copy_file(dir, t))
        .ok()
        .and_then(|b| serde_json::from_slice::<LocalCopy>(&b).ok())
        .is_some_and(|c| c == LocalCopy::of(rec, created_ms))
}

/// Deletes the local copy for `t` when it is the copy of `session_id`: a
/// session that was quit must not come back because its label was flipped
/// to running again. A copy that already belongs to a newer session in the
/// same terminal is left alone. Returns whether it removed one.
pub fn remove_copy_of(dir: &Path, t: &TerminalId, session_id: &str) -> bool {
    let file = copy_file(dir, t);
    std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<LocalCopy>(&b).ok())
        .is_some_and(|c| c.session_id == session_id)
        && std::fs::remove_file(&file).is_ok()
}

/// Deletes the local copy for `t`. Returns whether there was one.
pub fn remove_copy(dir: &Path, t: &TerminalId) -> bool {
    std::fs::remove_file(copy_file(dir, t)).is_ok()
}

fn holder(file: &Path) -> Option<String> {
    std::fs::read_to_string(file).ok()
}

/// Claims `session_id` for terminal `t` with a lock file in `dir` (created
/// if missing), so two terminals never own one session. True when the claim
/// is now `t`'s (including a repeat claim by the same terminal); false when
/// another terminal holds it. The file appears atomically with its owner
/// already written in it, so racing claims have exactly one winner.
///
/// A claim lasts until `release_claim`; callers that want claims to lapse at
/// reboot pass a folder keyed by boot. If the folder cannot be written, the
/// claim succeeds: failures lean toward resuming.
pub fn claim(dir: &Path, session_id: &str, t: &TerminalId) -> bool {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let file = claim_file(dir, session_id);
    if crate::private::dir(dir).is_err() {
        return true;
    }
    let tmp = dir.join(format!(
        ".tmp-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        now_ms()
    ));
    if crate::private::create_new(&tmp, t.as_str().as_bytes()).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return true;
    }
    let linked = std::fs::hard_link(&tmp, &file);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            holder(&file).as_deref() == Some(t.as_str())
        }
        Err(_) => true,
    }
}

/// Drops `t`'s claim on `session_id`. A claim held by another terminal is
/// left alone. Returns whether a claim was removed.
pub fn release_claim(dir: &Path, session_id: &str, t: &TerminalId) -> bool {
    let file = claim_file(dir, session_id);
    holder(&file).as_deref() == Some(t.as_str()) && std::fs::remove_file(&file).is_ok()
}

/// Environment names a record may keep, besides `LC_*` and `XDG_*_HOME`.
const ENV_ALLOW: &[&str] = &[
    "PATH",
    "LANG",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_PROJECT_DIR_NAME",
    "CODEX_HOME",
    "PI_CODING_AGENT_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
    "PI_OFFLINE",
    "OPENCODE_CONFIG_DIR",
    "OPENCODE_DB",
];

/// Name endings that are never stored, even under an allowed prefix.
const ENV_SECRET_SUFFIXES: &[&str] = &["_KEY", "_TOKEN", "_PASSWORD", "_SECRET"];

fn env_allowed(name: &str) -> bool {
    let secret = {
        let upper = name.to_ascii_uppercase();
        ENV_SECRET_SUFFIXES.iter().any(|s| upper.ends_with(s))
    };
    if secret {
        return false;
    }
    ENV_ALLOW.contains(&name)
        || (name.len() > "LC_".len() && name.starts_with("LC_"))
        || (name.len() > "XDG__HOME".len() && name.starts_with("XDG_") && name.ends_with("_HOME"))
}

/// The part of `env` a record may hold: `PATH`, `LANG`, `LC_*`,
/// `XDG_*_HOME` and the agents' own folder settings (see `ENV_ALLOW`).
/// Names ending in `_KEY`, `_TOKEN`, `_PASSWORD` or `_SECRET` are always
/// dropped.
///
/// Two values are checked as well as names: `PATH` keeps only its absolute
/// folders (a relative or empty entry would look programs up in the session
/// folder), and `OPENCODE_DB` is kept only when it is an absolute file path
/// (a database URL can carry a password).
///
/// This runs when a record is written and again when it is read at wake, so
/// a label written by something else cannot bring other variables along.
pub fn scrub_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(k, _)| env_allowed(k))
        .filter_map(|(k, v)| {
            let v = match k.as_str() {
                "PATH" => {
                    let dirs: Vec<&str> = v.split(':').filter(|d| d.starts_with('/')).collect();
                    if dirs.is_empty() {
                        return None;
                    }
                    dirs.join(":")
                }
                "OPENCODE_DB" if !v.starts_with('/') => return None,
                _ => v.clone(),
            };
            Some((k.clone(), v))
        })
        .collect()
}
