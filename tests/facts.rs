//! The rules that say how a conversation ended, for every agent, and the
//! small helpers around them.

use herdr_wake::facts::{self, Conversation, Role};
use herdr_wake::history::Ending;
use serde_json::json;

fn user(content: serde_json::Value) -> serde_json::Value {
    json!({"type":"user","message":{"role":"user","content":content}})
}

fn assistant(stop: serde_json::Value) -> serde_json::Value {
    json!({"type":"assistant","message":{"role":"assistant","stop_reason":stop,
           "content":[{"type":"text","text":"done"}]}})
}

// ------------------------------------------------------------------ Claude

#[test]
fn claude_a_complete_answer_is_finished() {
    for stop in ["end_turn", "stop_sequence"] {
        assert_eq!(
            facts::claude_ending(&assistant(json!(stop))),
            Some(Conversation::Finished)
        );
    }
}

#[test]
fn claude_an_answer_that_asks_for_a_tool_or_never_finished_is_mid_action() {
    for stop in [json!("tool_use"), json!(null), json!("max_tokens")] {
        assert_eq!(
            facts::claude_ending(&assistant(stop)),
            Some(Conversation::MidAction)
        );
    }
    // No stop_reason at all.
    let v = json!({"type":"assistant","message":{"role":"assistant","content":[]}});
    assert_eq!(facts::claude_ending(&v), Some(Conversation::MidAction));
}

#[test]
fn claude_your_message_without_an_answer_is_unanswered() {
    assert_eq!(
        facts::claude_ending(&user(json!("fix the bug"))),
        Some(Conversation::Unanswered)
    );
    assert_eq!(
        facts::claude_ending(&user(json!([{"type":"text","text":"fix the bug"}]))),
        Some(Conversation::Unanswered)
    );
}

#[test]
fn claude_an_interrupt_is_interrupted() {
    assert_eq!(
        facts::claude_ending(&user(
            json!([{"type":"text","text":"[Request interrupted by user]"}])
        )),
        Some(Conversation::Interrupted)
    );
    assert_eq!(
        facts::claude_ending(&user(json!("[Request interrupted by user for tool use]"))),
        Some(Conversation::Interrupted)
    );
}

#[test]
fn claude_a_tool_result_is_mid_action() {
    let v = user(json!([{"type":"tool_result","tool_use_id":"x","content":"ok"}]));
    assert_eq!(facts::claude_ending(&v), Some(Conversation::MidAction));
}

#[test]
fn claude_what_is_not_the_conversation_does_not_count() {
    // Meta rows, sub-agents and compaction summaries.
    for key in ["isMeta", "isSidechain", "isCompactSummary"] {
        let mut v = user(json!("hello"));
        v[key] = json!(true);
        assert_eq!(facts::claude_ending(&v), None, "{key}");
        let mut a = assistant(json!("end_turn"));
        a[key] = json!(true);
        assert_eq!(facts::claude_ending(&a), None, "{key}");
    }
    // Reminder-only, command and notification messages, empty text, bookkeeping.
    assert_eq!(
        facts::claude_ending(&user(json!(
            "<system-reminder>be careful</system-reminder>"
        ))),
        None
    );
    assert_eq!(
        facts::claude_ending(&user(json!("<command-name>/clear</command-name>"))),
        None
    );
    assert_eq!(
        facts::claude_ending(&user(json!(
            "<local-command-stdout>x</local-command-stdout>"
        ))),
        None
    );
    assert_eq!(facts::claude_ending(&user(json!("   "))), None);
    assert_eq!(
        facts::claude_ending(&json!({"type":"system","subtype":"turn_duration"})),
        None
    );
    assert_eq!(
        facts::claude_ending(&json!({"type":"summary","summary":"x"})),
        None
    );
}

#[test]
fn claude_a_reminder_in_front_of_your_words_does_not_hide_them() {
    let v = user(json!(
        "<system-reminder>one</system-reminder>\nplease continue"
    ));
    assert_eq!(facts::claude_ending(&v), Some(Conversation::Unanswered));
    let (role, text) = facts::claude_message(&v).unwrap();
    assert_eq!(role, Role::User);
    assert_eq!(text, "please continue");
}

