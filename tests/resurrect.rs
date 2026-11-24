//! `herdr-wake resurrect`: what is listed and what is left out, filters,
//! the table and JSON, numbers and ids, the checks before a session is
//! brought back, and the terminal that is opened.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
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

struct FakeTools {
    ls: Option<String>,
    hc_out: Result<String, String>,
    calls: RefCell<Vec<Vec<String>>>,
    codex_open: Vec<PathBuf>,
}

impl FakeTools {
    fn ok(out: &str) -> FakeTools {
        FakeTools {
            ls: None,
            hc_out: Ok(out.to_string()),
            calls: RefCell::default(),
            codex_open: Vec::new(),
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
    fn codex_open_files(&self) -> Vec<PathBuf> {
        self.codex_open.clone()
    }
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
        self.age(&path, ago_ms);
    }

    /// Sets a file's modification time to `ago_ms` before the fake now.
    fn age(&self, path: &Path, ago_ms: u64) {
        let then = UNIX_EPOCH + std::time::Duration::from_millis(self.now - ago_ms);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(then)
            .unwrap();
    }

    fn rows(&mut self) -> Vec<rs::Row> {
        self.collect(&FakeTools::ok("")).rows
    }

    fn collect(&mut self, tools: &FakeTools) -> rs::Collected {
        rs::collect(&mut self.f.ctx(), tools)
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

#[test]
fn a_transcript_written_in_the_last_two_minutes_counts_as_open() {
    let mut w = World::new();
    let cwd = w.cwd();
    w.journal(AgentKind::Claude, A, &[], HOUR, Ending::Quit);
    w.journal(AgentKind::Claude, B, &[], HOUR, Ending::Quit);
    w.claude_transcript_aged(A, &[user(&cwd, "just now")], 30_000);
    w.claude_transcript_aged(B, &[user(&cwd, "a while ago")], 3 * 60_000);
    // A transcript-only session follows the same rule.
    w.claude_transcript_aged(C, &[user(&cwd, "only a transcript")], 60_000);

    let rows = w.rows();
    assert!(row(&rows, A).open);
    assert!(!row(&rows, B).open);
    assert!(row(&rows, C).open);
    let (shown, _) = rs::select(&rows, &Filter::default(), 20);
    assert_eq!(ids(&shown), [B]);
}

#[test]
fn freshness_is_two_minutes_and_unknown_is_not_fresh() {
    let now = 1_000_000_000;
    assert!(rs::is_fresh(now, now));
    assert!(rs::is_fresh(now, now - 119_999));
    assert!(!rs::is_fresh(now, now - 120_000));
    assert!(!rs::is_fresh(now, 0));
    // A file dated in the future (a clock that stepped) counts as fresh.
    assert!(rs::is_fresh(now, now + 5_000));
}

#[test]
fn a_rollout_a_running_codex_holds_open_hides_its_session() {
    let mut w = World::new();
    w.journal(AgentKind::Codex, D, &[], HOUR, Ending::Quit);
    w.journal(AgentKind::Codex, C, &[], HOUR, Ending::Quit);
    let mut tools = FakeTools::ok("");
    tools.codex_open = vec![PathBuf::from(format!(
        "/home/u/.codex/sessions/2026/11/20/rollout-2026-11-20T09-15-02-{D}.jsonl"
    ))];
    let rows = w.collect(&tools).rows;
    assert!(row(&rows, D).open);
    assert!(!row(&rows, C).open);
}

#[test]
fn lsof_output_names_the_rollouts_and_nothing_else() {
    let out = format!(
        "p123\nf5\nn/home/u/.codex/sessions/2026/11/20/rollout-2026-11-20T09-15-02-{D}.jsonl\n\
         f6\nn/home/u/.codex/auth.json\nf7\nn/dev/ttys001\n\
         p124\nf4\nn/x/rollout-notes.txt\n"
    );
    let files = rs::parse_lsof(&out);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].to_string_lossy().ends_with(&format!("{D}.jsonl")));
    assert!(rs::parse_lsof("").is_empty());
}

#[test]
fn the_proc_scan_finds_rollouts_held_by_codex_processes_only() {
    let root = tempfile::tempdir().unwrap();
    let rollout = |id: &str| format!("/h/.codex/sessions/rollout-2026-11-20T09-15-02-{id}.jsonl");
    let proc = |pid: &str, cmdline: &str, fds: &[&str]| {
        let d = root.path().join(pid);
        std::fs::create_dir_all(d.join("fd")).unwrap();
        std::fs::write(d.join("cmdline"), cmdline).unwrap();
        for (i, target) in fds.iter().enumerate() {
            std::os::unix::fs::symlink(target, d.join("fd").join(i.to_string())).unwrap();
        }
    };
    let (x, y, z) = (
        "11111111-1111-4111-8111-111111111111",
        "22222222-1111-4111-8111-111111111111",
        "33333333-1111-4111-8111-111111111111",
    );
    // The native program, the script run by node, a different program.
    proc("100", "codex\0resume\0abc\0", &[&rollout(x), "/dev/null"]);
    proc(
        "101",
        "/usr/bin/node\0/lib/codex/bin/codex.js\0",
        &[&rollout(y)],
    );
    proc("102", "vim\0notes\0", &[&rollout(z)]);
    std::fs::create_dir_all(root.path().join("self")).unwrap();

    let mut found: Vec<String> = rs::proc_open_rollouts(root.path())
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    found.sort();
    assert_eq!(found, [rollout(x), rollout(y)]);
    assert!(rs::proc_open_rollouts(&root.path().join("missing")).is_empty());
}

#[test]
fn collect_says_whether_core_answered() {
    let mut w = World::new();
    assert!(w.collect(&FakeTools::ok("")).core_up);
    w.f.core.fail_with(Some(CoreError::NotRunning));
    assert!(!w.collect(&FakeTools::ok("")).core_up);
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
    rs::remember(&w.f.env, &w.f.home, w.now, &Filter::default(), &shown).unwrap();

    let got = rs::resolve(&rows, "2", &Filter::default(), &w.f.env, &w.f.home, w.now).unwrap();
    assert_eq!(got.id(), shown[1].id());
    assert!(rs::resolve(&rows, "9", &Filter::default(), &w.f.env, &w.f.home, w.now).is_err());
    assert!(rs::resolve(&rows, "0", &Filter::default(), &w.f.env, &w.f.home, w.now).is_err());
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
    rs::remember(&w.f.env, &w.f.home, w.now, &listed, &shown).unwrap();

    // The same filters, or none at all, work.
    assert!(rs::resolve(&rows, "1", &listed, &w.f.env, &w.f.home, w.now).is_ok());
    assert!(rs::resolve(&rows, "1", &Filter::default(), &w.f.env, &w.f.home, w.now).is_ok());
    // Other filters do not.
    let other = Filter {
        scope: Some(w.cwd()),
        ..Filter::default()
    };
    let err = rs::resolve(&rows, "1", &other, &w.f.env, &w.f.home, w.now).unwrap_err();
    assert!(err.contains("other filters"), "{err}");
}

#[test]
fn each_herdr_terminal_numbers_its_own_listing() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let (shown, _) = rs::select(&rows, &Filter::default(), 20);
    let mut one = w.f.env.clone();
    one.insert("HC_TERMINAL_ID".into(), "t1".into());
    let mut two = w.f.env.clone();
    two.insert("HC_TERMINAL_ID".into(), "t2".into());
    rs::remember(&one, &w.f.home, w.now, &Filter::default(), &shown).unwrap();

