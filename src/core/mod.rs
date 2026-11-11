//! The herdr-core seam: terminals, labels and version info over core's
//! JSON-RPC socket. Adapters: `socket::SocketCore` (Task 2) and
//! `fake::FakeCore` for tests. Nothing here ever starts a core.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod fake;
pub mod socket;

/// A terminal address as core reports it (for example `t3`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalId(pub String);

impl TerminalId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for TerminalId {
    fn from(s: &str) -> Self {
        TerminalId(s.to_string())
    }
}

impl From<String> for TerminalId {
    fn from(s: String) -> Self {
        TerminalId(s)
    }
}

/// The parts of core's terminal record herdr-wake uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terminal {
    pub address: TerminalId,
    pub labels: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub keep: bool,
    /// Core's lifecycle state, for example `live` or `finished`.
    pub state: String,
    pub pid: Option<u32>,
    pub created_ms: u64,
    /// The terminal's foreground process (core's `foreground` field).
    pub foreground_pid: Option<u32>,
}

impl Terminal {
    /// The state core gives a terminal whose program is running or kept.
    pub const LIVE: &'static str = "live";

    /// A live, kept terminal with no labels, cwd, pids or creation time.
    pub fn new(address: impl Into<TerminalId>) -> Terminal {
        Terminal {
            address: address.into(),
            labels: BTreeMap::new(),
            cwd: None,
            keep: true,
            state: Terminal::LIVE.to_string(),
            pid: None,
            created_ms: 0,
            foreground_pid: None,
        }
    }
}

/// Reply to core's `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: String,
    pub features: Vec<String>,
}

/// Why a core call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    /// No socket, or nothing listening on it.
    #[error("herdr-core is not running")]
    NotRunning,
    /// The call did not finish within its time budget.
    #[error("herdr-core did not answer in time")]
    Timeout,
    /// A malformed reply or an error other than not-found.
    #[error("herdr-core protocol error: {0}")]
    Protocol(String),
    /// The terminal does not exist.
    #[error("terminal not found")]
    NotFound,
}

/// Calls on a running herdr-core.
pub trait Core {
    fn hello(&mut self) -> Result<Hello, CoreError>;
    fn get(&mut self, t: &TerminalId) -> Result<Terminal, CoreError>;
    /// Applies label changes: `Some(v)` sets the key, `None` removes it.
    /// Returns the terminal as it stands afterwards.
    fn label(
        &mut self,
        t: &TerminalId,
        changes: &BTreeMap<String, Option<String>>,
    ) -> Result<Terminal, CoreError>;
    /// Live terminals, or every terminal including finished ones when `all`.
    fn list(&mut self, all: bool) -> Result<Vec<Terminal>, CoreError>;
}
