//! The start gate: staggered start slots shared by independent wrapper
//! processes through a locked file.

use herdr_wake::config::Config;
use herdr_wake::gate::{boot_key, confirm, enter, enter_ticket, remaining};
use herdr_wake::platform::fake::FakePlatform;
use herdr_wake::record::AgentKind;

const T0: u64 = 1_700_000_000_000;

#[test]
fn six_same_instant_calls_get_first_alone_then_stagger() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    let waits: Vec<u64> = (0..6)
        .map(|_| enter(dir.path(), AgentKind::Claude, &cfg, "b1-100", T0))
        .collect();
    assert_eq!(waits, vec![0, 8000, 9500, 11000, 12500, 14000]);
}

#[test]
fn callers_arriving_later_wait_less_and_never_negative() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    assert_eq!(enter(dir.path(), AgentKind::Claude, &cfg, "k", T0), 0);
    // Second slot is T0 + 8000; arriving 3 s later leaves 5 s.
    assert_eq!(
        enter(dir.path(), AgentKind::Claude, &cfg, "k", T0 + 3000),
        5000
    );
    // Third slot would be T0 + 9500; arriving at T0 + 20000 waits nothing.
    assert_eq!(
        enter(dir.path(), AgentKind::Claude, &cfg, "k", T0 + 20_000),
        0
    );
    // The next one follows the late arrival by the stagger.
    assert_eq!(
        enter(dir.path(), AgentKind::Claude, &cfg, "k", T0 + 20_000),
        1500
    );
}

#[test]
fn confirm_releases_the_first_alone_hold_early() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    assert_eq!(enter(dir.path(), AgentKind::Claude, &cfg, "k", T0), 0);
    confirm(dir.path(), AgentKind::Claude, "k", T0 + 2300);
    // Next slot: max(confirm time, first slot) + stagger = T0 + 3800.
    assert_eq!(
        enter(dir.path(), AgentKind::Claude, &cfg, "k", T0 + 2400),
        1400
    );
    assert_eq!(
        enter(dir.path(), AgentKind::Claude, &cfg, "k", T0 + 2400),
        2900
    );
}

#[test]
fn confirm_shortens_the_wait_of_a_caller_already_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    enter(dir.path(), AgentKind::Claude, &cfg, "k", T0);
    let t = enter_ticket(dir.path(), AgentKind::Claude, &cfg, "k", T0);
    assert_eq!(t.wait_ms, 8000);
    assert_eq!(
        remaining(dir.path(), AgentKind::Claude, &cfg, "k", &t, T0 + 1000),
        7000
    );
    confirm(dir.path(), AgentKind::Claude, "k", T0 + 2000);
    assert_eq!(
        remaining(dir.path(), AgentKind::Claude, &cfg, "k", &t, T0 + 2000),
        1500
    );
}

#[test]
fn a_late_confirm_never_delays() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    enter(dir.path(), AgentKind::Claude, &cfg, "k", T0);
    confirm(dir.path(), AgentKind::Claude, "k", T0 + 7900);
    // max(7900, 0) + 1500 = 9400 is later than the 8000 hold: keep 8000.
    assert_eq!(enter(dir.path(), AgentKind::Claude, &cfg, "k", T0), 8000);
}

#[test]
fn confirm_with_no_callers_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    confirm(dir.path(), AgentKind::Claude, "k", T0);
    assert_eq!(enter(dir.path(), AgentKind::Claude, &cfg, "k", T0), 0);
    assert_eq!(enter(dir.path(), AgentKind::Claude, &cfg, "k", T0), 8000);
}

#[test]
fn a_different_boot_key_starts_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    enter(dir.path(), AgentKind::Claude, &cfg, "old-1", T0);
    enter(dir.path(), AgentKind::Claude, &cfg, "old-1", T0);
    assert_eq!(enter(dir.path(), AgentKind::Claude, &cfg, "new-2", T0), 0);
    assert_eq!(
        enter(dir.path(), AgentKind::Claude, &cfg, "new-2", T0),
        8000
    );
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names.iter().any(|n| n.starts_with("old-1")),
        "old boot file left: {names:?}"
    );
}

#[test]
fn two_kinds_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::default();
    assert_eq!(enter(dir.path(), AgentKind::Claude, &cfg, "k", T0), 0);
    assert_eq!(enter(dir.path(), AgentKind::Codex, &cfg, "k", T0), 0);
    assert_eq!(enter(dir.path(), AgentKind::Claude, &cfg, "k", T0), 8000);
    assert_eq!(enter(dir.path(), AgentKind::Codex, &cfg, "k", T0), 8000);
}

#[test]
fn config_values_are_used() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        first_alone_ms: 100,
        stagger_ms: 10,
        ..Config::default()
    };
    let waits: Vec<u64> = (0..3)
        .map(|_| enter(dir.path(), AgentKind::Pi, &cfg, "k", T0))
        .collect();
    assert_eq!(waits, vec![0, 100, 110]);
}

#[test]
fn an_unusable_folder_means_no_wait() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, "x").unwrap();
    let cfg = Config::default();
    assert_eq!(enter(&file, AgentKind::Claude, &cfg, "k", T0), 0);
    assert_eq!(enter(&file, AgentKind::Claude, &cfg, "k", T0), 0);
}

#[test]
fn a_corrupt_gate_file_starts_fresh() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("k.json"), "{not json").unwrap();
    let cfg = Config::default();
    assert_eq!(enter(dir.path(), AgentKind::Claude, &cfg, "k", T0), 0);
    assert_eq!(enter(dir.path(), AgentKind::Claude, &cfg, "k", T0), 8000);
}

#[test]
fn concurrent_callers_get_distinct_slots() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let p = path.clone();
            std::thread::spawn(move || enter(&p, AgentKind::Claude, &Config::default(), "k", T0))
        })
        .collect();
    let mut waits: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    waits.sort_unstable();
    assert_eq!(waits, vec![0, 8000, 9500, 11000, 12500, 14000]);
}

#[test]
fn boot_key_names_boot_and_core_and_is_a_plain_file_name() {
    let p = FakePlatform::new();
    p.set_boot_id("ABCD-12/34");
    p.set_core_pid(Some(4321));
    let k = boot_key(&p);
    assert!(k.contains("ABCD-12"), "{k}");
    assert!(k.contains("4321"), "{k}");
    assert!(!k.contains('/'), "{k}");
    p.set_core_pid(Some(4322));
    assert_ne!(boot_key(&p), k);
    p.set_core_pid(None);
    assert!(!boot_key(&p).is_empty());
}