    assert!(rs::resolve(&rows, "1", &Filter::default(), &one, &w.f.home, w.now).is_ok());
    let err = rs::resolve(&rows, "1", &Filter::default(), &two, &w.f.home, w.now).unwrap_err();
    assert!(err.contains("no earlier listing"), "{err}");

    // The other terminal lists something else: the first is not disturbed.
    let narrow = Filter {
        agent: Some(AgentKind::Codex),
        ..Filter::default()
    };
    let (only_codex, _) = rs::select(&rows, &narrow, 20);
    rs::remember(&two, &w.f.home, w.now, &narrow, &only_codex).unwrap();
    let a = rs::resolve(&rows, "1", &Filter::default(), &one, &w.f.home, w.now).unwrap();
    let b = rs::resolve(&rows, "1", &Filter::default(), &two, &w.f.home, w.now).unwrap();
    assert_eq!(a.id(), shown[0].id());
    assert_eq!(b.agent(), AgentKind::Codex);
}

#[test]
fn a_listing_is_good_for_an_hour() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let (shown, _) = rs::select(&rows, &Filter::default(), 20);
    rs::remember(&w.f.env, &w.f.home, w.now, &Filter::default(), &shown).unwrap();
    let at = |later: u64| {
        rs::resolve(
            &rows,
            "1",
            &Filter::default(),
            &w.f.env,
            &w.f.home,
            w.now + later,
        )
    };
    assert!(at(HOUR).is_ok());
    let err = at(HOUR + 1).unwrap_err();
    assert!(
        err.contains("more than an hour old") && err.contains("again"),
        "{err}"
    );
}

#[test]
fn a_number_without_a_listing_is_an_error() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let err = rs::resolve(&rows, "1", &Filter::default(), &w.f.env, &w.f.home, w.now).unwrap_err();
    assert!(err.contains("no earlier listing"), "{err}");
}

#[test]
fn a_session_id_is_found_and_an_unknown_one_is_not() {
    let mut w = World::new();
    let rows = titled(&mut w);
    let f = Filter::default();
    assert_eq!(
        rs::resolve(&rows, B, &f, &w.f.env, &w.f.home, w.now)
            .unwrap()
            .id(),
        B
    );
    assert!(rs::resolve(&rows, "nope", &f, &w.f.env, &w.f.home, w.now).is_err());
}

