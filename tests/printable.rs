//! L-10: text that comes from a record or a hook payload never reaches the
//! terminal or the log with control characters in it.

mod common;

use std::path::PathBuf;

use common::{sample_record, Fixture};
use herdr_wake::core::{Terminal, TerminalId};
use herdr_wake::record::{self, State, LABEL};
use herdr_wake::trust::printable;
use herdr_wake::{paths, report, wake};

const ESC: char = '\u{1b}';

fn has_control(s: &str) -> bool {
    s.chars()
        .any(|c| (c.is_control() && c != '\t' && c != '\n') || ('\u{80}'..='\u{9f}').contains(&c))
}

#[test]
fn printable_strips_control_characters_and_keeps_the_rest() {
    assert_eq!(
        printable("plain text, caf\u{e9} \u{1f600}"),
        "plain text, caf\u{e9} \u{1f600}"
    );
    assert_eq!(printable("a\tb"), "a\tb", "a tab stays");
    assert_eq!(printable("a\nb\rc"), "abc");
    assert_eq!(printable("red\u{1b}[31m"), "red[31m");
    assert_eq!(printable("bell\u{7}del\u{7f}nul\u{0}"), "belldelnul");
    assert_eq!(printable("c1\u{9b}31m\u{85}"), "c131m");
    assert_eq!(printable(""), "");
}

fn hostile() -> String {
    format!("/tmp/x{ESC}]0;owned\u{7}\n1700000000000 pid=1 hook claude start: recorded")
}

#[test]
fn the_give_up_line_carries_no_control_characters() {
    let mut f = Fixture::temp();
    let mut rec = sample_record();
    rec.boot_id = "boot-0".into();
    rec.cwd = PathBuf::from(hostile());
    let agent = herdr_wake::agents::get(rec.agent);
    let decision = wake::decide(Some(&rec), Some(agent.as_ref()), &mut f.ctx());
    let wake::Decision::GiveUp { reason, manual } = decision else {
        panic!("{decision:?}");
    };
    let line = wake::give_up_line(&rec, &reason, manual.as_deref());
    assert!(line.contains("folder missing"), "{line}");
    assert!(line.contains("/tmp/x"), "{line}");
    assert!(!has_control(&line) && !line.contains('\n'), "{line:?}");
}

#[test]
fn log_lines_cannot_be_forged_with_a_newline() {
    let f = Fixture::temp();
    let job = wake::ConfirmJob {
        terminal: TerminalId::from("t1"),
        session_id: "s".into(),
        within_ms: 1,
        boot_key: "k".into(),
    };
    wake::mark_unconfirmed(&f.env, &f.home, &job, &hostile(), 5);
    let log = std::fs::read_to_string(paths::log_file(&f.env, &f.home)).unwrap();
    assert_eq!(log.lines().count(), 1, "{log:?}");
    assert!(!has_control(&log), "{log:?}");
}

#[test]
fn the_status_table_carries_no_control_characters() {
    let mut f = Fixture::temp();
    let mut rec = sample_record();
    rec.cwd = PathBuf::from(hostile());
    rec.state = State::Failed { reason: hostile() };
    let mut term = Terminal::new("t1");
    term.labels
        .insert(LABEL.into(), serde_json::to_string(&rec).unwrap());
    f.core.insert(term);
    let rows = report::status(&mut f.ctx()).unwrap();
    let text = report::render_status(&rows, true);
    assert!(text.contains("/tmp/x"), "{text}");
    assert!(!has_control(&text), "{text:?}");
    assert_eq!(
        text.lines().count(),
        3,
        "header, one row, summary: {text:?}"
    );
    let _ = record::VERSION;
}
