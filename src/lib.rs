//! herdr-wake: starts herdr-core at boot, records coding-agent sessions as
//! labels on herdr-core terminals, and resumes them after a reboot.

pub mod agents;
pub mod boot;
pub mod cli;
pub mod config;
pub mod core;
pub mod gate;
pub mod hook;
pub mod paths;
pub mod platform;
pub mod private;
pub mod record;
pub mod report;
pub mod setup;
pub mod trust;
pub mod wake;
pub mod watch;
