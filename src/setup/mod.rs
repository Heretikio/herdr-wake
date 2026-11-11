//! Install and uninstall: agent integrations, core shell setting, boot service (Task 11).
//!
//! Install order: `boot.env`, the saved real shell, every agent's edits,
//! herdr-core's `shell`, the boot service. Core's `shell` changes last
//! because a `shell` naming a binary that cannot start leaves every terminal
//! dead. Uninstall runs the other way round and restores core's `shell`
//! first, reading it back before it removes anything. Every path comes from
//! `ctx.env` and `ctx.home`, never from the process environment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};

use crate::agents::{self, Agent, Ctx, EditKind};
use crate::config::Config;
use crate::paths;
use crate::platform::{Action, BootOpts};

pub mod edits;
pub mod remote;

use edits::EditEnv;

/// What `install` should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Start herdr-wake at boot before anyone logs in (LaunchDaemon, linger).
    pub before_login: bool,
    /// Report what would change and write nothing.
    pub dry_run: bool,
    /// Absolute path of the herdr-wake binary to point everything at.
    pub bin: PathBuf,
}

/// What `install` or `uninstall` did (or would do, in a dry run), in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub actions: Vec<Action>,
    pub warnings: Vec<String>,
}

/// Variables `boot.env` carries: core's terminals get none of the login
/// environment otherwise.
const BOOT_ENV_KEYS: &[&str] = &["PATH", "LANG", "SHELL", "USER", "LOGNAME", "TMPDIR"];

/// Prefix of an `Action::what` that says an agent was left alone because it
/// is not installed: a note, not a change.
pub const SKIPPED_PREFIX: &str = "skipped ";

/// The second element of herdr-core's `shell` when it runs herdr-wake.
const SHELL_SUBCOMMAND: &str = "shell";

/// Test-only switch: when this is `1` in the environment, `install` and
/// `uninstall` leave the boot service alone (no launchd or systemd call, no
/// plist or unit). The integration suite sets it so a throwaway home never
/// reaches the developer's real login session. Not for users.
pub const SKIP_BOOT_SERVICE_ENV: &str = "HERDR_WAKE_SKIP_BOOT_SERVICE";

fn skip_boot_service(ctx: &Ctx) -> bool {
    ctx.env.get(SKIP_BOOT_SERVICE_ENV).is_some_and(|v| v == "1")
}

/// True when `segment` is a version number as package managers name
/// folders: `0.1.0`, `v1.2`, `0.1.0_1`, `2.1.322`. The first two
/// dot-separated parts must be numbers.
fn is_version_segment(segment: &str) -> bool {
    let s = segment.strip_prefix('v').unwrap_or(segment);
    let mut parts = s.split('.');
    let number =
        |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    number(parts.next()) && number(parts.next())
}

/// True when `path` names one version of a program, so an upgrade removes
/// it: it runs through `/Cellar/` (Homebrew), `/versions/`, or a folder
/// (or file) named like a version number.
pub fn is_versioned_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.contains("/Cellar/")
        || text.contains("/versions/")
        || path
            .components()
            .any(|c| c.as_os_str().to_str().is_some_and(is_version_segment))
}

/// The path of this herdr-wake to record as herdr-core's shell, in the boot
/// job and in the agent hooks: one that survives an upgrade.
///
/// `exe` is the executable as the OS reports it (macOS keeps the symlink it
/// was run through, Linux resolves it), `canonical` the same with every
/// symlink resolved, `links` the `herdr-wake` entries found on `PATH` and in
/// the standard bin folders, each with what it resolves to.
///
/// - `canonical` is not versioned: `exe` as it is, symlinks unresolved (the
///   resolved path when `exe` has `.` or `..` in it).
/// - `exe` itself is not versioned (`/opt/homebrew/bin/herdr-wake` leading
///   into the Cellar): `exe`.
/// - Otherwise the first of `links` that resolves to `canonical` and is not
///   versioned itself.
/// - With none, `Err` explains: recording the versioned path would leave
///   every terminal without a shell after the next upgrade.
pub fn pick_stable_path(
    exe: &Path,
    canonical: &Path,
    links: &[(PathBuf, PathBuf)],
) -> Result<PathBuf, String> {
    use std::path::Component;
    let clean = exe.is_absolute()
        && exe
            .components()
            .all(|c| !matches!(c, Component::CurDir | Component::ParentDir));
    if !is_versioned_path(canonical) {
        return Ok(if clean { exe } else { canonical }.to_path_buf());
    }
    if clean && !is_versioned_path(exe) {
        return Ok(exe.to_path_buf());
    }
    links
        .iter()
        .find(|(link, target)| target == canonical && !is_versioned_path(link))
        .map(|(link, _)| link.clone())
        .ok_or_else(|| {
            format!(
                "this herdr-wake runs from {}, a versioned path that the next upgrade removes, \
                 and no symlink to it was found on PATH or in the standard bin folders. \
                 Recording it as herdr-core's shell would leave every terminal without a shell \
                 after an upgrade. Run install through a stable path (for Homebrew: \
                 `$(brew --prefix)/bin/herdr-wake install`), or make a symlink to it in \
                 ~/.local/bin and run that",
                canonical.display()
            )
        })
}

