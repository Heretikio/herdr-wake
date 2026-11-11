//! `SocketCore` against an in-process listener that speaks core's plain
//! JSON-RPC mode: one JSON request per line, one JSON reply per line.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use herdr_wake::core::socket::{connect, socket_path};
use herdr_wake::core::{Core, CoreError, TerminalId};
use serde_json::{json, Value};

const BUDGET: Duration = Duration::from_secs(2);

/// A fake core: answers each request with `reply(method, params)`, which
/// returns either `{"result": ..}` or `{"error": ..}` (the id is filled in).
/// Every request it reads is kept in `seen`.
struct Server {
    path: PathBuf,
    seen: Arc<Mutex<Vec<Value>>>,
    connections: Arc<Mutex<usize>>,
    _dir: tempfile::TempDir,
}

impl Server {
    fn start<F>(reply: F) -> Server
    where
        F: Fn(&str, &Value) -> Value + Send + Sync + 'static,
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let connections = Arc::new(Mutex::new(0));
        let connections2 = connections.clone();
        let reply = Arc::new(reply);
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                *connections2.lock().unwrap() += 1;
                let seen = seen2.clone();
                let reply = reply.clone();
                thread::spawn(move || serve(conn, &seen, &*reply));
            }
        });
        Server {
            path,
            seen,
            connections,
            _dir: dir,
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

fn serve(conn: UnixStream, seen: &Mutex<Vec<Value>>, reply: &dyn Fn(&str, &Value) -> Value) {
    let mut out = conn.try_clone().unwrap();
    for line in BufReader::new(conn).lines() {
        let Ok(line) = line else { return };
        let req: Value = serde_json::from_str(&line).unwrap();
        seen.lock().unwrap().push(req.clone());
        let mut body = reply(req["method"].as_str().unwrap(), &req["params"]);
        body["jsonrpc"] = json!("2.0");
        body["id"] = req["id"].clone();
        if writeln!(out, "{body}").is_err() {
            return;
        }
    }
}

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/core")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn not_found() -> Value {
    json!({"error": {"code": -32000, "message": "not_found",
        "data": {"kind": "not_found", "hint": "hc ls lists terminals"}}})
}

#[test]
fn get_parses_a_full_terminal_record() {
    let server = Server::start(|method, _| match method {
        "terminal.get" => json!({"result": fixture("terminal-get.json")["result"]}),
        _ => not_found(),
    });
    let mut core = connect(&server.path, BUDGET).unwrap();

    let t = core.get(&TerminalId::from("t3")).unwrap();

    assert_eq!(t.address, TerminalId::from("t3"));
    assert_eq!(t.labels.get("team").map(String::as_str), Some("infra"));
    assert_eq!(t.labels.get("wake").map(String::as_str), Some("{\"v\":1}"));
    assert_eq!(t.cwd, Some(PathBuf::from("/home/u/proj")));
    assert!(t.keep);
    assert_eq!(t.state, "live");
    assert_eq!(t.pid, Some(21341));
    assert_eq!(t.created_ms, 1_794_354_275_682);
    assert_eq!(t.foreground_pid, Some(21377));
    let reqs = server.requests();
    assert_eq!(reqs[0]["method"], "terminal.get");
    assert_eq!(reqs[0]["params"], json!({"terminal": "t3"}));
    assert_eq!(reqs[0]["jsonrpc"], "2.0");
}

#[test]
fn get_tolerates_missing_optional_fields() {
    let server = Server::start(|_, _| {
        json!({"result": {"address": "t9", "labels": {}, "cwd": "/w", "state": "finished",
            "created_ms": 5, "brand_new_field": [1, 2]}})
    });
    let mut core = connect(&server.path, BUDGET).unwrap();

    let t = core.get(&TerminalId::from("t9")).unwrap();

    assert_eq!(t.address, TerminalId::from("t9"));
    assert!(t.labels.is_empty());
    assert!(!t.keep);
    assert_eq!(t.state, "finished");
    assert_eq!(t.pid, None);
    assert_eq!(t.foreground_pid, None);
    assert_eq!(t.created_ms, 5);
}

#[test]
fn not_found_error_maps_to_not_found() {
    let server = Server::start(|_, _| not_found());
    let mut core = connect(&server.path, BUDGET).unwrap();

    assert_eq!(
        core.get(&TerminalId::from("t404")),
        Err(CoreError::NotFound)
    );
}

