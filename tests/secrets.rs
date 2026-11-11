//! M-3: secrets given on an agent's command line never reach the record,
//! the resume command, the give-up line or `herdr-wake resume` output.

mod common;

use common::Fixture;
use herdr_wake::agents;
use herdr_wake::config::Config;
use herdr_wake::record::AgentKind::{self, Claude, Codex, OpenCode, Pi};
use herdr_wake::trust::{has_url_userinfo, strip_url_userinfo};

fn argv(kind: AgentKind, words: &[&str]) -> Vec<String> {
    std::iter::once(kind.as_str())
        .chain(words.iter().copied())
        .map(str::to_string)
        .collect()
}

fn kept(kind: AgentKind, words: &[&str]) -> Vec<String> {
    agents::get(kind).restorable_flags(&argv(kind, words), &Config::default())
}

fn withheld(kind: AgentKind, words: &[&str]) -> Vec<String> {
    agents::get(kind).withheld_flags(&argv(kind, words), &Config::default())
}

const MCP: &str = r#"{"mcpServers":{"gh":{"env":{"GITHUB_TOKEN":"not-a-real-token"}}}}"#;
const SETTINGS: &str = r#" {"env":{"ANTHROPIC_API_KEY":"not-a-real-key"}}"#;
const AGENTS: &str = r#"[{"name":"x","prompt":"secret plan"}]"#;

#[test]
fn claude_inline_json_values_are_never_stored() {
    let words = [
        "--model",
        "opus",
        "--mcp-config",
        MCP,
        "--settings",
        SETTINGS,
        "--agents",
        AGENTS,
        "--effort",
        "high",
    ];
    assert_eq!(
        kept(Claude, &words),
        ["--model", "opus", "--effort", "high"]
    );
    assert_eq!(
        withheld(Claude, &words),
        ["--mcp-config", "--settings", "--agents"]
    );
    // The `=` form and a mixed variadic list are dropped whole as well.
    assert!(kept(Claude, &[&format!("--settings={}", SETTINGS.trim())]).is_empty());
    assert!(kept(Claude, &["--mcp-config", "./a.json", MCP]).is_empty());
}

#[test]
fn claude_file_paths_for_the_same_flags_are_still_restored() {
    let words = [
        "--mcp-config",
        "./mcp.json",
        "--settings",
        "/abs/settings.json",
        "--agents",
        "agents.json",
    ];
    assert_eq!(kept(Claude, &words), words);
    assert!(withheld(Claude, &words).is_empty());
}

#[test]
fn codex_config_overrides_that_look_like_credentials_are_never_stored() {
    for value in [
        "model_providers.x.experimental_bearer_token=\"abc\"",
        "model_providers.x.env_key=\"OPENAI_API_KEY\"",
        "mcp_servers.gh.env.GITHUB_TOKEN=abc",
        "some.Password=hunter2",
        "x.client_secret=abc",
        "preferred_auth_method=apikey",
        "model_providers.x.http_headers={Authorization=\"Bearer abc\"}",
    ] {
        for form in [
            vec!["-c", value],
            vec!["--config", value],
            vec!["-m", "gpt-5", "-c", value, "--search"],
        ] {
            let got = kept(Codex, &form);
            assert!(!got.iter().any(|w| w.contains(value)), "{value}: {got:?}");
            assert!(!got.contains(&"-c".to_string()), "{got:?}");
            assert!(!got.contains(&"--config".to_string()), "{got:?}");
            assert_eq!(withheld(Codex, &form).len(), 1, "{value}");
        }
        let attached = format!("--config={value}");
        assert!(kept(Codex, &[&attached]).is_empty(), "{value}");
    }
    // A plain override is kept, and what surrounds a dropped one survives.
    assert_eq!(
        kept(Codex, &["-m", "gpt-5", "-c", "x.api_key=abc", "--search"]),
        ["-m", "gpt-5", "--search"]
    );
    assert_eq!(
        kept(Codex, &["-c", "model_reasoning_effort=high"]),
        ["-c", "model_reasoning_effort=high"]
    );
}

#[test]
fn url_userinfo_is_found_and_stripped() {
    for (url, stripped) in [
        (
            "https://user:pass@host:4096/x?y=1",
            "https://host:4096/x?y=1",
        ),
        ("http://tok@host", "http://host"),
        ("postgres://u:p%40ss@h/db", "postgres://h/db"),
        ("https://a:b@c@host/p", "https://host/p"),
    ] {
        assert!(has_url_userinfo(url), "{url}");
        assert_eq!(strip_url_userinfo(url), stripped);
    }
    for plain in [
        "https://host:4096/x",
        "http://host/a@b",
        "https://host/?q=a@b",
        "user@host",
        "anthropic/claude",
        "/a/path",
        "",
    ] {
        assert!(!has_url_userinfo(plain), "{plain}");
        assert_eq!(strip_url_userinfo(plain), plain);
    }
}

#[test]
fn opencode_server_keeps_its_address_and_loses_its_password() {
    assert_eq!(
        kept(
            OpenCode,
            &["--server", "https://user:pass@host:4096", "--model", "a/b"]
        ),
        ["--server", "https://host:4096", "--model", "a/b"]
    );
    assert_eq!(
        kept(OpenCode, &["--server=https://user:pass@host:4096"]),
        ["--server=https://host:4096"]
    );
    assert_eq!(
        kept(OpenCode, &["--server", "https://host:4096"]),
        ["--server", "https://host:4096"]
    );
}

