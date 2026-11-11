//! `install --host <name>`: copy the binary over SSH and install it there.
//!
//! The binary lands in the remote `~/.local/bin` under a temporary name and
//! is renamed into place, so a running copy is replaced atomically; then the
//! remote `herdr-wake install` runs with its own absolute path. The local
//! binary must be built for the remote OS and architecture: `ssh <host>
//! uname -sm` is checked first, and a mismatch stops before anything is
//! copied.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};

/// The remote folder for the binary, relative to the remote home.
const REMOTE_DIR: &str = ".local/bin";

/// The commands that install `local_bin` on `host`, in order: make the
/// folder, copy the binary to a temporary name, then move it into place and
/// run `herdr-wake install` (with `--before-login` when asked). Each command
/// passes `--` before the host so a host name cannot be read as an option.
pub fn remote_install_commands(
    host: &str,
    local_bin: &Path,
    before_login: bool,
) -> Vec<Vec<String>> {
    let dir = format!("\"$HOME/{REMOTE_DIR}\"");
    let tmp = format!("\"$HOME/{REMOTE_DIR}/herdr-wake.new\"");
    let dest = format!("\"$HOME/{REMOTE_DIR}/herdr-wake\"");
    let mut install = format!("chmod 755 {tmp} && mv -f {tmp} {dest} && {dest} install");
    if before_login {
        install.push_str(" --before-login");
    }
    let s = |v: &str| v.to_string();
    vec![
        vec![s("ssh"), s("--"), s(host), format!("mkdir -p {dir}")],
        vec![
            s("scp"),
            s("--"),
            local_bin.display().to_string(),
            format!("{host}:{REMOTE_DIR}/herdr-wake.new"),
        ],
        vec![s("ssh"), s("--"), s(host), install],
    ]
}

/// `(os, cpu)` in one spelling for both sides: `darwin` or `linux`, and
/// `aarch64` (also written `arm64`) or `x86_64` (also `amd64`). Anything
/// else is passed through in lowercase.
fn normalize(os: &str, cpu: &str) -> (String, String) {
    let os = match os.to_ascii_lowercase().as_str() {
        "macos" | "darwin" => "darwin".to_string(),
        other => other.to_string(),
    };
    let cpu = match cpu.to_ascii_lowercase().as_str() {
        "arm64" | "aarch64" => "aarch64".to_string(),
        "amd64" | "x86_64" => "x86_64".to_string(),
        other => other.to_string(),
    };
    (os, cpu)
}

/// Checks the remote's `uname -sm` output (for example `Linux x86_64`)
/// against the OS and CPU this binary was built for (`local_os` and
/// `local_cpu`, as `std::env::consts::OS` and `ARCH` spell them). The error
/// names both sides.
pub fn check_remote_platform(uname: &str, local_os: &str, local_cpu: &str) -> anyhow::Result<()> {
    let mut words = uname.split_whitespace();
    let (Some(os), Some(cpu)) = (words.next(), words.next()) else {
        bail!("could not read the remote system type from `uname -sm`: {uname:?}");
    };
    let remote = normalize(os, cpu);
    let local = normalize(local_os, local_cpu);
    if remote != local {
        bail!(
            "this herdr-wake binary is built for {} {}, but the remote host is {} {}; \
             copy a herdr-wake built for that system instead",
            local.0,
            local.1,
            remote.0,
            remote.1
        );
    }
    Ok(())
}

/// Runs `remote_install_commands` one by one, stopping at the first that
/// fails. Output goes to this process's terminal.
pub fn run_remote_install(host: &str, local_bin: &Path, before_login: bool) -> anyhow::Result<()> {
    if host.is_empty()
        || host.starts_with('-')
        || host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == ':' || c == '/')
    {
        bail!("not a usable ssh host name: {host:?}");
    }
    if !local_bin.is_absolute() || !local_bin.is_file() {
        bail!("{} is not a herdr-wake binary to copy", local_bin.display());
    }
    let uname = Command::new("ssh")
        .args(["--", host, "uname -sm"])
        .output()
        .context("running ssh")?;
    if !uname.status.success() {
        bail!(
            "ssh {host} uname -sm failed ({}): {}",
            uname.status,
            String::from_utf8_lossy(&uname.stderr).trim()
        );
    }
    check_remote_platform(
        &String::from_utf8_lossy(&uname.stdout),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )?;
    for argv in remote_install_commands(host, local_bin, before_login) {
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .status()
            .with_context(|| format!("running {}", argv[0]))?;
        if !status.success() {
            bail!("{} failed ({status}): {}", argv[0], argv.join(" "));
        }
    }
    Ok(())
}