#[test]
fn other_errors_map_to_protocol() {
    let server = Server::start(|_, _| {
        json!({"error": {"code": -32602, "message": "invalid_params",
            "data": {"kind": "invalid_params", "hint": "label keys can't contain ="}}})
    });
    let mut core = connect(&server.path, BUDGET).unwrap();

    match core.get(&TerminalId::from("t1")) {
        Err(CoreError::Protocol(msg)) => assert!(msg.contains("invalid_params"), "{msg}"),
        other => panic!("expected Protocol, got {other:?}"),
    }
}

#[test]
fn a_reply_that_is_not_json_is_a_protocol_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        let (conn, _) = listener.accept().unwrap();
        let mut out = conn.try_clone().unwrap();
        let mut line = String::new();
        BufReader::new(conn).read_line(&mut line).unwrap();
        writeln!(out, "this is not json").unwrap();
        thread::sleep(Duration::from_secs(5));
    });
    let mut core = connect(&path, BUDGET).unwrap();

    assert!(matches!(
        core.get(&TerminalId::from("t1")),
        Err(CoreError::Protocol(_))
    ));
}

#[test]
fn a_core_that_accepts_and_never_replies_times_out_within_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        let held: Vec<UnixStream> = listener.incoming().map_while(Result::ok).collect();
        drop(held);
    });
    let budget = Duration::from_millis(300);
    let started = Instant::now();

    let mut core = connect(&path, budget).unwrap();
    let got = core.get(&TerminalId::from("t1"));

    let took = started.elapsed();
    assert_eq!(got, Err(CoreError::Timeout));
    assert!(took >= budget, "returned early: {took:?}");
    assert!(took < budget + Duration::from_millis(700), "took {took:?}");
}

#[test]
fn the_budget_is_one_deadline_across_calls() {
    // Each answer takes 400 ms: the first call fits in 600 ms, the second
    // would end at 800 ms, past the deadline set at connect.
    let server = Server::start(|_, _| {
        thread::sleep(Duration::from_millis(400));
        json!({"result": {"version": "0.1.20", "features": []}})
    });
    let started = Instant::now();
    let mut core = connect(&server.path, Duration::from_millis(600)).unwrap();

    assert!(core.hello().is_ok());
    assert_eq!(core.hello(), Err(CoreError::Timeout));
    assert!(started.elapsed() < Duration::from_millis(1200));
}

#[test]
fn a_missing_socket_is_not_running_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr-core").join("default.sock");

    let got = connect(&path, BUDGET);

    assert_eq!(got.err(), Some(CoreError::NotRunning));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn a_socket_file_with_no_listener_is_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sock");
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists());

    assert_eq!(connect(&path, BUDGET).err(), Some(CoreError::NotRunning));
}

#[test]
fn hello_reads_version_and_features() {
    let server = Server::start(|_, _| {
        json!({"result": {"generation": 1, "version": "0.1.20",
            "core": {"id": "c1", "name": "ares"}, "framed": false,
            "methods": ["hello", "terminal.get"], "features": ["labels", "keep"],
            "login_session": null}})
    });
    let mut core = connect(&server.path, BUDGET).unwrap();

    let hello = core.hello().unwrap();

    assert_eq!(hello.version, "0.1.20");
    assert_eq!(hello.features, vec!["labels", "keep"]);
    assert_eq!(server.requests()[0]["method"], "hello");
    assert_eq!(server.requests()[0]["params"], json!({}));
}

#[test]
fn label_sends_values_and_an_empty_string_to_remove() {
    let server = Server::start(|_, _| {
        json!({"result": {"address": "t3", "labels": {"wake": "x"}, "cwd": "/w",
            "state": "live", "created_ms": 1}})
    });
    let mut core = connect(&server.path, BUDGET).unwrap();
    let mut changes = BTreeMap::new();
    changes.insert("wake".to_string(), Some("x".to_string()));
    changes.insert("old".to_string(), None);

    let t = core.label(&TerminalId::from("t3"), &changes).unwrap();

    assert_eq!(t.labels.get("wake").map(String::as_str), Some("x"));
    let req = &server.requests()[0];
    assert_eq!(req["method"], "terminal.label");
    assert_eq!(
        req["params"],
        json!({"terminal": "t3", "labels": {"wake": "x", "old": ""}})
    );
}