/// The warning every uninstall prints.
pub const RESTORE_DELAY_WARNING: &str =
    "herdr-core applies a restored shell setting at its next retry, which can take up to 60 s; \
     terminals that are being recovered right now may stay dead until then";

/// Installs herdr-wake for every agent adapter (`agents::all()`).
pub fn install(opts: &Options, ctx: &mut Ctx) -> anyhow::Result<Report> {
    install_with(opts, &agents::all(), ctx)
}

/// `install` with an explicit adapter list.
pub fn install_with(
    opts: &Options,
    agents: &[Box<dyn Agent>],
    ctx: &mut Ctx,
) -> anyhow::Result<Report> {
    let bin = &opts.bin;
    if !bin.is_absolute() {
        bail!(
            "the herdr-wake path must be absolute (herdr-core runs it without a PATH lookup): {}",
            bin.display()
        );
    }
    // Both paths are written into the boot job (a systemd unit, a plist),
    // herdr-core's config and shell hook commands: a newline or another
    // control character in one would add lines of its own there.
    for path in [bin.clone(), paths::boot_env_file(ctx.env, ctx.home)] {
        if path.to_string_lossy().chars().any(char::is_control) {
            bail!(
                "the path {:?} contains a control character; herdr-wake cannot be installed from or into such a folder",
                path.display().to_string()
            );
        }
    }
    let dry = opts.dry_run;
    let mut report = Report::default();
    if !bin.is_file() {
        let msg = format!(
            "{} does not exist; herdr-core would fail to start every terminal with it as the shell",
            bin.display()
        );
        if !dry {
            bail!(msg);
        }
        report.warnings.push(msg);
    }
    let env = edit_env(ctx);

    // herdr-wake's own folders are private (0700), also when an older build
    // or the user created them with a looser mode.
    if !dry {
        for dir in [
            paths::config_dir(ctx.env, ctx.home),
            paths::state_dir(ctx.env, ctx.home),
        ] {
            crate::private::dir(&dir).with_context(|| format!("preparing {}", dir.display()))?;
        }
    }

    // (a) boot.env
    let boot_env = paths::boot_env_file(ctx.env, ctx.home);
    let text = boot_env_text(&boot_env_vars(ctx.env));
    if std::fs::read_to_string(&boot_env).ok().as_deref() != Some(text.as_str()) {
        if !dry {
            crate::private::write(&boot_env, text.as_bytes())
                .with_context(|| format!("writing {}", boot_env.display()))?;
        }
        report.actions.push(Action::new(
            "write the login environment for boot",
            Some(boot_env),
        ));
    }
    if !ctx.env.contains_key("PATH") {
        report
            .warnings
            .push("PATH is not set; agents started at boot may not be found".into());
    }

    // (b) the real shell
    let core_path = core_config_path(ctx.env, ctx.home);
    let current = read_core_shell(&core_path)?;
    let cfg_path = paths::config_file(ctx.env, ctx.home);
    let mut cfg = Config::load(&cfg_path)?;
    match &current {
        Some(shell) if names_herdr_wake(shell, bin) => {
            if cfg.real_shell.is_none() {
                report.warnings.push(format!(
                    "herdr-core's shell already runs herdr-wake and no earlier shell was saved; \
                     uninstall will remove the setting so core uses $SHELL ({})",
                    core_path.display()
                ));
            }
        }
        _ => {
            if cfg.real_shell != current {
                cfg.real_shell = current.clone();
                if !dry {
                    cfg.save(&cfg_path)?;
                }
                let what = match &current {
                    Some(s) => format!("save herdr-core's shell {s:?} as the real shell"),
                    None => "save that herdr-core uses its default shell".to_string(),
                };
                report.actions.push(Action::new(what, Some(cfg_path)));
            }
        }
    }

    // (c) agent integrations. JSON files that did not exist before are
    // remembered, so uninstall can remove them instead of leaving `{}`.
    let created_path = paths::created_files_file(ctx.env, ctx.home);
    let mut created = read_created(&created_path);
    let before = created.len();
    for agent in agents {
        // An agent that is not installed gets nothing: no hook file, no
        // plugin, no folder. A later `install` picks it up.
        if !agent.present(ctx) {
            report.actions.push(Action::new(
                format!("{SKIPPED_PREFIX}{}: not installed", agent.kind()),
                None,
            ));
            continue;
        }
        for edit in agent.integration(bin, ctx) {
            let is_new = !matches!(edit.kind, EditKind::WriteFile { .. }) && !edit.path.exists();
            report.actions.extend(
                edits::apply(&edit, &env, dry)
                    .with_context(|| format!("editing {}", edit.path.display()))?,
            );
            if is_new && !dry && !created.contains(&edit.path) {
                created.push(edit.path.clone());
            }
        }
    }
    if created.len() != before {
        let body =
            serde_json::to_vec_pretty(&created).context("encoding the created-files list")?;
        crate::private::write(&created_path, &body)
            .with_context(|| format!("writing {}", created_path.display()))?;
    }

    // (d) core's shell
    let ours = vec![bin.display().to_string(), SHELL_SUBCOMMAND.to_string()];
    if current.as_ref() != Some(&ours) {
        report
            .actions
            .extend(set_core_shell(&core_path, Some(&ours), &env, dry)?);
    }

    // (e) boot service, after boot.env so both hold the same PATH
    if skip_boot_service(ctx) {
        report.warnings.push(format!(
            "{SKIP_BOOT_SERVICE_ENV}=1: the boot service was not installed (test-only switch)"
        ));
        return Ok(report);
    }
    let boot = BootOpts {
        before_login: opts.before_login,
        bin: bin.clone(),
    };
    report
        .actions
        .extend(ctx.platform.boot_service().install(&boot, dry)?);
    Ok(report)
}

