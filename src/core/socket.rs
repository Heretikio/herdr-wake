//! JSON-RPC client for the herdr-core Unix socket (Task 2).
//!
//! Plain mode: one JSON-RPC 2.0 request per line, one reply per line, one
//! request at a time per connection. A `SocketCore` gets one deadline when it
//! connects (`now + budget`); connect, every write and every read share it, so
//! a hung core (which still accepts connections) costs at most the budget in
//! total. A caller that needs more time later connects again.
//!
//! Nothing here starts a core or creates a file: a missing socket or a refused
//! connection is `CoreError::NotRunning`.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use super::{Core, CoreError, Hello, Terminal, TerminalId};

/// A connection to a running core. See the module docs for the deadline.
#[derive(Debug)]
pub struct SocketCore {
    path: PathBuf,
    stream: Option<UnixStream>,
    deadline: Instant,
    next_id: u64,
    buf: Vec<u8>,
}

/// Connects to the core listening on `socket`. `budget` is the total time
/// this connection may spend, across the connect and every later call.
pub fn connect(socket: &Path, budget: Duration) -> Result<SocketCore, CoreError> {
    let deadline = Instant::now() + budget;
    let stream = open(socket, deadline)?;
    Ok(SocketCore {
        path: socket.to_path_buf(),
        stream: Some(stream),
        deadline,
        next_id: 1,
        buf: Vec::new(),
    })
}

/// The socket of the core this process belongs to, by core's own rules:
/// `HC_TERMINAL_SOCKET` (set inside core's terminals), else `HC_SOCKET`, else
/// `socket` in core's `config.toml`, else
/// `$XDG_RUNTIME_DIR/herdr-core/<session>.sock`, else
/// `/run/user/<uid>/herdr-core/<session>.sock` when that folder exists, else
/// `<state home>/herdr-core/<session>/<session>.sock`. `<session>` is
/// `HC_SESSION` trimmed, default `default`. Empty variables count as unset.
/// Reads core's config file; creates nothing.
pub fn socket_path(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    let var = |k: &str| env.get(k).map(String::as_str).filter(|v| !v.is_empty());
    if let Some(s) = var("HC_TERMINAL_SOCKET").or_else(|| var("HC_SOCKET")) {
        return PathBuf::from(s);
    }
    let config_home = var("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
    if let Some(s) = config_socket(&config_home.join("herdr-core").join("config.toml")) {
        return PathBuf::from(s);
    }
    let session = var("HC_SESSION")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    let sock = format!("{session}.sock");
    if let Some(run) = var("XDG_RUNTIME_DIR") {
        return Path::new(run).join("herdr-core").join(sock);
    }
    let run = PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()));
    if run.is_dir() {
        return run.join("herdr-core").join(sock);
    }
    let state_home = var("XDG_STATE_HOME").map_or_else(|| home.join(".local/state"), PathBuf::from);
    state_home.join("herdr-core").join(session).join(sock)
}

/// The `socket` key of core's config, if the file parses and has one.
fn config_socket(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc = text.parse::<toml_edit::DocumentMut>().ok()?;
    let s = doc.get("socket")?.as_str()?;
    (!s.is_empty()).then(|| s.to_string())
}

/// Connects within the deadline. `UnixStream::connect` cannot time out and
/// blocks on Linux when the listen queue is full, so it runs on a helper
/// thread; on timeout that thread is left to finish on its own and its
/// stream is dropped.
fn open(path: &Path, deadline: Instant) -> Result<UnixStream, CoreError> {
    let remaining = remaining(deadline)?;
    let (tx, rx) = mpsc::channel();
    let target = path.to_path_buf();
    thread::Builder::new()
        .name("herdr-wake-connect".into())
        .spawn(move || {
            let _ = tx.send(UnixStream::connect(&target));
        })
        .map_err(|e| CoreError::Protocol(format!("cannot start connect thread: {e}")))?;
    match rx.recv_timeout(remaining) {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(e)) => Err(connect_error(path, &e)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(CoreError::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(CoreError::Protocol(
            "connect thread ended without a result".into(),
        )),
    }
}

fn connect_error(path: &Path, e: &io::Error) -> CoreError {
    let gone = matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) || e.raw_os_error() == Some(libc::ENOTDIR);
    if gone {
        CoreError::NotRunning
    } else {
        CoreError::Protocol(format!("connect {}: {e}", path.display()))
    }
}

/// Time left before the deadline, or `Timeout` when none is.
fn remaining(deadline: Instant) -> Result<Duration, CoreError> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(CoreError::Timeout)
    } else {
        Ok(left)
    }
}

fn io_error(e: io::Error) -> CoreError {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => CoreError::Timeout,
        io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::UnexpectedEof => CoreError::NotRunning,
        _ => CoreError::Protocol(format!("socket: {e}")),
    }
}

