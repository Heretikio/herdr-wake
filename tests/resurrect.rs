//! `herdr-wake resurrect`: what is listed and what is left out, filters,
//! the table and JSON, numbers and ids, the checks before a session is
//! brought back, and the terminal that is opened.

mod common;

use std::cell::RefCell;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use common::{agent_bin, Fixture};
use herdr_wake::core::{Core, CoreError, Terminal, TerminalId};
use herdr_wake::history::{self, Ending};
use herdr_wake::paths;
use herdr_wake::platform::fake::FakePlatform;
use herdr_wake::platform::{ExitInfo, Platform, ProcInfo};
use herdr_wake::record::{self, AgentKind, Record, State};
use herdr_wake::resurrect::{self as rs, Filter, Source, Tools, Transcript};
use serde_json::json;

const A: &str = "aaaaaaaa-1111-4222-8333-444455556666";
const B: &str = "bbbbbbbb-1111-4222-8333-444455556666";
const C: &str = "cccccccc-1111-4222-8333-444455556666";
const D: &str = "dddddddd-1111-4222-8333-444455556666";
const HOUR: u64 = 3_600_000;

fn real_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

struct World {
    f: Fixture,
    work: tempfile::TempDir,
    now: u64,
}

impl World {
    fn new() -> World {
        let mut f = Fixture::temp();
        let now = real_now();
        f.platform.set_now(now);
        f.env.insert(
            "PATH".into(),
            agent_bin(AgentKind::Claude)
                .parent()
                .unwrap()
                .display()
                .to_string(),
        );
        World {
            f,
            work: tempfile::tempdir().unwrap(),
            now,
        }
    }

    fn cwd(&self) -> PathBuf {
        self.work.path().to_path_buf()
    }

    fn history(&self) -> PathBuf {
        paths::history_dir(&self.f.env, &self.f.home)
    }

    /// A journal entry for a session of `agent` in the work folder, last
    /// active `ago_ms` ago and ended as `ending`.
    fn journal(&self, agent: AgentKind, id: &str, flags: &[&str], ago_ms: u64, ending: Ending) {
        let mut rec = Record::new(agent, id, self.cwd(), agent_bin(agent), "b");
        rec.flags = flags.iter().map(|s| s.to_string()).collect();
        let t = TerminalId::from("t1");
        history::started(&self.history(), 500, &t, &rec, self.now - ago_ms - HOUR).unwrap();
        if ending != Ending::Open {
            history::ended(
                &self.history(),
                500,
                agent,
                id,
                None,
                ending,
                self.now - ago_ms,
            )
            .unwrap();
        }
    }

    fn claude_file(&self, id: &str) -> PathBuf {
        self.f
            .home
            .join(".claude/projects")
            .join(herdr_wake::agents::claude::project_dir_name(&self.cwd()))
            .join(format!("{id}.jsonl"))
    }

    /// A Claude transcript last written ten days ago, so the journal's own
    /// times decide the order.
    fn claude_transcript(&self, id: &str, rows: &[serde_json::Value]) {
        self.claude_transcript_aged(id, rows, 240 * HOUR);
    }

    fn claude_transcript_aged(&self, id: &str, rows: &[serde_json::Value], ago_ms: u64) {
        let path = self.claude_file(id);
        write_lines(&path, rows);
        let then = UNIX_EPOCH + std::time::Duration::from_millis(self.now - ago_ms);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(then)
            .unwrap();
    }

    fn rows(&mut self) -> Vec<rs::Row> {
        rs::collect(&mut self.f.ctx())
    }
}

fn write_lines(path: &Path, rows: &[serde_json::Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::File::create(path).unwrap();
    for r in rows {
        writeln!(f, "{r}").unwrap();
    }
}

fn user(cwd: &Path, text: &str) -> serde_json::Value {
    json!({"type":"user","cwd":cwd,"entrypoint":"cli","isSidechain":false,
           "message":{"role":"user","content":text}})
}

fn row<'a>(rows: &'a [rs::Row], id: &str) -> &'a rs::Row {
    rows.iter()
        .find(|r| r.id() == id)
        .unwrap_or_else(|| panic!("no row {id}"))
}

fn ids(rows: &[&rs::Row]) -> Vec<String> {
    rows.iter().map(|r| r.id().to_string()).collect()
}

// ------------------------------------------------------------- listing

