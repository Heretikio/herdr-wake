//! Reading a whole transcript once, for the details that titles do not
//! give: how the conversation ended, cost, branch, models, pull requests.
//!
//! A transcript can be hundreds of megabytes, nearly all of it tool output,
//! so a line is parsed only when a byte search finds a key it may hold. The
//! last message lines are not kept: only where they are (offset and length),
//! and the few that decide the ending are read back at the end.
//!
//! What is read comes from files that a tool, or a person with write access,
//! can change: see `facts` for how it is cleaned.

use std::collections::{BTreeSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use memchr::memmem::Finder;
use serde_json::Value;

use crate::facts::{self, str_of, Facts, Pr};
use crate::trust;

/// The longest line of a transcript that is looked at; a longer one (a tool
/// result that holds a whole file, say) is skipped.
const MAX_LINE: u64 = 32 << 20;

/// How many of the last message lines are kept to judge an ending.
const ENDING_WINDOW: usize = 64;

/// The longest recap kept.
const RECAP_MAX: usize = 2000;

/// Calls `f(offset, line)` for every line of `path`, in order, without
/// the line break. A line longer than `MAX_LINE` is skipped.
pub(crate) fn each_line(path: &Path, mut f: impl FnMut(u64, &[u8])) -> std::io::Result<()> {
    let mut r = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut buf = Vec::new();
    let mut offset = 0u64;
    loop {
        buf.clear();
        let n = (&mut r).take(MAX_LINE).read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(());
        }
        let start = offset;
        offset += n as u64;
        if buf.last() == Some(&b'\n') {
            buf.pop();
        } else if n as u64 == MAX_LINE {
            // Too long: drop the rest of the line.
            loop {
                let chunk = r.fill_buf()?;
                if chunk.is_empty() {
                    break;
                }
                let len = chunk.len();
                if let Some(i) = memchr::memchr(b'\n', chunk) {
                    r.consume(i + 1);
                    offset += i as u64 + 1;
                    break;
                }
                r.consume(len);
                offset += len as u64;
            }
            continue;
        }
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        if !buf.is_empty() {
            f(start, &buf);
        }
    }
}

/// Offset and length of the last lines of interest.
type Window = VecDeque<(u64, usize)>;

fn note(window: &mut Window, cap: usize, offset: u64, line: &[u8]) {
    if window.len() == cap {
        window.pop_front();
    }
    window.push_back((offset, line.len()));
}

/// The lines `window` names, newest first, parsed as JSON; a line that does
/// not parse is left out.
fn window_rows(path: &Path, window: &Window) -> Vec<Value> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for &(offset, len) in window.iter().rev() {
        let mut bytes = vec![0u8; len];
        if file.seek(SeekFrom::Start(offset)).is_err() || file.read_exact(&mut bytes).is_err() {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            rows.push(v);
        }
    }
    rows
}

/// The string value that follows `key` (which ends with the opening quote)
/// found at `at`, when it is plain: no escapes.
fn plain_value(line: &[u8], at: usize, key_len: usize) -> Option<&str> {
    let rest = line.get(at + key_len..)?;
    let end = memchr::memchr(b'"', rest)?;
    let raw = &rest[..end];
    if raw.is_empty() || raw.contains(&b'\\') {
        return None;
    }
    std::str::from_utf8(raw).ok()
}

pub(crate) fn add_model(models: &mut BTreeSet<String>, name: &str) {
    let name = facts::clean(name, 100);
    if !name.is_empty() {
        models.insert(name);
    }
}

/// A recap on one line, at most `RECAP_MAX` characters; the hidden text when
/// it seems to hold a secret.
fn recap_text(text: &str) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if trust::text_holds_secret(&one) {
        facts::HIDDEN_SUMMARY.to_string()
    } else {
        facts::clean(&one, RECAP_MAX)
    }
}

/// A `pr-link` row as a pull request: `https` addresses only.
fn pr_of(v: &Value) -> Option<Pr> {
    let url = str_of(v, "prUrl")?;
    if !url.starts_with("https://")
        || url.len() > 500
        || url.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    Some(Pr {
        number: facts::whole(v.get("prNumber")),
        url: url.to_string(),
        repo: str_of(v, "prRepository")
            .map(|r| facts::clean(r, 200))
            .filter(|r| !r.is_empty()),
    })
}