#[test]
fn an_id_shared_by_two_agents_needs_the_agent() {
    let mut w = World::new();
    w.journal(AgentKind::Claude, A, &[], HOUR, Ending::Quit);
    w.journal(AgentKind::Codex, A, &[], HOUR, Ending::Quit);
    let rows = w.rows();
    let err = rs::resolve(&rows, A, &Filter::default(), &w.f.env, &w.f.home, w.now).unwrap_err();
    assert!(err.contains("--agent"), "{err}");
    let f = Filter {
        agent: Some(AgentKind::Codex),
        ..Filter::default()
    };
    assert_eq!(
        rs::resolve(&rows, A, &f, &w.f.env, &w.f.home, w.now)
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
    assert!(rs::resolve(&rows, "424242", &f, &w.f.env, &w.f.home, w.now).is_err());
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
    w.age(&moved.join(format!("{A}.jsonl")), 240 * HOUR);

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
    let mut core = w.f.core.clone();
    let err = rs::run_ticket(
        &w.f.platform,
        &env,
        &w.f.home,
        key,
        &mut core,
        &FakeTools::ok(""),
        &say,
    );
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
        codex_open: Vec::new(),
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
        codex_open: Vec::new(),
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
fn a_named_space_beats_the_terminal_and_folder_guesses() {
    let mut w = World::new();
    let mut here = Terminal::new("t7");
    here.labels.insert("herdr.space".into(), "sp-here".into());
    w.f.core.insert(here);
    w.f.env.insert("HC_TERMINAL_ID".into(), "t7".into());
    let tools = FakeTools {
        ls: app_ls(json!([
            {"id":"sp-work","name":"work","folder":w.cwd(),"machine":null,"active":true,"tabs":[]},
        ])),
        hc_out: Ok("t12\n".into()),
        calls: RefCell::default(),
        codex_open: Vec::new(),
    };
    let cwd = w.cwd();
    let space = rs::choose_space(
        Some("sp-panel_1"),
        &tools,
        &mut w.f.core,
        &w.f.env,
        &w.f.home,
        &cwd,
    );
    assert_eq!(space.as_deref(), Some("sp-panel_1"));
    // Without one, the terminal's own space wins as before.
    let guessed = rs::choose_space(None, &tools, &mut w.f.core, &w.f.env, &w.f.home, &cwd);
    assert_eq!(guessed.as_deref(), Some("sp-here"));
    let opened = rs::open_terminal_in(
        &tools,
        space,
        &cwd,
        Path::new("/b/herdr-wake"),
        "key-2",
        None,
    )
    .unwrap();
    assert_eq!(opened.space.as_deref(), Some("sp-panel_1"));
    let calls = tools.calls.borrow();
    assert!(calls[0]
        .windows(2)
        .any(|p| p == ["--label", "herdr.space=sp-panel_1"]));
    assert!(!calls[0].iter().any(|a| a.contains("sp-here")));
}

#[test]
fn a_space_id_is_plain_characters_only() {
    for ok in [
        "sp7ca99d146c9cd3e17fe081cbf0f1a940",
        "a",
        "sp-1_B",
        &"x".repeat(64),
    ] {
        assert!(rs::valid_space_id(ok), "{ok}");
    }
    for bad in [
        "",
        " ",
        "a b",
        "a=b",
        "sp\u{1b}[2J",
        "a\nb",
        "../x",
        "a,b",
        "\u{e9}",
        &"x".repeat(65),
    ] {
        assert!(!rs::valid_space_id(bad), "{bad:?}");
    }
}

#[test]
fn the_json_listing_has_what_the_panel_needs() {
    let mut w = World::new();
    w.journal(AgentKind::Claude, A, &[], HOUR, Ending::Quit);
    let rows = w.rows();
    let (shown, _) = rs::select(&rows, &Filter::default(), 20);
    let j = rs::list_json(&shown, w.now);
    let first = &j["sessions"][0];
    assert_eq!(first["transcript"], "gone");
    assert_eq!(first["folder_exists"], true);
    assert!(first["ended_ms"].is_u64(), "{first}");

    let mut w = World::new();
    let cwd = w.cwd();
    w.claude_transcript(A, &[user(&cwd, "hello")]);
    let rows = w.rows();
    let (shown, _) = rs::select(&rows, &Filter::default(), 20);
    let j = rs::list_json(&shown, w.now);
    let first = &j["sessions"][0];
    assert_eq!(first["source"], "transcript");
    assert_eq!(first["transcript"], "present");
    assert!(first["ended_ms"].is_null());
}

/// Tools that show one app tab for the new terminal and record renames.
struct AppTools {
    ls: String,
    did: RefCell<Vec<Vec<String>>>,
}

impl Tools for AppTools {
    fn herdr_app_ls(&self) -> Option<String> {
        Some(self.ls.clone())
    }
    fn hc(&self, _args: &[String]) -> Result<String, String> {
        Ok("t12 started\n".to_string())
    }
    fn codex_open_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn herdr_app_do(&self, args: &[String]) -> Result<String, String> {
        self.did.borrow_mut().push(args.to_vec());
        Ok(String::new())
    }
}

#[test]
fn the_new_tab_is_named_after_the_session_title() {
    let tools = AppTools {
        ls: json!({"spaces":[{"id":"sp1","tabs":[
            {"id":"tb1","terminals":[{"terminal":"t9"}]},
            {"id":"tb2","terminals":[{"terminal":"t12"}]},
        ]}]})
        .to_string(),
        did: RefCell::default(),
    };
    let opened = rs::open_terminal_in(
        &tools,
        Some("sp1".to_string()),
        Path::new("/w"),
        Path::new("/b/hw"),
        "k",
        Some("Fix  the\nlogin \u{1b}[2J bug"),
    )
    .unwrap();
    assert_eq!(opened.terminal, "t12 started");
    let did = tools.did.borrow();
    assert_eq!(did.len(), 1);
    assert_eq!(did[0][0], "tab.rename");
    let arg: serde_json::Value = serde_json::from_str(&did[0][1]).unwrap();
    assert_eq!(arg["id"], "tb2");
    assert_eq!(arg["name"], "Fix the login [2J bug");
}

#[test]
fn no_space_or_no_title_means_no_rename() {
    let tools = AppTools {
        ls: json!({"spaces":[]}).to_string(),
        did: RefCell::default(),
    };
    rs::open_terminal_in(
        &tools,
        None,
        Path::new("/w"),
        Path::new("/b"),
        "k",
        Some("t"),
    )
    .unwrap();
    rs::open_terminal_in(
        &tools,
        Some("sp1".into()),
        Path::new("/w"),
        Path::new("/b"),
        "k",
        None,
    )
    .unwrap();
    assert!(tools.did.borrow().is_empty());
    assert_eq!(rs::tab_name("   "), None);
    assert_eq!(rs::tab_name(&"x".repeat(100)).unwrap().chars().count(), 60);
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
        codex_open: Vec::new(),
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

// ------------------------------------------- one launch per session

#[test]
fn of_several_processes_with_one_key_exactly_one_runs_the_agent() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    for _ in 0..8 {
        w.f.platform
            .push_run_result(Ok(ExitInfo::exited(900, 0, 1)));
    }
    let said: Vec<Vec<String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| run_ticket(&w, &key).1))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(w.f.platform.runs().len(), 1, "the agent ran once");
    assert_eq!(said.iter().filter(|s| s.is_empty()).count(), 1);
    assert!(said
        .iter()
        .filter(|s| !s.is_empty())
        .all(|s| s[0].contains("no launch ticket")));
}

