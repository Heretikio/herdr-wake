//! Start gate: staggers agent starts across independent `herdr-wake shell`
//! processes (Task 9).
//!
//! Every wrapper that is about to resume an agent takes a slot. Per agent
//! kind, the first caller starts at once, the second waits until the first
//! has run alone for `first_alone_ms` (or until the first agent's hook calls
//! `confirm`, plus `stagger_ms`), and each later caller starts `stagger_ms`
//! after the previous slot. Slots are absolute times, so a caller that
//! arrives late waits less, never a negative amount.
//!
//! State lives in `<dir>/<boot key>.json`, read and written under an
//! exclusive `flock` on that file. The boot key names the boot and the core
//! process, so a file from an earlier boot or core is never consulted; such
//! files are deleted when seen. Every failure (folder unusable, lock not
//! taken within 1 s, unreadable file) leans toward starting: the caller
//! waits 0 and nothing blocks forever.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::platform::Platform;
use crate::record::AgentKind;

/// How long a caller tries to take the gate lock before giving up and
/// starting without waiting.
const LOCK_WAIT: Duration = Duration::from_secs(1);

/// One caller's place at the gate: its position among callers of its kind
/// and how long it had to wait when it entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    /// Position among callers of this kind (0 is the first), or `None` when
    /// the gate could not be used and the caller should not wait.
    pub index: Option<usize>,
    /// Milliseconds to wait, as of entering.
    pub wait_ms: u64,
}

/// Per kind: when each caller arrived, in order, and when the first agent
/// confirmed it is up.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct KindState {
    #[serde(default)]
    arrivals: Vec<u64>,
    #[serde(default)]
    confirmed: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GateState {
    #[serde(default)]
    kinds: BTreeMap<String, KindState>,
}

/// Absolute start time of caller `index`, from the arrivals and confirm.
fn slot(k: &KindState, cfg: &Config, index: usize) -> u64 {
    let mut prev: u64 = 0;
    for (i, &arrived) in k.arrivals.iter().enumerate().take(index + 1) {
        let earliest = match i {
            0 => arrived,
            1 => {
                let hold = prev.saturating_add(cfg.first_alone_ms);
                match k.confirmed {
                    Some(c) => hold.min(c.max(prev).saturating_add(cfg.stagger_ms)),
                    None => hold,
                }
            }
            _ => prev.saturating_add(cfg.stagger_ms),
        };
        prev = earliest.max(arrived);
    }
    prev
}

/// `boot id` plus core pid, as one plain file name: every byte outside
/// `[A-Za-z0-9_-]` becomes `_`. A missing core pid counts as 0.
pub fn boot_key(platform: &dyn Platform) -> String {
    let raw = format!(
        "{}-{}",
        platform.boot_id(),
        platform.core_pid().unwrap_or(0)
    );
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Takes a slot for `kind` and returns the milliseconds to wait before
/// starting (0 when the gate cannot be used).
pub fn enter(dir: &Path, kind: AgentKind, cfg: &Config, boot_key: &str, now_ms: u64) -> u64 {
    enter_ticket(dir, kind, cfg, boot_key, now_ms).wait_ms
}

/// Like `enter`, but returns the ticket so the caller can ask again with
/// `remaining` (a `confirm` from the first agent can shorten the wait).
pub fn enter_ticket(
    dir: &Path,
    kind: AgentKind,
    cfg: &Config,
    boot_key: &str,
    now_ms: u64,
) -> Ticket {
    let none = Ticket {
        index: None,
        wait_ms: 0,
    };
    remove_other_keys(dir, boot_key);
    with_state(dir, boot_key, |state| {
        let k = state.kinds.entry(kind.as_str().to_string()).or_default();
        k.arrivals.push(now_ms);
        let index = k.arrivals.len() - 1;
        Ticket {
            index: Some(index),
            wait_ms: slot(k, cfg, index).saturating_sub(now_ms),
        }
    })
    .unwrap_or(none)
}

/// The milliseconds `ticket` still has to wait at `now_ms`, taking any
/// `confirm` since it entered into account. 0 when the gate cannot be read.
pub fn remaining(
    dir: &Path,
    kind: AgentKind,
    cfg: &Config,
    boot_key: &str,
    ticket: &Ticket,
    now_ms: u64,
) -> u64 {
    let Some(index) = ticket.index else {
        return 0;
    };
    with_state(dir, boot_key, |state| {
        match state.kinds.get(kind.as_str()) {
            Some(k) if index < k.arrivals.len() => slot(k, cfg, index).saturating_sub(now_ms),
            _ => 0,
        }
    })
    .unwrap_or(0)
}

/// Records that the first resumed agent of `kind` is up, so the second
/// caller may start `stagger_ms` after `max(now_ms, first slot)` instead of
/// after the full `first_alone_ms`. Only the first confirm counts, and it
/// does nothing when no caller of `kind` has entered.
pub fn confirm(dir: &Path, kind: AgentKind, boot_key: &str, now_ms: u64) {
    let _ = with_state(dir, boot_key, |state| {
        if let Some(k) = state.kinds.get_mut(kind.as_str()) {
            if !k.arrivals.is_empty() && k.confirmed.is_none() {
                k.confirmed = Some(now_ms);
            }
        }
    });
}

fn state_file(dir: &Path, boot_key: &str) -> PathBuf {
    dir.join(format!("{boot_key}.json"))
}

/// Opens and locks the state file, applies `f`, writes the state back.
/// `None` when the folder or file cannot be used or the lock is not taken
/// within `LOCK_WAIT`.
fn with_state<T>(dir: &Path, boot_key: &str, f: impl FnOnce(&mut GateState) -> T) -> Option<T> {
    let mut file = crate::private::open_rw(&state_file(dir, boot_key)).ok()?;
    lock(&file)?;
    let mut text = String::new();
    let mut state: GateState = match file.read_to_string(&mut text) {
        Ok(_) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => GateState::default(),
    };
    let out = f(&mut state);
    let body = serde_json::to_vec(&state).ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;
    file.set_len(0).ok()?;
    file.write_all(&body).ok()?;
    // The lock is released when `file` closes.
    Some(out)
}

/// Takes an exclusive lock on `file`, retrying for up to `LOCK_WAIT`.
fn lock(file: &File) -> Option<()> {
    use rustix::fs::{flock, FlockOperation};
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match flock(file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Some(()),
            Err(e) if e == rustix::io::Errno::WOULDBLOCK || e == rustix::io::Errno::INTR => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

/// Deletes gate files of other boot keys: they belong to an earlier boot or
/// core and are never consulted again.
fn remove_other_keys(dir: &Path, boot_key: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let keep = format!("{boot_key}.json");
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".json") && name != keep {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