#[test]
fn the_journal_and_the_transcripts_are_merged_newest_first() {
    let mut w = World::new();
    let cwd = w.cwd();
    // A: journaled, with its transcript. B: journaled, transcript deleted.
    w.journal(
        AgentKind::Claude,
        A,
        &["--model", "opus"],
        3 * HOUR,
        Ending::Quit,
    );
    w.claude_transcript(
        A,
        &[
            user(&cwd, "work on A"),
            json!({"type":"ai-title","aiTitle":"A title"}),
        ],
    );
    w.journal(AgentKind::Claude, B, &[], 48 * HOUR, Ending::Killed);
    // C: a transcript only, started in bypass mode.
    w.claude_transcript_aged(
        C,
        &[
            json!({"type":"permission-mode","permissionMode":"bypassPermissions"}),
            user(&cwd, "only a transcript"),
        ],
        HOUR,
    );
    // D: a Codex rollout only.
    let rollout =
        w.f.home
            .join(".codex/sessions/2026/11/20")
            .join(format!("rollout-2026-11-20T09-15-02-{D}.jsonl"));
    write_lines(
        &rollout,
        &[
            json!({"type":"session_meta","payload":{"id":D,"cwd":cwd,"source":"cli","thread_source":"user"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user",
                   "content":[{"type":"input_text","text":"codex task"}]}}),
        ],
    );

    let rows = w.rows();
    assert_eq!(rows.len(), 4);
    let a = row(&rows, A);
    assert_eq!(a.source, Source::Wake);
    assert_eq!(a.title.as_deref(), Some("A title"));
    assert_eq!(a.ending(), Ending::Quit);
    assert_eq!(a.transcript, Transcript::Present);
    assert_eq!(a.entry.flags, ["--model", "opus"]);
    assert!(!a.flags_from_transcript);

    let b = row(&rows, B);
    assert_eq!(b.transcript, Transcript::Gone);
    assert_eq!(b.ending(), Ending::Killed);
    assert_eq!(b.title, None);

    let c = row(&rows, C);
    assert_eq!(c.source, Source::Transcript);
    assert!(c.flags_from_transcript);
    assert_eq!(c.entry.flags, ["--dangerously-skip-permissions"]);
    assert!(c.bypass);
    assert_eq!(c.title.as_deref(), Some("only a transcript"));
    assert_eq!(c.ending(), Ending::Unknown);

    let d = row(&rows, D);
    assert_eq!(
        (d.agent(), d.source),
        (AgentKind::Codex, Source::Transcript)
    );
    assert!(d.entry.flags.is_empty());
    assert_eq!(d.title.as_deref(), Some("codex task"));

    // Newest first: the Codex rollout (just written), C (1 h), A (3 h), B (2 d).
    let order: Vec<&str> = rows.iter().map(|r| r.id()).collect();
    assert_eq!(order, [D, C, A, B]);
}

#[test]
fn a_session_in_both_places_is_listed_once_from_the_journal() {
    let mut w = World::new();
    let cwd = w.cwd();
    w.journal(
        AgentKind::Claude,
        A,
        &["--model", "opus"],
        HOUR,
        Ending::Quit,
    );
    w.claude_transcript(
        A,
        &[
            json!({"type":"permission-mode","permissionMode":"plan"}),
            user(&cwd, "same session"),
        ],
    );
    let rows = w.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, Source::Wake);
    // The journal's flags win over the transcript's mode.
    assert_eq!(rows[0].entry.flags, ["--model", "opus"]);
}

#[test]
fn claude_transcript_modes_become_flags() {
    use herdr_wake::resurrect::claude_mode_flags;
    assert_eq!(
        claude_mode_flags(Some("bypassPermissions")),
        ["--dangerously-skip-permissions"]
    );
    for m in ["plan", "auto", "acceptEdits"] {
        assert_eq!(claude_mode_flags(Some(m)), ["--permission-mode", m]);
    }
    assert!(claude_mode_flags(Some("default")).is_empty());
    assert!(claude_mode_flags(None).is_empty());
}

#[test]
fn the_bypass_marker_follows_the_agents_own_flag_policy() {
    use herdr_wake::config::Config;
    use herdr_wake::resurrect::has_bypass;
    let cfg = Config::default();
    let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(has_bypass(
        AgentKind::Claude,
        &f(&["--dangerously-skip-permissions"]),
        &cfg
    ));
    assert!(has_bypass(
        AgentKind::Claude,
        &f(&["--permission-mode", "auto"]),
        &cfg
    ));
    assert!(!has_bypass(
        AgentKind::Claude,
        &f(&["--permission-mode", "plan"]),
        &cfg
    ));
    assert!(!has_bypass(
        AgentKind::Claude,
        &f(&["--model", "opus"]),
        &cfg
    ));
    assert!(has_bypass(AgentKind::Codex, &f(&["--yolo"]), &cfg));
    assert!(!has_bypass(AgentKind::Codex, &f(&["--model", "gpt"]), &cfg));
    // With restore_bypass off nothing is replayed, so nothing is marked.
    let off = Config {
        restore_bypass: false,
        ..Config::default()
    };
    assert!(!has_bypass(
        AgentKind::Claude,
        &f(&["--dangerously-skip-permissions"]),
        &off
    ));
    assert!(!has_bypass(AgentKind::Codex, &f(&["--yolo"]), &off));
}

#[test]
fn the_bypass_marker_in_a_listing_goes_with_restore_bypass_false() {
    let mut w = World::new();
    w.journal(
        AgentKind::Claude,
        A,
        &["--dangerously-skip-permissions"],
        HOUR,
        Ending::Quit,
    );
    assert!(row(&w.rows(), A).bypass);
    w.f.cfg.restore_bypass = false;
    assert!(!row(&w.rows(), A).bypass);
}

// ------------------------------------------------------- open sessions

fn terminal_with(id: &str, rec: Option<&Record>, foreground: Option<u32>) -> Terminal {
    let mut t = Terminal::new(id);
    if let Some(r) = rec {
        t.labels
            .insert(record::LABEL.into(), serde_json::to_string(r).unwrap());
    }
    t.foreground_pid = foreground;
    t
}

#[test]
fn a_running_record_on_a_live_terminal_hides_its_session() {
    let mut w = World::new();
    w.journal(AgentKind::Claude, A, &[], HOUR, Ending::Open);
    w.journal(AgentKind::Claude, B, &[], HOUR, Ending::Quit);
    let mut rec = Record::new(
        AgentKind::Claude,
        A,
        w.cwd(),
        agent_bin(AgentKind::Claude),
        "b",
    );
    rec.state = State::Running;
    w.f.core.insert(terminal_with("t1", Some(&rec), None));
    // An ended record does not hide anything.
    let mut ended = rec.clone();
    ended.session_id = B.into();
    ended.state = State::Ended;
    w.f.core.insert(terminal_with("t2", Some(&ended), None));

    let rows = w.rows();
    assert!(row(&rows, A).open);
    assert!(!row(&rows, B).open);
    let (shown, total) = rs::select(&rows, &Filter::default(), 20);
    assert_eq!((ids(&shown), total), (vec![B.to_string()], 1));
}