#[test]
fn a_second_launch_of_a_pending_session_is_refused_until_the_first_starts_or_lapses() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();

    let err = rs::write_ticket(&w.f.env, &w.f.home, w.now + 1, &l, false).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(err.to_string().contains("already pending"), "{err}");

    // Another session is not held up.
    let mut other = l.clone();
    other.entry.session_id = B.into();
    assert!(rs::write_ticket(&w.f.env, &w.f.home, w.now + 2, &other, false).is_ok());

    // Once the first terminal has taken its ticket, the session may be
    // launched again (it is then judged by the open checks).
    w.f.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 1)));
    run_ticket(&w, &key);
    assert!(rs::write_ticket(&w.f.env, &w.f.home, w.now + 3, &l, false).is_ok());
}

#[test]
fn a_pending_mark_lapses_with_its_ticket() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    let mark = std::fs::read_dir(paths::resurrect_dir(&w.f.env, &w.f.home))
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("pending-"))
        .expect("a pending mark")
        .path();
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&mark)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    assert!(rs::write_ticket(&w.f.env, &w.f.home, w.now + 1, &l, false).is_ok());
}

#[test]
fn the_new_terminal_checks_again_that_the_session_is_not_running() {
    // A Running record on a live terminal.
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    let mut rec = Record::new(
        AgentKind::Claude,
        A,
        w.cwd(),
        agent_bin(AgentKind::Claude),
        "b",
    );
    rec.state = State::Running;
    w.f.core.insert(terminal_with("t9", Some(&rec), None));
    let (_, said) = run_ticket(&w, &key);
    assert!(said[0].contains("running now"), "{said:?}");
    assert!(w.f.platform.runs().is_empty());

    // A live Claude session file.
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    let dir = w.f.home.join(".claude/sessions");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("77.json"),
        json!({"pid": 77, "sessionId": A}).to_string(),
    )
    .unwrap();
    w.f.platform
        .add_process(ProcInfo::new(77, 1, 1, vec!["claude".into()]));
    let (_, said) = run_ticket(&w, &key);
    assert!(said[0].contains("running now"), "{said:?}");
    assert!(w.f.platform.runs().is_empty());

    // A session file written a moment ago.
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    w.age(&w.claude_file(A), 10_000);
    let (_, said) = run_ticket(&w, &key);
    assert!(said[0].contains("running now"), "{said:?}");
    assert!(w.f.platform.runs().is_empty());
}

