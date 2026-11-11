//! In-memory `Core` for tests in this crate and in `tests/`.

use std::collections::BTreeMap;

use super::{Core, CoreError, Hello, Terminal, TerminalId};

/// Terminals in a map. `fail_with` makes every call return that error, to
/// stand in for a core that is down or slow.
#[derive(Debug, Clone)]
pub struct FakeCore {
    pub terminals: BTreeMap<TerminalId, Terminal>,
    pub hello: Hello,
    pub fail: Option<CoreError>,
    /// Number of calls made, of any kind (including failed ones).
    pub calls: usize,
}

impl Default for FakeCore {
    fn default() -> Self {
        FakeCore {
            terminals: BTreeMap::new(),
            hello: Hello {
                version: "0.1.20".to_string(),
                features: Vec::new(),
            },
            fail: None,
            calls: 0,
        }
    }
}

impl FakeCore {
    /// No terminals, version 0.1.20, no failure.
    pub fn new() -> FakeCore {
        FakeCore::default()
    }

    /// Adds or replaces a terminal, keyed by its address.
    pub fn insert(&mut self, t: Terminal) {
        self.terminals.insert(t.address.clone(), t);
    }

    /// Makes every later call fail with `err` (`None` clears it).
    pub fn fail_with(&mut self, err: Option<CoreError>) {
        self.fail = err;
    }

    /// The stored terminal, without counting a call.
    pub fn terminal(&self, t: &TerminalId) -> Option<&Terminal> {
        self.terminals.get(t)
    }

    fn enter(&mut self) -> Result<(), CoreError> {
        self.calls += 1;
        match &self.fail {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

impl Core for FakeCore {
    fn hello(&mut self) -> Result<Hello, CoreError> {
        self.enter()?;
        Ok(self.hello.clone())
    }

    fn get(&mut self, t: &TerminalId) -> Result<Terminal, CoreError> {
        self.enter()?;
        self.terminals.get(t).cloned().ok_or(CoreError::NotFound)
    }

    fn label(
        &mut self,
        t: &TerminalId,
        changes: &BTreeMap<String, Option<String>>,
    ) -> Result<Terminal, CoreError> {
        self.enter()?;
        let term = self.terminals.get_mut(t).ok_or(CoreError::NotFound)?;
        for (k, v) in changes {
            match v {
                Some(v) => {
                    term.labels.insert(k.clone(), v.clone());
                }
                None => {
                    term.labels.remove(k);
                }
            }
        }
        Ok(term.clone())
    }

    fn list(&mut self, all: bool) -> Result<Vec<Terminal>, CoreError> {
        self.enter()?;
        Ok(self
            .terminals
            .values()
            .filter(|t| all || t.state == Terminal::LIVE)
            .cloned()
            .collect())
    }
}
