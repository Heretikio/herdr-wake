//! Settings in `~/.config/herdr-wake/config.toml` (see `paths::config_file`).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// User settings. Every field is optional in the file; missing ones take the
/// defaults from `Config::default()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Master switch: false means every terminal wakes to a plain shell.
    pub enabled: bool,
    /// Restore permission-bypass flags on resume.
    pub restore_bypass: bool,
    /// Resume attempts per terminal per boot.
    pub attempts: u8,
    /// Gap between agent starts after the first of a kind, in ms.
    pub stagger_ms: u64,
    /// How long the first agent of each kind starts alone, in ms.
    pub first_alone_ms: u64,
    /// Folders (and everything under them) that never resume.
    pub exclude_dirs: Vec<PathBuf>,
    /// herdr-core's `shell` setting saved at install (None: core's default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub real_shell: Option<Vec<String>>,
}

/// True when `cwd` is one of `dirs` (the `exclude_dirs` setting) or inside
/// one. A leading `~` in an entry means `home`; whole path components are
/// compared, so `~/private` does not cover `~/private-notes`; relative
/// entries (and `~name`) never match. The one rule for the wrapper, the
/// hook and the boot summary.
pub fn is_excluded(cwd: &Path, dirs: &[PathBuf], home: &Path) -> bool {
    dirs.iter().any(|d| {
        let d = match d.strip_prefix("~") {
            Ok(rest) => home.join(rest),
            Err(_) => d.clone(),
        };
        d.is_absolute() && cwd.starts_with(&d)
    })
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            restore_bypass: true,
            attempts: 3,
            stagger_ms: 1500,
            first_alone_ms: 8000,
            exclude_dirs: Vec::new(),
            real_shell: None,
        }
    }
}

impl Config {
    /// Reads `path`. A missing file gives the defaults; an unreadable or
    /// malformed file is an error.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        toml_edit::de::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Writes `path` atomically as a private file (`private::write`: 0600
    /// in a 0700 folder, never through a symlink). Keys and comments
    /// already in the file are kept; only the values change. A `None`
    /// `real_shell` removes that key.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut doc = match std::fs::read_to_string(path) {
            Ok(t) => t
                .parse::<toml_edit::DocumentMut>()
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let fresh = toml_edit::ser::to_document(self).context("encoding config")?;
        if self.real_shell.is_none() {
            doc.remove("real_shell");
        }
        for (key, item) in fresh.iter() {
            match doc.get_mut(key) {
                Some(slot) => *slot = item.clone(),
                None => {
                    doc.insert(key, item.clone());
                }
            }
        }
        crate::private::write(path, doc.to_string().as_bytes())
            .with_context(|| format!("writing {}", path.display()))
    }
}
