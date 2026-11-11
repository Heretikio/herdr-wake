//! M-2: `restore_bypass = false` removes every launch flag that loosens an
//! agent's permissions, and nothing else. Table-driven over the four
//! adapters' `restorable_flags`.

use herdr_wake::agents;
use herdr_wake::config::Config;
use herdr_wake::record::AgentKind::{self, Claude, Codex, OpenCode, Pi};

fn flags(kind: AgentKind, line: &str, restore_bypass: bool) -> String {
    let cfg = Config {
        restore_bypass,
        ..Config::default()
    };
    let mut argv = vec![kind.as_str().to_string()];
    argv.extend(line.split_whitespace().map(str::to_string));
    agents::get(kind).restorable_flags(&argv, &cfg).join(" ")
}

/// (agent, launch flags, what is restored with the opt-out on). With the
/// opt-out off every row is restored whole.
const DROPPED: &[(AgentKind, &str, &str)] = &[
    // Claude Code
    (Claude, "--dangerously-skip-permissions", ""),
    (Claude, "--allow-dangerously-skip-permissions", ""),
    (Claude, "--permission-mode bypassPermissions", ""),
    (Claude, "--permission-mode=bypassPermissions", ""),
    (Claude, "--permission-mode acceptEdits", ""),
    (Claude, "--permission-mode auto", ""),
    (Claude, "--settings ./loose.json", ""),
    (Claude, "--settings=/abs/loose.json", ""),
    (Claude, "--allowed-tools Bash Write", ""),
    (Claude, "--allowedTools Bash(git:*) Edit", ""),
    (Claude, "--tools Bash,Edit", ""),
    (
        Claude,
        "--model opus --dangerously-skip-permissions --allowedTools Bash --effort high",
        "--model opus --effort high",
    ),
    // Codex
    (Codex, "--dangerously-bypass-approvals-and-sandbox", ""),
    (Codex, "--yolo", ""),
    (Codex, "--approve-for-me", ""),
    (Codex, "-s danger-full-access", ""),
    (Codex, "--sandbox danger-full-access", ""),
    (Codex, "--sandbox=danger-full-access", ""),
    (Codex, "-sdanger-full-access", ""),
    (Codex, "-a never", ""),
    (Codex, "--ask-for-approval never", ""),
    (Codex, "--ask-for-approval=never", ""),
    (Codex, "-c approval_policy=\"never\"", ""),
    (Codex, "-c approval_policy=never", ""),
    (Codex, "--config approval_policy=never", ""),
    (Codex, "--config=approval_policy=never", ""),
    (Codex, "-c sandbox_mode=\"danger-full-access\"", ""),
    (Codex, "-c sandbox_workspace_write.network_access=true", ""),
    (
        Codex,
        "-c sandbox_permissions=[\"disk-full-read-access\"]",
        "",
    ),
    (Codex, "-csandbox_mode=danger-full-access", ""),
    (Codex, "-p loose", ""),
    (Codex, "--profile loose", ""),
    (Codex, "--profile=loose", ""),
    (Codex, "-c profile=loose", ""),
    (
        Codex,
        "-m gpt-5 -s danger-full-access -a never -c approval_policy=\"never\" \
         -c sandbox_mode=\"danger-full-access\" --sandbox=danger-full-access --yolo --search",
        "-m gpt-5 --search",
    ),
    // Pi
    (Pi, "-a", ""),
    (Pi, "--approve", ""),
    (
        Pi,
        "--model sonnet --approve --offline",
        "--model sonnet --offline",
    ),
    // OpenCode: every alias comes back as --auto, or not at all.
    (OpenCode, "--auto", ""),
    (OpenCode, "--yolo", ""),
    (OpenCode, "--dangerously-skip-permissions", ""),
    (OpenCode, "--auto=true", ""),
    (OpenCode, "--model a/b --yolo", "--model a/b"),
];

#[test]
fn the_opt_out_drops_every_flag_that_loosens_permissions() {
    for &(kind, line, want) in DROPPED {
        assert_eq!(flags(kind, line, false), want, "{kind}: {line}");
    }
}

#[test]
fn with_restore_bypass_on_the_same_flags_come_back() {
    for &(kind, line, _) in DROPPED {
        let got = flags(kind, line, true);
        if kind == OpenCode {
            // Aliases are normalized to `--auto`, placed first.
            assert!(got.split(' ').any(|w| w == "--auto"), "{line}: {got}");
        } else {
            assert_eq!(got, line.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
}

/// Flags that tighten or do not touch permissions stay, opt-out or not.
const KEPT: &[(AgentKind, &str)] = &[
    (Claude, "--permission-mode plan"),
    (Claude, "--permission-mode default"),
    (Claude, "--disallowed-tools Bash"),
    (Claude, "--disallowedTools WebFetch"),
    (Claude, "--model opus --add-dir /x"),
    (Codex, "-s read-only"),
    (Codex, "--sandbox workspace-write"),
    (Codex, "--sandbox=read-only"),
    (Codex, "-a untrusted"),
    (Codex, "-a on-request"),
    (Codex, "--ask-for-approval on-failure"),
    (Codex, "-c model_reasoning_effort=high"),
    (Codex, "--config model=\"o3\""),
    (Codex, "-m gpt-5 --search --no-alt-screen"),
    (Pi, "-na"),
    (Pi, "--no-approve --model sonnet"),
    (OpenCode, "--model a/b --agent build"),
];

#[test]
fn flags_that_loosen_nothing_survive_the_opt_out() {
    for &(kind, line) in KEPT {
        let want = line.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(flags(kind, line, false), want, "{kind}: {line}");
        assert_eq!(flags(kind, line, true), want, "{kind}: {line}");
    }
}

#[test]
fn an_unknown_sandbox_or_approval_value_counts_as_loosening() {
    // A value this build has never heard of is not assumed to be safe.
    assert_eq!(flags(Codex, "-s some-future-mode", false), "");
    assert_eq!(flags(Codex, "-a whenever", false), "");
    assert_eq!(
        flags(Codex, "-s some-future-mode", true),
        "-s some-future-mode"
    );
}