#[test]
fn claude_session_files_with_a_live_claude_process_hide_their_session() {
    let mut w = World::new();
    w.journal(AgentKind::Claude, A, &[], HOUR, Ending::Open);
    w.journal(AgentKind::Claude, B, &[], HOUR, Ending::Open);
    w.journal(AgentKind::Claude, C, &[], HOUR, Ending::Open);
    let dir = w.f.home.join(".claude/sessions");
    std::fs::create_dir_all(&dir).unwrap();
    for (pid, id) in [(4242, A), (4243, B), (4244, C)] {
        std::fs::write(
            dir.join(format!("{pid}.json")),
            json!({"pid": pid, "sessionId": id, "kind": "interactive"}).to_string(),
        )
        .unwrap();
    }
    // 4242 is claude; 4243 is gone; 4244 is some other program (a reused pid).
    w.f.platform
        .add_process(ProcInfo::new(4242, 1, 1, vec!["claude".into()]));
    w.f.platform.add_process(ProcInfo::new(
        4244,
        1,
        1,
        vec!["/usr/bin/vim".into(), "x".into()],
    ));

    // Core is down: the session files still count.
    w.f.core.fail_with(Some(CoreError::NotRunning));
    let rows = w.rows();
    assert!(row(&rows, A).open);
    assert!(!row(&rows, B).open);
    assert!(!row(&rows, C).open);
}

#[test]
fn a_running_agent_that_names_the_session_hides_it() {
    let mut w = World::new();
    w.journal(AgentKind::Codex, D, &[], HOUR, Ending::Quit);
    w.journal(AgentKind::Codex, C, &[], HOUR, Ending::Quit);
    // codex resume <D> in a terminal with no record at all.
    w.f.platform.add_process(ProcInfo::new(
        900,
        1,
        1,
        vec!["codex".into(), "resume".into(), D.into()],
    ));
    w.f.core.insert(terminal_with("t3", None, Some(900)));
    let rows = w.rows();
    assert!(row(&rows, D).open);
    assert!(!row(&rows, C).open);
}

// ------------------------------------------------------------- filters

fn titled(w: &mut World) -> Vec<rs::Row> {
    let cwd = w.cwd();
    w.journal(AgentKind::Claude, A, &[], HOUR, Ending::Quit);
    w.claude_transcript(A, &[user(&cwd, "Fix the Login bug")]);
    w.journal(AgentKind::Codex, B, &[], 2 * HOUR, Ending::Quit);
    w.journal(AgentKind::Claude, C, &[], 3 * HOUR, Ending::Quit);
    w.claude_transcript(C, &[user(&cwd, "write release notes")]);
    w.rows()
}

#[test]
fn a_query_must_match_title_folder_id_or_agent_in_every_word() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let q = |text: &str| {
        let f = Filter {
            query: Some(text.to_string()),
            ..Filter::default()
        };
        ids(&rs::select(&rows, &f, 0).0)
    };
    assert_eq!(q("login"), [A]);
    assert_eq!(q("LOGIN BUG"), [A]);
    assert_eq!(q("login release"), Vec::<String>::new());
    assert_eq!(q("codex"), [B]);
    assert_eq!(q("cccccccc"), [C]);
    let folder = w
        .cwd()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_lowercase();
    assert_eq!(q(&folder).len(), 3);
}

#[test]
fn here_folder_agent_and_limit_narrow_the_list() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let select = |f: Filter, limit| ids(&rs::select(&rows, &f, limit).0);

    let inside = Filter {
        scope: Some(w.cwd()),
        ..Filter::default()
    };
    assert_eq!(select(inside, 0).len(), 3);
    let parent = Filter {
        scope: w.cwd().parent().map(Path::to_path_buf),
        ..Filter::default()
    };
    assert_eq!(select(parent, 0).len(), 3);
    let elsewhere = Filter {
        scope: Some(w.f.home.join("nowhere")),
        ..Filter::default()
    };
    assert!(select(elsewhere, 0).is_empty());

    let codex = Filter {
        agent: Some(AgentKind::Codex),
        ..Filter::default()
    };
    assert_eq!(select(codex, 0), [B]);

    let (shown, total) = rs::select(&rows, &Filter::default(), 2);
    assert_eq!((shown.len(), total), (2, 3));
    assert_eq!(ids(&shown), [A, B]);
}

#[test]
fn a_scope_matches_through_symlinks() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let link = w.f.home.join("link");
    std::os::unix::fs::symlink(w.cwd(), &link).unwrap();
    let f = Filter {
        scope: Some(link),
        ..Filter::default()
    };
    assert_eq!(rs::select(&rows, &f, 0).0.len(), 3);
}

// -------------------------------------------------------------- display

#[test]
fn age_reads_now_minutes_hours_days_months() {
    let now = 100_000_000_000_u64;
    for (ago_s, want) in [
        (0, "now"),
        (59, "now"),
        (60, "1m"),
        (5 * 60, "5m"),
        (3 * 3600, "3h"),
        (2 * 86_400, "2d"),
        (4 * 30 * 86_400, "4mo"),
        (400 * 86_400, "1y"),
    ] {
        assert_eq!(rs::age(now, now - ago_s * 1000), want, "{ago_s} s");
    }
    assert_eq!(rs::age(5, 10), "now");
}

