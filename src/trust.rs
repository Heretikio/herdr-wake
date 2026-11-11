//! What herdr-wake checks before it acts on a record: the folder a session
//! resumes in (decision 15), the shape of a session id, the program to run,
//! and the text it prints. A record is data from a terminal label, which
//! anything that can reach herdr-core's socket can write, so nothing in it
//! is taken on trust.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::record::AgentKind;

/// The real user id of this process.
pub fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

const STICKY: u32 = 0o1000;

/// Why a folder above the session folder makes it unsafe, from its mode and
/// owner, or `None` when it is fine. Refused: world-writable without the
/// sticky bit (anyone can swap what is under it), and any folder owned by
/// someone who is neither `uid` nor root (its owner can write to it, or make
/// it writable, whatever its mode says now).
fn parent_problem(mode: u32, owner: u32, uid: u32) -> Option<&'static str> {
    if mode & 0o002 != 0 && mode & STICKY == 0 {
        return Some("is world-writable without the sticky bit");
    }
    if owner != uid && owner != 0 {
        return Some("is writable by others: it is owned by neither you nor root");
    }
    None
}

/// Whether an agent may be resumed (or recorded) unattended in `path`, for
/// the user `uid`. `Err` says why not, in words for a person.
///
/// The folder must be a real folder (not a symlink), owned by `uid`, and not
/// writable by others, nor by a group unless it is the user's own private
/// group (see `group_is_private`). No folder above it, up to `/`, may be
/// world-writable without the sticky bit, or owned by anyone but `uid` and
/// root. A symlink above the folder is judged by the folders it leads
/// through. So a user-owned 0700 folder in sticky `/tmp` passes and a folder
/// in someone else's tree does not. Access control lists are not read.
pub fn folder_is_safe(path: &Path, uid: u32) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("its path is not absolute".into());
    }
    let md = std::fs::symlink_metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "it does not exist".to_string(),
        _ => format!("it cannot be read ({e})"),
    })?;
    if md.file_type().is_symlink() {
        return Err("it is a symlink".into());
    }
    if !md.is_dir() {
        return Err("it is not a folder".into());
    }
    if md.uid() != uid {
        return Err("it is not owned by you".into());
    }
    if md.mode() & 0o002 != 0 {
        return Err("it is writable by others".into());
    }
    if md.mode() & 0o020 != 0 && !group_is_private(md.gid(), uid) {
        return Err("it is writable by a group other people may belong to".into());
    }
    let check = |dir: &Path| -> Result<(), String> {
        let md = match std::fs::symlink_metadata(dir) {
            Ok(md) => md,
            Err(e) => return Err(format!("{} cannot be read ({e})", dir.display())),
        };
        if md.file_type().is_symlink() {
            // Judged through the resolved path below.
            return Ok(());
        }
        match parent_problem(md.mode(), md.uid(), uid) {
            Some(why) => Err(format!("{} {why}", dir.display())),
            None => Ok(()),
        }
    };
    for dir in path.ancestors().skip(1) {
        check(dir)?;
    }
    let real = std::fs::canonicalize(path).map_err(|e| format!("it cannot be resolved ({e})"))?;
    if real != path {
        for dir in real.ancestors().skip(1) {
            check(dir)?;
        }
    }
    Ok(())
}

/// Whether `gid` is the private group of the user `uid`: the user's primary
/// group, named like the user, with no other member listed. Distributions
/// that give each user such a group (Fedora, Debian, Ubuntu) pair it with
/// umask 002, so every folder a person creates there is group-writable while
/// nobody else is in the group. A shared group such as macOS `staff` fails
/// this test. Any lookup failure counts as not private.
pub fn group_is_private(gid: u32, uid: u32) -> bool {
    let Some((user, primary)) = user_name_and_gid(uid) else {
        return false;
    };
    if primary != gid {
        return false;
    }
    let Some((group, members)) = group_name_and_members(gid) else {
        return false;
    };
    private_group_rule(&user, &group, &members)
}

/// The pure part of [`group_is_private`]: same name, and nobody else listed.
pub fn private_group_rule(user: &str, group: &str, members: &[String]) -> bool {
    group == user && members.iter().all(|m| m == user)
}

fn user_name_and_gid(uid: u32) -> Option<(String, u32)> {
    let mut buf = vec![0u8; 16 * 1024];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: the buffers are valid for the given lengths and outlive the call.
    let rc =
        unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr().cast(), buf.len(), &mut out) };
    if rc != 0 || out.is_null() || pwd.pw_name.is_null() {
        return None;
    }
    // SAFETY: pw_name points into `buf` and is NUL-terminated on success.
    let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }
        .to_str()
        .ok()?
        .to_string();
    Some((name, pwd.pw_gid))
}

fn group_name_and_members(gid: u32) -> Option<(String, Vec<String>)> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::group = std::ptr::null_mut();
    // SAFETY: the buffers are valid for the given lengths and outlive the call.
    let rc =
        unsafe { libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr().cast(), buf.len(), &mut out) };
    if rc != 0 || out.is_null() || grp.gr_name.is_null() {
        return None;
    }
    // SAFETY: gr_name and the gr_mem array point into `buf` on success.
    let name = unsafe { std::ffi::CStr::from_ptr(grp.gr_name) }
        .to_str()
        .ok()?
        .to_string();
    let mut members = Vec::new();
    let mut p = grp.gr_mem;
    while !p.is_null() {
        // SAFETY: gr_mem is a NULL-terminated array of NUL-terminated strings.
        let m = unsafe { *p };
        if m.is_null() {
            break;
        }
        members.push(
            unsafe { std::ffi::CStr::from_ptr(m) }
                .to_string_lossy()
                .into_owned(),
        );
        p = unsafe { p.add(1) };
    }
    Some((name, members))
}