impl SocketCore {
    /// One call: the reply's `result`, or the mapped error. A transport
    /// failure (timeout, closed socket, unreadable reply) drops the
    /// connection so a stale reply can never answer a later request; the
    /// next call reconnects if the deadline allows.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, CoreError> {
        let reply = match self.exchange(method, params) {
            Ok(reply) => reply,
            Err(e) => {
                self.stream = None;
                self.buf.clear();
                return Err(e);
            }
        };
        match reply.get("error") {
            Some(err) if !err.is_null() => Err(rpc_error(err)),
            _ => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
        }
    }

    fn exchange(&mut self, method: &str, params: Value) -> Result<Value, CoreError> {
        if self.stream.is_none() {
            self.stream = Some(open(&self.path, self.deadline)?);
        }
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line = req.to_string().into_bytes();
        line.push(b'\n');
        let stream = self.stream.as_mut().expect("connected above");
        stream
            .set_write_timeout(Some(remaining(self.deadline)?))
            .map_err(io_error)?;
        stream.write_all(&line).map_err(io_error)?;
        loop {
            let line = self.read_line()?;
            let reply: Value = serde_json::from_slice(&line)
                .map_err(|e| CoreError::Protocol(format!("reply is not JSON: {e}")))?;
            match reply.get("id") {
                // A notification (no id): not our answer.
                None | Some(Value::Null) if reply.get("method").is_some() => continue,
                Some(v) if v.as_u64() == Some(id) => return Ok(reply),
                _ => {
                    // A null id is how JSON-RPC reports an unparseable request.
                    if let Some(err) = reply.get("error").filter(|e| !e.is_null()) {
                        return Err(CoreError::Protocol(describe(err)));
                    }
                    return Err(CoreError::Protocol(format!(
                        "reply for another request: {reply}"
                    )));
                }
            }
        }
    }

    /// Reads up to the next newline, within the deadline.
    fn read_line(&mut self) -> Result<Vec<u8>, CoreError> {
        let stream = self.stream.as_mut().expect("connected");
        loop {
            if let Some(at) = self.buf.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=at).collect();
                line.pop();
                return Ok(line);
            }
            stream
                .set_read_timeout(Some(remaining(self.deadline)?))
                .map_err(io_error)?;
            let mut chunk = [0u8; 8192];
            match stream.read(&mut chunk) {
                Ok(0) => return Err(CoreError::NotRunning),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(io_error(e)),
            }
        }
    }
}

/// A JSON-RPC error object: `data.kind == "not_found"` is `NotFound`,
/// anything else is `Protocol` with the kind, message and hint.
fn rpc_error(err: &Value) -> CoreError {
    if err.pointer("/data/kind").and_then(Value::as_str) == Some("not_found") {
        CoreError::NotFound
    } else {
        CoreError::Protocol(describe(err))
    }
}

fn describe(err: &Value) -> String {
    let kind = err
        .pointer("/data/kind")
        .and_then(Value::as_str)
        .unwrap_or("internal");
    let message = err.get("message").and_then(Value::as_str).unwrap_or("");
    let mut s = format!("{kind}: {message}");
    if let Some(hint) = err
        .pointer("/data/hint")
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
    {
        s.push_str(&format!(" ({hint})"));
    }
    s
}

/// Core's public terminal record, reduced to what herdr-wake reads. Unknown
/// fields are ignored; missing optional ones take their defaults.
#[derive(Deserialize)]
struct WireTerminal {
    address: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    keep: bool,
    #[serde(default)]
    state: String,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    created_ms: u64,
    #[serde(default)]
    foreground: Option<WireForeground>,
}

#[derive(Deserialize)]
struct WireForeground {
    #[serde(default)]
    pid: Option<i64>,
}

impl From<WireTerminal> for Terminal {
    fn from(w: WireTerminal) -> Terminal {
        Terminal {
            address: TerminalId(w.address),
            labels: w.labels,
            cwd: w.cwd.filter(|c| !c.is_empty()).map(PathBuf::from),
            keep: w.keep,
            state: w.state,
            pid: w.pid,
            created_ms: w.created_ms,
            foreground_pid: w
                .foreground
                .and_then(|f| f.pid)
                .and_then(|p| u32::try_from(p).ok())
                .filter(|p| *p > 0),
        }
    }
}

#[derive(Deserialize)]
struct WireHello {
    #[serde(default)]
    version: String,
    #[serde(default)]
    features: Vec<String>,
}

#[derive(Deserialize)]
struct WireList {
    #[serde(default)]
    terminals: Vec<WireTerminal>,
}

fn parse<T: serde::de::DeserializeOwned>(what: &str, v: Value) -> Result<T, CoreError> {
    serde_json::from_value(v).map_err(|e| CoreError::Protocol(format!("bad {what} reply: {e}")))
}

impl Core for SocketCore {
    fn hello(&mut self) -> Result<Hello, CoreError> {
        let h: WireHello = parse("hello", self.call("hello", json!({}))?)?;
        Ok(Hello {
            version: h.version,
            features: h.features,
        })
    }

    fn get(&mut self, t: &TerminalId) -> Result<Terminal, CoreError> {
        let v = self.call("terminal.get", json!({"terminal": t.as_str()}))?;
        Ok(parse::<WireTerminal>("terminal.get", v)?.into())
    }

    /// Core removes a label when its value is empty, so `None` is sent as
    /// `""` (and so is `Some("")`).
    fn label(
        &mut self,
        t: &TerminalId,
        changes: &BTreeMap<String, Option<String>>,
    ) -> Result<Terminal, CoreError> {
        let labels: serde_json::Map<String, Value> = changes
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone().unwrap_or_default())))
            .collect();
        let v = self.call(
            "terminal.label",
            json!({"terminal": t.as_str(), "labels": labels}),
        )?;
        Ok(parse::<WireTerminal>("terminal.label", v)?.into())
    }

    /// This machine's terminals only: core also lists connected machines'
    /// terminals as `machine/t7`, whose pids belong to another host.
    fn list(&mut self, all: bool) -> Result<Vec<Terminal>, CoreError> {
        let v = self.call("terminal.list", json!({"all": all}))?;
        let list: WireList = parse("terminal.list", v)?;
        Ok(list
            .terminals
            .into_iter()
            .filter(|t| !t.address.contains('/'))
            .map(Terminal::from)
            .collect())
    }
}