/// Uninstalls for every agent adapter. The binary is the one herdr-core's
/// `shell` names, else this process's own executable.
pub fn uninstall(dry_run: bool, ctx: &mut Ctx) -> anyhow::Result<Report> {
    let core_path = core_config_path(ctx.env, ctx.home);
    let named = read_core_shell(&core_path)?
        .and_then(|s| s.first().cloned())
        .map(PathBuf::from)
        .filter(|p| p.file_name().is_some_and(|n| n == "herdr-wake"));
    let bin = match named {
        Some(p) => p,
        None => std::env::current_exe().context("finding the herdr-wake binary")?,
    };
    uninstall_with(dry_run, &bin, &agents::all(), ctx)
}

/// `uninstall` with an explicit binary path and adapter list. Never deletes
/// `bin`; the last action names it for the user to delete.
pub fn uninstall_with(
    dry_run: bool,
    bin: &Path,
    agents: &[Box<dyn Agent>],
    ctx: &mut Ctx,
) -> anyhow::Result<Report> {
    let dry = dry_run;
    let env = edit_env(ctx);
    let mut report = Report::default();

    // (a) core's shell first, verified by reading it back.
    let core_path = core_config_path(ctx.env, ctx.home);
    let current = read_core_shell(&core_path)?;
    match &current {
        Some(shell) if names_herdr_wake(shell, bin) => {
            let cfg = Config::load(&paths::config_file(ctx.env, ctx.home))?;
            report.actions.extend(set_core_shell(
                &core_path,
                cfg.real_shell.as_deref(),
                &env,
                dry,
            )?);
            if dry {
                if cfg
                    .real_shell
                    .as_deref()
                    .is_some_and(|s| names_herdr_wake(s, bin))
                {
                    report.warnings.push(format!(
                        "the saved real shell names herdr-wake; uninstall would stop here. \
                         Set `shell` in {} by hand first",
                        core_path.display()
                    ));
                }
            } else {
                let back = read_core_shell(&core_path)?;
                if let Some(s) = back.as_deref().filter(|s| names_herdr_wake(s, bin)) {
                    bail!(
                        "herdr-core's shell in {} still names herdr-wake ({s:?}) after restoring it; \
                         nothing else was removed. Set `shell` there to your shell (or delete the \
                         line), then run uninstall again",
                        core_path.display()
                    );
                }
            }
        }
        Some(shell) => report.warnings.push(format!(
            "herdr-core's shell in {} is {shell:?}, not herdr-wake; left unchanged",
            core_path.display()
        )),
        None => {}
    }

    // (b) boot service
    if skip_boot_service(ctx) {
        report.warnings.push(format!(
            "{SKIP_BOOT_SERVICE_ENV}=1: the boot service was left alone (test-only switch)"
        ));
    } else {
        report
            .actions
            .extend(ctx.platform.boot_service().remove(dry)?);
    }

    // (c) agent integrations, newest first. Every agent is reverted,
    // installed or not, and in every layout an earlier install could have
    // used: an older herdr-wake set up agents that were never there.
    let mut touched: Vec<PathBuf> = Vec::new();
    for agent in agents.iter().rev() {
        let mut all = agent.integration(bin, ctx);
        all.extend(agent.former_integration(bin, ctx));
        for edit in all.iter().rev() {
            touched.push(edit.path.clone());
            match edits::revert(edit, &env, dry) {
                Ok(a) => report.actions.extend(a),
                Err(e) => report.warnings.push(format!(
                    "could not undo the edit to {}: {e:#}",
                    edit.path.display()
                )),
            }
        }
    }

    // (d) files install created from nothing, when the revert emptied them.
    let created_path = paths::created_files_file(ctx.env, ctx.home);
    for path in read_created(&created_path) {
        if is_empty_settings(&path) {
            match edits::remove_file(&path, &env, dry) {
                Ok(a) => report.actions.extend(a),
                Err(e) => report
                    .warnings
                    .push(format!("could not remove {}: {e:#}", path.display())),
            }
        }
        touched.push(path);
    }
    if !dry {
        let _ = std::fs::remove_file(&created_path);
    }

    // (d2) folders that held nothing but herdr-wake's files are empty now:
    // install created them (for an agent that was never installed, or as a
    // plugin folder), so they go too. A folder with anything else in it
    // stays, as do the home and the shared config folder.
    if !dry {
        let keep = [
            ctx.home.to_path_buf(),
            core_config_path(ctx.env, ctx.home)
                .parent()
                .and_then(Path::parent)
                .map_or_else(|| ctx.home.join(".config"), Path::to_path_buf),
            ctx.home.join(".config"),
        ];
        for file in &touched {
            report.actions.extend(remove_empty_parents(file, &keep));
        }
    }

    // (e) herdr-wake's own settings. Logs and backups stay in the state
    // folder.
    match remove_config_dir(&paths::config_dir(ctx.env, ctx.home), &env, dry) {
        Ok(a) => report.actions.extend(a),
        Err(e) => report
            .warnings
            .push(format!("could not remove herdr-wake's settings: {e:#}")),
    }

    // (f) the binary is the user's to delete.
    report.actions.push(Action::new(
        format!(
            "delete {} when nothing else uses it (uninstall leaves it in place)",
            bin.display()
        ),
        Some(bin.to_path_buf()),
    ));
    report.warnings.push(RESTORE_DELAY_WARNING.to_string());
    Ok(report)
}

