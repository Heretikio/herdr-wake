//! The history journal: what herdr-wake remembers about sessions after they
//! end, so `herdr-wake resurrect` can bring them back.
//!
//! A record lives on its terminal and goes with it, and a quit deletes its
//! local copy. The journal keeps one small JSON file per session in
//! `paths::history_dir`, named after the agent and the session id. It holds
//! what a resume needs and nothing the record does not already hold: the
//! flags exactly as the record keeps them (secret-looking values were left
//! out when the record was made) and the environment allowlist, scrubbed
//! again on every write. Files are private (`private::write`: 0700 folder,
//! 0600 files, atomic replace, never through a symlink) and are read back
//! only when they are plain files of a sane size.
//!
//! An entry is written when the hook records a started session, and updated
//! when the session ends: quit, killed, replaced by a new session id in the
//! same terminal (Claude's `/clear`), forgotten with `forget` or `off`. The
//! first definite ending wins; an entry that is still open or ended for an
//! unknown reason can be updated again, and a new start reopens it. The
//! journal is capped by `Config::history_limit`: the entries with the oldest
//! last activity are removed first.
//!
//! Nothing here can fail a hook: callers log the error and carry on.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::core::TerminalId;
use crate::platform::Platform;
use crate::record::{self, AgentKind, Record};

/// The entry schema version this build writes and reads.
pub const VERSION: u32 = 1;

/// Largest journal file that is read back.
const MAX_FILE: u64 = 256 * 1024;

/// How a journaled session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ending {
    /// Recorded as started and not seen to end.
    Open,
    /// The user quit the agent.
    Quit,
    /// The agent was killed.
    Killed,
    /// A new session id took over the same terminal.
    Replaced,
    /// `herdr-wake forget` or `off` removed the record.
    Forgotten,
    /// The agent said the session ended but not how.
    Unknown,
}

impl Ending {
    /// The lowercase name shown in listings and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Ending::Open => "open",
            Ending::Quit => "quit",
            Ending::Killed => "killed",
            Ending::Replaced => "replaced",
            Ending::Forgotten => "forgotten",
            Ending::Unknown => "unknown",
        }
    }

    /// True when a later ending may still replace this one.
    fn can_change(self) -> bool {
        matches!(self, Ending::Open | Ending::Unknown)
    }
}

/// One session in the journal. The field names follow `Record`, so an entry
/// turns back into a record for `Agent::resume_plan` (see `to_record`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub v: u32,
    pub agent: AgentKind,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<PathBuf>,
    /// The session's folder.
    pub cwd: PathBuf,
    /// The restorable flags exactly as the record kept them.
    #[serde(default)]
    pub flags: Vec<String>,
    /// The environment allowlist of the record (see `record::scrub_env`).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The program the record ran.
    pub bin: PathBuf,
    /// The herdr-core terminal the session last ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    pub ending: Ending,
}

impl Entry {
    /// An open entry for the session `rec` describes, started at `now_ms`.
    pub fn from_record(t: Option<&TerminalId>, rec: &Record, now_ms: u64) -> Entry {
        Entry {
            v: VERSION,
            agent: rec.agent,
            session_id: rec.session_id.clone(),
            session_file: rec.session_file.clone(),
            cwd: rec.cwd.clone(),
            flags: rec.flags.clone(),
            env: record::scrub_env(&rec.env),
            bin: rec.bin.clone(),
            terminal: t.map(|t| t.as_str().to_string()),
            started_ms: now_ms,
            ended_ms: None,
            ending: Ending::Open,
        }
    }

    /// A running record with this entry's facts, to hand to the agent's
    /// `resume_plan`. The environment is scrubbed again.
    pub fn to_record(&self) -> Record {
        let mut rec = Record::new(
            self.agent,
            self.session_id.clone(),
            self.cwd.clone(),
            self.bin.clone(),
            "",
        );
        rec.session_file = self.session_file.clone();
        rec.flags = self.flags.clone();
        rec.env = record::scrub_env(&self.env);
        rec
    }

    /// When the session was last active as far as the journal knows.
    pub fn last_ms(&self) -> u64 {
        self.ended_ms.unwrap_or(self.started_ms)
    }
}

/// `<dir>/<agent>-<session id>.json`, the journal file of a session.
pub fn file(dir: &Path, agent: AgentKind, session_id: &str) -> PathBuf {
    dir.join(format!(
        "{}-{}.json",
        agent.as_str(),
        record::plain_name(session_id)
    ))
}

fn read_file(path: &Path) -> Option<Entry> {
    let md = std::fs::symlink_metadata(path).ok()?;
    if !md.is_file() || md.len() > MAX_FILE {
        return None;
    }
    let entry: Entry = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    // A file only counts under its own name.
    (entry.v == VERSION && file(path.parent()?, entry.agent, &entry.session_id) == path)
        .then_some(entry)
}