#[test]
fn place_writes_home_as_a_tilde() {
    let home = Path::new("/home/u");
    assert_eq!(rs::place(Path::new("/home/u"), home), "~");
    assert_eq!(rs::place(Path::new("/home/u/git/x"), home), "~/git/x");
    assert_eq!(rs::place(Path::new("/srv/x"), home), "/srv/x");
}

#[test]
fn the_table_is_numbered_marks_bypass_and_deleted_transcripts() {
    let mut w = World::new();
    let cwd = w.cwd();
    w.journal(
        AgentKind::Claude,
        A,
        &["--dangerously-skip-permissions"],
        3 * HOUR,
        Ending::Quit,
    );
    w.claude_transcript(A, &[user(&cwd, "keep going")]);
    w.journal(AgentKind::Claude, B, &[], 48 * HOUR, Ending::Killed);
    let rows = w.rows();
    let (shown, total) = rs::select(&rows, &Filter::default(), 20);
    let text = rs::render_list(&shown, total, w.now, &w.f.home, false);

    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("#") && lines[0].contains("AGE") && lines[0].contains("TITLE"));
    assert!(
        lines[1].starts_with("1") && lines[1].contains("3h"),
        "{text}"
    );
    assert!(lines[1].contains("claude") && lines[1].contains("keep going"));
    assert!(
        lines[1].contains("quit") && lines[1].contains(" ! "),
        "{text}"
    );
    assert!(lines[1].trim_end().ends_with("wake"), "{text}");
    assert!(lines[2].starts_with("2") && lines[2].contains("2d"));
    assert!(lines[2].contains("(no title)") && lines[2].contains("killed"));
    assert!(
        lines[2].trim_end().ends_with("wake, transcript gone"),
        "{text}"
    );
    assert!(text.contains("! restores a permission bypass"));
    assert!(text.contains("herdr-wake resurrect <number>"));
    assert!(!text.contains('\u{2014}'));
}

#[test]
fn an_empty_list_says_so() {
    let w = World::new();
    assert_eq!(
        rs::render_list(&[], 0, w.now, &w.f.home, false),
        "No closed sessions found.\n"
    );
    assert_eq!(
        rs::render_list(&[], 0, w.now, &w.f.home, true),
        "No closed sessions match.\n"
    );
}

#[test]
fn titles_cannot_send_escape_sequences_to_the_terminal() {
    let mut w = World::new();
    let cwd = w.cwd();
    w.claude_transcript(A, &[user(&cwd, "evil \u{1b}[2J\u{7} title")]);
    let rows = w.rows();
    let (shown, total) = rs::select(&rows, &Filter::default(), 20);
    let text = rs::render_list(&shown, total, w.now, &w.f.home, false);
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{7}'),
        "{text:?}"
    );
    let j = rs::list_json(&shown, w.now).to_string();
    assert!(!j.contains('\u{1b}'), "{j:?}");
}

#[test]
fn the_json_listing_carries_the_same_facts() {
    let mut w = World::new();
    w.journal(
        AgentKind::Claude,
        A,
        &["--yolo-not-a-flag"],
        HOUR,
        Ending::Forgotten,
    );
    w.journal(AgentKind::Claude, B, &[], 2 * HOUR, Ending::Quit);
    let rows = w.rows();
    let (shown, _) = rs::select(&rows, &Filter::default(), 20);
    let j = rs::list_json(&shown, w.now);
    let first = &j["sessions"][0];
    assert_eq!(first["number"], 1);
    assert_eq!(first["agent"], "claude");
    assert_eq!(first["session_id"], A);
    assert_eq!(first["ending"], "forgotten");
    assert_eq!(first["source"], "wake");
    assert_eq!(first["bypass"], false);
    assert_eq!(first["transcript_gone"], true);
    assert_eq!(first["flags_from_transcript"], false);
    assert_eq!(first["age"], "1h");
    assert_eq!(j["sessions"][1]["number"], 2);
}

// ------------------------------------------------- numbers and session ids

#[test]
fn a_number_means_a_row_of_the_last_listing() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let (shown, _) = rs::select(&rows, &Filter::default(), 20);
    rs::remember(&w.f.env, &w.f.home, &Filter::default(), &shown).unwrap();

    let got = rs::resolve(&rows, "2", &Filter::default(), &w.f.env, &w.f.home).unwrap();
    assert_eq!(got.id(), shown[1].id());
    assert!(rs::resolve(&rows, "9", &Filter::default(), &w.f.env, &w.f.home).is_err());
    assert!(rs::resolve(&rows, "0", &Filter::default(), &w.f.env, &w.f.home).is_err());
}

#[test]
fn a_number_with_other_filters_than_the_listing_is_refused() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let listed = Filter {
        agent: Some(AgentKind::Claude),
        ..Filter::default()
    };
    let (shown, _) = rs::select(&rows, &listed, 20);
    rs::remember(&w.f.env, &w.f.home, &listed, &shown).unwrap();

    // The same filters, or none at all, work.
    assert!(rs::resolve(&rows, "1", &listed, &w.f.env, &w.f.home).is_ok());
    assert!(rs::resolve(&rows, "1", &Filter::default(), &w.f.env, &w.f.home).is_ok());
    // Other filters do not.
    let other = Filter {
        scope: Some(w.cwd()),
        ..Filter::default()
    };
    let err = rs::resolve(&rows, "1", &other, &w.f.env, &w.f.home).unwrap_err();
    assert!(err.contains("other filters"), "{err}");
}