#[test]
fn any_other_flag_value_with_url_userinfo_drops_the_flag() {
    let url = "https://user:pass@host/x";
    for (kind, words, want) in [
        (
            Claude,
            vec!["--model", "opus", "--mcp-config", url, "--plugin-dir", url],
            vec!["--model", "opus"],
        ),
        (
            Codex,
            vec![
                "-c",
                &format!("model_providers.x.base_url=\"{url}\"")[..],
                "--search",
            ],
            vec!["--search"],
        ),
        (Pi, vec!["--provider", url, "--offline"], vec!["--offline"]),
        (
            OpenCode,
            vec!["--model", url, "--standalone"],
            vec!["--standalone"],
        ),
    ] {
        let got = kept(kind, &words);
        assert_eq!(got, want, "{kind}");
        assert!(!withheld(kind, &words).is_empty(), "{kind}");
    }
}

#[test]
fn withheld_names_never_carry_a_value() {
    let all = [
        withheld(Claude, &["--settings", SETTINGS, "--mcp-config", MCP]),
        withheld(Codex, &["-c", "a.token=abc", "--config=b.secret=def"]),
        withheld(Pi, &["--provider", "https://u:p@h"]),
        withheld(OpenCode, &["--model", "https://u:p@h"]),
    ]
    .concat();
    assert_eq!(all.len(), 6, "{all:?}");
    for name in all {
        assert!(
            name.starts_with('-') && !name.contains(['=', '{', ':', '@']),
            "{name}"
        );
    }
}

// ---- the hook logs that a flag was withheld, without its value ----

mod hook_log {
    use super::*;
    use herdr_wake::agents::Observation;
    use herdr_wake::core::Terminal;
    use herdr_wake::core::TerminalId;
    use herdr_wake::hook::{self, HookOutcome, Start};
    use herdr_wake::record;
    use std::path::PathBuf;

    #[test]
    fn a_withheld_flag_is_logged_by_name_and_absent_from_the_record() {
        let mut f = Fixture::temp();
        f.core.insert(Terminal::new("t1"));
        f.env.insert("HC_TERMINAL_ID".into(), "t1".into());
        f.env
            .insert("HC_TERMINAL_SOCKET".into(), "/x/c.sock".into());
        let start = Start {
            session_id: "0b5a2c1e-1111-4222-8333-444455556666".into(),
            session_file: None,
            cwd: PathBuf::from("/home/u/proj"),
            terminal: None,
            agent_pid: None,
            argv: Some(argv(
                Claude,
                &[
                    "--model",
                    "opus",
                    "--mcp-config",
                    MCP,
                    "--settings",
                    SETTINGS,
                ],
            )),
        };
        let agent = agents::get(Claude);
        let out = hook::started(agent.as_ref(), start, &mut f.ctx(), &|_, _, _| {});
        assert_eq!(out, HookOutcome::Recorded);

        let rec = record::load(&mut f.core, &TerminalId::from("t1"))
            .unwrap()
            .unwrap();
        assert_eq!(rec.flags, ["--model", "opus"]);
        let label = &f.core.terminals[&TerminalId::from("t1")].labels[record::LABEL];
        assert!(!label.contains("not-a-real"), "{label}");

        let log = std::fs::read_to_string(herdr_wake::paths::log_file(&f.env, &f.home)).unwrap();
        assert!(
            log.contains("--mcp-config") && log.contains("--settings"),
            "{log}"
        );
        assert!(log.contains("not restorable"), "{log}");
        assert!(!log.contains("not-a-real"), "{log}");
        let _ = Observation::Ignore("unused");
    }
}

// ---- the give-up line and `resume` output ----

#[test]
fn a_value_stored_by_an_older_build_is_not_printed_in_the_manual_command() {
    use herdr_wake::record::Record;
    use herdr_wake::wake::{decide, manual_for, Decision};
    use std::path::PathBuf;

    let mut f = Fixture::temp();
    let cwd = f.home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut rec = Record::new(
        Claude,
        "0b5a2c1e-1111-4222-8333-444455556666",
        cwd,
        PathBuf::from("/opt/bin/claude"),
        "boot-0",
    );
    rec.flags = [
        "--model",
        "opus",
        "--mcp-config",
        MCP,
        "--settings",
        SETTINGS,
    ]
    .map(str::to_string)
    .to_vec();
    let agent = agents::get(Claude);

    // No transcript: the plan is permanent and the hint comes from the record.
    match decide(Some(&rec), Some(agent.as_ref()), &mut f.ctx()) {
        Decision::GiveUp { manual, .. } => {
            let manual = manual.expect("a command");
            assert!(manual.contains("--model opus --resume"), "{manual}");
            assert!(!manual.contains("not-a-real"), "{manual}");
            assert!(!manual.contains("mcp-config"), "{manual}");
        }
        other => panic!("{other:?}"),
    }
    let shown = manual_for(&rec, agent.as_ref(), &mut f.ctx()).unwrap();
    assert!(!shown.contains("not-a-real"), "{shown}");
}
