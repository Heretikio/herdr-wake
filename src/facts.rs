//! What the agents' own session files say about a session beyond its title:
//! how the conversation ended, what it cost, which branch and models it
//! used, the pull requests it opened. One module holds the rules for every
//! agent (Claude Code, Codex, Pi, OpenCode) so `transcripts` only has to
//! read files and `summary` only has to ask for words.
//!
//! The ending rules follow the way the other session-history tools judge a
//! transcript: only what the person said and what the agent answered count.
//! Context an agent puts into user messages (reminders, slash commands,
//! IDE context, `AGENTS.md`), sub-agent rows and compaction summaries are
//! not the person's words and never decide how a session ended.
//!
//! Everything here reads data from files that a tool, or a person with
//! write access, can change: numbers must be finite and not negative,
//! strings are cut and stripped of control characters, and a pull request
//! address must be `https`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::history::Ending;
use crate::trust;

/// How a conversation stood when it stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Conversation {
    /// The agent's last answer was complete.
    Finished,
    /// The person's last message got no answer.
    Unanswered,
    /// The person interrupted the agent.
    Interrupted,
    /// It stopped in the middle of a tool call.
    MidAction,
}

impl Conversation {
    /// The name in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Conversation::Finished => "finished",
            Conversation::Unanswered => "unanswered",
            Conversation::Interrupted => "interrupted",
            Conversation::MidAction => "mid-action",
        }
    }

    /// The short word in the table after a journal ending ("quit, finished").
    fn short(self) -> &'static str {
        match self {
            Conversation::MidAction => "mid-work",
            other => other.as_str(),
        }
    }

    /// The word for a session with no journal ending: how it stopped.
    fn stopped(self) -> &'static str {
        match self {
            Conversation::MidAction => "stopped mid-work",
            other => other.as_str(),
        }
    }
}

/// The ENDED column: the journal's ending and how the conversation ended,
/// whichever of the two are known. "quit, finished", "killed, mid-work";
/// "stopped mid-work", "finished" for a session only a transcript knows.
pub fn ended_label(ending: Ending, conversation: Option<Conversation>) -> String {
    match (ending, conversation) {
        (Ending::Unknown | Ending::Open, Some(c)) => c.stopped().to_string(),
        (e, Some(c)) => format!("{}, {}", e.as_str(), c.short()),
        (e, None) => e.as_str().to_string(),
    }
}

/// A pull request a session opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pr {
    pub number: Option<u64>,
    pub url: String,
    pub repo: Option<String>,
}

/// What a full read of a session's file found.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Facts {
    pub conversation: Option<Conversation>,
    pub branch: Option<String>,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
    pub started_ms: Option<u64>,
    pub lines_added: Option<u64>,
    pub lines_removed: Option<u64>,
    pub models: Vec<String>,
    pub prs: Vec<Pr>,
    /// Claude Code's own recap (or a saved summary) when nothing was said
    /// after it.
    pub recap: Option<String>,
    /// A summary made earlier and still good for the session file as it is
    /// now; filled in by `summary::attach_cached`, never stored with the
    /// transcript cache.
    #[serde(skip)]
    pub summary: Option<String>,
    #[serde(skip)]
    pub summary_source: Option<String>,
}

/// What a row knows beyond the journal entry: the facts of its session
/// file and where that file is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extra {
    pub facts: Facts,
    /// The agent's own session file, when it has one (OpenCode keeps its
    /// sessions in a database).
    pub path: Option<PathBuf>,
}

/// What a summary that holds a credential-looking value is shown as.
pub const HIDDEN_SUMMARY: &str = "(summary hidden: looks like a credential)";

// ------------------------------------------------------------ small helpers

pub fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

pub fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool) == Some(true)
}

/// A finite, non-negative number.
pub fn number(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
}

/// A whole, non-negative number that fits comfortably.
pub fn whole(v: Option<&Value>) -> Option<u64> {
    number(v).filter(|n| *n < 9e15).map(|n| {
        // Truncation is wanted: durations and counts are whole.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let w = n as u64;
        w
    })
}

