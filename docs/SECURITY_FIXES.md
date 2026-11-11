# herdr-wake security fixes

Date: 2026-11-10. This file answers [SECURITY_AUDIT.md](SECURITY_AUDIT.md) finding by finding: what was fixed, in which commit, how, and what was not fixed and why. It also lists the bugs found the same day by a real install and a real reboot on a Linux machine.

Every fix has automated tests. Most were written and seen failing before the fix; for M-5 several were written in the same step as the code and fail against the previous commit only by not compiling. None of it has been checked by a second reviewer, by a real second user account, by a real `sudo` run, or by a real LaunchDaemon install. After this file was first written, the fixed build ran through two real reboots (Linux x86_64 and macOS arm64, see [TEST_RESULTS.md](TEST_RESULTS.md)): the local-copy check showed older records as `untrusted`, private file modes were confirmed on disk, install skipped absent agents, and the monotonic clock held the stagger through a 130 s clock step. The Linux-only code also passed its tests in a Linux arm64 container. No release has been built, so the pinned workflow and the attestation step have never executed.

## Findings

| ID | Status | Commit | How |
|---|---|---|---|
| M-1 | fixed | `997aace` | One function, `trust::folder_is_safe(path, uid)`, refuses a folder that is not a real folder, is a symlink, is not owned by the user, is writable by group or others, or has a folder above it that is world-writable without the sticky bit or owned by anyone but the user and root. `wake::decide` gives up with `folder is not safe to resume in: <why>`; the hook does not record and logs why. |
| M-2 | fixed | `52f9c07` | With `restore_bypass = false`, Claude `--settings`, `--allowed-tools`/`--allowedTools` and `--tools` are dropped as well; Codex `--sandbox` and `--ask-for-approval` are kept only with a tightening value, `-c` keys `approval_policy`, `sandbox_mode`, `sandbox_*` and `profile` are dropped, as is `--profile`. Table-driven tests in `tests/bypass.rs`. |
| M-3 | fixed, with a gap | `a1b0534` (and `46b5273` for `OPENCODE_DB`) | Inline JSON for Claude `--settings`, `--mcp-config`, `--agents` is never stored (the flag is dropped, the log names it); Codex `-c` overrides that look like credentials are dropped; URL user names and passwords are stripped from OpenCode `--server` and drop any other flag; `OPENCODE_DB` is kept only as an absolute path. Not done: moving flags and environment out of the label into a private file (audit fix e). Flags are still in the label, so a secret in a flag value the patterns do not catch is still stored. |
| M-4 | fixed | `8875220` | `install`, `uninstall`, `adopt`, `resume`, `forget` (and `off`, `on`) refuse an effective uid of 0. The `is_root` branches of the macOS boot service are gone: the LaunchDaemon is always staged and the `sudo` commands printed. `Config::save` and every other write under the config and state folders go through `private::write`. The LaunchAgent plist is written through an `O_EXCL` temporary file. |
| M-5 | fixed | `46b5273` | At wake: the environment goes through `scrub_env` again (and `PATH` keeps only absolute folders), the session id must match `[A-Za-z0-9][A-Za-z0-9._-]{0,127}` (also at record time, for every agent), the program must be an absolute path to a regular, executable file owned by the user or root, not writable by others, and named for the agent, Pi's session file must be a regular file the user owns inside Pi's session folder, and flags are filtered through each adapter's allowlist (tests for all four in `tests/label_flags.rs`). Structural fix: the hook, `adopt` and `resume <terminal>` keep a private copy of each record in `<state>/records/<terminal>.json`; a label that differs from it on agent, session id, session file, folder, flags, environment or program, or belongs to a terminal with another creation time, is not resumed, is logged as `record does not match the local copy`, and shows as `untrusted` in `status`. Not done: passing the id as `--resume=<id>` (an id can no longer start with `-`), and requiring the agents' folder variables to be absolute (Codex supports a `CODEX_HOME` relative to the session folder on purpose). |
| L-6 | fixed, with a gap | `8875220` | `private::write`, `append`, `open_rw` and `create_new` create files 0600 in 0700 folders, refuse a symlink, and tighten looser modes. Used for the log, gate files, the boot summary, claims, unconfirmed markers, record copies, backups, `boot.env`, `installed-files.json`, `config.toml`, the Linux linger marker and unit backup. Install tightens the config and state folders. Not done: backups are still never pruned. |
| L-7 | partly fixed | `f20a4b2` | The daemon's user name comes from the user database by uid, never from `$USER`, and must match `[A-Za-z0-9][A-Za-z0-9._-]*`; the plist is staged 0600 in a 0700 folder; `sudo plutil -lint <staged>` is the first printed command. Not done: printing the plist's `UserName` and `ProgramArguments` next to the commands, and a SHA-256 check inside the `sudo` line. A process running as the user can still rewrite the staged file before the user installs it; that needs malware already running as a user who then types `sudo`. `HOME` still comes from the environment. |
| L-8 | partly fixed | `6c7c816` | Every action in the three workflows is pinned to a full commit SHA with its version in a comment; default permissions are `contents: read`; the release job (only) has `id-token: write` and `attestations: write` and runs `actions/attest-build-provenance` on the archives before publishing. Not done: Dependabot for the pins, immutable releases and a tag ruleset (repository settings, not files), verifying the attestation in `install.sh`, and `cargo audit` in CI. |
| L-9 | partly fixed | `d16088b` | `install.sh` stages with `mktemp` in the bin folder and says in a comment that the checksum guards against corruption, not a compromised release. `uninstall.sh` runs `HERDR_WAKE_BIN`, else the default install path, and only then a `herdr-wake` in an absolute `PATH` folder, which it names first. Not done: refusing a group or world-writable bin folder, and the last-line truncation (a download cut inside `main "$@"` can run the installer without its arguments). |
| L-10 | fixed | `3f64e27` | `trust::printable` strips control characters (below 0x20 except tab, 0x7f, and C1) from the give-up line, every log line, the status table, doctor output, `resume` and `adopt` output and the boot details. `shell_quote` was not changed: a printed command whose path held control characters is shown without them and so would not run as printed. |
| I-11 | fixed | `296a4c8` | Install refuses a binary path or `boot.env` path that contains a control character. |
| I-12 | not fixed | | CI and nightly still pipe herdr-core's installer into `sh`. The token there is read-only and no secret is present; the release workflow does not do it. |
| I-13 | fixed | `0ffba52` | One function, `config::is_excluded`, used by the wrapper, the hook and the boot summary: a leading `~` is expanded, whole path components are compared. |
| I-14 | not fixed | | `install` still rewrites `boot.env` without a backup, and `doctor`'s advice still does not name the file. Not in the scope of this change. |
| I-15 | not fixed | | The flag allowlists are still compiled in, not checked against `--help`. Documented in LIMITS.md. Decision 15's text was left for the maintainer to change. |
| I-16 | no change needed now | | Nothing reads a resume reference from herdr-core. `trust::valid_session_id` is the check to apply if that ever comes. |
| Section 4, item 2 | fixed | `f20a4b2` | `StandardOutPath` and `StandardErrorPath` are gone from `assets/launchd/daemon.plist`; the boot run writes its summary to herdr-wake's own log. |
| Section 5 (README warnings) | fixed | the commit that adds this file | README "Before you install" now has five points (unattended restart with bypass flags and the exact list the opt-out removes; the `shell` setting; login timing; project configuration and folder safety; never under sudo), and a Security section that links here. |
| Small items without an ID | not fixed | | `doctor` still reads all of `holder.log` for its last line; Linux `shutting_down` still runs `busctl` and `systemctl` by bare name; `install --host X --dry-run` still prints the host before validating it. |

