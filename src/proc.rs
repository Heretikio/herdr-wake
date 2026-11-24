//! Running an outside program with a time limit: for the helpers that read
//! OpenCode's database (`sqlite3`) and write summaries (`claude`).
//!
//! The program is started with an argument list (never a shell), with the
//! environment it is given (not the caller's), its output bounded, and it is
//! killed when the time is up.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What a finished program printed.
#[derive(Debug)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Why a program gave no output.
#[derive(Debug, PartialEq, Eq)]
pub enum RunError {
    /// It could not be started.
    Spawn(String),
    /// It did not finish in time and was killed.
    Timeout,
}

/// How much of the program's standard output is read (the rest is dropped).
const MAX_STDOUT: u64 = 64 * 1024 * 1024;
/// How much of its standard error is read.
const MAX_STDERR: u64 = 1024 * 1024;

/// Runs `program` with `args` in `cwd` with exactly the environment `env`,
/// `stdin` written to it (then closed), for at most `timeout`.
pub fn run(
    program: &Path,
    args: &[String],
    env: &BTreeMap<String, String>,
    stdin: Option<Vec<u8>>,
    cwd: &Path,
    timeout: Duration,
) -> Result<Output, RunError> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RunError::Spawn(e.to_string()))?;
    let writer = stdin.and_then(|bytes| {
        let mut pipe = child.stdin.take()?;
        // A thread, so a program that answers before it has read everything
        // cannot leave both sides waiting.
        Some(std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        }))
    });
    let (Some(mut out), Some(mut err)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunError::Spawn("no output pipe".to_string()));
    };
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut out).take(MAX_STDOUT).read_to_end(&mut bytes);
        // Whatever is left is read and dropped so the program is not blocked.
        let _ = std::io::copy(&mut out, &mut std::io::sink());
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut err).take(MAX_STDERR).read_to_end(&mut bytes);
        let _ = std::io::copy(&mut err, &mut std::io::sink());
        bytes
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // After a kill the readers are left to finish by themselves: a child of
    // the program may still hold the pipes.
    let status = status.ok_or(RunError::Timeout)?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    Ok(Output {
        code: status.code(),
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}
