//! Applies and reverts one `Edit` to another tool's file.
//!
//! JSON edits go through jsonc-parser's CST, so comments, trailing commas,
//! key order and the user's own entries survive byte for byte; only
//! herdr-wake's entries are added or removed. Every change backs up the file
//! as it was to `<backups>/<ms>-<flattened path>` first and lands by writing
//! a temporary file beside it and renaming it into place, keeping the mode.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use jsonc_parser::cst::{CstArray, CstInputValue, CstNode, CstObject, CstRootNode};
use jsonc_parser::ParseOptions;
use serde_json::Value;

use crate::agents::{Edit, EditKind};
use crate::platform::Action;

/// Where edits keep what they replace or remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditEnv {
    /// Backup folder (`paths::backups_dir`).
    pub backups: PathBuf,
    /// The Trash folder (macOS `~/.Trash`). `None`: removed files are
    /// deleted after a copy is placed in `backups`.
    pub trash: Option<PathBuf>,
    /// Timestamp for backup names, in ms since the epoch.
    pub now_ms: u64,
    /// The files already backed up with this `EditEnv` (one install or
    /// uninstall run): each file is backed up once, as it was before the
    /// run's first change to it, however many edits it gets.
    pub backed_up: std::cell::RefCell<std::collections::BTreeSet<PathBuf>>,
}

/// Mode for JSON files that do not exist yet.
const NEW_FILE_MODE: u32 = 0o600;

/// Makes `edit` true. Idempotent: returns no actions when the file already
/// holds exactly what the edit asks for. With `dry_run`, writes nothing and
/// returns the actions it would take.
pub fn apply(edit: &Edit, env: &EditEnv, dry_run: bool) -> anyhow::Result<Vec<Action>> {
    let path = &edit.path;
    match &edit.kind {
        EditKind::WriteFile { body, mode } => apply_write_file(path, body, *mode, env, dry_run),
        EditKind::JsonHook {
            event,
            matcher,
            command,
            timeout,
        } => {
            let hook = Hook {
                event,
                matcher: matcher.as_deref(),
                command,
                timeout: *timeout,
            };
            edit_json(path, env, dry_run, &format!("add {event} hook"), |root| {
                hook.add(root, path)
            })
        }
        EditKind::JsoncListEntry { key, value } => edit_json(
            path,
            env,
            dry_run,
            &format!("add {value:?} to {key}"),
            |root| list_add(root, path, key, value),
        ),
    }
}

/// Undoes `edit`: removes only herdr-wake's entries (and containers that
/// removal left empty), or for `WriteFile` moves the file to the Trash
/// (deletes it after a backup copy when there is no Trash). Returns no
/// actions when there is nothing to undo.
pub fn revert(edit: &Edit, env: &EditEnv, dry_run: bool) -> anyhow::Result<Vec<Action>> {
    let path = &edit.path;
    match &edit.kind {
        EditKind::WriteFile { .. } => remove_file(path, env, dry_run),
        EditKind::JsonHook {
            event,
            matcher,
            command,
            timeout,
        } => {
            if !path.exists() {
                return Ok(Vec::new());
            }
            let hook = Hook {
                event,
                matcher: matcher.as_deref(),
                command,
                timeout: *timeout,
            };
            edit_json(
                path,
                env,
                dry_run,
                &format!("remove {event} hook"),
                |root| hook.remove(root, path),
            )
        }
        EditKind::JsoncListEntry { key, value } => {
            if !path.exists() {
                return Ok(Vec::new());
            }
            edit_json(
                path,
                env,
                dry_run,
                &format!("remove {value:?} from {key}"),
                |root| list_remove(root, path, key, value),
            )
        }
    }
}

// --- Files -----------------------------------------------------------------

/// Copies `path` to `<backups>/<ms>-<flattened path>` (a numbered suffix
/// when that name is taken) and returns the copy's path. With `dry_run`,
/// returns the name without copying. `None` when this run already backed
/// the file up: the first copy holds it as it was before the run.
pub(crate) fn backup(path: &Path, env: &EditEnv, dry_run: bool) -> anyhow::Result<Option<PathBuf>> {
    if !env.backed_up.borrow_mut().insert(path.to_path_buf()) {
        return Ok(None);
    }
    let flat = path
        .to_string_lossy()
        .trim_start_matches('/')
        .replace('/', "_");
    let base = format!("{}-{flat}", env.now_ms);
    let mut dest = env.backups.join(&base);
    let mut n = 1;
    while dest.exists() {
        dest = env.backups.join(format!("{base}.{n}"));
        n += 1;
    }
    if dry_run {
        return Ok(Some(dest));
    }
    // A backup can hold whatever the other tool's file held (API keys in
    // an agent's settings), so it is private whatever the source's mode.
    fs::read(path)
        .and_then(|bytes| crate::private::write(&dest, &bytes))
        .with_context(|| format!("backing up {} to {}", path.display(), dest.display()))?;
    Ok(Some(dest))
}

