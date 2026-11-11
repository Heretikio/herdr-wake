# Agent adapters: Claude Code and Codex CLI

Researched 2026-11-10. Claude Code 2.1.322 (local `--help`). Codex source: `openai/codex` commit `ad3d352`, cited `CX:path:line` under `codex-rs/`. Surveyed tools (clones in `scratchpad/repos/`): HD herdrdev_herdr, DM dmazlum_herdr-revive, GU guillaume86_herdr-recover, TH thuanlm215_herdr-agent-resume, PI pine2D_herdr-session-resume, CA cantona_herdr-revive, QU quinnjr_herdr-auto-resume. Docs: [CC hooks](https://code.claude.com/docs/en/hooks), [CC env vars](https://code.claude.com/docs/en/env-vars), [CC plugins](https://code.claude.com/docs/en/plugins-reference), [Codex hooks](https://learn.chatgpt.com/docs/hooks).

## Claude Code

### 1. Session discovery

Hook events `SessionStart` and `SessionEnd` in `$CLAUDE_CONFIG_DIR/settings.json` (default `~/.claude`). Payload: `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `permission_mode`, `source` (`startup|resume|clear|compact|fork`), optional `model`, `agent_type`, `session_title`; `agent_id` is present only inside a subagent or teammate. `CLAUDE_CODE_SESSION_ID` in the hook env matches `session_id` (CC env vars).

```json
{"hooks":{
 "SessionStart":[{"matcher":"^(startup|resume|clear|compact|fork)$",
   "hooks":[{"type":"command","command":"'/ABS/herdr-wake' hook claude start","timeout":10}]}],
 "SessionEnd":[{"hooks":[{"type":"command","command":"'/ABS/herdr-wake' hook claude end","timeout":5}]}]}}
```

Anchored matcher from HD (`src/integration/claude_settings.rs:19`).

Install like HD: parse JSONC preserving formatting, drop only our entries, append one canonical entry, reject duplicate keys, atomic write with backup (`claude_settings.rs:65-95`, `:550`). Uninstall removes only our commands. Settings edits reach running sessions without a restart ([CC settings](https://code.claude.com/docs/en/settings), "When edits take effect"). Cover every config dir in use.

**Plugin versus settings.json.** A plugin (`hooks/hooks.json`, `${CLAUDE_PLUGIN_ROOT}`) installs and removes cleanly through `claude plugin` (GU, `.claude-plugin/plugin.json:10-17`). Against it: a plugin needs a marketplace and the `claude` CLI at install time (awkward over SSH and per `CLAUDE_CONFIG_DIR`), and "Timeouts set on plugin-provided hooks don't raise the budget" for SessionEnd (CC hooks, SessionEnd). Both are blocked by `allowManagedHooksOnly`. **Recommend settings.json**; `doctor` reports `disableAllHooks`/`allowManagedHooksOnly`.

**Fallback:** `$CLAUDE_CONFIG_DIR/sessions/<pid>.json` (undocumented; seen locally) holds `pid`, `sessionId`, `cwd`, `procStart`, `kind` (`interactive`), `entrypoint` (`cli`), `status`. PI matches `procStart` to `/proc/<pid>/stat` field 22 (`herdr_resume.py:289-302`). Use for `adopt` and cross-checks.

### 2. Id changes

`/clear`: SessionEnd `reason=clear` (old id), then SessionStart `source=clear` (new id). Interactive `/resume`: SessionEnd `resume`, then SessionStart `resume` (picked id). Compaction: SessionStart `compact` (same id). `--fork-session`: `source=fork`, new id (CC hooks). Newest SessionStart wins; a late SessionEnd `clear|resume` must not delete it (DM `lib/core.js:30`, GU `hook/recover-end.sh:48`). Order by a monotonic sequence (`assets/claude/herdr-agent-state.sh:60`).

### 3. What to skip

- Sub-agents: `agent_id` non-empty (HD `herdr-agent-state.sh:56`).
- `claude -p` (SessionStart still fires in `-p`): `sessions/<pid>.json` `kind != interactive` (GU `recover-hook.sh:145`), or `-p`/`--print` in argv.
- Nested runs: `CLAUDE_CODE_CHILD_SESSION=1` in the claude process env; Claude itself drops these from `--resume` (CC env vars). Hooks always carry it, so read the parent claude's env, not the hook's.
- SDK and IDE runs: `entrypoint != cli` (values other than `cli` are unverified), and no herdr-core terminal id in env.
- `--bare` and `--safe-mode` skip hooks entirely (`claude --help`).

### 4. End of session

SessionEnd reasons (CC hooks): `clear`, `resume`, `logout`, `prompt_input_exit`, `other` (`bypass_permissions_disabled` was removed in 2.1.234). Budget 1.5 s, raised by a per-hook `timeout` up to 60 s or `CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS`. Mapping: `prompt_input_exit`, `logout` mean a user quit; `clear`, `resume` mean an id change; `other` is ambiguous (DM keeps the record, `RECIPE.md:257-261`). Hooks at shutdown may not finish, so the shell-outlived test (Codex section 4) is authoritative.

### 6. Resume

`cd <cwd>; [ENV] claude <restored flags> --resume <id>`. Transcripts live in `projects/<cwd with non-alphanumerics replaced by ->/<id>.jsonl`; an empty session has none, and `--resume` then says "No conversation found" (DM `revive.mjs:215-230`). A wrong `CLAUDE_CONFIG_DIR` gives the same error (CA `docs/VALIDATION.md:148-153`). Pre-check the transcript under the recorded config dir: missing means a permanent failure. The exit code could not be tested here.

## Codex CLI

### 1. Session discovery

Hooks are on by default (`[features] hooks = false` turns them off; `codex_hooks` is a deprecated alias). They are read from `$CODEX_HOME/hooks.json` or inline `[hooks]` in `config.toml` (Codex hooks). `SessionStart` matches on `source` `startup|resume|clear|compact|fork` (`CX:hooks/src/events/session_start.rs:22-41`). Payload fields: `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`, `permission_mode`, `source`.

```json
{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"'/ABS/herdr-wake' hook codex start","timeout":10}]}]}}
```

**Trust:** user-layer hooks start `Untrusted` and are not registered until reviewed in `/hooks` (`CX:hooks/src/engine/discovery.rs:1000-1016`, `:917-923`). Trust is hash-bound: keep the command stable, append, never reorder (TH `scripts/install.py:119-120`). Never write `trusted_hash` ourselves; `doctor` points the user to `/hooks`. Merge and uninstall: HD `targets.rs:160-222`, `:603-650`.

**Fallbacks:**
- Rollouts: `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl` (`CX:rollout/src/recorder.rs:1680-1700`, may be compressed).
- State DB: `$CODEX_HOME/state_5.sqlite` (`CX:state/src/sqlite.rs:53`), table `threads` with `id, rollout_path, created_at, updated_at, source, cwd, archived, thread_source, agent_role` (`CX:state/migrations/0001,0030,0013`), plus `thread_spawn_edges`. PI matches on `cwd`, live process start and `source` (`herdr_resume.py:305-332`).

### 2. Id changes

Same five sources: `Cleared` maps to `clear` with a new thread, fork to `fork`, compaction to `compact` (`CX:core/src/session/session.rs:2207-2218`, `core/src/session/mod.rs:4999`). The newest SessionStart wins.

### 3. What to skip

- Spawned sub-agents fire `SubagentStart`, not SessionStart. Other `SubAgent` sources (review, compact, guardian auto-review) fire nothing (`CX:core/src/hook_runtime.rs:136-157`; guardian is `SubAgent(Other)`, `CX:core/src/thread_manager.rs:2882-2887`). Older Codex (0.166) did leak the auto-review thread (#5837), so verify anyway: the DB row must have `source='cli'` and `thread_source` of `user` or NULL, or rollout line 1 must say `thread_source` `user` (GU `recover-hook.sh:209-213`).
- `codex exec` (source `exec`), `vscode`, and `mcp` sessions do fire SessionStart (`CX:protocol/src/protocol.rs:3188-3199`). The DB `source` check skips them.
- Nested Codex in a Codex tool shell: `CODEX_THREAD_ID` is set and differs from `session_id` (HD `assets/codex/herdr-agent-state.sh:62-64`).

### 4. End of session

**Confirmed indistinguishable.** SessionEnd runs only on archive, delete, or 30 minutes idle and unopened; `reason` is "always `other`"; it never runs for subagents (Codex hooks). `/quit` sends no SessionEnd (DM `README.md:292-297`). A user quit exits 0, a fatal error exits 1 (`CX:cli/src/main.rs:771-792`). No SIGHUP/SIGTERM handler turned up in `tui/src` or `cli/src`, so a shutdown kill should show as a signal death.

**Agent-independent detection (use for both agents):** record the terminal's shell identity: pid plus start time, and on Linux also `boot_id` (QU `src/monitor.rs:216-231`). When the agent pid exits, wait about 2 s (TH `EXIT_GRACE`, `agent_resume.py:114,638-656`; GU waits 8 s).
- Shell alive and the same: the user quit, so forget the record.
- Shell gone: shutdown, so keep the record.

When herdr-wake launched the agent (decision 9), `waitpid` adds the status: exit 0 or 130 means a quit; SIGHUP, SIGTERM or SIGKILL means a kill. A zsh `precmd` marker is an extra signal (PI `herdr-resume.zsh:20-27`).

### 5. The daemon

Since `daemon_auto_start` (stable, default true, `CX:features/src/lib.rs:1135-1139`), the TUI attaches to a shared app-server. Hooks capture `std::env::vars_os()` of the process that owns the session (`CX:hooks/src/registry.rs:82`), which is the daemon. They therefore carry the env of whichever terminal started the daemon (GU `recover-hook.sh:197-202`; HD 0.9.4 `integrations.mdx:126`).

These force an in-process server: `--no-daemon`, `--oss`, `--profile`, any `-c` that is not a `features.*` bool, `--strict-config`, `--dangerously-bypass-hook-trust` (`CX:tui/src/daemon_startup.rs:65-122`). Setting `daemon_auto_start=false` still attaches to a daemon that is already running (`CX:tui/src/startup_orchestration.rs:574-578,743-752`, HD docs).

**Mapping without disabling the daemon:** ignore the hook's terminal env. Bind `session_id` to the herdr-core terminal whose foreground process is `codex` and whose:
- argv names that UUID (resumed runs), or
- cwd equals the payload `cwd` and process start time is at most the DB `created_at` plus a few seconds (GU `recover-hook.sh:203-208`; PI).

Two candidates: do not bind, mark ambiguous. Fallback: `--no-daemon` with consent (decision 16).

### 6. Resume

`cd <cwd>; [CODEX_HOME=..] codex resume <restored flags> <id>` (HD `src/agent_resume.rs:212-213`). For a missing id it prints `ERROR: No saved session found with ID <id>. Run \`codex resume\` without an ID to choose from existing sessions.` and exits 1 (`CX:tui/src/lib.rs:1612-1631`, `cli/src/main.rs:776-792`). Pre-check the DB row (`archived=0`) and `rollout_path`.