#[test]
fn a_number_without_a_listing_is_an_error() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let err = rs::resolve(&rows, "1", &Filter::default(), &w.f.env, &w.f.home).unwrap_err();
    assert!(err.contains("no earlier listing"), "{err}");
}

#[test]
fn a_session_id_is_found_and_an_unknown_one_is_not() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let f = Filter::default();
    assert_eq!(
        rs::resolve(&rows, B, &f, &w.f.env, &w.f.home).unwrap().id(),
        B
    );
    assert!(rs::resolve(&rows, "nope", &f, &w.f.env, &w.f.home).is_err());
}

#[test]
fn an_id_shared_by_two_agents_needs_the_agent() {
    let mut w = World::new();
    w.journal(AgentKind::Claude, A, &[], HOUR, Ending::Quit);
    w.journal(AgentKind::Codex, A, &[], HOUR, Ending::Quit);
    let rows = w.rows();
    let err = rs::resolve(&rows, A, &Filter::default(), &w.f.env, &w.f.home).unwrap_err();
    assert!(err.contains("--agent"), "{err}");
    let f = Filter {
        agent: Some(AgentKind::Codex),
        ..Filter::default()
    };
    assert_eq!(
        rs::resolve(&rows, A, &f, &w.f.env, &w.f.home)
            .unwrap()
            .agent(),
        AgentKind::Codex
    );
}

// -------------------------------------------------------------- prepare

/// A journaled Claude session that can be brought back: its transcript is
/// there and its folder is safe.
fn ready(w: &mut World, flags: &[&str]) -> rs::Row {
    let cwd = w.cwd();
    w.journal(AgentKind::Claude, A, flags, HOUR, Ending::Quit);
    w.claude_transcript(A, &[user(&cwd, "resume me")]);
    row(&w.rows(), A).clone()
}

#[test]
fn a_session_is_brought_back_with_its_recorded_flags_bypass_included() {
    let mut w = World::new();
    let r = ready(
        &mut w,
        &["--model", "opus", "--dangerously-skip-permissions"],
    );
    let l = rs::prepare(&r, &mut w.f.ctx(), false).unwrap();
    assert_eq!(l.cwd, w.cwd());
    assert_eq!(
        l.argv,
        [
            agent_bin(AgentKind::Claude).to_string_lossy().to_string(),
            "--model".into(),
            "opus".into(),
            "--dangerously-skip-permissions".into(),
            "--resume".into(),
            A.into(),
        ]
    );
}

#[test]
fn restore_bypass_false_still_drops_the_bypass() {
    let mut w = World::new();
    let r = ready(
        &mut w,
        &["--model", "opus", "--dangerously-skip-permissions"],
    );
    w.f.cfg.restore_bypass = false;
    let l = rs::prepare(&r, &mut w.f.ctx(), false).unwrap();
    assert!(!l.argv.iter().any(|a| a == "--dangerously-skip-permissions"));
    assert!(l.argv.iter().any(|a| a == "--model"));
}

#[test]
fn fork_adds_fork_session_for_claude_only() {
    let mut w = World::new();
    let r = ready(&mut w, &[]);
    let l = rs::prepare(&r, &mut w.f.ctx(), true).unwrap();
    assert_eq!(l.argv.last().map(String::as_str), Some("--fork-session"));
    assert!(l.argv.iter().any(|a| a == "--resume"));

    w.journal(AgentKind::Codex, D, &[], HOUR, Ending::Quit);
    let codex = row(&w.rows(), D).clone();
    let err = rs::prepare(&codex, &mut w.f.ctx(), true).unwrap_err();
    assert!(err.contains("Claude"), "{err}");
}