/// Writes `bytes` to `path` through a temporary file in the same folder and
/// a rename, creating the folder. The file keeps its current mode; a new
/// file gets `new_mode`. `force_mode` sets the mode in either case. A
/// symlink is followed, so the link stays and its target changes.
pub(crate) fn write_atomic(
    path: &Path,
    bytes: &[u8],
    new_mode: u32,
    force_mode: bool,
) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let target = match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            fs::canonicalize(path).with_context(|| format!("resolving {}", path.display()))?
        }
        _ => path.to_path_buf(),
    };
    let mode = match fs::metadata(&target) {
        Ok(m) if !force_mode => m.permissions().mode() & 0o7777,
        _ => new_mode,
    };
    let dir = target
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent folder", target.display()))?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = target
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", target.display()))?
        .to_string_lossy();
    let tmp = dir.join(format!(".{name}.herdr-wake-{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let result = (|| -> anyhow::Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.set_permissions(fs::Permissions::from_mode(mode))
            .with_context(|| format!("setting mode on {}", tmp.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("syncing {}", tmp.display()))?;
        fs::rename(&tmp, &target).with_context(|| format!("replacing {}", target.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn read_optional(path: &Path) -> anyhow::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn apply_write_file(
    path: &Path,
    body: &str,
    mode: u32,
    env: &EditEnv,
    dry_run: bool,
) -> anyhow::Result<Vec<Action>> {
    use std::os::unix::fs::PermissionsExt;
    let current = read_optional(path)?;
    let current_mode = fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o7777);
    if current.as_deref() == Some(body) && current_mode == Some(mode) {
        return Ok(Vec::new());
    }
    let mut actions = Vec::new();
    if current.as_deref().is_some_and(|c| c != body) {
        if let Some(copy) = backup(path, env, dry_run)? {
            actions.push(Action::new(
                format!("back up {}", path.display()),
                Some(copy),
            ));
        }
    }
    if !dry_run {
        write_atomic(path, body.as_bytes(), mode, true)?;
    }
    actions.push(Action::new(
        format!("write {}", path.display()),
        Some(path.to_path_buf()),
    ));
    Ok(actions)
}

pub(crate) fn remove_file(
    path: &Path,
    env: &EditEnv,
    dry_run: bool,
) -> anyhow::Result<Vec<Action>> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(Vec::new());
    }
    match &env.trash {
        Some(trash) => {
            let name = path
                .file_name()
                .ok_or_else(|| anyhow!("{} has no file name", path.display()))?
                .to_string_lossy()
                .into_owned();
            let mut dest = trash.join(&name);
            if dest.exists() {
                dest = trash.join(format!("{name}.{}", env.now_ms));
            }
            if !dry_run {
                fs::create_dir_all(trash)
                    .with_context(|| format!("creating {}", trash.display()))?;
                if fs::rename(path, &dest).is_err() {
                    fs::copy(path, &dest).with_context(|| {
                        format!("copying {} to {}", path.display(), dest.display())
                    })?;
                    fs::remove_file(path)
                        .with_context(|| format!("removing {}", path.display()))?;
                }
            }
            Ok(vec![Action::new(
                format!("move {} to the Trash", path.display()),
                Some(dest),
            )])
        }
        None => {
            let mut actions = Vec::new();
            if let Some(copy) = backup(path, env, dry_run)? {
                actions.push(Action::new(
                    format!("back up {}", path.display()),
                    Some(copy),
                ));
            }
            if !dry_run {
                fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
            }
            actions.push(Action::new(
                format!("remove {}", path.display()),
                Some(path.to_path_buf()),
            ));
            Ok(actions)
        }
    }
}

// --- JSON with comments ----------------------------------------------------

/// JSON plus comments and trailing commas, nothing looser.
fn jsonc_options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}

/// Reads `path` (a missing or blank file counts as `{}`), runs `change` on
/// its root object, and writes the result if the text changed.
fn edit_json(
    path: &Path,
    env: &EditEnv,
    dry_run: bool,
    what: &str,
    change: impl FnOnce(&CstObject) -> anyhow::Result<()>,
) -> anyhow::Result<Vec<Action>> {
    let current = read_optional(path)?;
    let text = match current.as_deref() {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => "{}\n".to_string(),
    };
    let root = CstRootNode::parse(&text, &jsonc_options())
        .map_err(|e| anyhow!("parsing {}: {e}", path.display()))?;
    let obj = root
        .value()
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow!("{} must hold a JSON object", path.display()))?;
    reject_duplicate_keys(&CstNode::from(obj.clone()), path)?;
    change(&obj)?;
    let updated = root.to_string();
    if current.is_some() && updated == text {
        return Ok(Vec::new());
    }
    // The result must still parse; never write a file the tool cannot read.
    CstRootNode::parse(&updated, &jsonc_options()).map_err(|e| {
        anyhow!(
            "internal error: edit of {} would not parse: {e}",
            path.display()
        )
    })?;
    let mut actions = Vec::new();
    if current.is_some() {
        if let Some(copy) = backup(path, env, dry_run)? {
            actions.push(Action::new(
                format!("back up {}", path.display()),
                Some(copy),
            ));
        }
    }
    if !dry_run {
        write_atomic(path, updated.as_bytes(), NEW_FILE_MODE, false)?;
    }
    actions.push(Action::new(
        format!("{what} in {}", path.display()),
        Some(path.to_path_buf()),
    ));
    Ok(actions)
}

