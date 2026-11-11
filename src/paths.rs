//! Where herdr-wake keeps its files. Every function takes the environment and
//! home folder as parameters; nothing here reads the process environment.
//!
//! Config: `$XDG_CONFIG_HOME/herdr-wake`, default `~/.config/herdr-wake`.
//! State: `$XDG_STATE_HOME/herdr-wake`, default `~/.local/state/herdr-wake`.
//! An XDG variable counts only when it is set to an absolute path, as the
//! XDG base directory spec requires. The same rules apply on macOS.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const APP: &str = "herdr-wake";

fn xdg(env: &BTreeMap<String, String>, var: &str, home: &Path, default: &str) -> PathBuf {
    match env.get(var) {
        Some(v) if Path::new(v).is_absolute() => PathBuf::from(v),
        _ => home.join(default),
    }
}

/// `~/.config/herdr-wake`, or `$XDG_CONFIG_HOME/herdr-wake`.
pub fn config_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    xdg(env, "XDG_CONFIG_HOME", home, ".config").join(APP)
}

/// `~/.local/state/herdr-wake`, or `$XDG_STATE_HOME/herdr-wake`.
pub fn state_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    xdg(env, "XDG_STATE_HOME", home, ".local/state").join(APP)
}

/// `<config dir>/config.toml`.
pub fn config_file(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    config_dir(env, home).join("config.toml")
}

/// `<config dir>/boot.env`: `PATH`, `LANG`, `LC_*` captured at install.
pub fn boot_env_file(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    config_dir(env, home).join("boot.env")
}

/// `<state dir>/backups`: copies of other tools' files before an edit.
pub fn backups_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    state_dir(env, home).join("backups")
}

/// `<state dir>/herdr-wake.log`.
pub fn log_file(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    state_dir(env, home).join("herdr-wake.log")
}

/// `<state dir>/gate`: the start gate's lock and timestamps.
pub fn gate_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    state_dir(env, home).join("gate")
}

/// `<state dir>/claims`: one lock file per claimed session id.
pub fn claims_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    state_dir(env, home).join("claims")
}

/// `<state dir>/records`: one private copy per terminal of the record
/// herdr-wake itself stored there (see `record::write_copy`).
pub fn records_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    state_dir(env, home).join("records")
}

/// `<state dir>/unconfirmed`: one marker per resume whose agent has not
/// reported itself, in a folder per boot.
pub fn unconfirmed_dir(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    state_dir(env, home).join("unconfirmed")
}

/// `<state dir>/installed-files.json`: files `install` created from nothing,
/// so `uninstall` may remove them when they end up empty.
pub fn created_files_file(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    state_dir(env, home).join("installed-files.json")
}

/// `<state dir>/last-boot.json`: counts from the last `herdr-wake boot`.
pub fn last_boot_file(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    state_dir(env, home).join("last-boot.json")
}