#[test]
fn the_same_checks_as_a_wake_refuse_what_is_not_safe() {
    // A folder that is gone.
    let mut w = World::new();
    let r = ready(&mut w, &[]);
    std::fs::remove_dir_all(w.work.path()).unwrap();
    let err = rs::prepare(&r, &mut w.f.ctx(), false).unwrap_err();
    assert!(err.contains("folder missing"), "{err}");

    // A folder others can write.
    let mut w = World::new();
    let r = ready(&mut w, &[]);
    std::fs::set_permissions(w.work.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let err = rs::prepare(&r, &mut w.f.ctx(), false).unwrap_err();
    assert!(
        err.contains("not safe") && err.contains("writable by others"),
        "{err}"
    );

    // A transcript that was deleted.
    let mut w = World::new();
    let r = ready(&mut w, &[]);
    std::fs::remove_file(w.claude_file(A)).unwrap();
    let err = rs::prepare(&r, &mut w.f.ctx(), false).unwrap_err();
    assert!(err.contains("session not found"), "{err}");

    // A program that is not the agent's.
    let mut w = World::new();
    let mut r = ready(&mut w, &[]);
    r.entry.bin = PathBuf::from("/bin/sh");
    let err = rs::prepare(&r, &mut w.f.ctx(), false).unwrap_err();
    assert!(err.contains("not safe to run"), "{err}");

    // A session id that could be an option.
    let mut w = World::new();
    let mut r = ready(&mut w, &[]);
    r.entry.session_id = "--evil".into();
    let err = rs::prepare(&r, &mut w.f.ctx(), false).unwrap_err();
    assert!(err.contains("not valid"), "{err}");

    // A session that is running now.
    let mut w = World::new();
    let mut r = ready(&mut w, &[]);
    r.open = true;
    let err = rs::prepare(&r, &mut w.f.ctx(), false).unwrap_err();
    assert!(err.contains("running now"), "{err}");
}

#[test]
fn an_open_claude_session_can_be_forked_but_not_resumed_twice() {
    let mut w = World::new();
    let mut r = ready(&mut w, &[]);
    r.open = true;
    let err = rs::prepare(&r, &mut w.f.ctx(), false).unwrap_err();
    assert!(err.contains("--fork"), "{err}");
    let l = rs::prepare(&r, &mut w.f.ctx(), true).unwrap();
    assert_eq!(l.argv.last().map(String::as_str), Some("--fork-session"));

    // Not for the others: forking is Claude's.
    w.journal(AgentKind::Codex, D, &[], HOUR, Ending::Quit);
    let mut codex = row(&w.rows(), D).clone();
    codex.open = true;
    assert!(rs::prepare(&codex, &mut w.f.ctx(), true).is_err());
    assert!(rs::prepare(&codex, &mut w.f.ctx(), false).is_err());
}

#[test]
fn an_id_that_looks_like_a_number_is_found_by_resolve_id() {
    let mut w = World::new();
    w.journal(AgentKind::Claude, "424242", &[], HOUR, Ending::Quit);
    let rows = w.rows();
    let f = Filter::default();
    assert_eq!(rs::resolve_id(&rows, "424242", &f).unwrap().id(), "424242");
    // `resolve` reads it as a row number, which the listing does not have.
    assert!(rs::resolve(&rows, "424242", &f, &w.f.env, &w.f.home).is_err());
    assert!(rs::resolve_id(&rows, "nope", &f).is_err());
}

#[test]
fn a_relative_config_folder_is_found_under_the_sessions_folder_like_a_resume_does() {
    let mut w = World::new();
    let cwd = w.cwd();
    w.journal(AgentKind::Claude, A, &[], HOUR, Ending::Quit);
    // The session ran with CLAUDE_CONFIG_DIR=cc (relative to its folder).
    let mut entry = history::load(&w.history(), AgentKind::Claude, A).unwrap();
    entry.env.insert("CLAUDE_CONFIG_DIR".into(), "cc".into());
    std::fs::write(
        history::file(&w.history(), AgentKind::Claude, A),
        serde_json::to_vec(&entry).unwrap(),
    )
    .unwrap();
    let moved = cwd
        .join("cc/projects")
        .join(herdr_wake::agents::claude::project_dir_name(&cwd));
    write_lines(
        &moved.join(format!("{A}.jsonl")),
        &[user(&cwd, "relative config")],
    );

    let rows = w.rows();
    let r = row(&rows, A);
    assert_eq!(r.transcript, Transcript::Present);
    assert_eq!(r.title.as_deref(), Some("relative config"));
    // And the resume check agrees.
    assert!(rs::prepare(r, &mut w.f.ctx(), false).is_ok());
}

#[test]
fn a_transcript_only_session_takes_its_program_from_path() {
    let mut w = World::new();
    let cwd = w.cwd();
    w.claude_transcript(
        C,
        &[
            json!({"type":"permission-mode","permissionMode":"plan"}),
            user(&cwd, "from a transcript"),
        ],
    );
    let r = row(&w.rows(), C).clone();
    let l = rs::prepare(&r, &mut w.f.ctx(), false).unwrap();
    assert_eq!(
        l.argv[0],
        agent_bin(AgentKind::Claude).to_string_lossy().to_string()
    );
    assert_eq!(&l.argv[1..3], ["--permission-mode", "plan"]);
    assert_eq!(&l.argv[3..], ["--resume", C]);
    assert_eq!(l.entry.bin, agent_bin(AgentKind::Claude));

    // No claude on PATH: nothing to run.
    w.f.env.insert("PATH".into(), "/nonexistent".into());
    let err = rs::prepare(&r, &mut w.f.ctx(), false).unwrap_err();
    assert!(err.contains("cannot find claude on PATH"), "{err}");
}

#[test]
fn the_printed_command_carries_a_config_folder_that_is_not_the_shells_own() {
    let mut w = World::new();
    let mut r = ready(&mut w, &[]);
    // The session used another Claude config folder (its transcript too).
    let other = w.f.home.join("cc");
    let moved = other
        .join("projects")
        .join(herdr_wake::agents::claude::project_dir_name(&w.cwd()));
    std::fs::create_dir_all(&moved).unwrap();
    std::fs::rename(w.claude_file(A), moved.join(format!("{A}.jsonl"))).unwrap();
    r.entry
        .env
        .insert("CLAUDE_CONFIG_DIR".into(), other.display().to_string());

    let l = rs::prepare(&r, &mut w.f.ctx(), false).unwrap();
    let cmd = l.command(&w.f.env);
    let want_dir = w.cwd().display().to_string();
    assert!(
        cmd.starts_with(&format!(
            "cd {want_dir} && CLAUDE_CONFIG_DIR={} ",
            other.display()
        )),
        "{cmd}"
    );
    assert!(cmd.ends_with(&format!("--resume {A}")), "{cmd}");

    // The shell already has it: not repeated.
    let mut env = w.f.env.clone();
    env.insert("CLAUDE_CONFIG_DIR".into(), other.display().to_string());
    assert!(!l.command(&env).contains("CLAUDE_CONFIG_DIR"));
}

// -------------------------------------------------------------- tickets

fn launch_of(w: &mut World, flags: &[&str]) -> rs::Launch {
    let r = ready(w, flags);
    rs::prepare(&r, &mut w.f.ctx(), false).unwrap()
}

/// How many times a run ended in the real shell (the fake exec fails, so
/// each hand-over tries every fallback shell in turn).
fn shells(w: &World) -> usize {
    w.f.platform
        .execs()
        .iter()
        .filter(|e| e.argv == ["/bin/zsh", "-l"])
        .count()
}

fn run_ticket(w: &World, key: &str) -> (std::io::Error, Vec<String>) {
    let said = RefCell::new(Vec::new());
    let say = |s: &str| said.borrow_mut().push(s.to_string());
    let mut env = w.f.env.clone();
    env.insert("SHELL".into(), "/bin/zsh".into());
    let err = rs::run_ticket(&w.f.platform, &env, &w.f.home, key, &say);
    (err, said.into_inner())
}

#[test]
fn the_new_terminal_runs_the_agent_then_the_shell_in_the_folder() {
    let mut w = World::new();
    let l = launch_of(&mut w, &["--model", "opus"]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    w.f.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 60_000)));

    let (_, said) = run_ticket(&w, &key);

    let runs = w.f.platform.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].argv, l.argv);
    assert_eq!(runs[0].cwd, w.cwd());
    // Not supervised: the agent's own hook records it as usual.
    assert!(!runs[0].env.contains_key("HERDR_WAKE_SUPERVISED"));
    assert_eq!(
        runs[0].env.get("SHELL").map(String::as_str),
        Some("/bin/zsh")
    );
    let exec = w.f.platform.execs();
    assert_eq!(exec[0].argv, ["/bin/zsh", "-l"]);
    assert_eq!(exec[0].cwd, w.cwd());
    assert!(said.is_empty(), "{said:?}");
}

