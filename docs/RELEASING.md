# Releasing herdr-wake

## Checklist

1. Bump `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock` follows, and commit.
2. Run the reboot matrix (decision 13: 4 agents x 2 OSes, plus one phone-first test per OS) on real hosts. Update the dated results table with the herdr-wake, herdr-core and agent versions; mark anything not run as untested.
3. Make sure CI is green on the commit, then tag and push: `git tag v0.1.0 && git push origin v0.1.0`. The tag must equal `v` plus the Cargo.toml version, or the release fails.
4. On a clean machine (no `~/.local/bin/herdr-wake`), run the one-liner and check `herdr-wake doctor`:

   ```sh
   curl -fsSL https://github.com/Heretikio/herdr-wake/releases/latest/download/install.sh | sh
   ```

   Flags go after `sh -s --`, for example `| sh -s -- --before-login`. `HERDR_WAKE_DRY_RUN=1` prints the target, URLs and paths without downloading.

A tag with a hyphen (`v0.2.0-rc.1`) becomes a prerelease, which `latest` skips.

## Changing the owner or repository

- Workflows use `${{ github.repository }}`: nothing to change.
- `scripts/install.sh` and `scripts/uninstall.sh`: the one `REPO=` line in `main()`. Users can also override it with `HERDR_WAKE_REPO=owner/repo`.
- The install one-liner above and in the README.

## Workflows

**ci.yml** (every push and pull request): on macOS arm64 and Ubuntu x86_64, installs the newest herdr-core with its official installer (`https://releases.herdr.dev/core/install.sh`, so the integration tests run rather than skip), then `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. A lint job fails on any em dash and runs shellcheck on `scripts/`.

**nightly.yml** (daily at 06:17 UTC, or run by hand): the full test suite on both OSes against the newest herdr-core. On failure it opens, or comments on, one issue titled "Nightly failed against herdr-core <version>". GitHub pauses schedules after 60 idle days; re-enable it in the Actions tab.

**release.yml** (tags `v*`): checks the tag against Cargo.toml, builds four targets, strips them, and publishes `herdr-wake-<version>-<target>.tar.gz` with a `.sha256` for each, plus `install.sh` and `uninstall.sh`, with generated notes.

- macOS (`aarch64-apple-darwin`, `x86_64-apple-darwin`): both on `macos-latest` with Apple's own toolchain, which builds either arch. The binary is ad-hoc signed again after `strip`.
- Linux (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`): the Rust musl target plus `musl-tools`, each on a runner of its own CPU (`ubuntu-latest`, `ubuntu-24.04-arm`). No cross linker, Docker or zig, and each static binary is smoke-tested where it was built. If the arm runner is unavailable (it may be, for a private repository), switch that row to `cross`.
