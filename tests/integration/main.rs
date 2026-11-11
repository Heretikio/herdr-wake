//! Integration suite: the built `herdr-wake` binary against a real
//! herdr-core in a throwaway home, with stub agents (`tests/stubs/agent.sh`)
//! standing in for claude, codex, pi and opencode.
//!
//! Runs with plain `cargo test` whenever `hc` is installed and is skipped
//! (each test passes after printing a notice) when it is not. See
//! `harness.rs` for the isolation rules. Stopping and starting the isolated
//! core is what a reboot does to kept terminals.

mod harness;

mod resume;
mod setup;
mod tools;