#[test]
fn a_ticket_works_once() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    w.f.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 1)));
    run_ticket(&w, &key);
    assert_eq!(w.f.platform.runs().len(), 1);

    let (_, said) = run_ticket(&w, &key);
    assert_eq!(w.f.platform.runs().len(), 1);
    assert!(said[0].contains("no launch ticket"), "{said:?}");
    // Still ends in a shell.
    assert_eq!(shells(&w), 2);
}

#[test]
fn a_fork_ticket_adds_fork_session() {
    let mut w = World::new();
    let r = ready(&mut w, &[]);
    let l = rs::prepare(&r, &mut w.f.ctx(), true).unwrap();
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, true).unwrap();
    w.f.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 1)));
    run_ticket(&w, &key);
    assert_eq!(
        w.f.platform.runs()[0].argv.last().map(String::as_str),
        Some("--fork-session")
    );
}

#[test]
fn a_bad_key_a_missing_or_old_ticket_never_runs_anything_and_ends_in_a_shell() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    for key in ["", "../x", "a b", "nope"] {
        let (_, said) = run_ticket(&w, key);
        assert!(
            said[0].starts_with("herdr-wake: cannot bring the session back"),
            "{said:?}"
        );
    }
    // Out of date: written an hour ago.
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now - HOUR, &l, false).unwrap();
    let (_, said) = run_ticket(&w, &key);
    assert!(said[0].contains("out of date"), "{said:?}");
    assert!(w.f.platform.runs().is_empty());
    assert_eq!(shells(&w), 5);
}

#[test]
fn a_ticket_is_checked_again_when_it_runs() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    // The folder changed hands between the ticket and the run.
    std::fs::set_permissions(w.work.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

    let (_, said) = run_ticket(&w, &key);
    assert!(w.f.platform.runs().is_empty());
    assert!(
        said[0].contains("could not resume claude session") && said[0].contains("not safe"),
        "{said:?}"
    );
    assert_eq!(shells(&w), 1);
}

#[test]
fn a_symlinked_ticket_is_refused() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    let dir = paths::resurrect_dir(&w.f.env, &w.f.home);
    let file = dir.join(format!("run-{key}.json"));
    let real = dir.join("real.json");
    std::fs::rename(&file, &real).unwrap();
    std::os::unix::fs::symlink(&real, &file).unwrap();

    let (_, said) = run_ticket(&w, &key);
    assert!(said[0].contains("not a plain file"), "{said:?}");
    assert!(w.f.platform.runs().is_empty());
}

#[test]
fn a_ticket_is_private() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    let dir = paths::resurrect_dir(&w.f.env, &w.f.home);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join(format!("run-{key}.json"))), 0o600);
}

#[test]
fn an_agent_that_cannot_start_leaves_the_manual_command_and_a_shell() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    // No scripted result: the fake's run_foreground fails.
    let (_, said) = run_ticket(&w, &key);
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("it could not start"), "{said:?}");
    assert!(said[0].contains("To retry: cd "), "{said:?}");
    assert_eq!(shells(&w), 1);
}

// ------------------------------------------------------ opening the terminal