/// The entry of one session, or `None` when there is none.
pub fn load(dir: &Path, agent: AgentKind, session_id: &str) -> Option<Entry> {
    read_file(&file(dir, agent, session_id))
}

/// Every readable entry, in no particular order. Files that are symlinks,
/// too big, malformed, of another version or under the wrong name are
/// skipped.
pub fn read_all(dir: &Path) -> Vec<Entry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|e| read_file(&e.path()))
        .collect()
}

/// Writes `entry` and then trims the journal to `limit` entries, never
/// removing the one just written.
fn store(dir: &Path, limit: usize, entry: &Entry) -> io::Result<()> {
    let path = file(dir, entry.agent, &entry.session_id);
    let body = serde_json::to_vec(entry).map_err(io::Error::other)?;
    crate::private::write(&path, &body)?;
    prune(dir, limit, &path);
    Ok(())
}

/// How old a temporary file left by an interrupted write must be before
/// `prune` removes it.
const STALE_TMP: std::time::Duration = std::time::Duration::from_secs(3600);

/// Removes the entries with the oldest last activity until `limit` are left.
/// `keep` is never removed. A file that cannot be read as an entry (too big,
/// malformed, another schema version, a symlink) is neither counted nor
/// removed: it may be a newer build's. Temporary files left by an
/// interrupted write are removed once they are an hour old.
fn prune(dir: &Path, limit: usize, keep: &Path) {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for e in listing.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.contains(".tmp-") {
            let stale = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > STALE_TMP);
            if stale && e.file_type().is_ok_and(|t| t.is_file()) {
                let _ = std::fs::remove_file(e.path());
            }
        } else if name.ends_with(".json") && !name.starts_with('.') {
            files.push(e.path());
        }
    }
    if files.len() <= limit {
        return;
    }
    let mut aged: Vec<(u64, PathBuf)> = files
        .into_iter()
        .filter(|p| p != keep)
        .filter_map(|p| Some((read_file(&p)?.last_ms(), p)))
        .collect();
    // The entry just written counts too.
    let total = aged.len() + 1;
    if total <= limit {
        return;
    }
    aged.sort();
    for (_, p) in aged.into_iter().take(total - limit.max(1)) {
        let _ = std::fs::remove_file(p);
    }
}

/// Records that the session of `rec` is open in terminal `t` at `now_ms`:
/// a new entry, or the existing one reopened (it keeps its first start time
/// and takes the new flags, program, environment and terminal). With
/// `limit` 0 nothing is written.
pub fn started(
    dir: &Path,
    limit: usize,
    t: &TerminalId,
    rec: &Record,
    now_ms: u64,
) -> io::Result<()> {
    if limit == 0 {
        return Ok(());
    }
    let mut entry = Entry::from_record(Some(t), rec, now_ms);
    if let Some(old) = load(dir, rec.agent, &rec.session_id) {
        entry.started_ms = old.started_ms;
    }
    store(dir, limit, &entry)
}

/// Records that a session ended at `now_ms` with `ending`. The existing
/// entry is updated; when there is none, `fallback` (the terminal and
/// record the caller holds) makes one, and with neither nothing is written.
/// An entry that already has a definite ending keeps it (the first one
/// wins); one that is open or ended for an unknown reason takes the new one.
/// With `limit` 0 nothing is written.
pub fn ended(
    dir: &Path,
    limit: usize,
    agent: AgentKind,
    session_id: &str,
    fallback: Option<(&TerminalId, &Record)>,
    ending: Ending,
    now_ms: u64,
) -> io::Result<()> {
    if limit == 0 {
        return Ok(());
    }
    let mut entry = match (load(dir, agent, session_id), fallback) {
        (Some(e), _) => e,
        (None, Some((t, rec))) if rec.agent == agent && rec.session_id == session_id => {
            Entry::from_record(Some(t), rec, now_ms)
        }
        _ => return Ok(()),
    };
    if !entry.ending.can_change() {
        return Ok(());
    }
    entry.ending = ending;
    entry.ended_ms = Some(now_ms);
    store(dir, limit, &entry)
}

/// Runs a journal write for the caller's setup and logs a failure instead of
/// returning it: a hook, a watcher and the wrapper never fail over the
/// journal.
pub fn journal(
    platform: &dyn Platform,
    env: &BTreeMap<String, String>,
    home: &Path,
    cfg: &Config,
    write: impl FnOnce(&Path, usize, u64) -> io::Result<()>,
) {
    let now = platform.now_ms();
    if let Err(e) = write(
        &crate::paths::history_dir(env, home),
        cfg.history_limit,
        now,
    ) {
        crate::hook::log_line(env, home, now, &format!("history not written: {e}"));
    }
}