/// How many folders above a removed file are looked at.
const EMPTY_PARENT_LEVELS: usize = 3;

/// Removes the folder `file` was in when it is empty now, then that
/// folder's parent, up to `EMPTY_PARENT_LEVELS` levels. Stops at the first
/// folder that holds anything, is a symlink, or is one of `keep`.
fn remove_empty_parents(file: &Path, keep: &[PathBuf]) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut dir = file.parent();
    for _ in 0..EMPTY_PARENT_LEVELS {
        let Some(d) = dir else { break };
        if d.parent().is_none() || keep.iter().any(|k| k == d) {
            break;
        }
        let real_dir = std::fs::symlink_metadata(d).is_ok_and(|m| m.is_dir());
        let empty = std::fs::read_dir(d).is_ok_and(|mut entries| entries.next().is_none());
        if !real_dir || !empty || std::fs::remove_dir(d).is_err() {
            break;
        }
        actions.push(Action::new(
            format!("remove the empty folder {}", d.display()),
            Some(d.to_path_buf()),
        ));
        dir = d.parent();
    }
    actions
}

/// The paths in the created-files list; empty when it is missing or
/// unreadable.
fn read_created(path: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// True when `path` holds nothing but what a reverted edit leaves behind:
/// `{}` or `{"hooks":{}}`, comments and whitespace aside.
fn is_empty_settings(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    if text.contains("//") || text.contains("/*") {
        return false; // someone wrote a comment: the file is theirs now
    }
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(serde_json::Value::Object(map)) => map
            .iter()
            .all(|(k, v)| k == "hooks" && v.as_object().is_some_and(serde_json::Map::is_empty)),
        _ => false,
    }
}

