//! The integration harness: a real `hc` and a real herdr-core in a
//! throwaway home, driven through the built `herdr-wake` binary.
//!
//! Rules (see also docs/TESTING.md):
//!
//! - Every test gets its own `HOME` under a temp folder and its own core,
//!   whose socket lives in a short folder (`/private/tmp/hw-<pid>-<n>` on
//!   macOS, `/tmp/hw-<pid>-<n>` elsewhere): a Unix socket path is limited to
//!   about 103 bytes and a temp home is far deeper than that.
//! - Every `hc` call runs with a cleared environment: that `HOME`, a minimal
//!   `PATH` and `SSH_CONNECTION=test`. The last one forces core's direct
//!   start path, so it never touches launchd jobs shared with the
//!   developer's real core. No `HC_*` or `XDG_*` variable is ever set.
//! - A "reboot" is `hc server stop` followed by `hc server start`.
//! - Dropping a `Harness` stops its core, waits for every process of that
//!   home to go, and checks that the herdr processes that ran before the
//!   suite started are all still there (the real home is never touched).
//! - Without `hc`, `Harness::new` prints a notice and returns `None`: the
//!   test passes without running.
//!
//! Tests may run in parallel: nothing is shared between two harnesses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use herdr_wake::core::socket::{self, SocketCore};
use herdr_wake::core::{Core, CoreError, Terminal, TerminalId};
use herdr_wake::record::{self, Record};

/// The built binary under test.
pub const BIN: &str = env!("CARGO_BIN_EXE_herdr-wake");

/// Short test values for `stagger_ms` and `first_alone_ms`.
pub const STAGGER_MS: u64 = 300;
pub const FIRST_ALONE_MS: u64 = 3000;

/// Marks a temp home as one of this suite's, in its folder name.
const HOME_MARK: &str = "hw-it-";

const MIN_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

static NEXT: AtomicU32 = AtomicU32::new(0);

/// `hc`, from `HERDR_WAKE_TEST_HC`, `~/.local/bin/hc` or `PATH`.
fn find_hc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HERDR_WAKE_TEST_HC") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin/hc"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join("hc")));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// One herdr-related process: pid and command line.
type Proc = (u32, String);

/// Processes named `herdr`, `herdr-core` or `hc` (by the basename of their
/// first word): what `pgrep -fl herdr` shows, minus the noise that pattern
/// also matches (a build folder called herdr-wake, this test binary). Read
/// with `ps`, because `pgrep -l` prints different things on macOS and Linux.
fn herdr_procs() -> Vec<Proc> {
    let out = Command::new("ps")
        .args(["-axww", "-o", "pid=,command="])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, cmd) = line.trim().split_once(' ')?;
            let first = cmd.split_whitespace().next()?;
            let base = first.rsplit('/').next()?;
            matches!(base, "herdr" | "herdr-core" | "hc")
                .then(|| Some((pid.parse().ok()?, cmd.trim().to_string())))
                .flatten()
        })
        .collect()
}

/// The `HOME` in the environment of `pid`, when it can be read.
fn home_of(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
        bytes
            .split(|b| *b == 0)
            .filter_map(|kv| std::str::from_utf8(kv).ok())
            .find_map(|kv| kv.strip_prefix("HOME=").map(str::to_string))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = Command::new("ps")
            .args(["-Eww", "-o", "command=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .find_map(|w| w.strip_prefix("HOME=").map(str::to_string))
    }
}

/// Every process whose environment has `HOME=<home>`: the isolated core,
/// its holder, wrappers, shells, stub agents and watchers.
fn procs_of_home(home: &Path) -> Vec<Proc> {
    let want = format!("HOME={}", home.display());
    let me = std::process::id();
    #[cfg(target_os = "linux")]
    {
        let mut out = Vec::new();
        for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(environ) = std::fs::read(e.path().join("environ")) else {
                continue;
            };
            let ours = environ.split(|b| *b == 0).any(|kv| kv == want.as_bytes());
            if ours && pid != me {
                let cmd = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
                let cmd = String::from_utf8_lossy(&cmd).replace('\0', " ");
                out.push((pid, cmd.trim().to_string()));
            }
        }
        out
    }
    #[cfg(not(target_os = "linux"))]
    {
        // One `ps` for everything: -E appends each process's environment.
        let out = Command::new("ps")
            .args(["-axEww", "-o", "pid=,command="])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let (pid, cmd) = line.trim().split_once(' ')?;
                let pid: u32 = pid.parse().ok()?;
                (pid != me && cmd.split_whitespace().any(|w| w == want))
                    .then(|| (pid, cmd.chars().take(160).collect()))
            })
            .collect()
    }
}