## 7. Flags and env

| Claude | Class |
|---|---|
| `--dangerously-skip-permissions`, `--allow-dangerously-skip-permissions`, `--permission-mode` | restore (opt-out) |
| `--model`, `--effort`, `--fallback-model`, `--agent`, `--agents`, `--add-dir`, `--settings`, `--setting-sources`, `--mcp-config`, `--strict-mcp-config`, `--plugin-dir`, `--allowed-tools`, `--disallowed-tools`, `--tools`, `--append-system-prompt`, `--system-prompt`, `--chrome`/`--no-chrome`, `--betas`, `--restricted` | restore; resolve relative paths against the launch cwd |
| `-p` and print-only flags, prompt, `-c`, `-r`, `--session-id`, `--fork-session`, `--from-pr`, `--teleport`, `--cloud`, `--bg`, `--desktop`, `-w`, `--tmux`, `--file`, `--debug*`, `--ide`, `-n` | never |
| `--remote-control`, `--bare`, `--safe-mode`, `--autocompact` | needs validation |

| Codex | Class |
|---|---|
| `--dangerously-bypass-approvals-and-sandbox`/`--yolo`, `--approve-for-me` | restore (opt-out) |
| `-m`, `-p`, `-s`, `-a`, `-c k=v`, `--enable`/`--disable`, `--add-dir`, `--search`, `--oss`, `--local-provider`, `--no-alt-screen`, `--no-daemon`, `--strict-config` | restore |
| prompt, `-i`, `-C` (use the recorded cwd), `--remote`, `--dangerously-bypass-hook-trust`, `exec`, `fork`, `--last` | never |
| `--worktree` | needs validation |