fn reject_duplicate_keys(node: &CstNode, path: &Path) -> anyhow::Result<()> {
    if let Some(obj) = node.as_object() {
        let mut names = HashSet::new();
        for prop in obj.properties() {
            let name = prop
                .decoded_name()
                .ok_or_else(|| anyhow!("{}: a property has no readable name", path.display()))?;
            if !names.insert(name.clone()) {
                bail!("{} contains duplicate key {name:?}", path.display());
            }
            if let Some(v) = prop.value() {
                reject_duplicate_keys(&v, path)?;
            }
        }
    } else if let Some(arr) = node.as_array() {
        for el in arr.elements() {
            reject_duplicate_keys(&el, path)?;
        }
    }
    Ok(())
}

/// The plain JSON value of a CST node (comments dropped).
fn to_json(node: &CstNode) -> Option<Value> {
    if let Some(obj) = node.as_object() {
        let mut map = serde_json::Map::new();
        for prop in obj.properties() {
            map.insert(prop.decoded_name()?, to_json(&prop.value()?)?);
        }
        return Some(Value::Object(map));
    }
    if let Some(arr) = node.as_array() {
        return arr
            .elements()
            .iter()
            .map(to_json)
            .collect::<Option<Vec<_>>>()
            .map(Value::Array);
    }
    if let Some(s) = node.as_string_lit() {
        return s.decoded_value().ok().map(Value::String);
    }
    if let Some(b) = node.as_boolean_lit() {
        return Some(Value::Bool(b.value()));
    }
    if node.as_null_keyword().is_some() {
        return Some(Value::Null);
    }
    if let Some(n) = node.as_number_lit() {
        return serde_json::from_str(&n.to_string()).ok();
    }
    None
}

/// The object under `key`, created empty when missing.
fn object_prop(obj: &CstObject, key: &str, path: &Path) -> anyhow::Result<CstObject> {
    match obj.get(key) {
        Some(p) => p
            .object_value()
            .ok_or_else(|| anyhow!("{}: {key:?} must be a JSON object", path.display())),
        None => obj
            .append(key, CstInputValue::Object(Vec::new()))
            .object_value()
            .ok_or_else(|| anyhow!("internal error: creating {key:?}")),
    }
}

/// The array under `key`, created empty when missing.
fn array_prop(obj: &CstObject, key: &str, path: &Path) -> anyhow::Result<CstArray> {
    match obj.get(key) {
        Some(p) => p
            .array_value()
            .ok_or_else(|| anyhow!("{}: {key:?} must be a JSON array", path.display())),
        None => obj
            .append(key, CstInputValue::Array(Vec::new()))
            .array_value()
            .ok_or_else(|| anyhow!("internal error: creating {key:?}")),
    }
}

struct Hook<'a> {
    event: &'a str,
    matcher: Option<&'a str>,
    command: &'a str,
    timeout: u32,
}

