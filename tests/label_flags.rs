//! M-5 (f): the flags stored in a label go through each adapter's flag
//! policy again when the resume command is built, so a flag the policy does
//! not allow never reaches the command line, whoever wrote the label.

mod common;

use std::path::{Path, PathBuf};

use common::Fixture;
use herdr_wake::agents::{self, Plan};
use herdr_wake::record::{AgentKind, Record};

const ID: &str = "019a6f2e-7c41-7d30-9f0e-3b2a1c4d5e6f";

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A fixture and a record of `kind` whose session exists on disk, so the
/// real adapter's `resume_plan` gets as far as building the command.
fn setup(kind: AgentKind) -> (Fixture, Record) {
    let f = Fixture::temp();
    let cwd = f.home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rec = Record::new(
        kind,
        ID,
        cwd.clone(),
        PathBuf::from(format!("/opt/bin/{kind}")),
        "boot-0",
    );
    let touch = |p: &Path| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "{}\n").unwrap();
    };
    match kind {
        AgentKind::Claude => touch(
            &f.home
                .join(".claude/projects")
                .join(agents::claude::project_dir_name(&cwd))
                .join(format!("{ID}.jsonl")),
        ),
        AgentKind::Codex => touch(&f.home.join(format!(
            ".codex/sessions/2026/11/10/rollout-2026-11-10T09-15-02-{ID}.jsonl"
        ))),
        AgentKind::Pi => {
            let file = f.home.join(format!(".pi/agent/sessions/p/{ID}.jsonl"));
            touch(&file);
            rec.session_file = Some(file);
        }
        AgentKind::OpenCode => {}
    }
    (f, rec)
}

fn argv_for(kind: AgentKind, flags: &[&str], restore_bypass: bool) -> Vec<String> {
    let (mut f, mut rec) = setup(kind);
    f.cfg.restore_bypass = restore_bypass;
    rec.flags = strings(flags);
    match agents::get(kind).resume_plan(&rec, &mut f.ctx()) {
        Plan::Run { argv, .. } => argv,
        Plan::Permanent(why) => panic!("{kind}: {why}"),
    }
}

#[test]
fn claude_flags_from_the_label_are_filtered_again() {
    let got = argv_for(
        AgentKind::Claude,
        &[
            "--model",
            "opus",
            "--evil-unknown",
            "x",
            "--print",
            "--session-id",
            "zzz",
            "--plugin-url",
            "https://evil.example/p",
            "--dangerously-skip-permissions",
        ],
        false,
    );
    assert_eq!(
        got,
        strings(&["/opt/bin/claude", "--model", "opus", "--resume", ID])
    );
}

#[test]
fn codex_flags_from_the_label_are_filtered_again() {
    let got = argv_for(
        AgentKind::Codex,
        &[
            "-m",
            "gpt-5",
            "--evil-unknown",
            "--remote",
            "ws://evil.example",
            "--dangerously-bypass-hook-trust",
            "-C",
            "/tmp",
            "--yolo",
        ],
        false,
    );
    assert_eq!(
        got,
        strings(&["/opt/bin/codex", "resume", "-m", "gpt-5", ID])
    );
}

#[test]
fn pi_flags_from_the_label_are_filtered_again() {
    let (f, rec) = setup(AgentKind::Pi);
    let file = rec.session_file.clone().unwrap();
    drop(f);
    let got = argv_for(
        AgentKind::Pi,
        &[
            "--model",
            "sonnet",
            "--extension",
            "/tmp/evil.ts",
            "--api-key",
            "not-a-real-key",
            "--evil-unknown",
            "-a",
        ],
        false,
    );
    // The session file differs per fixture: compare everything before it.
    assert_eq!(
        got[..got.len() - 1],
        strings(&["/opt/bin/pi", "--model", "sonnet", "--session"])
    );
    assert_eq!(Path::new(got.last().unwrap()).file_name(), file.file_name());
}

#[test]
fn opencode_flags_from_the_label_are_filtered_again() {
    let got = argv_for(
        AgentKind::OpenCode,
        &[
            "--model",
            "a/b",
            "--port",
            "1",
            "--evil-unknown",
            "--prompt",
            "do something",
            "--yolo",
        ],
        false,
    );
    assert_eq!(
        got,
        strings(&["/opt/bin/opencode", "--model", "a/b", "--session", ID])
    );
}

#[test]
fn bypass_flags_come_back_only_while_restore_bypass_is_on() {
    for (kind, flag) in [
        (AgentKind::Claude, "--dangerously-skip-permissions"),
        (AgentKind::Codex, "--yolo"),
        (AgentKind::Pi, "--approve"),
        (AgentKind::OpenCode, "--auto"),
    ] {
        assert!(
            argv_for(kind, &[flag], true).contains(&flag.to_string()),
            "{kind}"
        );
        assert!(
            !argv_for(kind, &[flag], false).contains(&flag.to_string()),
            "{kind}"
        );
    }
}