#[test]
fn a_fork_ticket_runs_even_though_the_session_is_open() {
    let mut w = World::new();
    let r = ready(&mut w, &[]);
    let l = rs::prepare(&r, &mut w.f.ctx(), true).unwrap();
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, true).unwrap();
    w.age(&w.claude_file(A), 10_000);
    w.f.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 1)));
    let (_, said) = run_ticket(&w, &key);
    assert!(said.is_empty(), "{said:?}");
    assert_eq!(
        w.f.platform.runs()[0].argv.last().map(String::as_str),
        Some("--fork-session")
    );
}

// ------------------------------------------- what is shown, what is refused

fn words(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn flags_are_shown_by_name_except_a_few_enumerated_values() {
    let shown = rs::display_words(
        &words(&[
            "--model",
            "opus",
            "--append-system-prompt",
            "never tell anyone hunter2",
            "--add-dir",
            "/secret/place",
            "--permission-mode",
            "plan",
            "--dangerously-skip-permissions",
            "--mcp-config=./mcp.json",
            "--sandbox=read-only",
            "-sdanger-full-access",
            "-a",
            "never",
            "-c",
            "model_provider=x",
            "-uhunter2",
        ]),
        &[],
    );
    assert_eq!(
        shown,
        words(&[
            "--model",
            "opus",
            "--append-system-prompt",
            "--add-dir",
            "--permission-mode",
            "plan",
            "--dangerously-skip-permissions",
            "--mcp-config",
            "--sandbox=read-only",
            "-sdanger-full-access",
            "-a",
            "never",
            "-c",
            "-u",
        ])
    );
    let joined = shown.join(" ");
    for secret in [
        "hunter2",
        "secret/place",
        "mcp.json",
        "model_provider",
        "never tell",
    ] {
        assert!(!joined.contains(secret), "{secret} shown in {joined}");
    }
}

#[test]
fn the_json_listing_shows_flag_names_only() {
    let mut w = World::new();
    w.journal(
        AgentKind::Claude,
        A,
        &[
            "--append-system-prompt",
            "hunter2 is the password",
            "--model",
            "opus",
        ],
        HOUR,
        Ending::Quit,
    );
    let rows = w.rows();
    let (shown, _) = rs::select(&rows, &Filter::default(), 20);
    let j = rs::list_json(&shown, w.now);
    assert_eq!(
        j["sessions"][0]["flags"],
        json!(["--append-system-prompt", "--model", "opus"])
    );
    assert!(!j.to_string().contains("hunter2"));
    let table = rs::render_list(&shown, 1, w.now, &w.f.home, false);
    assert!(!table.contains("hunter2"));
}

#[test]
fn the_shown_command_hides_values_and_the_printed_one_keeps_them() {
    let mut w = World::new();
    let l = launch_of(
        &mut w,
        &["--model", "opus", "--append-system-prompt", "hunter2 words"],
    );
    let exact = l.command(&w.f.env);
    assert!(exact.contains("'hunter2 words'"), "{exact}");
    let shown = l.display_command(&w.f.env);
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(
        shown.contains("--append-system-prompt --model opus")
            || shown.contains("--model opus --append-system-prompt"),
        "{shown}"
    );
    assert!(shown.ends_with(&format!("--resume {A}")), "{shown}");
}

#[test]
fn the_command_is_built_from_parts_so_a_folder_cannot_confuse_it() {
    let launch = rs::Launch {
        entry: history::Entry::from_record(
            None,
            &Record::new(AgentKind::Claude, A, "/x".into(), "/bin/claude".into(), "b"),
            1,
        ),
        argv: words(&["/bin/claude", "--model", "it's", "--resume", A]),
        env: [("CLAUDE_CONFIG_DIR".to_string(), "/c c && d".to_string())]
            .into_iter()
            .collect(),
        cwd: PathBuf::from("/tmp/a && b/it's"),
        before: Vec::new(),
    };
    assert_eq!(
        launch.command(&BTreeMap::new()),
        format!(
            "cd '/tmp/a && b/it'\\''s' && CLAUDE_CONFIG_DIR='/c c && d' /bin/claude --model 'it'\\''s' --resume {A}"
        )
    );
    // The shell's own value is not repeated.
    let current: BTreeMap<_, _> = [("CLAUDE_CONFIG_DIR".to_string(), "/c c && d".to_string())]
        .into_iter()
        .collect();
    assert!(!launch.command(&current).contains("CLAUDE_CONFIG_DIR"));
}

#[test]
fn a_command_that_cannot_be_shown_safely_is_refused() {
    // Control characters in a flag.
    let mut w = World::new();
    let r = ready(&mut w, &["--model", "op\u{1b}[31mus"]);
    let err = rs::prepare(&r, &mut w.f.ctx(), false).unwrap_err();
    assert!(err.contains("control or direction-changing"), "{err}");

    // A direction-changing character in the folder.
    let mut w = World::new();
    let mut r = ready(&mut w, &[]);
    let odd = w.f.home.join("proj\u{202e}txt");
    std::fs::create_dir(&odd).unwrap();
    r.entry.cwd = odd;
    let err = rs::prepare(&r, &mut w.f.ctx(), false).unwrap_err();
    assert!(err.contains("control or direction-changing"), "{err}");

    // A line break in the folder name.
    let mut w = World::new();
    let mut r = ready(&mut w, &[]);
    let odd = w.f.home.join("proj\nrm");
    std::fs::create_dir(&odd).unwrap();
    r.entry.cwd = odd;
    assert!(rs::prepare(&r, &mut w.f.ctx(), false).is_err());
}

// ------------------------------------------------ the real tools, safely

/// A folder of fake `hc` and `herdr-app` programs, as a PATH.
fn tool_dir(hc: &str, hc_mode: u32) -> (tempfile::TempDir, BTreeMap<String, String>) {
    let d = tempfile::tempdir().unwrap();
    let write = |name: &str, body: &str, mode: u32| {
        let p = d.path().join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    write("hc", hc, hc_mode);
    write("herdr-app", "#!/bin/sh\necho '{\"spaces\":[]}'\n", 0o755);
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), d.path().display().to_string());
    (d, env)
}