struct FakeTools {
    ls: Option<String>,
    hc_out: Result<String, String>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeTools {
    fn ok(out: &str) -> FakeTools {
        FakeTools {
            ls: None,
            hc_out: Ok(out.to_string()),
            calls: RefCell::default(),
        }
    }
}

impl Tools for FakeTools {
    fn herdr_app_ls(&self) -> Option<String> {
        self.ls.clone()
    }
    fn hc(&self, args: &[String]) -> Result<String, String> {
        self.calls.borrow_mut().push(args.to_vec());
        self.hc_out.clone()
    }
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn hc_is_asked_for_a_labelled_terminal_that_runs_the_ticket() {
    let args = rs::hc_run_args(
        Path::new("/work/app"),
        Some("sp1"),
        Path::new("/usr/local/bin/herdr-wake"),
        "123-456",
    );
    assert_eq!(
        args,
        strs(&[
            "run",
            "--bg",
            "--no-parent",
            "--keep",
            "--cwd",
            "/work/app",
            "--label",
            "herdr.space=sp1",
            "--",
            "/usr/local/bin/herdr-wake",
            "resurrect",
            "--run",
            "123-456",
        ])
    );
    let bare = rs::hc_run_args(Path::new("/w"), None, Path::new("/b/hw"), "k");
    assert!(!bare.iter().any(|a| a == "--label"));
}

#[test]
fn inside_a_herdr_terminal_the_new_terminal_joins_its_space() {
    let mut w = World::new();
    let mut here = Terminal::new("t7");
    here.labels.insert("herdr.space".into(), "sp-here".into());
    w.f.core.insert(here);
    w.f.env.insert("HC_TERMINAL_ID".into(), "t7".into());
    let tools = FakeTools::ok("t12\n");
    let cwd = w.cwd();
    let opened = rs::open_terminal(
        &tools,
        &mut w.f.core,
        &w.f.env,
        &w.f.home,
        &cwd,
        Path::new("/b/herdr-wake"),
        "key-1",
    )
    .unwrap();
    assert_eq!(opened.space.as_deref(), Some("sp-here"));
    assert_eq!(opened.terminal, "t12");
    let calls = tools.calls.borrow();
    assert!(calls[0]
        .windows(2)
        .any(|p| p == ["--label", "herdr.space=sp-here"]));
    assert!(calls[0].ends_with(&strs(&[
        "--",
        "/b/herdr-wake",
        "resurrect",
        "--run",
        "key-1"
    ])));
}

fn app_ls(spaces: serde_json::Value) -> Option<String> {
    Some(json!({ "spaces": spaces }).to_string())
}

#[test]
fn outside_herdr_the_space_whose_folder_holds_the_session_is_found() {
    let mut w = World::new();
    let proj = w.cwd().join("deep/er");
    std::fs::create_dir_all(&proj).unwrap();
    let mut t = Terminal::new("t3");
    t.cwd = Some(w.cwd().join("deep"));
    w.f.core.insert(t);
    let tools = FakeTools {
        ls: app_ls(json!([
            {"id":"sp-home","name":"home","folder":w.f.home,"machine":null,"active":true,"tabs":[]},
            {"id":"sp-root","name":"r","folder":"/","machine":null,"active":false,"tabs":[]},
            {"id":"sp-remote","name":"far","folder":w.cwd(),"machine":"box","active":false,"tabs":[]},
            {"id":"sp-work","name":"work","folder":w.cwd(),"machine":null,"active":false,"tabs":[]},
            {"id":"sp-deep","name":"deep","folder":"/nowhere","machine":null,"active":false,
             "tabs":[{"id":"tb1","terminals":[{"terminal":"t3"}]}]},
        ])),
        hc_out: Ok("t1".into()),
        calls: RefCell::default(),
    };
    // The deepest folder wins: the space whose terminal sits in work/deep.
    let space = rs::find_space(&tools, &mut w.f.core, &w.f.env, &w.f.home, &proj);
    assert_eq!(space.as_deref(), Some("sp-deep"));

    // Without that terminal, the space whose folder holds the session.
    w.f.core.terminals.clear();
    let space = rs::find_space(&tools, &mut w.f.core, &w.f.env, &w.f.home, &proj);
    assert_eq!(space.as_deref(), Some("sp-work"));
}

#[test]
fn a_home_or_root_space_holds_only_sessions_exactly_there() {
    let mut w = World::new();
    let tools = FakeTools {
        ls: app_ls(json!([
            {"id":"sp-home","name":"home","folder":w.f.home,"machine":null,"active":true,"tabs":[]},
        ])),
        hc_out: Ok(String::new()),
        calls: RefCell::default(),
    };
    let inside = w.f.home.join("proj");
    std::fs::create_dir_all(&inside).unwrap();
    assert_eq!(
        rs::find_space(&tools, &mut w.f.core, &w.f.env, &w.f.home, &inside),
        None
    );
    let at_home = rs::find_space(&tools, &mut w.f.core, &w.f.env, &w.f.home, &w.f.home);
    assert_eq!(at_home.as_deref(), Some("sp-home"));
}

#[test]
fn with_no_desktop_app_there_is_no_label_and_hc_still_runs() {
    let mut w = World::new();
    let tools = FakeTools::ok("");
    let cwd = w.cwd();
    let opened = rs::open_terminal(
        &tools,
        &mut w.f.core,
        &w.f.env,
        &w.f.home,
        &cwd,
        Path::new("/b/hw"),
        "k",
    )
    .unwrap();
    assert_eq!(opened.space, None);
    assert_eq!(opened.terminal, "");
    assert!(!tools.calls.borrow()[0].iter().any(|a| a == "--label"));
}

#[test]
fn an_hc_failure_is_reported() {
    let mut w = World::new();
    let tools = FakeTools {
        ls: None,
        hc_out: Err("hc failed: nope".into()),
        calls: RefCell::default(),
    };
    let cwd = w.cwd();
    let err = rs::open_terminal(
        &tools,
        &mut w.f.core,
        &w.f.env,
        &w.f.home,
        &cwd,
        Path::new("/b/hw"),
        "k",
    )
    .unwrap_err();
    assert_eq!(err, "hc failed: nope");
}

#[test]
fn nothing_is_started_by_listing() {
    // A core that is down: the listing still works and is not an error.
    let mut w = World::new();
    w.journal(AgentKind::Claude, A, &[], HOUR, Ending::Quit);
    w.f.core.fail_with(Some(CoreError::NotRunning));
    assert_eq!(w.rows().len(), 1);
    let _: &FakePlatform = &w.f.platform;
    let _ = w.f.platform.now_ms();
    let _ = w.f.core.hello();
}
