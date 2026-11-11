//! Helpers shared by integration tests: a sample record and a `Ctx` over
//! the fakes.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use herdr_wake::record::{AgentKind, ProcRef, Record, State};

/// A running Claude session record with every field filled in.
pub fn sample_record() -> Record {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    Record {
        v: 1,
        agent: AgentKind::Claude,
        session_id: "0b5a2c1e-1111-4222-8333-444455556666".to_string(),
        session_file: Some(PathBuf::from(
            "/home/u/.claude/projects/-home-u-proj/0b5a.jsonl",
        )),
        cwd: PathBuf::from("/home/u/proj"),
        flags: vec!["--model".to_string(), "opus".to_string()],
        env,
        bin: PathBuf::from("/usr/local/bin/claude"),
        agent_proc: Some(ProcRef {
            pid: 4242,
            start: 1_700_000_000_123,
        }),
        shell_proc: Some(ProcRef {
            pid: 4200,
            start: 1_700_000_000_001,
        }),
        boot_id: "boot-1".to_string(),
        state: State::Running,
        attempts: 0,
        seq: 7,
    }
}

/// A real, executable file named after the agent, in a temporary folder
/// that lives as long as the test process: what a record's `bin` must be
/// for `trust::bin_is_safe` to pass.
pub fn agent_bin(kind: AgentKind) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::OnceLock;
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let d = tempfile::tempdir().unwrap().keep();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        for k in AgentKind::ALL {
            let p = d.join(k.as_str());
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        d
    });
    dir.join(kind.as_str())
}

/// Writes herdr-wake's local copy for every record stored in `core`, as
/// the hook does when it records a session, so the records are trusted.
pub fn trust_records(core: &FakeCore, env: &BTreeMap<String, String>, home: &std::path::Path) {
    let dir = herdr_wake::paths::records_dir(env, home);
    for (id, t) in &core.terminals {
        if let Some(rec) = herdr_wake::record::of_terminal(t) {
            herdr_wake::record::write_copy(&dir, id, t.created_ms, &rec).unwrap();
        }
    }
}

use herdr_wake::agents::Ctx;
use herdr_wake::config::Config;
use herdr_wake::core::fake::FakeCore;
use herdr_wake::platform::fake::FakePlatform;

/// Owns everything a `Ctx` borrows. Change the fields, then call `ctx()`.
pub struct Fixture {
    pub platform: FakePlatform,
    pub core: FakeCore,
    pub env: BTreeMap<String, String>,
    pub home: PathBuf,
    pub cfg: Config,
    /// Keeps a temporary home alive (see `Fixture::temp`).
    pub tmp: Option<tempfile::TempDir>,
}

impl Fixture {
    /// Fakes with no processes or terminals, `HOME=/home/u`, default config.
    pub fn new() -> Fixture {
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), "/home/u".to_string());
        Fixture {
            platform: FakePlatform::new(),
            core: FakeCore::new(),
            env,
            home: PathBuf::from("/home/u"),
            cfg: Config::default(),
            tmp: None,
        }
    }

    /// A fixture whose home is a real temporary folder that lives as long
    /// as the fixture, for code that writes herdr-wake's state files.
    pub fn temp() -> Fixture {
        let (mut f, dir) = Fixture::with_temp_home();
        f.tmp = Some(dir);
        f
    }

    /// Writes the local copy for every stored record (see `trust_records`).
    pub fn trust(&self) {
        trust_records(&self.core, &self.env, &self.home);
    }

    /// A fixture whose home is a real temporary folder (kept alive by the
    /// returned guard), for tests that touch files.
    pub fn with_temp_home() -> (Fixture, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut f = Fixture::new();
        f.home = dir.path().to_path_buf();
        f.env
            .insert("HOME".to_string(), dir.path().display().to_string());
        (f, dir)
    }

    pub fn ctx(&mut self) -> Ctx<'_> {
        Ctx {
            platform: &self.platform,
            core: &mut self.core,
            env: &self.env,
            home: &self.home,
            cfg: &self.cfg,
        }
    }
}
