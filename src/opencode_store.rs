//! OpenCode's sessions, which it keeps in one SQLite database
//! (`~/.local/share/opencode/opencode.db`, tables `session`, `message` and
//! `part`; there is no transcript file).
//!
//! The database is read through the `sqlite3` program, never a library:
//! `sqlite3 -readonly -json <db> <query>`, an argument list, five seconds at
//! most, the program found on `PATH` and checked like any program herdr-wake
//! runs. Without `sqlite3`, or when it fails, OpenCode sessions that left
//! nothing else are not listed.
//!
//! Sessions with a parent (a sub-agent's) and archived sessions are left
//! out. A session's rows are read again only when its `time_updated`
//! changed; the cache of `transcripts` keeps the rest. Session ids are
//! checked (`trust::valid_session_id`) before they go into a query, so a
//! quote can never be in one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::facts::{self, Conversation, Facts, Role};
use crate::record::AgentKind;
use crate::transcripts::{tidy, Cache, Found, Meta};
use crate::{agents, proc, trust};

/// How long one `sqlite3` call may take.
pub const SQLITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How many sessions are read per query.
const BATCH: usize = 100;

/// Most sessions listed.
const MAX_SESSIONS: usize = 5000;

/// Runs a query: what `sqlite3 -json` printed, or why it failed.
pub type Sql<'a> = &'a dyn Fn(&str) -> Result<String, String>;

/// The database: `OPENCODE_DB` when it is set (a relative one is inside the
/// data folder), else `<data folder>/opencode.db`, the data folder being
/// `$XDG_DATA_HOME/opencode` or `~/.local/share/opencode`.
pub fn db_path(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    let data = match env.get("XDG_DATA_HOME") {
        Some(v) if Path::new(v).is_absolute() => PathBuf::from(v).join("opencode"),
        _ => home.join(".local/share/opencode"),
    };
    match env.get("OPENCODE_DB").filter(|v| !v.is_empty()) {
        Some(v) if v != ":memory:" => data.join(v),
        _ => data.join("opencode.db"),
    }
}