/// Removes herdr-wake's settings folder (`config.toml`, `boot.env`): moved
/// to the Trash where there is one, else each file is copied to the backups
/// folder and then deleted. A folder holding anything else is left with
/// whatever is not herdr-wake's.
fn remove_config_dir(dir: &Path, env: &EditEnv, dry_run: bool) -> anyhow::Result<Vec<Action>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    if let Some(trash) = &env.trash {
        let mut dest = trash.join("herdr-wake-config");
        if dest.exists() {
            dest = trash.join(format!("herdr-wake-config.{}", env.now_ms));
        }
        if !dry_run {
            std::fs::create_dir_all(trash)
                .with_context(|| format!("creating {}", trash.display()))?;
            std::fs::rename(dir, &dest)
                .with_context(|| format!("moving {} to {}", dir.display(), dest.display()))?;
        }
        return Ok(vec![Action::new(
            format!("move {} to the Trash", dir.display()),
            Some(dest),
        )]);
    }
    let mut actions = Vec::new();
    for name in ["config.toml", "boot.env"] {
        actions.extend(edits::remove_file(&dir.join(name), env, dry_run)?);
    }
    if !dry_run {
        // Only succeeds when nothing else is in it.
        let _ = std::fs::remove_dir(dir);
    }
    actions.push(Action::new(
        format!("remove {} if it is empty", dir.display()),
        Some(dir.to_path_buf()),
    ));
    Ok(actions)
}

fn edit_env(ctx: &Ctx) -> EditEnv {
    EditEnv {
        backups: paths::backups_dir(ctx.env, ctx.home),
        trash: trash_dir(ctx.home),
        now_ms: ctx.platform.now_ms(),
        backed_up: Default::default(),
    }
}

#[cfg(target_os = "macos")]
fn trash_dir(home: &Path) -> Option<PathBuf> {
    Some(home.join(".Trash"))
}

#[cfg(not(target_os = "macos"))]
fn trash_dir(_home: &Path) -> Option<PathBuf> {
    None
}

/// True when `shell` runs herdr-wake: its program is `bin` or a file named
/// `herdr-wake`.
fn names_herdr_wake(shell: &[String], bin: &Path) -> bool {
    shell.first().is_some_and(|p| {
        let p = Path::new(p);
        p == bin || p.file_name().is_some_and(|n| n == "herdr-wake")
    })
}

// --- herdr-core's config ---------------------------------------------------

/// herdr-core's config file: `$XDG_CONFIG_HOME/herdr-core/config.toml`
/// (any non-empty value, as core reads it), else
/// `<home>/.config/herdr-core/config.toml`.
pub fn core_config_path(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    let base = match env.get("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(".config"),
    };
    base.join("herdr-core").join("config.toml")
}