#[test]
fn claude_messages_for_a_summary_are_words_only() {
    assert!(facts::claude_message(&user(json!(
        [{"type":"tool_result","content":"secret output"}]
    )))
    .is_none());
    let a = json!({"type":"assistant","message":{"content":[
        {"type":"thinking","thinking":"hmm"},
        {"type":"text","text":"Here is the fix."},
        {"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}});
    assert_eq!(
        facts::claude_message(&a),
        Some((Role::Assistant, "Here is the fix.".to_string()))
    );
    assert!(
        facts::claude_message(&user(json!("<task-notification>x</task-notification>"))).is_none()
    );
    // `<hc` is a tag, `<hcl>` is not.
    assert!(facts::claude_message(&user(json!("<hc terminal=1>x</hc>"))).is_none());
    assert!(facts::claude_message(&user(json!("a <hcl> b"))).is_some());
}

#[test]
fn a_reminder_tag_must_end_its_word() {
    assert_eq!(
        facts::strip_reminders("a <system-reminders>keep</system-reminders> b"),
        "a <system-reminders>keep</system-reminders> b"
    );
    assert_eq!(
        facts::strip_reminders("a <system-reminder>drop</system-reminder> b"),
        "a  b"
    );
    // Unterminated: stays.
    assert_eq!(
        facts::strip_reminders("a <system-reminder>never closed"),
        "a <system-reminder>never closed"
    );
}

#[test]
fn only_a_person_clears_a_recap() {
    assert!(facts::claude_is_person(&user(json!("next question"))));
    assert!(!facts::claude_is_person(&user(json!([
        {"type":"tool_result","content":"x"}
    ]))));
    let mut compact = user(json!("This session is being continued"));
    compact["isCompactSummary"] = json!(true);
    assert!(!facts::claude_is_person(&compact));
}

// ------------------------------------------------------------------- Codex

fn codex_message(role: &str, kind: &str, text: &str) -> serde_json::Value {
    json!({"type":"response_item","payload":{"type":"message","role":role,
           "content":[{"type":kind,"text":text}]}})
}

#[test]
fn codex_turns_complete_or_are_aborted() {
    assert_eq!(
        facts::codex_ending(&json!({"type":"event_msg","payload":{"type":"task_complete"}})),
        Some(Conversation::Finished)
    );
    assert_eq!(
        facts::codex_ending(&json!({"type":"event_msg","payload":{"type":"turn_aborted"}})),
        Some(Conversation::Interrupted)
    );
    assert_eq!(
        facts::codex_ending(&json!({"type":"event_msg","payload":{"type":"token_count"}})),
        None
    );
}

#[test]
fn codex_tool_calls_and_their_output_are_mid_action() {
    for kind in [
        "function_call",
        "function_call_output",
        "local_shell_call",
        "custom_tool_call",
        "custom_tool_call_output",
    ] {
        assert_eq!(
            facts::codex_ending(&json!({"type":"response_item","payload":{"type":kind}})),
            Some(Conversation::MidAction),
            "{kind}"
        );
    }
    // Reasoning is not a conversation event.
    assert_eq!(
        facts::codex_ending(&json!({"type":"response_item","payload":{"type":"reasoning"}})),
        None
    );
}

#[test]
fn codex_messages_end_as_the_agent_or_you_spoke_last() {
    assert_eq!(
        facts::codex_ending(&codex_message("assistant", "output_text", "all done")),
        Some(Conversation::Finished)
    );
    assert_eq!(
        facts::codex_ending(&codex_message("user", "input_text", "do the thing")),
        Some(Conversation::Unanswered)
    );
}

#[test]
fn codex_context_in_user_messages_is_not_yours() {
    for ctx in [
        "<environment_context><cwd>/x</cwd></environment_context>",
        "# AGENTS.md instructions for /x\n\nbe nice",
        "<turn_aborted>\nthe previous turn was interrupted",
    ] {
        assert_eq!(
            facts::codex_ending(&codex_message("user", "input_text", ctx)),
            None,
            "{ctx}"
        );
        assert!(facts::codex_message(&codex_message("user", "input_text", ctx)).is_none());
    }
    // The developer role is the harness.
    assert_eq!(
        facts::codex_ending(&codex_message("developer", "input_text", "rules")),
        None
    );
    // IDE context in front of the request: the request counts.
    let ide = "# Context from my IDE setup:\n\n## Open tabs\n- a.rs\n\n## My request for Codex:\nrename the function";
    assert_eq!(
        facts::codex_message(&codex_message("user", "input_text", ide)),
        Some((Role::User, "rename the function".to_string()))
    );
    // IDE context without a request is nothing.
    assert!(facts::codex_message(&codex_message(
        "user",
        "input_text",
        "# Context from my IDE setup:\n\n## Open tabs\n- a.rs"
    ))
    .is_none());
}

// ---------------------------------------------------------------------- Pi

fn pi(role: &str, extra: serde_json::Value, text: &str) -> serde_json::Value {
    let mut m = json!({"role":role,"content":[{"type":"text","text":text}]});
    if let Some(o) = extra.as_object() {
        for (k, v) in o {
            m[k] = v.clone();
        }
    }
    json!({"type":"message","message":m})
}

#[test]
fn pi_stop_reasons_map_to_endings() {
    let end = |reason: &str| facts::pi_ending(&pi("assistant", json!({"stopReason":reason}), "x"));
    assert_eq!(end("stop"), Some(Conversation::Finished));
    assert_eq!(end("length"), Some(Conversation::Finished));
    assert_eq!(end("toolUse"), Some(Conversation::MidAction));
    assert_eq!(end("aborted"), Some(Conversation::Interrupted));
    assert_eq!(end("error"), Some(Conversation::Unanswered));
    assert_eq!(
        facts::pi_ending(&pi("user", json!({}), "hello")),
        Some(Conversation::Unanswered)
    );
    assert_eq!(
        facts::pi_ending(&pi("toolResult", json!({}), "out")),
        Some(Conversation::MidAction)
    );
}

#[test]
fn pi_commands_you_ran_and_extension_rows_do_not_count() {
    assert_eq!(
        facts::pi_ending(
            &json!({"type":"message","message":{"role":"bashExecution","command":"ls"}})
        ),
        None
    );
    assert_eq!(
        facts::pi_ending(&json!({"type":"message","message":{"role":"system","content":""}})),
        None
    );
    assert_eq!(
        facts::pi_ending(&json!({"type":"custom_message","content":"memory"})),
        None
    );
    assert_eq!(facts::pi_ending(&pi("user", json!({}), "   ")), None);
}

// ---------------------------------------------------------------- OpenCode

#[test]
fn opencode_messages_end_by_their_finish_reason_and_error() {
    use facts::opencode_ending as e;
    assert_eq!(e(Some("user"), None, None), Some(Conversation::Unanswered));
    assert_eq!(
        e(Some("assistant"), Some("stop"), None),
        Some(Conversation::Finished)
    );
    assert_eq!(
        e(Some("assistant"), Some("tool-calls"), None),
        Some(Conversation::MidAction)
    );
    assert_eq!(
        e(Some("assistant"), None, None),
        Some(Conversation::MidAction)
    );
    assert_eq!(
        e(Some("assistant"), None, Some("MessageAbortedError")),
        Some(Conversation::Interrupted)
    );
    assert_eq!(
        e(Some("assistant"), None, Some("APIError")),
        Some(Conversation::Unanswered)
    );
    assert_eq!(e(Some("tool"), None, None), None);
    assert_eq!(e(None, None, None), None);
}

// ----------------------------------------------------------------- the rest

#[test]
fn the_ended_column_shows_the_most_useful_of_the_two() {
    use Conversation::*;
    assert_eq!(
        facts::ended_label(Ending::Quit, Some(Finished)),
        "quit, finished"
    );
    assert_eq!(
        facts::ended_label(Ending::Killed, Some(MidAction)),
        "killed, mid-work"
    );
    assert_eq!(
        facts::ended_label(Ending::Quit, Some(Unanswered)),
        "quit, unanswered"
    );
    assert_eq!(
        facts::ended_label(Ending::Quit, Some(Interrupted)),
        "quit, interrupted"
    );
    assert_eq!(facts::ended_label(Ending::Quit, None), "quit");
    // A session only a transcript knows says how it stopped.
    assert_eq!(
        facts::ended_label(Ending::Unknown, Some(MidAction)),
        "stopped mid-work"
    );
    assert_eq!(
        facts::ended_label(Ending::Unknown, Some(Finished)),
        "finished"
    );
    assert_eq!(
        facts::ended_label(Ending::Open, Some(Unanswered)),
        "unanswered"
    );
    assert_eq!(facts::ended_label(Ending::Unknown, None), "unknown");
}

#[test]
fn iso_times_become_epoch_milliseconds() {
    assert_eq!(facts::epoch_ms("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(facts::epoch_ms("1970-01-01T00:00:01.5Z"), Some(1500));
    assert_eq!(
        facts::epoch_ms("2026-11-24T14:13:08.262Z"),
        Some(1_795_529_588_262)
    );
    // Without a zone it is UTC; an offset is applied.
    assert_eq!(
        facts::epoch_ms("2026-11-24T14:13:08"),
        Some(1_795_529_588_000)
    );
    assert_eq!(
        facts::epoch_ms("2026-11-24T16:13:08+02:00"),
        Some(1_795_529_588_000)
    );
    // Leap day, and a date before the epoch is not a time here.
    assert_eq!(
        facts::epoch_ms("2024-02-29T00:00:00Z"),
        Some(1_709_164_800_000)
    );
    assert_eq!(facts::epoch_ms("1969-12-31T23:59:59Z"), None);
    for bad in [
        "",
        "yesterday",
        "2026-13-01T00:00:00Z",
        "2026-11-24",
        "2026-11-24T25:00:00Z",
        "2026-11-24T14:13:08.Z",
    ] {
        assert_eq!(facts::epoch_ms(bad), None, "{bad:?}");
    }
}

#[test]
fn numbers_from_files_must_be_finite_and_not_negative() {
    assert_eq!(facts::number(Some(&json!(1.5))), Some(1.5));
    assert_eq!(facts::number(Some(&json!(-1))), None);
    assert_eq!(facts::number(Some(&json!("5"))), None);
    assert_eq!(facts::number(None), None);
    assert_eq!(facts::whole(Some(&json!(41.9))), Some(41));
    assert_eq!(facts::whole(Some(&json!(1e300))), None);
    assert_eq!(facts::whole(Some(&json!(-3))), None);
}

#[test]
fn text_from_files_loses_control_characters_and_is_cut() {
    assert_eq!(facts::clean("a\u{1b}[31mb\u{202e}c", 100), "a[31mbc");
    assert_eq!(facts::clean("abcdef", 3), "abc");
}
