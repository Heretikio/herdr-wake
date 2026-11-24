//! What a full read of a session file finds (how the conversation ended,
//! cost, branch, models, pull requests, recap) for Claude Code, Codex, Pi and
//! OpenCode, and how it reaches the listing.

mod common;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use common::{agent_bin, Fixture};
use herdr_wake::facts::{Conversation, Facts};
use herdr_wake::opencode_store;
use herdr_wake::record::AgentKind;
use herdr_wake::resurrect::{self as rs, Filter, Tools, Transcript};
use herdr_wake::transcripts::{self, Cache};
use serde_json::json;

const ID: &str = "0b5a2c1e-1111-4222-8333-444455556666";
const ID2: &str = "0b5a2c1e-1111-4222-8333-777788889999";
const PI_ID: &str = "01a1d4dc-b19a-7795-ba3a-48120e83a624";
const OC_ID: &str = "ses_e37d06101ffeBzsnIV0fYNrlMx";

fn write_lines(path: &Path, rows: &[serde_json::Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::File::create(path).unwrap();
    for r in rows {
        writeln!(f, "{r}").unwrap();
    }
}

fn claude_file(config: &Path, id: &str) -> PathBuf {
    config
        .join("projects/-work-app")
        .join(format!("{id}.jsonl"))
}

fn user(text: &str) -> serde_json::Value {
    json!({"type":"user","cwd":"/work/app","entrypoint":"cli","isSidechain":false,
           "gitBranch":"feat/login",
           "message":{"role":"user","content":text}})
}

fn assistant(stop: serde_json::Value, text: &str) -> serde_json::Value {
    json!({"type":"assistant","cwd":"/work/app","isSidechain":false,
           "message":{"role":"assistant","stop_reason":stop,
                      "content":[{"type":"text","text":text}]}})
}

fn tool_use() -> serde_json::Value {
    json!({"type":"assistant","isSidechain":false,
           "message":{"role":"assistant","stop_reason":"tool_use",
                      "content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}})
}

fn tool_result() -> serde_json::Value {
    json!({"type":"user","isSidechain":false,
           "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}})
}

fn claude_facts(rows: &[serde_json::Value]) -> Facts {
    let c = tempfile::tempdir().unwrap();
    write_lines(&claude_file(c.path(), ID), rows);
    let found = transcripts::scan_claude(c.path(), &mut Cache::new());
    assert_eq!(found.len(), 1, "the session is listed");
    found[0].meta.facts.clone()
}

// ------------------------------------------------------------------ Claude

#[test]
fn claude_endings_follow_the_last_real_message() {
    use Conversation::*;
    let table: Vec<(&str, Vec<serde_json::Value>, Option<Conversation>)> = vec![
        (
            "finished",
            vec![user("hi"), assistant(json!("end_turn"), "hello")],
            Some(Finished),
        ),
        ("unanswered", vec![user("hi")], Some(Unanswered)),
        (
            "interrupted",
            vec![
                user("hi"),
                tool_use(),
                json!({"type":"user","isSidechain":false,"message":{"role":"user",
                       "content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}),
            ],
            Some(Interrupted),
        ),
        (
            "mid-action after a tool call",
            vec![user("hi"), tool_use()],
            Some(MidAction),
        ),
        (
            "mid-action after a tool result",
            vec![user("hi"), tool_use(), tool_result()],
            Some(MidAction),
        ),
        (
            "bookkeeping, hooks and compaction after the answer do not count",
            vec![
                user("hi"),
                assistant(json!("end_turn"), "done"),
                json!({"type":"system","subtype":"turn_duration"}),
                json!({"type":"file-history-snapshot"}),
                json!({"type":"user","isMeta":true,"message":{"role":"user","content":"meta"}}),
                json!({"type":"user","isCompactSummary":true,
                       "message":{"role":"user","content":"This session is being continued"}}),
                user("<system-reminder>hook output</system-reminder>"),
                user("<command-name>/usage</command-name>"),
            ],
            Some(Finished),
        ),
    ];
    for (name, rows, want) in table {
        assert_eq!(claude_facts(&rows).conversation, want, "{name}");
    }
}

#[test]
fn claude_the_ending_is_found_when_bookkeeping_fills_the_end_of_the_file() {
    let mut rows = vec![user("hi"), assistant(json!("end_turn"), "done")];
    // Far more than the 128 KB the title read looks at.
    for i in 0..4000 {
        rows.push(json!({"type":"file-history-snapshot","n":i,"pad":"x".repeat(80)}));
    }
    assert_eq!(
        claude_facts(&rows).conversation,
        Some(Conversation::Finished)
    );
}

#[test]
fn claude_cost_branch_models_and_pull_requests() {
    let f = claude_facts(&[
        user("hi"),
        assistant(json!("end_turn"), "done"),
        json!({"type":"cost-state","totalCostUSD":1.0,"totalDuration":10,"startTime":1,
               "totalLinesAdded":1,"totalLinesRemoved":1,"modelUsage":{"old-model":{}}}),
        json!({"type":"pr-link","prNumber":4,"prUrl":"https://github.com/acme/webshop/pull/4",
               "prRepository":"acme/webshop"}),
        // The same address again, and one that is not https.
        json!({"type":"pr-link","prNumber":4,"prUrl":"https://github.com/acme/webshop/pull/4",
               "prRepository":"acme/webshop"}),
        json!({"type":"pr-link","prNumber":5,"prUrl":"http://github.com/acme/webshop/pull/5"}),
        json!({"type":"pr-link","prNumber":6,"prUrl":"javascript:alert(1)"}),
        json!({"type":"pr-link","prNumber":7,"prUrl":"https://example.com/a b"}),
        json!({"type":"pr-link","prNumber":8,"prUrl":"https://gitlab.com/g/p/-/merge_requests/8"}),
        // The last cost record wins.
        json!({"type":"cost-state","totalCostUSD":2.5,"totalDuration":47_000,
               "startTime":1_794_369_857_849u64,"totalLinesAdded":312,"totalLinesRemoved":58,
               "modelUsage":{"claude-opus-5-5":{"costUSD":2.0},"claude-haiku-5-5":{"costUSD":0.5}}}),
    ]);
    assert_eq!(f.branch.as_deref(), Some("feat/login"));
    assert_eq!(f.cost_usd, Some(2.5));
    assert_eq!(f.duration_ms, Some(47_000));
    assert_eq!(f.started_ms, Some(1_794_369_857_849));
    assert_eq!(f.lines_added, Some(312));
    assert_eq!(f.lines_removed, Some(58));
    assert_eq!(f.models, vec!["claude-haiku-5-5", "claude-opus-5-5"]);
    let urls: Vec<&str> = f.prs.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![
            "https://github.com/acme/webshop/pull/4",
            "https://gitlab.com/g/p/-/merge_requests/8"
        ]
    );
    assert_eq!(f.prs[0].number, Some(4));
    assert_eq!(f.prs[0].repo.as_deref(), Some("acme/webshop"));
    assert_eq!(f.prs[1].repo, None);
}

#[test]
fn claude_without_cost_records_the_details_are_empty_not_zero() {
    let f = claude_facts(&[user("hi")]);
    assert_eq!(f.cost_usd, None);
    assert_eq!(f.duration_ms, None);
    assert_eq!(f.started_ms, None);
    assert_eq!(f.lines_added, None);
    assert!(f.models.is_empty() && f.prs.is_empty());
    // A cost record with nonsense numbers gives nothing either.
    let f = claude_facts(&[
        user("hi"),
        json!({"type":"cost-state","totalCostUSD":-3,"totalDuration":"long","startTime":null}),
    ]);
    assert_eq!(
        (f.cost_usd, f.duration_ms, f.started_ms),
        (None, None, None)
    );
}

#[test]
fn claude_the_last_branch_wins_and_an_escaped_one_is_ignored() {
    let mut a = user("one");
    a["gitBranch"] = json!("main");
    let mut b = user("two");
    b["gitBranch"] = json!("topic/\"quoted\"");
    let f = claude_facts(&[a.clone(), b]);
    // The odd one is skipped; the plain one stays.
    assert_eq!(f.branch.as_deref(), Some("main"));
    let mut c = user("three");
    c["gitBranch"] = json!("release");
    assert_eq!(claude_facts(&[a, c]).branch.as_deref(), Some("release"));
}

#[test]
fn claude_a_recap_with_nothing_said_after_it_is_kept() {
    let recap = json!({"type":"system","subtype":"away_summary","isMeta":false,
                       "content":"The login bug is fixed.\nNext: deploy."});
    let f = claude_facts(&[
        user("hi"),
        assistant(json!("end_turn"), "ok"),
        recap.clone(),
    ]);
    assert_eq!(
        f.recap.as_deref(),
        Some("The login bug is fixed. Next: deploy.")
    );

    // A later request of yours: the recap no longer says where it ended.
    let f = claude_facts(&[user("hi"), recap.clone(), user("one more thing")]);
    assert_eq!(f.recap, None);

    // A tool result, a hook reminder or a command after it are not requests.
    let f = claude_facts(&[
        user("hi"),
        recap.clone(),
        tool_result(),
        user("<system-reminder>x</system-reminder>"),
        user("<command-name>/usage</command-name>"),
    ]);
    assert!(f.recap.is_some());

    // The latest recap wins.
    let second = json!({"type":"system","subtype":"away_summary","content":"Second recap."});
    let f = claude_facts(&[user("hi"), recap, second]);
    assert_eq!(f.recap.as_deref(), Some("Second recap."));
}

#[test]
fn claude_a_saved_summary_row_counts_as_a_recap() {
    let f = claude_facts(&[
        json!({"type":"summary","summary":"Refactored the parser."}),
        user("hi"),
        assistant(json!("end_turn"), "ok"),
    ]);
    // A request follows it: not a recap of where the session ended.
    assert_eq!(f.recap, None);
    let f = claude_facts(&[
        user("hi"),
        assistant(json!("end_turn"), "ok"),
        json!({"type":"summary","summary":"Refactored the parser."}),
    ]);
    assert_eq!(f.recap.as_deref(), Some("Refactored the parser."));
}

#[test]
fn claude_a_recap_that_holds_a_secret_is_hidden_in_the_cache_too() {
    let c = tempfile::tempdir().unwrap();
    write_lines(
        &claude_file(c.path(), ID),
        &[
            user("hi"),
            json!({"type":"system","subtype":"away_summary",
                   "content":"Used the key sk-ant-abcdEFGH1234567890xyz to deploy."}),
        ],
    );
    let state = tempfile::tempdir().unwrap();
    let cache_file = state.path().join("transcripts.json");
    let mut cache = Cache::load(&cache_file);
    let found = transcripts::scan_claude(c.path(), &mut cache);
    cache.save(&cache_file).unwrap();
    assert_eq!(
        found[0].meta.facts.recap.as_deref(),
        Some(herdr_wake::facts::HIDDEN_SUMMARY)
    );
    assert!(!std::fs::read_to_string(&cache_file)
        .unwrap()
        .contains("sk-ant-abcdEFGH"));
}

#[test]
fn claude_a_huge_line_does_not_stop_the_read() {
    let c = tempfile::tempdir().unwrap();
    let path = claude_file(c.path(), ID);
    write_lines(&path, &[user("hi")]);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    // A 40 MB line is skipped whole (the limit is 32 MB), then the answer.
    f.write_all(b"{\"type\":\"user\",\"x\":\"").unwrap();
    f.write_all(&vec![b'a'; 40 << 20]).unwrap();
    f.write_all(b"\"}\n").unwrap();
    writeln!(f, "{}", assistant(json!("end_turn"), "done")).unwrap();
    drop(f);
    let found = transcripts::scan_claude(c.path(), &mut Cache::new());
    assert_eq!(
        found[0].meta.facts.conversation,
        Some(Conversation::Finished)
    );
}

#[test]
fn the_full_read_is_cached_with_the_title_read() {
    let c = tempfile::tempdir().unwrap();
    let path = claude_file(c.path(), ID);
    write_lines(&path, &[user("hi"), assistant(json!("end_turn"), "done")]);
    let state = tempfile::tempdir().unwrap();
    let cache_file = state.path().join("transcripts.json");
    let mut cache = Cache::load(&cache_file);
    transcripts::scan_claude(c.path(), &mut cache);
    cache.save(&cache_file).unwrap();

    // Same size and time: the cached facts are used even if the content differs.
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    let same_size = std::fs::read_to_string(&path)
        .unwrap()
        .replace("end_turn", "tool_use");
    std::fs::write(&path, same_size).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(before)
        .unwrap();
    let mut cache = Cache::load(&cache_file);
    let found = transcripts::scan_claude(c.path(), &mut cache);
    assert_eq!(
        found[0].meta.facts.conversation,
        Some(Conversation::Finished),
        "unchanged file stat: no second read"
    );

    // A changed file is read again.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("{}\n", user("next")).as_bytes())
        .unwrap();
    let found = transcripts::scan_claude(c.path(), &mut cache);
    assert_eq!(
        found[0].meta.facts.conversation,
        Some(Conversation::Unanswered)
    );
}

// ------------------------------------------------------------------- Codex

fn rollout(home: &Path, id: &str) -> PathBuf {
    home.join(format!(
        "sessions/2026/11/24/rollout-2026-11-24T14-13-08-{id}.jsonl"
    ))
}

fn codex_meta(id: &str) -> serde_json::Value {
    json!({"timestamp":"2026-11-24T14:13:08.262Z","type":"session_meta",
           "payload":{"id":id,"timestamp":"2026-11-24T14:13:08.000Z","cwd":"/work/app",
                      "originator":"codex-tui","source":"cli","thread_source":"user",
                      "git":{"branch":"codex/topic","commit_hash":"abc"}}})
}

fn codex_item(kind: &str) -> serde_json::Value {
    json!({"type":"response_item","payload":{"type":kind}})
}

fn codex_text(role: &str, text: &str) -> serde_json::Value {
    let kind = if role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    json!({"type":"response_item","payload":{"type":"message","role":role,
           "content":[{"type":kind,"text":text}]}})
}

fn codex_event(kind: &str, ms: Option<u64>) -> serde_json::Value {
    let mut p = json!({"type":kind});
    if let Some(ms) = ms {
        p["duration_ms"] = json!(ms);
    }
    json!({"type":"event_msg","payload":p})
}

fn codex_facts(rows: &[serde_json::Value]) -> Facts {
    let h = tempfile::tempdir().unwrap();
    write_lines(&rollout(h.path(), ID), rows);
    let found = transcripts::scan_codex(h.path(), &mut Cache::new());
    assert_eq!(found.len(), 1, "the session is listed");
    found[0].meta.facts.clone()
}

fn codex_session(tail: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut rows = vec![
        codex_meta(ID),
        json!({"type":"turn_context","payload":{"model":"gpt-6","cwd":"/work/app"}}),
        codex_text(
            "user",
            "<environment_context><cwd>/work/app</cwd></environment_context>",
        ),
        codex_text("user", "add a test"),
    ];
    rows.extend(tail);
    rows
}

#[test]
fn codex_endings_and_details() {
    use Conversation::*;
    let table: Vec<(&str, Vec<serde_json::Value>, Option<Conversation>)> = vec![
        (
            "finished by task_complete",
            vec![
                codex_item("function_call"),
                codex_item("function_call_output"),
                codex_text("assistant", "done"),
                codex_event("task_complete", Some(8000)),
            ],
            Some(Finished),
        ),
        ("unanswered", vec![], Some(Unanswered)),
        (
            "mid-action",
            vec![
                codex_text("assistant", "looking"),
                codex_item("function_call"),
            ],
            Some(MidAction),
        ),
        (
            "interrupted",
            vec![
                codex_item("function_call"),
                codex_event("turn_aborted", Some(500)),
            ],
            Some(Interrupted),
        ),
        (
            "token counts and reasoning after the answer do not count",
            vec![
                codex_text("assistant", "done"),
                codex_event("token_count", None),
                codex_item("reasoning"),
                // The harness's own developer message about an abort is not an event.
                codex_text(
                    "developer",
                    "<turn_aborted>The previous turn was interrupted",
                ),
            ],
            Some(Finished),
        ),
    ];
    for (name, tail, want) in table {
        assert_eq!(
            codex_facts(&codex_session(tail)).conversation,
            want,
            "{name}"
        );
    }
}

#[test]
fn codex_branch_models_start_and_duration() {
    let f = codex_facts(&codex_session(vec![
        codex_event("task_complete", Some(8000)),
        json!({"type":"turn_context","payload":{"model":"gpt-6-mini"}}),
        codex_event("turn_aborted", Some(500)),
        codex_event("task_complete", Some(1500)),
    ]));
    assert_eq!(f.branch.as_deref(), Some("codex/topic"));
    assert_eq!(f.models, vec!["gpt-6", "gpt-6-mini"]);
    assert_eq!(f.started_ms, Some(1_795_529_588_000));
    assert_eq!(f.duration_ms, Some(10_000));
    assert_eq!(f.cost_usd, None);
    assert_eq!(f.lines_added, None);
}

#[test]
fn codex_without_git_or_turns_has_no_details() {
    let mut meta = codex_meta(ID);
    meta["payload"].as_object_mut().unwrap().remove("git");
    let f = codex_facts(&[meta, codex_text("user", "hello")]);
    assert_eq!(f.branch, None);
    assert_eq!(f.duration_ms, None);
    assert!(f.models.is_empty());
}

// ---------------------------------------------------------------------- Pi

fn pi_file(root: &Path, id: &str) -> PathBuf {
    root.join(format!("--work-app--/2026-11-13T12-23-15-611Z_{id}.jsonl"))
}

fn pi_header(id: &str) -> serde_json::Value {
    json!({"type":"session","version":3,"id":id,"timestamp":"2026-11-13T12:23:15.611Z",
           "cwd":"/work/app"})
}

fn pi_msg(role: &str, extra: serde_json::Value, text: &str) -> serde_json::Value {
    let mut m = json!({"role":role,"content":[{"type":"text","text":text}]});
    for (k, v) in extra.as_object().unwrap() {
        m[k] = v.clone();
    }
    json!({"type":"message","id":"m","message":m})
}

fn pi_session() -> Vec<serde_json::Value> {
    vec![
        pi_header(PI_ID),
        json!({"type":"model_change","provider":"local","modelId":"qwen-coder"}),
        json!({"type":"thinking_level_change","thinkingLevel":"off"}),
        json!({"type":"message","message":{"role":"system","content":"","sections":{}}}),
        pi_msg("user", json!({}), "list the files"),
        pi_msg(
            "assistant",
            json!({"model":"qwen-coder","stopReason":"toolUse","durationMs":600,
                   "usage":{"cost":{"total":0.25}}}),
            "running ls",
        ),
        pi_msg("toolResult", json!({}), "a b c"),
        pi_msg(
            "assistant",
            json!({"model":"qwen-coder-2","stopReason":"stop","durationMs":400,
                   "usage":{"cost":{"total":0.25}}}),
            "a, b and c",
        ),
    ]
}

fn pi_found(rows: &[serde_json::Value]) -> Vec<transcripts::Found> {
    let root = tempfile::tempdir().unwrap();
    write_lines(&pi_file(root.path(), PI_ID), rows);
    transcripts::scan_pi(root.path(), &mut Cache::new())
}

#[test]
fn pi_session_files_are_read_for_title_folder_and_details() {
    let found = pi_found(&pi_session());
    assert_eq!(found.len(), 1);
    let m = &found[0].meta;
    assert_eq!(m.agent, AgentKind::Pi);
    assert_eq!(m.session_id, PI_ID);
    assert_eq!(m.cwd, Some(PathBuf::from("/work/app")));
    assert_eq!(m.title(), Some("list the files"));
    assert_eq!(m.facts.conversation, Some(Conversation::Finished));
    assert_eq!(m.facts.models, vec!["qwen-coder", "qwen-coder-2"]);
    assert_eq!(m.facts.cost_usd, Some(0.5));
    assert_eq!(m.facts.duration_ms, Some(1000));
    assert_eq!(m.facts.started_ms, Some(1_794_572_595_611));
}

#[test]
fn pi_a_session_name_beats_the_first_prompt_and_the_last_name_wins() {
    let mut rows = pi_session();
    rows.insert(5, json!({"type":"session_info","name":"first name"}));
    rows.push(json!({"type":"session_info","name":"  Fix the\nbuild "}));
    assert_eq!(pi_found(&rows)[0].meta.title(), Some("Fix the build"));
    // An empty name clears it.
    rows.push(json!({"type":"session_info","name":""}));
    assert_eq!(pi_found(&rows)[0].meta.title(), Some("list the files"));
}

#[test]
fn pi_endings() {
    use Conversation::*;
    let ends = |stop: &str| {
        let mut rows = pi_session();
        rows.pop();
        rows.push(pi_msg("assistant", json!({"stopReason":stop}), "x"));
        pi_found(&rows)[0].meta.facts.conversation
    };
    assert_eq!(ends("stop"), Some(Finished));
    assert_eq!(ends("toolUse"), Some(MidAction));
    assert_eq!(ends("aborted"), Some(Interrupted));
    assert_eq!(ends("error"), Some(Unanswered));
    // Your message last, then a bash command you ran yourself.
    let mut rows = pi_session();
    rows.push(pi_msg("user", json!({}), "and now?"));
    rows.push(
        json!({"type":"message","message":{"role":"bashExecution","command":"ls","output":""}}),
    );
    assert_eq!(pi_found(&rows)[0].meta.facts.conversation, Some(Unanswered));
}

#[test]
fn pi_files_that_are_not_sessions_are_not_listed() {
    // No header, a wrong header, an id that is not safe, a relative folder.
    for first in [
        json!({"type":"message","message":{"role":"user","content":"hi"}}),
        json!({"type":"session","id":PI_ID}),
        json!({"type":"session","id":"../../etc/passwd","cwd":"/work/app"}),
        json!({"type":"session","id":PI_ID,"cwd":"work/app"}),
        json!({"type":"session","id":"-rf","cwd":"/work/app"}),
    ] {
        let mut rows = pi_session();
        rows[0] = first.clone();
        assert!(pi_found(&rows).is_empty(), "{first}");
    }
    // A file of garbage, and an empty session (opened and left).
    let root = tempfile::tempdir().unwrap();
    let f = pi_file(root.path(), PI_ID);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "not json at all\n").unwrap();
    assert!(transcripts::scan_pi(root.path(), &mut Cache::new()).is_empty());
    assert!(pi_found(&[pi_header(PI_ID)]).is_empty());
    // A missing folder.
    assert!(transcripts::scan_pi(Path::new("/no/such/pi"), &mut Cache::new()).is_empty());
}

#[test]
fn pi_folder_follows_the_environment() {
    let home = Path::new("/home/u");
    let mut env = BTreeMap::new();
    assert_eq!(
        transcripts::pi_dir(&env, home),
        home.join(".pi/agent/sessions")
    );
    env.insert("PI_CODING_AGENT_DIR".into(), "~/pi".into());
    assert_eq!(transcripts::pi_dir(&env, home), home.join("pi/sessions"));
    env.insert("PI_CODING_AGENT_SESSION_DIR".into(), "/data/pi".into());
    assert_eq!(transcripts::pi_dir(&env, home), PathBuf::from("/data/pi"));
}

// ---------------------------------------------------------------- OpenCode

/// A stand-in for `sqlite3` that answers the three kinds of query the scan
/// makes, and counts them.
struct FakeDb {
    sessions: serde_json::Value,
    messages: serde_json::Value,
    prompts: serde_json::Value,
    calls: RefCell<Vec<String>>,
    fail_messages: Cell<bool>,
}

impl FakeDb {
    fn new() -> FakeDb {
        FakeDb {
            sessions: json!([{
                "id": OC_ID, "directory": "/work/app", "title": "New session - 2026-11-11T01:18:46.527Z",
                "cost": 0.4, "added": 12, "removed": 3, "time_created": 1_794_359_926_527u64,
                "time_updated": 1_794_359_934_770u64
            }]),
            messages: json!([
                {"session_id": OC_ID, "role": "user", "finish": null, "error": null, "model": null,
                 "created": 1000, "completed": null},
                {"session_id": OC_ID, "role": "assistant", "finish": "tool-calls", "error": null,
                 "model": "qwen3-coder", "created": 1100, "completed": 1600},
                {"session_id": OC_ID, "role": "assistant", "finish": "stop", "error": null,
                 "model": "qwen3-coder", "created": 1700, "completed": 2200}
            ]),
            prompts: json!([{"session_id": OC_ID, "text": "add a retry to the client"}]),
            calls: RefCell::default(),
            fail_messages: Cell::new(false),
        }
    }

    fn run(&self, q: &str) -> Result<String, String> {
        self.calls.borrow_mut().push(q.to_string());
        if q.contains("from session where parent_id is null") {
            return Ok(self.sessions.to_string());
        }
        if self.fail_messages.get() {
            return Err("sqlite3 failed".to_string());
        }
        if q.contains("from message where session_id in") {
            return Ok(self.messages.to_string());
        }
        if q.contains("limit 1) as text") {
            return Ok(self.prompts.to_string());
        }
        Err("unexpected query".to_string())
    }

    fn scan(&self, cache: &mut Cache) -> Vec<transcripts::Found> {
        let sql = |q: &str| self.run(q);
        opencode_store::scan(Path::new("/x/opencode.db"), &sql, cache)
    }
}

#[test]
fn opencode_sessions_come_from_the_database() {
    let db = FakeDb::new();
    let found = db.scan(&mut Cache::new());
    assert_eq!(found.len(), 1);
    let m = &found[0].meta;
    assert_eq!(m.agent, AgentKind::OpenCode);
    assert_eq!(m.session_id, OC_ID);
    assert_eq!(m.cwd, Some(PathBuf::from("/work/app")));
    // OpenCode's placeholder name is not a title; the first prompt is.
    assert_eq!(m.title(), Some("add a retry to the client"));
    assert_eq!(found[0].mtime_ms, 1_794_359_934_770);
    let f = &m.facts;
    assert_eq!(f.conversation, Some(Conversation::Finished));
    assert_eq!(f.models, vec!["qwen3-coder"]);
    assert_eq!(f.duration_ms, Some(1000));
    assert_eq!(f.cost_usd, Some(0.4));
    assert_eq!((f.lines_added, f.lines_removed), (Some(12), Some(3)));
    assert_eq!(f.started_ms, Some(1_794_359_926_527));
}

#[test]
fn opencode_a_real_title_is_kept_and_free_models_have_no_cost() {
    let mut db = FakeDb::new();
    db.sessions[0]["title"] = json!("Retry logic for the HTTP client");
    db.sessions[0]["cost"] = json!(0.0);
    db.sessions[0]["added"] = json!(0);
    db.sessions[0]["removed"] = json!(0);
    let found = db.scan(&mut Cache::new());
    let m = &found[0].meta;
    assert_eq!(m.title(), Some("Retry logic for the HTTP client"));
    assert_eq!(m.facts.cost_usd, None);
    assert_eq!(m.facts.lines_added, None);
}

#[test]
fn opencode_endings() {
    use Conversation::*;
    let ends = |last: serde_json::Value| {
        let mut db = FakeDb::new();
        db.messages = json!([
            {"session_id": OC_ID, "role": "user", "created": 1},
            last
        ]);
        db.scan(&mut Cache::new())[0].meta.facts.conversation
    };
    let assistant = |finish: serde_json::Value, error: serde_json::Value| {
        json!({"session_id": OC_ID, "role": "assistant", "finish": finish, "error": error,
               "created": 2, "completed": 3})
    };
    assert_eq!(ends(assistant(json!("stop"), json!(null))), Some(Finished));
    assert_eq!(
        ends(assistant(json!("tool-calls"), json!(null))),
        Some(MidAction)
    );
    assert_eq!(ends(assistant(json!(null), json!(null))), Some(MidAction));
    assert_eq!(
        ends(assistant(json!(null), json!("MessageAbortedError"))),
        Some(Interrupted)
    );
    assert_eq!(
        ends(assistant(json!(null), json!("APIError"))),
        Some(Unanswered)
    );
    // Only your message.
    let mut db = FakeDb::new();
    db.messages = json!([{"session_id": OC_ID, "role": "user", "created": 1}]);
    assert_eq!(
        db.scan(&mut Cache::new())[0].meta.facts.conversation,
        Some(Unanswered)
    );
}

#[test]
fn opencode_a_session_is_read_again_only_when_it_changed() {
    let db = FakeDb::new();
    let mut cache = Cache::new();
    db.scan(&mut cache);
    let first = db.calls.borrow().len();
    assert_eq!(first, 3, "listing, messages, prompts");
    db.calls.borrow_mut().clear();
    let again = db.scan(&mut cache);
    assert_eq!(again.len(), 1);
    assert_eq!(db.calls.borrow().len(), 1, "only the listing");

    // A new update time: read again.
    let mut changed = FakeDb::new();
    changed.sessions[0]["time_updated"] = json!(1_794_359_999_999u64);
    changed.messages = json!([{"session_id": OC_ID, "role": "user", "created": 1}]);
    let found = changed.scan(&mut cache);
    assert_eq!(
        found[0].meta.facts.conversation,
        Some(Conversation::Unanswered)
    );
}

#[test]
fn opencode_a_failed_read_is_not_remembered() {
    let mut db = FakeDb::new();
    db.sessions[0]["title"] = json!("Retry logic");
    db.fail_messages.set(true);
    let mut cache = Cache::new();
    let found = db.scan(&mut cache);
    // Still listed from the session row, with nothing known about the end.
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.facts.conversation, None);
    db.fail_messages.set(false);
    let found = db.scan(&mut cache);
    assert_eq!(
        found[0].meta.facts.conversation,
        Some(Conversation::Finished)
    );
}

#[test]
fn opencode_rows_that_are_not_safe_to_use_are_skipped() {
    let mut db = FakeDb::new();
    db.sessions = json!([
        {"id": "ses_ok1234567890", "directory": "/work/app", "title": "ok", "time_updated": 5},
        {"id": "ses_x'; drop table session; --", "directory": "/work/app", "title": "bad id", "time_updated": 5},
        {"id": "ses_relative12345", "directory": "work/app", "title": "relative", "time_updated": 5},
        {"id": "ses_notime1234567", "directory": "/work/app", "title": "no time"}
    ]);
    let found = db.scan(&mut Cache::new());
    let ids: Vec<&str> = found.iter().map(|f| f.meta.session_id.as_str()).collect();
    assert_eq!(ids, vec!["ses_ok1234567890"]);
    for q in db.calls.borrow().iter() {
        assert!(!q.contains("drop table"), "{q}");
    }
}

#[test]
fn opencode_without_sqlite_gives_no_rows() {
    let sql = |_: &str| Err::<String, _>("sqlite3 is not installed".to_string());
    assert!(opencode_store::scan(Path::new("/x/opencode.db"), &sql, &mut Cache::new()).is_empty());
}

#[test]
fn opencode_database_path_follows_the_environment() {
    let home = Path::new("/home/u");
    let mut env = BTreeMap::new();
    assert_eq!(
        opencode_store::db_path(&env, home),
        home.join(".local/share/opencode/opencode.db")
    );
    env.insert("XDG_DATA_HOME".into(), "/data".into());
    assert_eq!(
        opencode_store::db_path(&env, home),
        PathBuf::from("/data/opencode/opencode.db")
    );
    env.insert("OPENCODE_DB".into(), "other.db".into());
    assert_eq!(
        opencode_store::db_path(&env, home),
        PathBuf::from("/data/opencode/other.db")
    );
    env.insert("OPENCODE_DB".into(), "/elsewhere/oc.db".into());
    assert_eq!(
        opencode_store::db_path(&env, home),
        PathBuf::from("/elsewhere/oc.db")
    );
}

/// The real `sqlite3` on a real database made with the schema OpenCode 1.17
/// uses (the columns that matter): sub-agent and archived sessions are left
/// out, text parts are read, synthetic ones are not. Skipped when there is
/// no `sqlite3` on this machine.
#[test]
fn opencode_with_the_real_sqlite3() {
    let mut env = BTreeMap::new();
    env.insert(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_default(),
    );
    if herdr_wake::agents::on_path("sqlite3", &env).is_none() {
        eprintln!("skipped: no sqlite3 on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    let setup = format!(
        "create table session (id text primary key, project_id text, parent_id text, \
           directory text not null, title text not null, cost real default 0 not null, \
           summary_additions integer, summary_deletions integer, time_created integer not null, \
           time_updated integer not null, time_archived integer);\n\
         create table message (id text primary key, session_id text not null, time_created integer not null, \
           time_updated integer not null, data text not null);\n\
         create table part (id text primary key, message_id text not null, session_id text not null, \
           time_created integer not null, time_updated integer not null, data text not null);\n\
         insert into session values ('{OC_ID}', 'p', null, '/work/app', 'Retry logic', 0.5, 10, 2, 1000, 5000, null);\n\
         insert into session values ('ses_child1234567', 'p', '{OC_ID}', '/work/app', 'Sub-agent', 0, 0, 0, 1000, 5000, null);\n\
         insert into session values ('ses_archived12345', 'p', null, '/work/app', 'Archived', 0, 0, 0, 1000, 5000, 4000);\n\
         insert into message values ('m1', '{OC_ID}', 1000, 1000, '{{\"role\":\"user\",\"time\":{{\"created\":1000}}}}');\n\
         insert into message values ('m2', '{OC_ID}', 1100, 1200, '{{\"role\":\"assistant\",\"modelID\":\"qwen3-coder\",\"finish\":\"stop\",\"time\":{{\"created\":1100,\"completed\":1900}}}}');\n\
         insert into part values ('p1', 'm1', '{OC_ID}', 1000, 1000, '{{\"type\":\"text\",\"text\":\"add a retry\"}}');\n\
         insert into part values ('p2', 'm1', '{OC_ID}', 1001, 1001, '{{\"type\":\"text\",\"text\":\"(system note)\",\"synthetic\":true}}');\n\
         insert into part values ('p3', 'm2', '{OC_ID}', 1100, 1100, '{{\"type\":\"text\",\"text\":\"Added it.\"}}');\n\
         insert into part values ('p4', 'm2', '{OC_ID}', 1101, 1101, '{{\"type\":\"step-finish\"}}');\n"
    );
    let out = std::process::Command::new("sqlite3")
        .arg(&db)
        .arg(setup)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let sql = |q: &str| opencode_store::run_sqlite3(&env, &db, q);
    let found = opencode_store::scan(&db, &sql, &mut Cache::new());
    assert_eq!(found.len(), 1, "no sub-agent, no archived session");
    let m = &found[0].meta;
    assert_eq!(m.session_id, OC_ID);
    assert_eq!(m.title(), Some("Retry logic"));
    assert_eq!(m.prompt.as_deref(), Some("add a retry"));
    assert_eq!(m.facts.conversation, Some(Conversation::Finished));
    assert_eq!(m.facts.models, vec!["qwen3-coder"]);
    assert_eq!(m.facts.duration_ms, Some(800));
    assert_eq!(m.facts.cost_usd, Some(0.5));
    assert_eq!(m.facts.lines_added, Some(10));

    let said = opencode_store::conversation(&sql, OC_ID).unwrap();
    let texts: Vec<&str> = said.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(texts, vec!["add a retry", "Added it."]);
    assert_eq!(opencode_store::updated_ms(&sql, OC_ID).unwrap(), Some(5000));
    assert_eq!(
        opencode_store::updated_ms(&sql, "ses_nothere123456").unwrap(),
        None
    );
    assert!(opencode_store::updated_ms(&sql, "x'; select 1; --").is_err());
}

#[test]
fn run_sqlite3_says_why_it_cannot() {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/nonexistent".to_string());
    let db = Path::new("/tmp/none.db");
    assert_eq!(
        opencode_store::run_sqlite3(&env, db, "select 1").unwrap_err(),
        "sqlite3 is not installed"
    );
}

// ------------------------------------------------- through the listing

struct SqlTools {
    db: FakeDb,
}

impl Tools for SqlTools {
    fn herdr_app_ls(&self) -> Option<String> {
        None
    }
    fn hc(&self, _: &[String]) -> Result<String, String> {
        Ok(String::new())
    }
    fn codex_open_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn sqlite(&self, _db: &Path, query: &str) -> Result<String, String> {
        self.db.run(query)
    }
}

fn world() -> (Fixture, tempfile::TempDir) {
    let (mut f, dir) = Fixture::with_temp_home();
    // A session file written in the last two minutes counts as open.
    f.platform.set_now(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    );
    f.env.insert(
        "PATH".into(),
        agent_bin(AgentKind::Claude)
            .parent()
            .unwrap()
            .display()
            .to_string(),
    );
    (f, dir)
}

fn age_all(path: &Path, ms_ago: u64) {
    let then = std::time::SystemTime::now() - std::time::Duration::from_millis(ms_ago);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(then)
        .unwrap();
}

const DAY: u64 = 86_400_000;

fn collect_all(f: &mut Fixture, db: FakeDb) -> Vec<rs::Row> {
    let tools = SqlTools { db };
    rs::collect(&mut f.ctx(), &tools).rows
}

#[test]
fn the_listing_shows_the_details_of_every_agent() {
    let (mut f, dir) = world();
    let home = dir.path();
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path();

    // Claude, ended mid-work, with a cost record and a pull request.
    let claude = home
        .join(".claude/projects")
        .join(herdr_wake::agents::claude::project_dir_name(cwd))
        .join(format!("{ID}.jsonl"));
    write_lines(
        &claude,
        &[
            json!({"type":"user","cwd":cwd,"entrypoint":"cli","gitBranch":"feat/login",
                   "message":{"role":"user","content":"fix the login bug"}}),
            tool_use(),
            json!({"type":"cost-state","totalCostUSD":2.41,"totalDuration":47_000,
                   "startTime":1_794_369_857_849u64,"totalLinesAdded":312,"totalLinesRemoved":58,
                   "modelUsage":{"claude-opus-5-5":{}}}),
            json!({"type":"pr-link","prNumber":128,"prUrl":"https://github.com/acme/webshop/pull/128",
                   "prRepository":"acme/webshop"}),
        ],
    );
    age_all(&claude, 10 * DAY);

    // Codex, finished.
    let rollout_path = rollout(&home.join(".codex"), ID2);
    let mut meta = codex_meta(ID2);
    meta["payload"]["cwd"] = json!(cwd);
    write_lines(
        &rollout_path,
        &[
            meta,
            json!({"type":"turn_context","payload":{"model":"gpt-6"}}),
            codex_text("user", "add a test"),
            codex_text("assistant", "added"),
            codex_event("task_complete", Some(9000)),
        ],
    );
    age_all(&rollout_path, 11 * DAY);

    // Pi, finished.
    let pi_path = pi_file(&home.join(".pi/agent/sessions"), PI_ID);
    let mut rows = pi_session();
    rows[0]["cwd"] = json!(cwd);
    write_lines(&pi_path, &rows);
    age_all(&pi_path, 12 * DAY);

    // OpenCode, from the database.
    let mut db = FakeDb::new();
    db.sessions[0]["directory"] = json!(cwd);
    db.sessions[0]["time_updated"] = json!(1_700_000_000_000u64);

    let rows = collect_all(&mut f, db);
    let by = |agent: AgentKind| rows.iter().find(|r| r.agent() == agent).unwrap();
    assert_eq!(rows.len(), 4);

    let (shown, _) = rs::select(&rows, &Filter::default(), 20);
    let j = rs::list_json(&shown, 0);
    let sessions = j["sessions"].as_array().unwrap();
    let get = |agent: &str| {
        sessions
            .iter()
            .find(|s| s["agent"] == agent)
            .unwrap()
            .clone()
    };

    let c = get("claude");
    assert_eq!(c["conversation"], "mid-action");
    assert_eq!(c["branch"], "feat/login");
    assert_eq!(c["cost_usd"], 2.41);
    assert_eq!(c["duration_ms"], 47_000);
    assert_eq!(c["started_ms"], 1_794_369_857_849u64);
    assert_eq!(c["lines_added"], 312);
    assert_eq!(c["lines_removed"], 58);
    assert_eq!(c["models"], json!(["claude-opus-5-5"]));
    assert_eq!(
        c["prs"],
        json!([{"number":128,"url":"https://github.com/acme/webshop/pull/128","repo":"acme/webshop"}])
    );
    assert_eq!(c["summary"], serde_json::Value::Null);
    assert_eq!(c["summary_source"], serde_json::Value::Null);
    // The journal knows nothing of it: only the transcript's word.
    assert_eq!(c["ending"], "unknown");

    let x = get("codex");
    assert_eq!(x["conversation"], "finished");
    assert_eq!(x["branch"], "codex/topic");
    assert_eq!(x["models"], json!(["gpt-6"]));
    assert_eq!(x["duration_ms"], 9000);
    assert_eq!(x["cost_usd"], serde_json::Value::Null);

    let p = get("pi");
    assert_eq!(p["conversation"], "finished");
    assert_eq!(p["title"], "list the files");
    assert_eq!(p["cwd"], json!(cwd));
    assert_eq!(p["flags"], json!([]));
    assert_eq!(p["source"], "transcript");

    let o = get("opencode");
    assert_eq!(o["conversation"], "finished");
    assert_eq!(o["title"], "add a retry to the client");
    assert_eq!(o["source"], "transcript");
    assert_eq!(o["transcript_gone"], false);
    assert_eq!(by(AgentKind::OpenCode).transcript, Transcript::Unknown);
    assert!(by(AgentKind::OpenCode).entry.session_file.is_none());
    assert!(by(AgentKind::Pi).entry.session_file.is_some());

    // The table's ENDED column says how it stopped.
    let table = rs::render_list(&shown, shown.len(), 0, home, false);
    let line = |needle: &str| table.lines().find(|l| l.contains(needle)).unwrap();
    assert!(
        line("fix the login bug").contains("stopped mid-work"),
        "{table}"
    );
    assert!(line("add a test").contains("finished"), "{table}");
}

#[test]
fn a_pi_or_opencode_session_that_left_only_its_own_files_is_resumed_with_no_extra_flags() {
    let (mut f, dir) = world();
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path();
    let pi_path = pi_file(&dir.path().join(".pi/agent/sessions"), PI_ID);
    let mut rows = pi_session();
    rows[0]["cwd"] = json!(cwd);
    write_lines(&pi_path, &rows);
    age_all(&pi_path, 3 * DAY);
    let mut db = FakeDb::new();
    db.sessions[0]["directory"] = json!(cwd);
    db.sessions[0]["time_updated"] = json!(1_700_000_000_000u64);
    let rows = collect_all(&mut f, db);

    for (agent, id) in [(AgentKind::Pi, PI_ID), (AgentKind::OpenCode, OC_ID)] {
        let row = rows.iter().find(|r| r.agent() == agent).unwrap();
        assert!(
            row.entry.flags.is_empty(),
            "{agent}: no flags from a transcript"
        );
        let launch =
            rs::prepare(row, &mut f.ctx(), false).unwrap_or_else(|e| panic!("{agent}: {e}"));
        let words = &launch.argv[1..];
        match agent {
            AgentKind::Pi => assert_eq!(words, ["--session", pi_path.to_str().unwrap()]),
            _ => assert_eq!(words, ["--session", id]),
        }
        assert_eq!(launch.cwd, cwd);
    }
}

#[test]
fn a_journal_row_gets_the_details_of_its_transcript_too() {
    use herdr_wake::core::TerminalId;
    use herdr_wake::history::{self, Ending};
    use herdr_wake::paths;
    use herdr_wake::record::Record;
    let (mut f, dir) = world();
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    f.platform.set_now(now);
    let claude = dir
        .path()
        .join(".claude/projects")
        .join(herdr_wake::agents::claude::project_dir_name(cwd))
        .join(format!("{ID}.jsonl"));
    write_lines(
        &claude,
        &[
            json!({"type":"user","cwd":cwd,"entrypoint":"cli","message":{"role":"user","content":"hello"}}),
            assistant(json!("end_turn"), "hi there"),
        ],
    );
    age_all(&claude, 10 * DAY);
    let rec = Record::new(
        AgentKind::Claude,
        ID,
        cwd.to_path_buf(),
        agent_bin(AgentKind::Claude),
        "b",
    );
    let hist = paths::history_dir(&f.env, &f.home);
    history::started(&hist, 500, &TerminalId::from("t1"), &rec, now - 5 * DAY).unwrap();
    history::ended(
        &hist,
        500,
        AgentKind::Claude,
        ID,
        None,
        Ending::Quit,
        now - 4 * DAY,
    )
    .unwrap();

    let rows = collect_all(&mut f, FakeDb::new());
    let row = rows.iter().find(|r| r.id() == ID).unwrap();
    assert_eq!(row.source, rs::Source::Wake);
    assert_eq!(row.extra.facts.conversation, Some(Conversation::Finished));
    assert_eq!(row.extra.path.as_deref(), Some(claude.as_path()));
    let (shown, _) = rs::select(&rows, &Filter::default(), 20);
    let table = rs::render_list(&shown, shown.len(), now, dir.path(), false);
    assert!(table.contains("quit, finished"), "{table}");
}

#[test]
fn the_titles_and_details_of_a_cached_run_survive_a_second_listing() {
    let (mut f, dir) = world();
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path();
    let pi_path = pi_file(&dir.path().join(".pi/agent/sessions"), PI_ID);
    let mut rows = pi_session();
    rows[0]["cwd"] = json!(cwd);
    write_lines(&pi_path, &rows);
    age_all(&pi_path, 3 * DAY);
    let first = collect_all(&mut f, FakeDb::new());
    let second = collect_all(&mut f, FakeDb::new());
    let a = first.iter().find(|r| r.agent() == AgentKind::Pi).unwrap();
    let b = second.iter().find(|r| r.agent() == AgentKind::Pi).unwrap();
    assert_eq!(a.extra.facts, b.extra.facts);
    assert_eq!(b.extra.facts.conversation, Some(Conversation::Finished));
}