/// What the whole Claude Code transcript says: how the conversation ended,
/// the branch, Claude's last `cost-state` record, the pull requests, and its
/// own recap when nothing was said after it.
pub(crate) fn claude_facts(path: &Path) -> Facts {
    let user = Finder::new(br#""type":"user""#);
    let assistant = Finder::new(br#""type":"assistant""#);
    let cost_row = Finder::new(br#""type":"cost-state""#);
    let pr_row = Finder::new(br#""type":"pr-link""#);
    let away = Finder::new(br#""subtype":"away_summary""#);
    let saved = Finder::new(br#""type":"summary""#);
    let branch_key = Finder::new(br#""gitBranch":""#);

    let mut window = Window::new();
    let mut cost: Option<Value> = None;
    let mut prs: Vec<Pr> = Vec::new();
    let mut branch: Option<String> = None;
    let mut recap: Option<String> = None;

    let _ = each_line(path, |offset, line| {
        let is_user = user.find(line).is_some();
        if is_user || assistant.find(line).is_some() {
            note(&mut window, ENDING_WINDOW, offset, line);
        }
        if let Some(at) = branch_key.find(line) {
            if let Some(b) = plain_value(line, at, branch_key.needle().len()) {
                branch = Some(facts::clean(b, 200)).filter(|b| !b.is_empty());
            }
        }
        if cost_row.find(line).is_some() {
            if let Ok(v) = serde_json::from_slice::<Value>(line) {
                if str_of(&v, "type") == Some("cost-state") {
                    cost = Some(v);
                }
            }
        } else if pr_row.find(line).is_some() {
            if let Ok(v) = serde_json::from_slice::<Value>(line) {
                if str_of(&v, "type") == Some("pr-link") {
                    if let Some(pr) = pr_of(&v) {
                        if !prs.iter().any(|p| p.url == pr.url) {
                            prs.push(pr);
                        }
                    }
                }
            }
        } else if away.find(line).is_some() || saved.find(line).is_some() {
            if let Ok(v) = serde_json::from_slice::<Value>(line) {
                let text = match (str_of(&v, "type"), str_of(&v, "subtype")) {
                    (Some("system"), Some("away_summary")) => str_of(&v, "content"),
                    (Some("summary"), _) => str_of(&v, "summary"),
                    _ => None,
                };
                if let Some(t) = text.map(recap_text).filter(|t| !t.is_empty()) {
                    recap = Some(t);
                }
            }
        } else if recap.is_some() && is_user {
            // A later request: the recap no longer says where it ended.
            if let Ok(v) = serde_json::from_slice::<Value>(line) {
                if facts::claude_is_person(&v) {
                    recap = None;
                }
            }
        }
    });

    let mut f = Facts {
        conversation: window_rows(path, &window)
            .iter()
            .find_map(facts::claude_ending),
        branch,
        prs,
        recap,
        ..Facts::default()
    };
    if let Some(c) = &cost {
        f.cost_usd = facts::number(c.get("totalCostUSD"));
        f.duration_ms = facts::whole(c.get("totalDuration"));
        f.started_ms = facts::whole(c.get("startTime"));
        f.lines_added = facts::whole(c.get("totalLinesAdded"));
        f.lines_removed = facts::whole(c.get("totalLinesRemoved"));
        if let Some(usage) = c.get("modelUsage").and_then(Value::as_object) {
            let mut models = BTreeSet::new();
            for name in usage.keys() {
                add_model(&mut models, name);
            }
            f.models = models.into_iter().collect();
        }
    }
    f
}

/// What the whole Codex rollout says: how the conversation ended, the
/// branch, the models, how long the turns took and when it started.
pub(crate) fn codex_facts(path: &Path) -> Facts {
    let response = Finder::new(br#""type":"response_item""#);
    let complete = Finder::new(br#""type":"task_complete""#);
    let aborted = Finder::new(br#""type":"turn_aborted""#);
    let context = Finder::new(br#""type":"turn_context""#);
    let session = Finder::new(br#""type":"session_meta""#);
    let duration_key = Finder::new(br#""duration_ms""#);

    let mut window = Window::new();
    let mut f = Facts::default();
    let mut models = BTreeSet::new();
    let mut duration: Option<u64> = None;

    let _ = each_line(path, |offset, line| {
        let ended = complete.find(line).is_some() || aborted.find(line).is_some();
        if ended || response.find(line).is_some() {
            note(&mut window, ENDING_WINDOW * 2, offset, line);
        }
        if ended && duration_key.find(line).is_some() {
            if let Ok(v) = serde_json::from_slice::<Value>(line) {
                if str_of(&v, "type") == Some("event_msg") {
                    if let Some(ms) = facts::whole(v.pointer("/payload/duration_ms")) {
                        duration = Some(duration.unwrap_or(0).saturating_add(ms));
                    }
                }
            }
        } else if context.find(line).is_some() {
            if let Ok(v) = serde_json::from_slice::<Value>(line) {
                if let Some(m) = v.pointer("/payload/model").and_then(Value::as_str) {
                    add_model(&mut models, m);
                }
            }
        } else if session.find(line).is_some() {
            if let Ok(v) = serde_json::from_slice::<Value>(line) {
                if str_of(&v, "type") == Some("session_meta") {
                    if let Some(b) = v.pointer("/payload/git/branch").and_then(Value::as_str) {
                        f.branch = Some(facts::clean(b, 200)).filter(|b| !b.is_empty());
                    }
                    f.started_ms = v
                        .pointer("/payload/timestamp")
                        .and_then(Value::as_str)
                        .and_then(facts::epoch_ms)
                        .or(f.started_ms);
                }
            }
        }
    });

    f.conversation = window_rows(path, &window)
        .iter()
        .find_map(facts::codex_ending);
    f.duration_ms = duration;
    f.models = models.into_iter().collect();
    f
}