/// The herdr processes that ran before the first isolated core started.
fn baseline() -> &'static Vec<Proc> {
    static BASELINE: OnceLock<Vec<Proc>> = OnceLock::new();
    BASELINE.get_or_init(herdr_procs)
}

/// Fails when the real home's herdr processes changed since the suite
/// started: one of them is gone, or a herdr process appeared that does not
/// belong to one of this suite's throwaway homes.
pub fn assert_real_home_unchanged() {
    let now = herdr_procs();
    for old in baseline() {
        assert!(
            now.contains(old),
            "a herdr process from before the suite is gone: {old:?}"
        );
    }
    for new in now.iter().filter(|p| !baseline().contains(p)) {
        // A process that exited meanwhile, or whose environment cannot be
        // read, is not evidence either way.
        if let Some(home) = home_of(new.0) {
            assert!(
                home.contains(HOME_MARK),
                "a herdr process outside the test homes appeared (HOME={home}): {new:?}"
            );
        }
    }
}

/// One line of the stub agents' marker file.
#[derive(Debug, Clone)]
pub struct Start {
    pub ms: u64,
    pub terminal: String,
    pub kind: String,
    pub pid: u32,
    pub resumed: bool,
    pub id: String,
    pub cwd: PathBuf,
    pub supervised: bool,
    pub config_dir: String,
    /// The stub's arguments, split on spaces.
    pub argv: Vec<String>,
}