impl Hook<'_> {
    fn canonical_json(&self) -> Value {
        let mut group = serde_json::Map::new();
        if let Some(m) = self.matcher {
            group.insert("matcher".into(), Value::String(m.into()));
        }
        group.insert(
            "hooks".into(),
            serde_json::json!([{"type": "command", "command": self.command, "timeout": self.timeout}]),
        );
        Value::Object(group)
    }

    fn canonical_input(&self) -> CstInputValue {
        let mut group = Vec::new();
        if let Some(m) = self.matcher {
            group.push(("matcher".to_string(), CstInputValue::String(m.into())));
        }
        group.push((
            "hooks".to_string(),
            CstInputValue::Array(vec![CstInputValue::Object(vec![
                ("type".into(), CstInputValue::String("command".into())),
                ("command".into(), CstInputValue::String(self.command.into())),
                ("timeout".into(), CstInputValue::from(self.timeout)),
            ])]),
        ));
        CstInputValue::Object(group)
    }

    fn is_ours(&self, entry: &CstNode) -> bool {
        to_json(entry)
            .as_ref()
            .and_then(|v| v.get("command"))
            .and_then(Value::as_str)
            == Some(self.command)
    }

    /// Removes our command hooks from every group in `entries` except
    /// `keep` (by index), and groups that become empty. True if anything
    /// was removed.
    fn strip(&self, entries: &CstArray, keep: Option<usize>, path: &Path) -> anyhow::Result<bool> {
        let mut removed = false;
        for (i, group) in entries.elements().into_iter().enumerate() {
            if Some(i) == keep {
                continue;
            }
            let Some(group_obj) = group.as_object() else {
                continue;
            };
            let Some(prop) = group_obj.get("hooks") else {
                continue;
            };
            let Some(cmds) = prop.array_value() else {
                bail!(
                    "{}: hooks.{}[{i}].hooks must be a JSON array",
                    path.display(),
                    self.event
                );
            };
            let mut here = false;
            for cmd in cmds.elements() {
                if self.is_ours(&cmd) {
                    cmd.remove();
                    here = true;
                }
            }
            if here && cmds.elements().is_empty() {
                group.remove();
            }
            removed |= here;
        }
        Ok(removed)
    }

    fn add(&self, root: &CstObject, path: &Path) -> anyhow::Result<()> {
        let hooks = object_prop(root, "hooks", path)?;
        let entries = array_prop(&hooks, self.event, path)?;
        let canonical = self.canonical_json();
        let keep = entries
            .elements()
            .iter()
            .position(|g| to_json(g).as_ref() == Some(&canonical));
        self.strip(&entries, keep, path)?;
        if keep.is_none() {
            entries.append(self.canonical_input());
        }
        Ok(())
    }

    fn remove(&self, root: &CstObject, path: &Path) -> anyhow::Result<()> {
        let Some(hooks_prop) = root.get("hooks") else {
            return Ok(());
        };
        let hooks = hooks_prop
            .object_value()
            .ok_or_else(|| anyhow!("{}: \"hooks\" must be a JSON object", path.display()))?;
        let Some(event_prop) = hooks.get(self.event) else {
            return Ok(());
        };
        let entries = event_prop.array_value().ok_or_else(|| {
            anyhow!(
                "{}: hooks.{} must be a JSON array",
                path.display(),
                self.event
            )
        })?;
        if !self.strip(&entries, None, path)? {
            return Ok(());
        }
        if entries.elements().is_empty() {
            event_prop.remove();
            if hooks.properties().is_empty() {
                hooks_prop.remove();
            }
        }
        Ok(())
    }
}

fn list_add(root: &CstObject, path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    let list = array_prop(root, key, path)?;
    let present = list
        .elements()
        .iter()
        .any(|el| to_json(el).as_ref().and_then(Value::as_str) == Some(value));
    if !present {
        list.append(CstInputValue::String(value.into()));
    }
    Ok(())
}

fn list_remove(root: &CstObject, path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    let Some(prop) = root.get(key) else {
        return Ok(());
    };
    let list = prop
        .array_value()
        .ok_or_else(|| anyhow!("{}: {key:?} must be a JSON array", path.display()))?;
    let mut removed = false;
    for el in list.elements() {
        if to_json(&el).as_ref().and_then(Value::as_str) == Some(value) {
            el.remove();
            removed = true;
        }
    }
    if removed && list.elements().is_empty() {
        prop.remove();
    }
    Ok(())
}