/// Runs `query` on `db` with `sqlite3`. `Err` says why not: no `sqlite3`,
/// an unsafe one, no database, a failure or a timeout.
pub fn run_sqlite3(
    env: &BTreeMap<String, String>,
    db: &Path,
    query: &str,
) -> Result<String, String> {
    let program = agents::on_path("sqlite3", env).ok_or("sqlite3 is not installed")?;
    trust::configured_program_is_safe(&program, trust::current_uid())
        .map_err(|why| format!("sqlite3 at {} is not safe to run: {why}", program.display()))?;
    let md = std::fs::symlink_metadata(db).map_err(|_| "there is no OpenCode database")?;
    if !md.is_file() && !md.file_type().is_symlink() {
        return Err("the OpenCode database is not a file".to_string());
    }
    let args = [
        "-readonly".to_string(),
        "-json".to_string(),
        db.to_string_lossy().into_owned(),
        query.to_string(),
    ];
    // PATH only: nothing of the caller's environment is needed.
    let mut child_env = BTreeMap::new();
    if let Some(path) = env.get("PATH") {
        child_env.insert("PATH".to_string(), path.clone());
    }
    let out = proc::run(
        &program,
        &args,
        &child_env,
        None,
        Path::new("/"),
        SQLITE_TIMEOUT,
    )
    .map_err(|e| match e {
        proc::RunError::Timeout => "sqlite3 took too long".to_string(),
        proc::RunError::Spawn(why) => format!("cannot run sqlite3: {why}"),
    })?;
    if out.code != Some(0) {
        let first = String::from_utf8_lossy(&out.stderr)
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| trust::printable(l).chars().take(200).collect::<String>())
            .unwrap_or_default();
        return Err(format!("sqlite3 failed: {first}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The rows `sqlite3 -json` printed (nothing at all for no rows).
fn rows(output: &str) -> Vec<Value> {
    if output.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<Value>>(output).unwrap_or_default()
}

/// `'a','b'`: ids that passed `valid_session_id`.
fn id_list(ids: &[&str]) -> String {
    ids.iter()
        .filter(|i| trust::valid_session_id(i))
        .map(|i| format!("'{i}'"))
        .collect::<Vec<_>>()
        .join(",")
}

/// OpenCode's name for a session until it names it.
fn untitled(title: &str) -> bool {
    title.strip_prefix("New session - ").is_some_and(|rest| {
        rest.ends_with('Z')
            && rest[..rest.len() - 1]
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | '.' | 'T'))
    })
}

/// What the messages of one session say.
#[derive(Default)]
struct Messages {
    conversation: Option<Conversation>,
    models: std::collections::BTreeSet<String>,
    duration: Option<u64>,
}

/// Every listable OpenCode session in `db`, through `sql`.
pub fn scan(db: &Path, sql: Sql, cache: &mut Cache) -> Vec<Found> {
    let Ok(listing) = sql(&format!(
        "select id, directory, title, cost, summary_additions as added, \
         summary_deletions as removed, time_created, time_updated from session \
         where parent_id is null and time_archived is null \
         order by time_updated desc limit {MAX_SESSIONS}"
    )) else {
        return Vec::new();
    };
    struct Item {
        id: String,
        cwd: PathBuf,
        title: Option<String>,
        cost: Option<f64>,
        lines: Option<(u64, u64)>,
        created: Option<u64>,
        updated: u64,
    }
    let mut found = Vec::new();
    let mut pending: Vec<Item> = Vec::new();
    for row in rows(&listing) {
        let (Some(id), Some(cwd), Some(updated)) = (
            facts::str_of(&row, "id").filter(|i| trust::valid_session_id(i)),
            facts::str_of(&row, "directory").filter(|d| Path::new(d).is_absolute()),
            facts::whole(row.get("time_updated")),
        ) else {
            continue;
        };
        let key = format!("opencode:{}#{id}", db.display());
        match cache.peek(&key, 0, updated) {
            Some(Some(meta)) => found.push(Found {
                meta,
                path: db.to_path_buf(),
                mtime_ms: updated,
            }),
            Some(None) => {}
            None => {
                let added = facts::whole(row.get("added")).unwrap_or(0);
                let removed = facts::whole(row.get("removed")).unwrap_or(0);
                pending.push(Item {
                    id: id.to_string(),
                    cwd: PathBuf::from(cwd),
                    title: facts::str_of(&row, "title")
                        .filter(|t| !untitled(t))
                        .map(tidy)
                        .filter(|t| !t.is_empty()),
                    // 0 is a free model, not a cost.
                    cost: facts::number(row.get("cost")).filter(|c| *c > 0.0),
                    lines: (added + removed > 0).then_some((added, removed)),
                    created: facts::whole(row.get("time_created")),
                    updated,
                });
            }
        }
    }
    for batch in pending.chunks(BATCH) {
        let ids: Vec<&str> = batch.iter().map(|i| i.id.as_str()).collect();
        let list = id_list(&ids);
        let messages = sql(&format!(
            "select session_id, json_extract(data, '$.role') as role, \
             json_extract(data, '$.finish') as finish, \
             json_extract(data, '$.error.name') as error, \
             json_extract(data, '$.modelID') as model, \
             json_extract(data, '$.time.created') as created, \
             json_extract(data, '$.time.completed') as completed \
             from message where session_id in ({list}) order by session_id, time_created, id"
        ));
        let prompts = sql(&format!(
            "select s.id as session_id, (select substr(json_extract(p.data, '$.text'), 1, 4000) \
             from message m join part p on p.message_id = m.id \
             where m.session_id = s.id and json_extract(m.data, '$.role') = 'user' \
             and json_extract(p.data, '$.type') = 'text' \
             and coalesce(json_extract(p.data, '$.synthetic'), 0) = 0 \
             order by m.time_created, m.id, p.id limit 1) as text \
             from session s where s.id in ({list})"
        ));
        // A failed read is not remembered: the next run asks again.
        let complete = messages.is_ok() && prompts.is_ok();
        let mut by_id: BTreeMap<String, Messages> = BTreeMap::new();
        for m in rows(&messages.unwrap_or_default()) {
            let Some(sid) = facts::str_of(&m, "session_id") else {
                continue;
            };
            let entry = by_id.entry(sid.to_string()).or_default();
            let role = facts::str_of(&m, "role");
            if let Some(c) = facts::opencode_ending(
                role,
                facts::str_of(&m, "finish"),
                facts::str_of(&m, "error"),
            ) {
                entry.conversation = Some(c);
            }
            if role == Some("assistant") {
                if let Some(model) = facts::str_of(&m, "model") {
                    crate::fullread::add_model(&mut entry.models, model);
                }
                if let (Some(c), Some(d)) = (
                    facts::whole(m.get("created")),
                    facts::whole(m.get("completed")),
                ) {
                    if d >= c {
                        entry.duration = Some(entry.duration.unwrap_or(0) + (d - c));
                    }
                }
            }
        }
        let mut first_prompt: BTreeMap<String, String> = BTreeMap::new();
        for p in rows(&prompts.unwrap_or_default()) {
            if let (Some(sid), Some(text)) =
                (facts::str_of(&p, "session_id"), facts::str_of(&p, "text"))
            {
                let text = tidy(text);
                if !text.is_empty() {
                    first_prompt.insert(sid.to_string(), text);
                }
            }
        }
        for item in batch {
            let m = by_id.remove(&item.id).unwrap_or_default();
            let meta = Meta {
                agent: AgentKind::OpenCode,
                session_id: item.id.clone(),
                cwd: Some(item.cwd.clone()),
                custom_title: item.title.clone(),
                ai_title: None,
                prompt: first_prompt.remove(&item.id),
                mode: None,
                facts: Facts {
                    conversation: m.conversation,
                    cost_usd: item.cost,
                    duration_ms: m.duration,
                    started_ms: item.created,
                    lines_added: item.lines.map(|l| l.0),
                    lines_removed: item.lines.map(|l| l.1),
                    models: m.models.into_iter().collect(),
                    ..Facts::default()
                },
            };
            let key = format!("opencode:{}#{}", db.display(), item.id);
            if complete {
                cache.store(key, 0, item.updated, Some(meta.clone()));
            }
            if !meta.is_empty() {
                found.push(Found {
                    meta,
                    path: db.to_path_buf(),
                    mtime_ms: item.updated,
                });
            }
        }
    }
    found.retain(|f| !f.meta.is_empty());
    found
}

/// What was said in session `id`, oldest first: the person's text and the
/// agent's text parts, without those OpenCode adds itself (synthetic ones).
pub fn conversation(sql: Sql, id: &str) -> Result<Vec<(Role, String)>, String> {
    if !trust::valid_session_id(id) {
        return Err("the session id is not valid".to_string());
    }
    let out = sql(&format!(
        "select m.id as mid, json_extract(m.data, '$.role') as role, \
         json_extract(p.data, '$.text') as text \
         from message m join part p on p.message_id = m.id \
         where m.session_id = '{id}' and json_extract(p.data, '$.type') = 'text' \
         and coalesce(json_extract(p.data, '$.synthetic'), 0) = 0 \
         order by m.time_created, m.id, p.id"
    ))?;
    let mut said: Vec<(String, Role, String)> = Vec::new();
    for r in rows(&out) {
        let role = match facts::str_of(&r, "role") {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => continue,
        };
        let (Some(mid), Some(text)) = (facts::str_of(&r, "mid"), facts::str_of(&r, "text")) else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        match said.last_mut() {
            Some((last, _, body)) if last == mid => {
                body.push('\n');
                body.push_str(text);
            }
            _ => said.push((mid.to_string(), role, text.to_string())),
        }
    }
    Ok(said.into_iter().map(|(_, r, t)| (r, t)).collect())
}

/// When session `id` last changed (milliseconds); `None` when there is no
/// such session.
pub fn updated_ms(sql: Sql, id: &str) -> Result<Option<u64>, String> {
    if !trust::valid_session_id(id) {
        return Err("the session id is not valid".to_string());
    }
    let out = sql(&format!(
        "select time_updated from session where id = '{id}'"
    ))?;
    Ok(rows(&out)
        .first()
        .and_then(|r| facts::whole(r.get("time_updated"))))
}
