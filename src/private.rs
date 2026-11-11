//! The one way herdr-wake writes under its own config and state folders.
//!
//! Folders are created 0700 and files 0600, whatever the umask. A looser
//! folder or file that already exists is tightened. Nothing is ever written
//! through a symlink: a link where a file should be is an error, not a
//! redirect. Whole files are replaced atomically (a temporary file created
//! with `O_EXCL` beside the target, then a rename); the log is appended to
//! and the gate file is updated in place under its lock, both through
//! handles opened here with `O_NOFOLLOW`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Mode of every folder herdr-wake owns.
pub const DIR_MODE: u32 = 0o700;
/// Mode of every file herdr-wake owns.
pub const FILE_MODE: u32 = 0o600;

fn refused(path: &Path, why: &str) -> io::Error {
    io::Error::other(format!("{} {why}", path.display()))
}

/// Makes `path` a private folder: created 0700 with any missing parents, or
/// tightened to 0700 when it exists with group or other bits. A symlink or
/// a file in its place is an error.
pub fn dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(md) if md.file_type().is_symlink() => {
            Err(refused(path, "is a symlink; refusing to write through it"))
        }
        Ok(md) if !md.is_dir() => Err(refused(path, "is not a folder")),
        Ok(md) => {
            if md.permissions().mode() & 0o077 != 0 {
                fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))?;
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(DIR_MODE)
                .create(path)?;
            // The builder's mode is cut by the umask, never widened; a race
            // with another creator could leave their mode, so set it.
            fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))
        }
        Err(e) => Err(e),
    }
}

fn parent_of(path: &Path) -> io::Result<&Path> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| refused(path, "has no parent folder"))
}

fn refuse_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(md) if md.file_type().is_symlink() => {
            Err(refused(path, "is a symlink; refusing to write through it"))
        }
        Ok(md) if md.is_dir() => Err(refused(path, "is a folder")),
        _ => Ok(()),
    }
}

/// Replaces `path` with `bytes` atomically, as a 0600 file in a 0700
/// folder. Refuses when `path` is a symlink.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let parent = parent_of(path)?;
    dir(parent)?;
    refuse_symlink(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| refused(path, "has no file name"))?
        .to_string_lossy();
    let tmp = parent.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&tmp)?;
        f.set_permissions(fs::Permissions::from_mode(FILE_MODE))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Opens `path` without following a symlink, creating it 0600 in a 0700
/// folder, and tightens a looser existing file.
fn open(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    dir(parent_of(path)?)?;
    let f = options
        .create(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => refused(path, "is a symlink; refusing to write through it"),
            _ => e,
        })?;
    let md = f.metadata()?;
    if !md.is_file() {
        return Err(refused(path, "is not a regular file"));
    }
    if md.permissions().mode() & 0o077 != 0 {
        f.set_permissions(fs::Permissions::from_mode(FILE_MODE))?;
    }
    Ok(f)
}

/// Appends `text` to `path` (the log). See `open`.
pub fn append(path: &Path, text: &str) -> io::Result<()> {
    open(path, OpenOptions::new().append(true))?.write_all(text.as_bytes())
}

/// Opens `path` for reading and writing in place (the gate file, which is
/// changed under a lock on the file itself). See `open`.
pub fn open_rw(path: &Path) -> io::Result<File> {
    open(path, OpenOptions::new().read(true).write(true))
}

/// Creates `path` with `bytes`, 0600 in a 0700 folder, only if nothing is
/// there yet (`O_EXCL`, so never through a symlink). For files that are
/// then hard-linked into place.
pub fn create_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    dir(parent_of(path)?)?;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)?;
    f.set_permissions(fs::Permissions::from_mode(FILE_MODE))?;
    f.write_all(bytes)
}