#[test]
fn list_sends_all_and_returns_local_terminals() {
    let server = Server::start(|_, _| {
        json!({"result": {"hidden_finished": 0, "machines": [], "terminals": [
            {"address": "t1", "labels": {}, "cwd": "/a", "state": "live", "created_ms": 1},
            {"address": "t2", "labels": {}, "cwd": "/b", "state": "finished", "created_ms": 2},
            {"address": "rohan/t7", "labels": {}, "cwd": "/c", "state": "live", "created_ms": 3}
        ]}})
    });
    let mut core = connect(&server.path, BUDGET).unwrap();

    let all = core.list(true).unwrap();

    let addrs: Vec<&str> = all.iter().map(|t| t.address.as_str()).collect();
    assert_eq!(addrs, vec!["t1", "t2"]);
    let req = &server.requests()[0];
    assert_eq!(req["method"], "terminal.list");
    assert_eq!(req["params"], json!({"all": true}));
}

#[test]
fn calls_reuse_one_connection() {
    let server = Server::start(|_, _| json!({"result": {"version": "0.1.20", "features": []}}));
    let mut core = connect(&server.path, BUDGET).unwrap();

    core.hello().unwrap();
    core.hello().unwrap();

    let ids: Vec<Value> = server.requests().iter().map(|r| r["id"].clone()).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert_eq!(*server.connections.lock().unwrap(), 1);
}

// socket_path

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn socket_path_prefers_the_terminal_socket() {
    let e = env(&[
        ("HC_TERMINAL_SOCKET", "/tmp/hw-1/c.sock"),
        ("HC_SOCKET", "/tmp/other.sock"),
        ("XDG_RUNTIME_DIR", "/run/user/501"),
    ]);
    assert_eq!(
        socket_path(&e, Path::new("/home/u")),
        PathBuf::from("/tmp/hw-1/c.sock")
    );
}

#[test]
fn socket_path_takes_hc_socket_next() {
    let e = env(&[("HC_SOCKET", "/tmp/other.sock"), ("XDG_RUNTIME_DIR", "/r")]);
    assert_eq!(
        socket_path(&e, Path::new("/home/u")),
        PathBuf::from("/tmp/other.sock")
    );
}

#[test]
fn socket_path_reads_the_socket_key_from_core_config() {
    let home = tempfile::tempdir().unwrap();
    let cfg_dir = home.path().join(".config/herdr-core");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "# isolated core\nshell = [\"/bin/zsh\"]\nsocket = \"/private/tmp/hw-3/c.sock\"\n",
    )
    .unwrap();
    let e = env(&[("XDG_RUNTIME_DIR", "/r")]);

    assert_eq!(
        socket_path(&e, home.path()),
        PathBuf::from("/private/tmp/hw-3/c.sock")
    );
}

#[test]
fn socket_path_uses_the_runtime_dir_and_session() {
    let home = Path::new("/nonexistent-home");
    assert_eq!(
        socket_path(&env(&[("XDG_RUNTIME_DIR", "/run/user/501")]), home),
        PathBuf::from("/run/user/501/herdr-core/default.sock")
    );
    assert_eq!(
        socket_path(
            &env(&[("XDG_RUNTIME_DIR", "/r"), ("HC_SESSION", " work ")]),
            home
        ),
        PathBuf::from("/r/herdr-core/work.sock")
    );
}

#[test]
fn socket_path_falls_back_to_the_state_dir() {
    let uid = rustix::process::getuid().as_raw();
    if Path::new(&format!("/run/user/{uid}")).is_dir() {
        // Linux with a user runtime dir: core would use it; covered below.
        assert_eq!(
            socket_path(&env(&[]), Path::new("/h")),
            PathBuf::from(format!("/run/user/{uid}/herdr-core/default.sock"))
        );
        return;
    }
    assert_eq!(
        socket_path(&env(&[]), Path::new("/h")),
        PathBuf::from("/h/.local/state/herdr-core/default/default.sock")
    );
    assert_eq!(
        socket_path(
            &env(&[("XDG_STATE_HOME", "/s"), ("HC_SESSION", "x")]),
            Path::new("/h")
        ),
        PathBuf::from("/s/herdr-core/x/x.sock")
    );
}

#[test]
fn socket_path_ignores_empty_variables() {
    let e = env(&[
        ("HC_TERMINAL_SOCKET", ""),
        ("HC_SOCKET", ""),
        ("XDG_RUNTIME_DIR", "/r"),
        ("HC_SESSION", ""),
    ]);
    assert_eq!(
        socket_path(&e, Path::new("/nonexistent-home")),
        PathBuf::from("/r/herdr-core/default.sock")
    );
}