#[test]
fn hc_and_herdr_app_must_pass_the_program_safety_check() {
    let args = vec!["run".to_string()];
    // A good hc.
    let (_d, env) = tool_dir("#!/bin/sh\necho t5\n", 0o755);
    assert_eq!(rs::SystemTools::new(env).hc(&args).unwrap().trim(), "t5");

    // Writable by the group: not run.
    let (_d, env) = tool_dir("#!/bin/sh\necho t5\n", 0o775);
    let err = rs::SystemTools::new(env).hc(&args).unwrap_err();
    assert!(
        err.contains("not safe to run") && err.contains("writable by group"),
        "{err}"
    );

    // Not executable.
    let (_d, env) = tool_dir("#!/bin/sh\necho t5\n", 0o644);
    assert!(rs::SystemTools::new(env).hc(&args).is_err());

    // Missing.
    let env: BTreeMap<_, _> = [("PATH".to_string(), "/nonexistent".to_string())].into();
    let err = rs::SystemTools::new(env.clone()).hc(&args).unwrap_err();
    assert!(err.contains("cannot find hc"), "{err}");
    assert_eq!(rs::SystemTools::new(env).herdr_app_ls(), None);

    // A herdr-app that is not safe is treated as absent.
    let (d, env) = tool_dir("#!/bin/sh\n", 0o755);
    assert!(rs::SystemTools::new(env.clone()).herdr_app_ls().is_some());
    std::fs::set_permissions(
        d.path().join("herdr-app"),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    assert_eq!(rs::SystemTools::new(env).herdr_app_ls(), None);
}

#[test]
fn a_failing_hc_reports_its_first_error_line_and_a_slow_one_is_stopped() {
    let args = vec!["run".to_string()];
    let (_d, env) = tool_dir("#!/bin/sh\necho 'no such space' >&2\nexit 3\n", 0o755);
    let err = rs::SystemTools::new(env).hc(&args).unwrap_err();
    assert_eq!(err, "hc failed: no such space");

    let (_d, env) = tool_dir("#!/bin/sh\nsleep 30\n", 0o755);
    let mut tools = rs::SystemTools::new(env.clone());
    tools.hc_timeout = std::time::Duration::from_millis(300);
    let started = std::time::Instant::now();
    let err = tools.hc(&args).unwrap_err();
    assert!(err.contains("did not finish"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));

    let (_d, env) = tool_dir("#!/bin/sh\n", 0o755);
    std::fs::write(
        std::path::Path::new(&env["PATH"]).join("herdr-app"),
        "#!/bin/sh\nsleep 30\n",
    )
    .unwrap();
    let mut tools = rs::SystemTools::new(env);
    tools.app_timeout = std::time::Duration::from_millis(300);
    let started = std::time::Instant::now();
    assert_eq!(tools.herdr_app_ls(), None);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

// ------------------------------------------------------ the run lock

use herdr_wake::platform::{BootService, Platform as PlatformTrait};
use herdr_wake::record::ProcRef;
use std::sync::mpsc;
use std::sync::Mutex;

/// A platform whose `run_foreground` takes as long as the test wants: it
/// says it has started, then waits to be let go. Everything else is the
/// fake's.
struct SlowAgent {
    inner: FakePlatform,
    started: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl PlatformTrait for SlowAgent {
    fn process(&self, pid: u32) -> Option<ProcInfo> {
        self.inner.process(pid)
    }
    fn alive(&self, p: &ProcRef) -> bool {
        self.inner.alive(p)
    }
    fn boot_id(&self) -> String {
        self.inner.boot_id()
    }
    fn login_shell_argv(&self, shell: &Path) -> Vec<String> {
        self.inner.login_shell_argv(shell)
    }
    fn exec(&self, argv: &[String], env: &BTreeMap<String, String>, cwd: &Path) -> std::io::Error {
        self.inner.exec(argv, env, cwd)
    }
    fn run_foreground(
        &self,
        argv: &[String],
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> std::io::Result<ExitInfo> {
        let r = self.inner.run_foreground(argv, env, cwd);
        self.started.lock().unwrap().send(()).unwrap();
        // The agent runs, and has not registered itself anywhere yet.
        self.release.lock().unwrap().recv().unwrap();
        r
    }
    fn shutting_down(&self) -> bool {
        self.inner.shutting_down()
    }
    fn core_pid(&self) -> Option<u32> {
        self.inner.core_pid()
    }
    fn boot_service(&self) -> &dyn BootService {
        self.inner.boot_service()
    }
    fn now_ms(&self) -> u64 {
        self.inner.now_ms()
    }
    fn mono_ms(&self) -> u64 {
        self.inner.mono_ms()
    }
}

fn locks(w: &World) -> Vec<String> {
    std::fs::read_dir(paths::resurrect_dir(&w.f.env, &w.f.home))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("running-"))
                .collect()
        })
        .unwrap_or_default()
}

/// This test process, as the fake platform's process table shows it.
fn register_self(w: &World, start: u64) {
    w.f.platform.add_process(ProcInfo::new(
        std::process::id(),
        1,
        start,
        vec!["herdr-wake".into()],
    ));
}

fn lock_text(agent: &str, id: &str, pid: u32, start: u64) -> String {
    json!({"agent": agent, "id": id, "pid": pid, "start": start}).to_string()
}

fn write_lock(w: &World, text: &str) {
    let dir = paths::resurrect_dir(&w.f.env, &w.f.home);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("running-claude-{A}")), text).unwrap();
}