Flag sources: `CX:utils/cli/src/shared_options.rs:10-76`, `tui/src/cli.rs:70-88`. Env to restore: `CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_PROJECT_DIR_NAME` (paired with it), `CODEX_HOME`. Provider vars (`ANTHROPIC_BASE_URL`, `CLAUDE_CODE_USE_BEDROCK`/`VERTEX`) need validation. Never store secrets (`ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`).

**Capture:**
- Read NUL-separated argv: macOS `sysctl KERN_PROCARGS2` (bounded by argc, because env follows argv; HD fixed exactly that bug, `CHANGELOG.md:1251`, `src/platform/macos.rs:609-640`, CA `src/platform/macos.rs:233-262`); Linux `/proc/<pid>/cmdline` and `environ`.
- `ps -o args` joins words with spaces, which is lossy (TH `agent_resume.py:286-294`; DM splits on spaces).
- SIP can hide macOS environ (CA `docs/MACOS.md:28-30`).
- `/proc` reads can block on WSL (HD `src/platform/linux.rs:379-386`).
- For Claude the hook's parent is claude, so read the ppid chain. For Codex in daemon mode, read the pane's foreground process via herdr-core.

## 8. Known bugs to avoid

- **#5547** (open): a reported resume argv outlived `/clear` and restore reopened the old session. Fix: store only `{id, cwd, flags, env}` and rebuild argv at boot, never cache it.
- **#5837** (open, Codex 0.166): the auto-review thread replaced the pane's session. Fix: the thread_source/source check plus foreground matching.
- **HD #620:** nested `claude -p` overwrote the session.
- **HD #1789/#1927:** nested or ephemeral Codex did the same.
- **HD #5246:** Codex turn hooks caused timeout-clamp warnings, so install only SessionStart for Codex.
- **CA:** profile and wrapper mismatches; record `CLAUDE_CONFIG_DIR`.