fn read_core_doc(path: &Path) -> anyhow::Result<Option<toml_edit::DocumentMut>> {
    match std::fs::read_to_string(path) {
        Ok(t) => t
            .parse::<toml_edit::DocumentMut>()
            .map(Some)
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// herdr-core's `shell` setting, `None` when the file or key is missing.
/// A `shell` that is not an array of strings is an error.
pub fn read_core_shell(path: &Path) -> anyhow::Result<Option<Vec<String>>> {
    let Some(doc) = read_core_doc(path)? else {
        return Ok(None);
    };
    let Some(item) = doc.get("shell") else {
        return Ok(None);
    };
    let bad = || anyhow!("{}: `shell` must be an array of strings", path.display());
    let arr = item.as_array().ok_or_else(bad)?;
    arr.iter()
        .map(|v| v.as_str().map(str::to_string).ok_or_else(bad))
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Some)
}

/// Sets (or with `None` removes) `shell`, keeping every other key, comment
/// and the comment on the `shell` line. Backs the file up first.
fn set_core_shell(
    path: &Path,
    shell: Option<&[String]>,
    env: &EditEnv,
    dry_run: bool,
) -> anyhow::Result<Vec<Action>> {
    let existing = read_core_doc(path)?;
    let original = existing.as_ref().map(ToString::to_string);
    let mut doc = existing.unwrap_or_default();
    match shell {
        Some(argv) => {
            let mut arr: toml_edit::Array = argv.iter().map(String::as_str).collect();
            arr.fmt();
            let mut value = toml_edit::Value::Array(arr);
            if let Some(old) = doc.get("shell").and_then(|i| i.as_value()) {
                *value.decor_mut() = old.decor().clone();
            }
            match doc.get_mut("shell") {
                Some(item) => *item = toml_edit::Item::Value(value),
                // A root key prints before every [table], so it never lands
                // inside one.
                None => {
                    doc.insert("shell", toml_edit::Item::Value(value));
                }
            }
        }
        None => {
            doc.remove("shell");
        }
    }
    let updated = doc.to_string();
    if original.as_deref() == Some(updated.as_str()) || (original.is_none() && shell.is_none()) {
        return Ok(Vec::new());
    }
    let mut actions = Vec::new();
    if original.is_some() {
        if let Some(copy) = edits::backup(path, env, dry_run)? {
            actions.push(Action::new(
                format!("back up {}", path.display()),
                Some(copy),
            ));
        }
    }
    if !dry_run {
        edits::write_atomic(path, updated.as_bytes(), 0o644, false)?;
    }
    let what = match shell {
        Some(argv) => format!("set herdr-core's shell to {argv:?}"),
        None => "remove herdr-core's shell setting (core uses $SHELL)".to_string(),
    };
    actions.push(Action::new(what, Some(path.to_path_buf())));
    Ok(actions)
}

// --- boot.env ----------------------------------------------------------------

/// The variables from `env` that `boot.env` carries: `PATH`, `LANG`, `LC_*`,
/// `SHELL`, `USER`, `LOGNAME`, `TMPDIR`. Values with a newline are skipped.
pub fn boot_env_vars(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(k, v)| {
            (BOOT_ENV_KEYS.contains(&k.as_str()) || k.starts_with("LC_")) && !v.contains('\n')
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// `KEY=value` lines, readable by systemd's `EnvironmentFile=` and by
/// `parse_boot_env`. Values with anything beyond `[A-Za-z0-9_./:@%+,=-]` are
/// double-quoted with `\`, `"`, `$` and backtick escaped.
pub fn boot_env_text(vars: &BTreeMap<String, String>) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_./:@%+,=-".contains(c);
    let mut out = String::new();
    for (k, v) in vars {
        if v.chars().all(plain) {
            out.push_str(&format!("{k}={v}\n"));
        } else {
            let mut q = String::with_capacity(v.len() + 2);
            for c in v.chars() {
                if matches!(c, '\\' | '"' | '$' | '`') {
                    q.push('\\');
                }
                q.push(c);
            }
            out.push_str(&format!("{k}=\"{q}\"\n"));
        }
    }
    out
}

/// Parses `boot.env` (and reads `boot_env_text` output back): one
/// `KEY=VALUE` per line, blank lines and `#` comment
/// lines skipped, an optional `export ` prefix dropped. A value wrapped in
/// single quotes is taken literally; in double quotes, `\"`, `\\`, `\$` and
/// `` \` `` are unescaped. Nothing is expanded: `$HOME` stays `$HOME`.
/// Lines without `=` or with an empty or invalid key are skipped.
pub fn parse_boot_env(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let valid = !key.is_empty()
            && !key.starts_with(|c: char| c.is_ascii_digit())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            continue;
        }
        out.insert(key.to_string(), unquote(value.trim()));
    }
    out
}

fn unquote(v: &str) -> String {
    let wrapped = |q: char| v.len() >= 2 && v.starts_with(q) && v.ends_with(q);
    if wrapped('\'') {
        return v[1..v.len() - 1].to_string();
    }
    if wrapped('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some(n @ ('"' | '\\' | '$' | '`')) => out.push(n),
                    Some(n) => {
                        out.push('\\');
                        out.push(n);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    v.to_string()
}