#[test]
fn a_second_launch_back_to_back_is_refused_while_the_agent_has_not_registered() {
    let mut w = World::new();
    let first = launch_of(&mut w, &[]);
    register_self(&w, 7);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let slow = SlowAgent {
        inner: FakePlatform::new(),
        started: Mutex::new(started_tx),
        release: Mutex::new(release_rx),
    };
    slow.inner.set_now(w.now);
    slow.inner.add_process(ProcInfo::new(
        std::process::id(),
        1,
        7,
        vec!["herdr-wake".into()],
    ));
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &first, false).unwrap();

    let env = {
        let mut e = w.f.env.clone();
        e.insert("SHELL".into(), "/bin/zsh".into());
        e
    };
    std::thread::scope(|scope| {
        let (slow_ref, home, key, mut core, env_ref) =
            (&slow, w.f.home.clone(), key.clone(), w.f.core.clone(), &env);
        let run = scope.spawn(move || {
            rs::run_ticket(
                slow_ref,
                env_ref,
                &home,
                &key,
                &mut core,
                &FakeTools::ok(""),
                &|_| {},
            );
        });
        // The first agent is up, but wrote no session file, has no record
        // and its transcript is as old as before.
        started_rx.recv().unwrap();
        assert_eq!(locks(&w).len(), 1, "the run lock is held while it runs");

        // The listing sees it as running.
        let rows = rs::collect(&mut w.f.ctx(), &FakeTools::ok("")).rows;
        assert!(row(&rows, A).open, "the lock makes it open");
        // A second `resurrect` is refused before it opens anything.
        let err = rs::prepare(row(&rows, A), &mut w.f.ctx(), false).unwrap_err();
        assert!(err.contains("running now"), "{err}");

        // Even a second terminal that got its ticket anyway refuses.
        let second = launch_of_row(&mut w, rows.iter().find(|r| r.id() == A).unwrap());
        let key2 = rs::write_ticket(&w.f.env, &w.f.home, w.now + 1, &second, false).unwrap();
        // The launching side checks the lock once it holds the pending mark,
        // and gives the mark back when it backs off.
        assert!(rs::run_is_live(
            &slow,
            &w.f.env,
            &w.f.home,
            second.entry.agent,
            A
        ));
        rs::abandon_ticket(&w.f.env, &w.f.home, &key2, second.entry.agent, A);
        let key2 = rs::write_ticket(&w.f.env, &w.f.home, w.now + 2, &second, false).unwrap();
        let said = RefCell::new(Vec::new());
        let mut core = w.f.core.clone();
        let mut env2 = env.clone();
        env2.insert("X".into(), "1".into());
        let plain = FakePlatform::new();
        plain.set_now(w.now);
        plain.add_process(ProcInfo::new(
            std::process::id(),
            1,
            7,
            vec!["herdr-wake".into()],
        ));
        rs::run_ticket(
            &plain,
            &env2,
            &w.f.home,
            &key2,
            &mut core,
            &FakeTools::ok(""),
            &|s| said.borrow_mut().push(s.to_string()),
        );
        let said = said.into_inner();
        assert!(
            said[0].contains("running now") || said[0].contains("already being brought back"),
            "{said:?}"
        );
        assert!(plain.runs().is_empty(), "the second agent never started");

        release_tx.send(()).unwrap();
        run.join().unwrap();
    });
    assert!(
        locks(&w).is_empty(),
        "the lock is gone when the agent exits"
    );
    assert_eq!(slow.inner.runs().len(), 1);
}