## 9. Boot concurrency

Concurrent `claude` processes race single-use OAuth refresh tokens in one Keychain item and can log every session out ([#76905](https://github.com/anthropics/claude-code/issues/76905), closed stale, unfixed; see #41507, #57335). After a long downtime every token is expired, which makes this worst at boot. Others space starts 0.1 to 2.5 s (HD `CHANGELOG.md:88`, TH, GU, CA).

**Recommend (to validate in the reboot matrix):** start one Claude first and wait until its `sessions/<pid>.json` shows a status or 8 s pass; then start the rest about 1.5 s apart. Do the same for the first Codex, so only one daemon auto-starts.

## Adapter spec

| | Claude Code | Codex |
|---|---|---|
| Learn id | SessionStart hook in settings.json; fallback `sessions/<pid>.json` | SessionStart in hooks.json, trusted via `/hooks`; bind by foreground argv/cwd; fallback `state_5.sqlite` |
| Accept if | no `agent_id`, `kind=interactive`, not `-p`, terminal env present | DB `source=cli`, `thread_source` user/NULL, `CODEX_THREAD_ID` unset or equal, unique terminal match |
| Id change | newest SessionStart wins; ignore End `clear`/`resume` | newest SessionStart wins |
| Flags/env | parent argv via procargs/cmdline; `CLAUDE_CONFIG_DIR` | pane foreground argv; `CODEX_HOME` |
| Quit | End `prompt_input_exit`/`logout`, or shell outlived agent | shell outlived agent; launched by us: exit 0/130 |
| Resume | `claude <flags> --resume <id>` in cwd | `codex resume <flags> <id>` in cwd |
| Permanent fail | no `projects/*/<id>.jsonl`; "No conversation found" | no DB row or rollout; exit 1 "No saved session found" |
| Stagger | first alone (8 s), then 1.5 s | first alone, then 1.5 s |

**Open:**
- Claude's exit code for an unknown id.
- Whether `/exit` reports `prompt_input_exit` or `other`.
- SessionStart behavior on Codex versions between 0.166 and `ad3d352`.
- Whether `codex resume` honours `-C`.
- Ambiguous same-cwd Codex starts.