/// `s` with control characters removed, cut to `max` characters.
pub fn clean(s: &str, max: usize) -> String {
    trust::printable(s).chars().take(max).collect()
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `text` without `<system-reminder ...>...</system-reminder>` blocks (an
/// unterminated one stays).
pub fn strip_reminders(text: &str) -> String {
    const OPEN: &str = "<system-reminder";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(OPEN) {
        let after = &rest[i + OPEN.len()..];
        // `<system-reminder` must end the word: `<system-reminders>` is not it.
        let boundary = after.chars().next().is_none_or(|c| !word_char(c));
        let closed = boundary.then(|| after.find('>')).flatten().and_then(|gt| {
            after[gt + 1..]
                .find(CLOSE)
                .map(|c| gt + 1 + c + CLOSE.len())
        });
        match closed {
            Some(end) => {
                out.push_str(&rest[..i]);
                rest = &after[end..];
            }
            None => {
                out.push_str(&rest[..i + OPEN.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// True when `text` holds the tag `<name` as a whole word (`<hc` but not
/// `<hcl>`).
pub fn has_tag(text: &str, name: &str) -> bool {
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(i) = text[from..].find(&open) {
        let end = from + i + open.len();
        if text[end..].chars().next().is_none_or(|c| !word_char(c)) {
            return true;
        }
        from = end;
    }
    false
}

/// The tags whose presence makes a Claude user message the harness's, not
/// the person's.
const HARNESS_TAGS: &[&str] = &[
    "command-name",
    "local-command",
    "task-notification",
    "cross-session-message",
    "hc",
];

/// Whose words a message holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// The text of a message's content: a string, or the `text` blocks of a
/// list joined with `sep`.
fn text_of_content(content: &Value, sep: &str) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => Some(
            blocks
                .iter()
                .filter(|b| matches!(str_of(b, "type"), Some("text") | None))
                .filter_map(|b| str_of(b, "text"))
                .collect::<Vec<_>>()
                .join(sep),
        ),
        _ => None,
    }
}

// ------------------------------------------------------------------ Claude

/// Claude Code row: the words of the person or the agent, for a summary:
/// `None` for tool output, reminders, commands, sub-agents and anything
/// that is not a user or assistant row.
pub fn claude_message(v: &Value) -> Option<(Role, String)> {
    if flag(v, "isMeta") || flag(v, "isSidechain") {
        return None;
    }
    let role = match str_of(v, "type")? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let text = text_of_content(v.pointer("/message/content")?, "\n")?;
    let text = strip_reminders(&text);
    if HARNESS_TAGS.iter().any(|t| has_tag(&text, t)) {
        return None;
    }
    let text = text.trim();
    (!text.is_empty()).then(|| (role, text.to_string()))
}

/// How the conversation stood after Claude Code row `v`; `None` when the row
/// does not count (hooks, commands, sub-agents, summaries, bookkeeping).
pub fn claude_ending(v: &Value) -> Option<Conversation> {
    if flag(v, "isMeta") || flag(v, "isSidechain") || flag(v, "isCompactSummary") {
        return None;
    }
    let message = v.get("message");
    match str_of(v, "type")? {
        "assistant" => {
            let stop = message.and_then(|m| str_of(m, "stop_reason"));
            Some(if matches!(stop, Some("end_turn" | "stop_sequence")) {
                Conversation::Finished
            } else {
                Conversation::MidAction
            })
        }
        "user" => {
            let content = message?.get("content")?;
            if let Value::Array(blocks) = content {
                if blocks
                    .iter()
                    .any(|b| str_of(b, "type") == Some("tool_result"))
                {
                    return Some(Conversation::MidAction);
                }
            }
            let text = text_of_content(content, " ")?;
            let text = strip_reminders(&text);
            let text = text.trim();
            if text.is_empty() {
                None
            } else if text.starts_with("[Request interrupted") {
                Some(Conversation::Interrupted)
            } else if text.starts_with('<') {
                // A command, its output or a notification.
                None
            } else {
                Some(Conversation::Unanswered)
            }
        }
        _ => None,
    }
}

/// True when Claude Code row `v` is a message of the person (not a
/// compaction summary, command or tool result).
pub fn claude_is_person(v: &Value) -> bool {
    str_of(v, "type") == Some("user")
        && !flag(v, "isCompactSummary")
        && matches!(claude_message(v), Some((Role::User, _)))
}

// ------------------------------------------------------------------- Codex

/// Codex row: the words of the person or Codex. `None` for anything else,
/// including the context Codex sends as user messages (`AGENTS.md`, the
/// environment, the IDE's open files).
pub fn codex_message(v: &Value) -> Option<(Role, String)> {
    if str_of(v, "type") != Some("response_item") {
        return None;
    }
    let item = v.get("payload")?;
    if str_of(item, "type") != Some("message") {
        return None;
    }
    let role = match str_of(item, "role")? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let mut texts: Vec<String> = Vec::new();
    for part in item.get("content")?.as_array()? {
        let Some(text) = str_of(part, "text") else {
            continue;
        };
        let mut text = text.trim().to_string();
        if role == Role::User {
            if text.starts_with("# Context from my IDE setup") {
                text = text
                    .split_once("## My request for Codex:")
                    .map(|(_, rest)| rest.trim().to_string())
                    .unwrap_or_default();
            }
            if text.starts_with('<') || text.starts_with("# AGENTS.md instructions") {
                continue;
            }
        }
        if !text.is_empty() {
            texts.push(text);
        }
    }
    (!texts.is_empty()).then(|| (role, texts.join("\n")))
}

/// How the conversation stood after Codex row `v`: a turn completes or is
/// aborted, and between those it calls tools.
pub fn codex_ending(v: &Value) -> Option<Conversation> {
    let item = v.get("payload")?;
    match str_of(v, "type")? {
        "event_msg" => match str_of(item, "type")? {
            "task_complete" => Some(Conversation::Finished),
            "turn_aborted" => Some(Conversation::Interrupted),
            _ => None,
        },
        "response_item" => {
            let kind = str_of(item, "type").unwrap_or("");
            if kind.ends_with("_call") || kind.ends_with("_call_output") {
                return Some(Conversation::MidAction);
            }
            match codex_message(v)?.0 {
                Role::Assistant => Some(Conversation::Finished),
                Role::User => Some(Conversation::Unanswered),
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------- Pi

/// Pi row: the words of the person or the agent (text blocks only, no
/// thinking and no tool calls).
pub fn pi_message(v: &Value) -> Option<(Role, String)> {
    if str_of(v, "type") != Some("message") {
        return None;
    }
    let m = v.get("message")?;
    let role = match str_of(m, "role")? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let text = text_of_content(m.get("content")?, "\n")?;
    let text = text.trim();
    (!text.is_empty()).then(|| (role, text.to_string()))
}

/// How the conversation stood after Pi row `v`. A bash command the person
/// ran themselves (`!cmd`) and the extensions' own rows do not count.
pub fn pi_ending(v: &Value) -> Option<Conversation> {
    if str_of(v, "type") != Some("message") {
        return None;
    }
    let m = v.get("message")?;
    match str_of(m, "role")? {
        "user" => pi_message(v).map(|_| Conversation::Unanswered),
        "toolResult" => Some(Conversation::MidAction),
        "assistant" => Some(match str_of(m, "stopReason") {
            Some("stop" | "length") => Conversation::Finished,
            Some("aborted") => Conversation::Interrupted,
            // The request failed: nothing answered it.
            Some("error") => Conversation::Unanswered,
            _ => Conversation::MidAction,
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------- OpenCode

/// How the conversation stood after an OpenCode message: `role`, its
/// `finish` reason and the name of its `error`, if any.
pub fn opencode_ending(
    role: Option<&str>,
    finish: Option<&str>,
    error: Option<&str>,
) -> Option<Conversation> {
    match role? {
        "user" => Some(Conversation::Unanswered),
        "assistant" => Some(match (error, finish) {
            (Some("MessageAbortedError"), _) => Conversation::Interrupted,
            (Some(_), _) => Conversation::Unanswered,
            (None, Some(f)) if f != "tool-calls" => Conversation::Finished,
            _ => Conversation::MidAction,
        }),
        _ => None,
    }
}

// -------------------------------------------------------------------- time

/// An ISO time (`2026-11-24T14:13:08.262Z`) as milliseconds since the
/// epoch; `None` when it is not one. A missing zone means UTC.
pub fn epoch_ms(stamp: &str) -> Option<u64> {
    let s = stamp.trim();
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = s.get(from..to)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    let mut millis = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        let three: String = format!("{digits}000").chars().take(3).collect();
        millis = three.parse().ok()?;
        rest = &frac[digits.len()..];
    }
    let offset_min = match rest {
        "" | "Z" | "z" => 0,
        zone if zone.len() == 6 && matches!(zone.as_bytes()[0], b'+' | b'-') => {
            let hh: i64 = zone.get(1..3)?.parse().ok()?;
            let mm: i64 = zone.get(4..6)?.parse().ok()?;
            let total = hh * 60 + mm;
            if zone.starts_with('-') {
                -total
            } else {
                total
            }
        }
        _ => return None,
    };
    // Days since 1970-01-01 for the civil date (proleptic Gregorian).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60;
    u64::try_from(secs * 1000 + millis).ok()
}