/// A launch made from a row that is flagged open (for a second ticket).
fn launch_of_row(w: &mut World, row: &rs::Row) -> rs::Launch {
    let mut r = row.clone();
    r.open = false;
    rs::prepare(&r, &mut w.f.ctx(), false).unwrap()
}

#[test]
fn the_run_lock_itself_refuses_a_second_run_when_the_open_checks_see_nothing() {
    // The lock is held by a live process, nothing else shows the session.
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    w.f.platform
        .add_process(ProcInfo::new(555, 1, 9, vec!["herdr-wake".into()]));
    write_lock(&w, &lock_text("claude", A, 555, 9));
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    // `is_open_now` sees the live lock too: either line refuses.
    let (_, said) = run_ticket(&w, &key);
    assert!(said[0].contains("running now"), "{said:?}");
    assert!(w.f.platform.runs().is_empty());
    assert_eq!(locks(&w).len(), 1, "someone else's lock is left alone");
}

#[test]
fn a_lock_left_by_a_crash_does_not_block_and_is_replaced() {
    // Dead pid, a reused pid (other start), a lock nobody can read.
    for text in [
        lock_text("claude", A, 999_999, 5),
        lock_text("claude", A, 555, 1),
        "{not json".to_string(),
    ] {
        let mut w = World::new();
        let l = launch_of(&mut w, &[]);
        w.f.platform
            .add_process(ProcInfo::new(555, 1, 9, vec!["something else".into()]));
        write_lock(&w, &text);
        register_self(&w, 7);
        let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
        w.f.platform
            .push_run_result(Ok(ExitInfo::exited(900, 0, 1)));

        let (_, said) = run_ticket(&w, &key);

        assert!(said.is_empty(), "{text}: {said:?}");
        assert_eq!(w.f.platform.runs().len(), 1, "{text}");
        assert!(locks(&w).is_empty(), "{text}: the lock is cleaned up");
        // The listing does not take the stale lock for a running session.
        assert!(!row(&w.rows(), A).open, "{text}");
    }
}

#[test]
fn a_lock_of_a_dead_run_is_not_open_and_a_live_one_is() {
    let mut w = World::new();
    w.journal(AgentKind::Claude, A, &[], HOUR, Ending::Quit);
    write_lock(&w, &lock_text("claude", A, 999_999, 5));
    assert!(!row(&w.rows(), A).open);
    w.f.platform
        .add_process(ProcInfo::new(999_999, 1, 5, vec!["herdr-wake".into()]));
    assert!(row(&w.rows(), A).open);
    // A pid that is alive but is another process now.
    write_lock(&w, &lock_text("claude", A, 999_999, 6));
    assert!(!row(&w.rows(), A).open);
}

#[test]
fn the_lock_is_dropped_when_the_agent_cannot_start_and_a_fork_takes_none() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    register_self(&w, 7);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    // No scripted result: the agent cannot start.
    run_ticket(&w, &key);
    assert!(locks(&w).is_empty());

    // A fork does not lock the session it copies.
    let mut w = World::new();
    let r = ready(&mut w, &[]);
    let l = rs::prepare(&r, &mut w.f.ctx(), true).unwrap();
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, true).unwrap();
    write_lock(&w, &lock_text("claude", A, 999_999, 5)); // stale, ignored
    w.age(&w.claude_file(A), 10_000); // fresh file: open, fork allowed
    w.f.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 1)));
    let (_, said) = run_ticket(&w, &key);
    assert!(said.is_empty(), "{said:?}");
    assert_eq!(w.f.platform.runs().len(), 1);
}

#[test]
fn the_pending_mark_stays_until_the_run_lock_exists() {
    let mut w = World::new();
    let l = launch_of(&mut w, &[]);
    register_self(&w, 7);
    let key = rs::write_ticket(&w.f.env, &w.f.home, w.now, &l, false).unwrap();
    let dir = paths::resurrect_dir(&w.f.env, &w.f.home);
    let pending = || {
        std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("pending-"))
    };
    assert!(pending());
    w.f.platform
        .push_run_result(Ok(ExitInfo::exited(900, 0, 1)));
    run_ticket(&w, &key);
    assert!(!pending(), "gone once the run is over");
    // While the run lock is held the pending mark is already gone, and the
    // lock stands in: the back-to-back test above covers that gap.
}