pub struct Harness {
    pub home: PathBuf,
    /// A folder inside the home to run agents in.
    pub work: PathBuf,
    pub socket: PathBuf,
    hc: PathBuf,
    sock_dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Harness {
    /// A fresh home with the isolated core config, herdr-wake's test config
    /// and the stub agents on its `PATH`. `None` (after a printed notice)
    /// when `hc` is not installed. The core is not started yet.
    pub fn new() -> Option<Harness> {
        let Some(hc) = find_hc() else {
            eprintln!(
                "integration: hc is not installed; skipping (set HERDR_WAKE_TEST_HC to its path)"
            );
            return None;
        };
        baseline();
        let tmp = tempfile::Builder::new()
            .prefix(HOME_MARK)
            .tempdir()
            .expect("temp home");
        // The stubs report `pwd -P`, so work with the resolved path.
        let home = tmp.path().canonicalize().expect("canonical home");
        let base = if cfg!(target_os = "macos") {
            "/private/tmp"
        } else {
            "/tmp"
        };
        let sock_dir = PathBuf::from(format!(
            "{base}/hw-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&sock_dir)
                .expect("socket folder");
        }
        let socket = sock_dir.join("c.sock");
        assert!(
            socket.as_os_str().len() < 100,
            "socket path too long for a Unix socket: {}",
            socket.display()
        );
        let h = Harness {
            work: home.join("work"),
            home,
            socket,
            hc,
            sock_dir,
            _tmp: tmp,
        };
        for dir in [
            ".config/herdr-core",
            ".config/herdr-wake",
            "stub",
            "bin",
            "work",
        ] {
            std::fs::create_dir_all(h.home.join(dir)).unwrap();
        }
        h.write_core_config(&format!("shell = [\"{BIN}\", \"shell\"]\n"));
        h.write_config("");
        std::fs::write(h.home.join("stub/herdr-wake"), BIN).unwrap();
        let stub = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/stubs/agent.sh");
        for kind in ["claude", "codex", "pi", "opencode"] {
            // argv[0] is the agent's name, so the process looks like it.
            let launcher = format!(
                "#!/usr/bin/env bash\nSTUB_KIND={kind} exec -a {kind} bash '{}' \"$@\"\n",
                stub.display()
            );
            let path = h.home.join("bin").join(kind);
            std::fs::write(&path, launcher).unwrap();
            make_executable(&path);
        }
        Some(h)
    }

    /// Writes the isolated core's config: the short socket plus `extra`.
    pub fn write_core_config(&self, extra: &str) {
        std::fs::write(
            self.core_config(),
            format!("socket = \"{}\"\n{extra}", self.socket.display()),
        )
        .unwrap();
    }

    pub fn core_config(&self) -> PathBuf {
        self.home.join(".config/herdr-core/config.toml")
    }

    /// Writes herdr-wake's config: `/bin/sh` as the real shell, short
    /// stagger values, plus `extra` (which may override them).
    pub fn write_config(&self, extra: &str) {
        let mut text = String::new();
        if !extra.contains("real_shell") {
            text.push_str("real_shell = [\"/bin/sh\"]\n");
        }
        if !extra.contains("stagger_ms") {
            text.push_str(&format!("stagger_ms = {STAGGER_MS}\n"));
        }
        if !extra.contains("first_alone_ms") {
            text.push_str(&format!("first_alone_ms = {FIRST_ALONE_MS}\n"));
        }
        text.push_str(extra);
        std::fs::write(self.home.join(".config/herdr-wake/config.toml"), text).unwrap();
    }

    fn env(&self) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), self.home.display().to_string());
        env.insert(
            "PATH".to_string(),
            format!("{}:{MIN_PATH}", self.home.join("bin").display()),
        );
        env
    }

    /// Runs `hc` the only way the suite ever does (see the module docs).
    pub fn hc(&self, args: &[&str]) -> Output {
        Command::new(&self.hc)
            .args(args)
            .env_clear()
            .envs(self.env())
            .env("SSH_CONNECTION", "test")
            .current_dir(&self.work)
            .stdin(Stdio::null())
            .output()
            .expect("running hc")
    }

    /// `hc` that must succeed; returns its stdout.
    pub fn hc_ok(&self, args: &[&str]) -> String {
        let out = self.hc(args);
        assert!(
            out.status.success(),
            "hc {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Runs the binary under test outside any terminal, with the isolated
    /// home and `extra` variables.
    pub fn hw_env(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        Command::new(BIN)
            .args(args)
            .env_clear()
            .envs(self.env())
            .envs(extra.iter().copied())
            .current_dir(&self.home)
            .stdin(Stdio::null())
            .output()
            .expect("running herdr-wake")
    }

    pub fn hw(&self, args: &[&str]) -> Output {
        self.hw_env(args, &[])
    }

    /// `herdr-wake` that must exit 0; returns its stdout.
    pub fn hw_ok(&self, args: &[&str]) -> String {
        let out = self.hw(args);
        assert!(
            out.status.success(),
            "herdr-wake {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    pub fn start(&self) {
        self.hc_ok(&["server", "start"]);
    }

    pub fn stop(&self) {
        self.hc_ok(&["server", "stop"]);
    }

    /// What a reboot does to kept terminals: stop the core, start it again.
    pub fn restart(&self) {
        self.stop();
        self.start();
    }

    /// A new kept terminal running core's configured shell in `cwd`.
    /// Returns its address once the wrapper has handed over to the shell.
    pub fn terminal_in(&self, cwd: &Path) -> String {
        std::fs::create_dir_all(cwd).unwrap();
        let out = self.hc_ok(&[
            "run",
            "--bg",
            "--keep",
            "--json",
            "--cwd",
            &cwd.display().to_string(),
        ]);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("hc run json");
        let t = v["address"].as_str().expect("address").to_string();
        self.wait_for_shell(&t);
        t
    }

    pub fn terminal(&self) -> String {
        self.terminal_in(&self.work)
    }

    /// Types `text` and presses enter in terminal `t`.
    pub fn send(&self, t: &str, text: &str) {
        self.hc_ok(&["send", t, text]);
    }

    /// The terminal's screen and history.
    pub fn read(&self, t: &str) -> String {
        self.hc_ok(&["read", t])
    }

    /// The path of the `hc` the suite drives.
    pub fn hc_path(&self) -> &Path {
        &self.hc
    }

    pub fn core(&self) -> Result<SocketCore, CoreError> {
        socket::connect(&self.socket, Duration::from_secs(2))
    }

    /// Core's record of terminal `t`, straight from the socket.
    pub fn get(&self, t: &str) -> Terminal {
        self.core()
            .and_then(|mut c| c.get(&TerminalId::from(t)))
            .unwrap_or_else(|e| panic!("terminal.get {t}: {e}"))
    }

    /// The herdr-wake record on terminal `t`.
    pub fn record(&self, t: &str) -> Option<Record> {
        self.core()
            .and_then(|mut c| record::load(&mut c, &TerminalId::from(t)))
            .unwrap_or_else(|e| panic!("loading the record of {t}: {e}"))
    }

    /// The command line of the terminal's foreground process, as `ps` has
    /// it (empty when there is none).
    pub fn foreground(&self, t: &str) -> String {
        let Some(pid) = self.core().ok().and_then(|mut c| {
            c.get(&TerminalId::from(t))
                .ok()
                .and_then(|t| t.foreground_pid)
        }) else {
            return String::new();
        };
        let out = Command::new("ps")
            .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Waits until the terminal's foreground process is the plain shell.
    pub fn wait_for_shell(&self, t: &str) {
        self.wait(&format!("{t} to show a plain shell"), || {
            let fg = self.foreground(t);
            (fg == "/bin/sh" || fg.starts_with("/bin/sh ")).then_some(())
        });
    }

    /// Every line of the stubs' marker file so far.
    pub fn starts(&self) -> Vec<Start> {
        let text = std::fs::read_to_string(self.home.join("stub/starts.log")).unwrap_or_default();
        text.lines()
            .map(|line| {
                let f: BTreeMap<&str, &str> = line
                    .split('\t')
                    .filter_map(|kv| kv.split_once('='))
                    .collect();
                let get = |k: &str| f.get(k).copied().unwrap_or_default();
                Start {
                    ms: get("ms").parse().unwrap_or(0),
                    terminal: get("terminal").to_string(),
                    kind: get("kind").to_string(),
                    pid: get("pid").parse().unwrap_or(0),
                    resumed: get("resumed") == "1",
                    id: get("id").to_string(),
                    cwd: PathBuf::from(get("cwd")),
                    supervised: get("supervised") == "1",
                    config_dir: get("config_dir").to_string(),
                    argv: get("argv")
                        .split(' ')
                        .filter(|w| !w.is_empty())
                        .map(str::to_string)
                        .collect(),
                }
            })
            .collect()
    }

    /// The stub starts in terminal `t` that were resumes.
    pub fn resumes(&self, t: &str) -> Vec<Start> {
        self.starts()
            .into_iter()
            .filter(|s| s.resumed && s.terminal == t)
            .collect()
    }

    /// Starts a stub agent by typing `line` into the shell of `t`, and waits
    /// until it has started and its record is stored. Returns the record.
    pub fn launch(&self, t: &str, line: &str) -> Record {
        let before = self.starts().len();
        self.send(t, line);
        self.wait(&format!("the agent in {t} to start"), || {
            self.starts()
                .iter()
                .skip(before)
                .any(|s| s.terminal == t)
                .then_some(())
        });
        self.wait(&format!("a running record on {t}"), || {
            self.record(t)
                .filter(|r| r.state == record::State::Running && r.agent_proc.is_some())
        })
    }

    /// Makes the stub in `t` quit the way a person quits an agent.
    pub fn quit(&self, t: &str) {
        std::fs::write(self.home.join("stub").join(format!("quit-{t}")), "").unwrap();
    }

    /// Polls `f` every 50 ms until it returns `Some`, for at most 20 s.
    pub fn wait<T>(&self, what: &str, f: impl FnMut() -> Option<T>) -> T {
        self.wait_up_to(Duration::from_secs(20), what, f)
    }

    pub fn wait_up_to<T>(
        &self,
        limit: Duration,
        what: &str,
        mut f: impl FnMut() -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(v) = f() {
                return v;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {what}\n--- herdr-wake.log\n{}\n--- starts.log\n{}",
                    self.log(),
                    std::fs::read_to_string(self.home.join("stub/starts.log")).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// herdr-wake's own log in the isolated home.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.home.join(".local/state/herdr-wake/herdr-wake.log"))
            .unwrap_or_default()
    }

    /// The processes that belong to this home right now.
    pub fn processes(&self) -> Vec<Proc> {
        procs_of_home(&self.home)
    }

    /// Waits (up to 10 s) until no process of this home is left; returns
    /// whatever still is. Watchers take a few seconds to notice a stop.
    pub fn wait_until_no_processes(&self) -> Vec<Proc> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = self.processes();
            if left.is_empty() || Instant::now() >= deadline {
                return left;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Every file under the home with its bytes, for before/after checks.
    pub fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.insert(p.clone(), std::fs::read(&p).unwrap_or_default());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.home, &mut out);
        out
    }
}

pub fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Stop the core if the test left it running. The check is a plain
        // socket connection, which never starts a core.
        if self.core().is_ok() {
            let _ = self.hc(&["server", "stop"]);
        }
        let left = self.wait_until_no_processes();
        for (pid, _) in &left {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        let _ = std::fs::remove_dir_all(&self.sock_dir);
        if !std::thread::panicking() {
            assert!(
                left.is_empty(),
                "processes of the isolated home were still running after the core stopped: {left:#?}"
            );
            assert_real_home_unchanged();
        }
    }
}
