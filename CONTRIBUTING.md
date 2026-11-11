# Contributing to herdr-wake

Thanks for helping. Bug reports with `herdr-wake doctor` output and the relevant lines of `~/.local/state/herdr-wake/herdr-wake.log` are the most useful thing you can send. Reboot-test results from your own machines are the second most useful: see [docs/TESTING.md](docs/TESTING.md#the-real-reboot-matrix).

## Build

You need a stable Rust toolchain (`rustup`).

```sh
cargo build --release
./target/release/herdr-wake --help
```

To try a local build for real, copy it somewhere stable first, then install from there:

```sh
cp target/release/herdr-wake ~/.local/bin/herdr-wake
~/.local/bin/herdr-wake install --dry-run   # see what it would change
~/.local/bin/herdr-wake install
```

Do not run `install` straight from `target/`. Install writes the binary's absolute path into herdr-core's `shell` setting and into every agent hook, and a `cargo clean` would then leave every recovered terminal unable to start. If that happens, see "Before you install" in the README.

## Test

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

All three must pass, and on a Mac also the Linux lint:

```sh
cargo clippy --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
```

`cargo test` includes end-to-end tests that start real herdr-core servers in throwaway home folders. They are built never to touch your own herdr-core; [docs/TESTING.md](docs/TESTING.md) explains how, how to skip them, and how to run the Linux tests in Docker.

When you test by hand, keep your real sessions out of it: run `hc` only against a throwaway `HOME` set up the way the harness does it (`tests/integration/harness.rs`). Any `hc` command against your real home starts or talks to your real herdr-core.

## House rules

- **No em dash character, anywhere**: code, comments, docs, commit messages. Use a colon, a comma or parentheses. CI fails on one.
- **Plain, short sentences** in docs and messages. Say what happens and what to do.
- **Never claim "supported"** for an agent or OS that has not passed a real reboot test (decision 13 in [docs/DECISIONS.md](docs/DECISIONS.md)). Untested means labeled untested.
- **The design rules in [docs/DESIGN.md](docs/DESIGN.md) hold**, including its "Corrections from the prototype". The ones most often at stake:
  - `herdr-wake shell` cannot fail: every path ends in a real shell.
  - `herdr-wake hook` never prints and always exits 0.
  - Store facts (id, folder, flags, env), rebuild the resume command at wake time.
  - No secrets in records.
  - Never check on herdr-core with `hc`; use the socket or the process table.
  - Edits to other tools' files add or remove only herdr-wake's own entries, keep comments and order, and back up first. Never change a hook command string between versions (Codex trust is bound to it).
- **Keep core logic free of Unix-only assumptions** where you can. Windows is planned; OS-specific code goes in `src/platform/` behind `#[cfg(target_os = "...")]`.
- **Limits are documented.** If your change adds or removes a limit, update [docs/LIMITS.md](docs/LIMITS.md) in the same change.

## Layout

| Path | What |
|---|---|
| `src/cli.rs` | Argument parsing and the user commands |
| `src/hook.rs`, `src/watch.rs` | Record flow: agent hooks, and the watcher that notices a deliberate quit |
| `src/wake.rs`, `src/gate.rs` | Resume flow: what `herdr-wake shell` does, and the staggered start |
| `src/boot.rs` | `herdr-wake boot`: start core, wait for resumes, write the summary |
| `src/record.rs` | The record stored as a label on each terminal |
| `src/core/` | The herdr-core socket client (`SocketCore`) and a fake for tests (`FakeCore`) |
| `src/agents/` | One adapter per agent |
| `src/platform/` | One adapter per OS, plus a fake |
| `src/setup/` | Install and uninstall, comment-preserving edits, remote install |
| `src/report.rs` | `status` and `doctor` |
| `assets/` | Plugin sources and boot-service templates that install writes out |
| `tests/` | Per-module tests, fixtures from the real agents, the end-to-end suite, the stand-in agent script |

## Adding an agent adapter

An adapter teaches herdr-wake one agent: how its hooks report a session, which of its flags to restore, how to resume it, and what to install. The interface is the `Agent` trait in `src/agents/mod.rs`:

```rust
pub trait Agent {
    fn kind(&self) -> AgentKind;
    fn observe(&self, event: &str, payload: &[u8], ctx: &mut Ctx) -> Observation;
    fn restorable_flags(&self, argv: &[String], cfg: &Config) -> Vec<String>;
    fn resume_plan(&self, rec: &Record, ctx: &mut Ctx) -> Plan;
    fn integration(&self, bin: &Path, ctx: &mut Ctx) -> Vec<Edit>;
}
```

Steps:

1. **Research first.** Write down, with sources, how the agent reports a session start and end, how ids change (`/clear`, resume, fork, compaction), how to tell sub-agents and one-shot runs apart, how a quit differs from a kill, its resume command and what it does with an unknown id, and its flags. `docs/research/agents-*.md` are the models.
2. **Name it.** Add a variant to `AgentKind` in `src/record.rs` (`ALL`, `as_str`, `FromStr`). The compiler then points at every `match` that needs a case, for example `manual_hint` in `src/wake.rs`.
3. **Write `src/agents/<name>.rs`** with a type that implements `Default` and `Agent`:
   - `observe`: turn one hook call (`herdr-wake hook <name> <event>`, JSON on stdin) into `Observation::Started`, `Observation::Ended` (with `deliberate` set when the payload says whether the user quit) or `Observation::Ignore` with a short reason. Ignore sub-agents and one-shot runs here.
   - `restorable_flags`: a table of every flag the agent knows, each classed as restore, bypass (restored only with `restore_bypass`), never, or needs validation (not restored). Unknown flags are dropped.
   - `resume_plan`: rebuild the command from the record. Return `Plan::Permanent(reason)` when the folder or the session is gone, so no retries are wasted. Use `before` for commands that must finish first, and `confirm_within_ms` when the agent could silently start a different session.
   - `integration`: the edits install makes, as `EditKind::WriteFile`, `EditKind::JsonHook` or `EditKind::JsoncListEntry`. Setup applies, backs up and reverts them for you.
4. **Register it** in `agents::get`, and teach `agents::identify` its process name so `adopt` can recognize it.
5. **Plugin or hook source.** If the agent needs a plugin file, put it in `assets/<name>/` and render it with the binary path and version (see `pi::render`). It must run only inside a herdr-core terminal (`HC_TERMINAL_ID` set), never print, and never block the agent.
6. **Environment.** If the agent has its own folder variable (like `CODEX_HOME`), add it to `ENV_ALLOW` in `src/record.rs`. Never add anything that can hold a secret.
7. **Doctor.** Add a check in `src/report.rs` that the integration is in place, and that it is not switched off by the agent's own settings.
8. **Tests.** Capture real payloads into `tests/fixtures/<name>/` (no secrets) and test the adapter against them in `tests/<name>.rs`. Teach `tests/stubs/agent.sh` to play the agent (`STUB_KIND`) and add an end-to-end case in `tests/integration/`.
9. **Docs.** Add the agent to the README's tables as "automated tests only", add its rows to the reboot matrix as "not yet run", and list its limits in `docs/LIMITS.md`.

## Adding a platform

A platform adapter hides one OS. The interface is the `Platform` trait in `src/platform/mod.rs`: `process` (argv, environment, start time and parent of a pid), `alive`, `boot_id`, `login_shell_argv`, `exec`, `run_foreground` (run the agent as the terminal's foreground job, then take the terminal back), `shutting_down`, `core_pid` (found from the process table, never by running `hc`), `boot_service` and `now_ms`. `BootService` installs, removes and reports the job that runs `herdr-wake boot`.

Steps:

1. **Research** how the OS starts a per-user job at boot or login, what environment it gets, how to read another process's argv and environment, and how to see a shutdown coming. `docs/research/boot-start.md` covers macOS, Linux and a first look at Windows.
2. **Write `src/platform/<os>.rs`** and select it in `platform::current()` with `#[cfg(target_os = "...")]`. Keep every OS call behind the trait; put boot-service templates in `assets/`.
3. **Beyond the trait.** For Windows in particular, these also need work: the herdr-core client in `src/core/socket.rs` (core uses a named pipe there), the `Os` enum and OS-specific checks in `src/report.rs`, trash handling in `src/setup/`, and the places that use `std::os::unix` or `libc` directly (`src/hook.rs`, `src/wake.rs`, `src/setup/edits.rs`).
4. **Tests.** Unit-test the adapter with injected runners and fake `/proc`-style roots, as `tests/platform_linux.rs` and `tests/platform_macos.rs` do. Add the OS to CI and make the end-to-end suite run there.
5. **Installer and release.** Add the target to `scripts/install.sh` and `.github/workflows/release.yml` (see [docs/RELEASING.md](docs/RELEASING.md)).
6. **Docs.** Add the platform to the README as "automated tests only", add its reboot-matrix rows as "not yet run", and write its limits into `docs/LIMITS.md`.

## Commits and pull requests

One change per pull request. A one-line subject in the imperative ("Detect OpenCode v2 by its service socket"), a blank line, then why. Say in the description what you tested and on which OS, and whether a real agent or only the stand-in was involved.