## Not fixed, in one place

- M-3 (e): flags and environment are still stored in the label, not in a private file with a reference.
- L-6: backups are never pruned.
- L-7: no plist summary or hash next to the `sudo` commands.
- L-8: no Dependabot, no repository rulesets, no attestation check in the installer, no `cargo audit`.
- L-9: a writable bin folder is not refused; last-line truncation.
- I-12, I-14, I-15, and the small items without an ID.
- Audit section 4, items 1, 3, 4, 5 and 6 (herdr-core's socket access control, how the agents parse an id that starts with `-`, agent sandboxes and the socket, real reboot behavior, third-party crates) remain unassessed. The session id rule makes item 3 moot for herdr-wake's own command lines.

## Choices made while fixing that a reviewer should look at

- **Group-writable folders are refused**, as decision 15 and the task say. On systems with umask 002 that is every new folder. ACLs are not read.
- **A folder above the session's that another user owns is refused whatever its mode** (its owner can always write to it). The audit text said "writable by others"; this is stricter.
- **The local copy turns every record from an earlier build into `untrusted`.** `herdr-wake resume <terminal>` accepts a record, which is also how a user can bless a label someone else wrote; it prints a notice and the command.
- **The copy of a session that ended is deleted**, because the state is not among the compared fields and a label could otherwise be flipped from ended back to running.
- **The program name check accepts the name as recorded** (before symlinks are resolved) as well as the resolved name, so `~/.local/bin/claude` pointing at a file with another name passes. Ownership and mode are checked on the resolved file.
- **Codex `--sandbox` and `--ask-for-approval` use allowlists** (`read-only`, `workspace-write`; `untrusted`, `on-request`, `on-failure`): an unknown value is dropped when the opt-out is on.
- **The credential pattern for Codex `-c` is broad** (`key`, `token`, `secret`, `password`, `auth`, `bearer` anywhere in key or value) and drops harmless overrides such as `preferred_auth_method`.
- **`Swatinem/rust-cache` is pinned to v2.9.3**, the newest release tag, not to the commit the floating `v2` tag points at (that commit has no version tag). `dtolnay/rust-toolchain` is pinned to the head of its `stable` branch on 2026-11-10 and now passes `toolchain: stable` explicitly.

## Bugs found in real install and reboot testing

Found on a Linux machine on 2026-11-10 and fixed in the same series. These are not audit findings.

| Bug | Commit | Fix |
|---|---|---|
| The wall clock stepped back 130 s during boot (chronyd correcting a hardware clock that was ahead), after `herdr-wake boot` and the first wrapper had started. Gate slots and the gate confirm were compared as wall-clock times, so the second agent waited the full first-alone hold although the first had confirmed; the boot summary's `finished_ms` was earlier than `started_ms`. | `38e8658` | `Platform::mono_ms` (`CLOCK_BOOTTIME` on Linux, `CLOCK_MONOTONIC` on macOS, a settable clock in `FakePlatform`) is used for gate slots, the stagger, the first-alone hold, the gate confirm and the boot run's duration (`duration_ms`; `finished_ms` is the start plus it). The wrapper's fast-fail time, the retry pause, the confirmation deadline and the settle loop already used monotonic time or counted sleeps. The wall clock stays for log lines, the summary's start time and `seq` (guarded by `next_seq`). A test steps the fake wall clock back 130 s between the first slot and the confirm and expects the second agent one stagger later; with the old code it waited 131.5 s. |
| `install` wrote Codex, Pi and OpenCode files although those agents were not installed, creating `~/.codex`, `~/.pi/agent` and `~/.config/opencode` from nothing. | `619b7e6` | An agent is set up only when its config folder exists or its program is on `PATH`; otherwise install prints `skipped <agent>: not installed` and creates nothing. Uninstall still reverts every agent and both OpenCode layouts, and removes folders it leaves empty. OpenCode's layout follows `opencode --version`. |
| `install` backed up `settings.json` twice in one run. | `619b7e6` | One backup per file per run, as the file was before the run. |

Fixed in the same series, from review rather than from the reboot:

| Change | Commit |
|---|---|
| Install never records a versioned path (Homebrew `Cellar`, `versions/`) as herdr-core's shell | `e3c3c7e` |
| `auto_continue` removed from the settings | `2fb3dcd` |
| A `config.toml` that cannot be parsed resumes nothing; `doctor` reports it as a failed check | `900cb2c` |
| Per-terminal opt-out: `herdr-wake off <terminal>` and `on <terminal>` (decision 15) | `dda47c1` |
| `doctor` warns about a Pi older than 0.79.4 or an OpenCode 2.x older than 2.0.28 | `e2e75fd` |