/// Whether `id` may be used as a session id: 1 to 128 characters from
/// `[A-Za-z0-9._-]`, the first a letter or digit. So it can never be read as
/// an option, never climbs out of a folder when joined to a path, and holds
/// no control or shell characters.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `text` without control characters: everything below 0x20 except tab,
/// 0x7f, and the C1 range 0x80 to 0x9f. Applied to whatever comes from a
/// record or a hook payload before it is printed to a terminal or written
/// to the log, so a label cannot send escape sequences to the terminal or
/// start a forged line in the log.
pub fn printable(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\t' || !(c.is_control() || ('\u{80}'..='\u{9f}').contains(&c)))
        .collect()
}

/// True when a flag value is inline JSON (its first character that is not a
/// space is `{` or `[`) instead of a file path. Such values routinely hold
/// tokens (`--mcp-config`, `--settings`), so they are never stored.
pub fn is_inline_json(value: &str) -> bool {
    value.trim_start().starts_with(['{', '['])
}

/// The byte range of the userinfo (`user:pass@`, `token@`) in the first
/// URL inside `value`, the `@` included.
fn url_userinfo(value: &str) -> Option<std::ops::Range<usize>> {
    let start = value.find("://")? + 3;
    let rest = &value[start..];
    let end = rest
        .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace() || c == '"')
        .unwrap_or(rest.len());
    let at = rest[..end].rfind('@')?;
    Some(start..start + at + 1)
}

/// True when `value` holds a URL with a user name or password in it
/// (`scheme://user:pass@host`).
pub fn has_url_userinfo(value: &str) -> bool {
    url_userinfo(value).is_some()
}

/// `value` with the user name and password taken out of its URL.
pub fn strip_url_userinfo(value: &str) -> String {
    match url_userinfo(value) {
        Some(range) => format!("{}{}", &value[..range.start], &value[range.end..]),
        None => value.to_string(),
    }
}

/// True when `text` (a config key or value) looks like it names or holds a
/// credential: it contains `key`, `token`, `secret`, `password`, `auth` or
/// `bearer`, in any letter case.
pub fn looks_like_credential(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["key", "token", "secret", "password", "auth", "bearer"]
        .iter()
        .any(|w| lower.contains(w))
}

/// The file names an agent's program goes by.
fn program_names(kind: AgentKind) -> &'static [&'static str] {
    match kind {
        AgentKind::Claude => &["claude", "claude.exe"],
        AgentKind::Codex => &["codex", "codex.js"],
        AgentKind::Pi => &["pi", "pi.js"],
        AgentKind::OpenCode => &["opencode", "opencode.js"],
    }
}

/// Whether `bin` may be run as the program of agent `kind` for the user
/// `uid`. `Err` says why not.
///
/// The path must be absolute and lead (through any symlinks) to a regular,
/// executable file that group and others cannot write and that `uid` or root
/// owns. Its name must be the agent's: the file name as recorded or as
/// resolved (`claude`, `codex`, `pi`, `opencode`, with `.js` for script
/// launchers), or for Claude a file under `claude/versions/` (the native
/// build) or `claude-code/cli.js` (the npm build).
pub fn bin_is_safe(kind: AgentKind, bin: &Path, uid: u32) -> Result<(), String> {
    if !bin.is_absolute() {
        return Err("its path is not absolute".into());
    }
    let real = std::fs::canonicalize(bin).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "it does not exist".to_string(),
        _ => format!("it cannot be resolved ({e})"),
    })?;
    let md = std::fs::metadata(&real).map_err(|e| format!("it cannot be read ({e})"))?;
    if !md.is_file() {
        return Err("it is not a regular file".into());
    }
    if md.mode() & 0o111 == 0 {
        return Err("it is not executable".into());
    }
    if md.mode() & 0o022 != 0 {
        return Err("it is writable by group or others".into());
    }
    if md.uid() != uid && md.uid() != 0 {
        return Err("it is owned by neither you nor root".into());
    }
    let names = program_names(kind);
    let named = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| names.contains(&n))
    };
    let shape = kind == AgentKind::Claude && {
        let s = real.to_string_lossy();
        s.contains("/claude/versions/") || s.ends_with("/claude-code/cli.js")
    };
    if named(bin) || named(&real) || shape {
        Ok(())
    } else {
        Err(format!("it is not {kind}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parents_like_the_system_defaults_pass() {
        // /, /home, /Users (root, 0755), /tmp (root, sticky), a 0750 home.
        assert_eq!(parent_problem(0o40755, 0, 501), None);
        assert_eq!(parent_problem(0o41777, 0, 501), None);
        assert_eq!(parent_problem(0o40750, 501, 501), None);
        // /Users/Shared and /opt/homebrew style: root or own, group-writable.
        assert_eq!(parent_problem(0o40775, 0, 501), None);
        assert_eq!(parent_problem(0o40775, 501, 501), None);
    }

    #[test]
    fn parents_others_control_are_refused() {
        assert!(parent_problem(0o40777, 0, 501).is_some());
        assert!(parent_problem(0o40777, 501, 501).is_some());
        // A colleague's tree, whatever its mode: its owner can write there.
        assert!(parent_problem(0o40775, 502, 501).is_some());
        assert!(parent_problem(0o41777, 502, 501).is_some());
        assert!(parent_problem(0o40755, 502, 501).is_some());
    }
}
