//! `watch::run` against the fakes, with an injected sleeper and stop flag.

mod common;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use herdr_wake::core::fake::FakeCore;
use herdr_wake::core::{Core, CoreError, Hello, Terminal, TerminalId};
use herdr_wake::platform::fake::FakePlatform;
use herdr_wake::platform::ProcInfo;
use herdr_wake::record::{self, AgentKind, ProcRef, Record, State};
use herdr_wake::watch::{run, WatchOutcome};

const AGENT: ProcRef = ProcRef {
    pid: 500,
    start: 50,
};
const SHELL: ProcRef = ProcRef {
    pid: 400,
    start: 40,
};

/// A `Core` the test keeps a handle to after `run` takes its box.
#[derive(Clone, Default)]
struct Shared(Rc<RefCell<FakeCore>>);

impl Core for Shared {
    fn hello(&mut self) -> Result<Hello, CoreError> {
        self.0.borrow_mut().hello()
    }
    fn get(&mut self, t: &TerminalId) -> Result<Terminal, CoreError> {
        self.0.borrow_mut().get(t)
    }
    fn label(
        &mut self,
        t: &TerminalId,
        changes: &BTreeMap<String, Option<String>>,
    ) -> Result<Terminal, CoreError> {
        self.0.borrow_mut().label(t, changes)
    }
    fn list(&mut self, all: bool) -> Result<Vec<Terminal>, CoreError> {
        self.0.borrow_mut().list(all)
    }
}

struct World {
    platform: FakePlatform,
    core: Shared,
    t: TerminalId,
}

impl World {
    /// t1 holds a running record of agent 500 under shell 400. The shell is
    /// alive; the agent has already exited.
    fn new() -> World {
        let w = World {
            platform: FakePlatform::new(),
            core: Shared::default(),
            t: TerminalId::from("t1"),
        };
        w.core.0.borrow_mut().insert(Terminal::new("t1"));
        w.platform
            .add_process(ProcInfo::new(400, 300, 40, vec!["-zsh".into()]));
        w.store(AGENT, Some(SHELL), "s1", 10);
        w
    }

    fn store(&self, agent: ProcRef, shell: Option<ProcRef>, session: &str, seq: u64) {
        let mut rec = Record::new(
            AgentKind::Claude,
            session,
            PathBuf::from("/home/u/proj"),
            PathBuf::from("/x/claude"),
            "boot-1",
        );
        rec.agent_proc = Some(agent);
        rec.shell_proc = shell;
        rec.seq = seq;
        assert!(record::save(&mut *self.core.0.borrow_mut(), &self.t, &rec).unwrap());
    }

    fn state(&self) -> State {
        record::load(&mut *self.core.0.borrow_mut(), &self.t)
            .unwrap()
            .unwrap()
            .state
    }

    fn connect(&self) -> impl Fn() -> Result<Box<dyn Core>, CoreError> + '_ {
        || Ok(Box::new(self.core.clone()) as Box<dyn Core>)
    }
}

fn no_sleep(_ms: u64) {}
fn never() -> bool {
    false
}

#[test]
fn quit_with_the_shell_alive_ends_the_record() {
    let w = World::new();
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &no_sleep,
        &never,
    );
    assert_eq!(out, WatchOutcome::EndedRecord);
    assert_eq!(w.state(), State::Ended);
}

#[test]
fn polls_every_second_then_waits_the_grace_period() {
    let w = World::new();
    w.platform
        .add_process(ProcInfo::new(500, 400, 50, vec!["claude".into()]));
    let sleeps = RefCell::new(Vec::new());
    let sleeper = |ms: u64| {
        sleeps.borrow_mut().push(ms);
        if sleeps.borrow().len() == 3 {
            w.platform.remove_process(500);
        }
    };

    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &sleeper,
        &never,
    );

    assert_eq!(out, WatchOutcome::EndedRecord);
    assert_eq!(*sleeps.borrow(), vec![1000, 1000, 1000, 2000]);
}

#[test]
fn shell_gone_leaves_the_record() {
    let w = World::new();
    w.platform.remove_process(400);
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &no_sleep,
        &never,
    );
    assert!(matches!(out, WatchOutcome::LeftRecord(_)), "{out:?}");
    assert_eq!(w.state(), State::Running);
}

#[test]
fn shell_dying_during_the_grace_period_leaves_the_record() {
    let w = World::new();
    let sleeper = |ms: u64| {
        if ms == 2000 {
            w.platform.remove_process(400);
        }
    };
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &sleeper,
        &never,
    );
    assert!(matches!(out, WatchOutcome::LeftRecord(_)), "{out:?}");
    assert_eq!(w.state(), State::Running);
}

#[test]
fn shell_pid_reused_by_another_process_leaves_the_record() {
    let w = World::new();
    w.platform
        .add_process(ProcInfo::new(400, 1, 99, vec!["other".into()]));
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &no_sleep,
        &never,
    );
    assert!(matches!(out, WatchOutcome::LeftRecord(_)), "{out:?}");
    assert_eq!(w.state(), State::Running);
}

#[test]
fn shutting_down_leaves_the_record() {
    let w = World::new();
    w.platform.set_shutting_down(true);
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &no_sleep,
        &never,
    );
    assert_eq!(out, WatchOutcome::LeftRecord("shutting down"));
    assert_eq!(w.state(), State::Running);
}

#[test]
fn shutdown_starting_while_core_is_read_leaves_the_record() {
    let w = World::new();
    let connect = || {
        w.platform.set_shutting_down(true);
        Ok(Box::new(w.core.clone()) as Box<dyn Core>)
    };
    let out = run(&w.t, AGENT, SHELL, &w.platform, &connect, &no_sleep, &never);
    assert_eq!(out, WatchOutcome::LeftRecord("shutting down"));
    assert_eq!(w.state(), State::Running);
}

#[test]
fn stop_flag_leaves_the_record() {
    let w = World::new();
    let stopped = Cell::new(false);
    // The signal lands during the grace period.
    let sleeper = |ms: u64| {
        if ms == 2000 {
            stopped.set(true);
        }
    };
    let stop = || stopped.get();
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &sleeper,
        &stop,
    );
    assert_eq!(out, WatchOutcome::LeftRecord("stopped"));
    assert_eq!(w.state(), State::Running);
}

#[test]
fn stop_flag_while_the_agent_runs_leaves_the_record() {
    let w = World::new();
    w.platform
        .add_process(ProcInfo::new(500, 400, 50, vec!["claude".into()]));
    let stopped = Cell::new(false);
    let sleeper = |_ms: u64| stopped.set(true);
    let stop = || stopped.get();
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &sleeper,
        &stop,
    );
    assert_eq!(out, WatchOutcome::LeftRecord("stopped"));
    assert_eq!(w.state(), State::Running);
}

#[test]
fn boot_id_change_leaves_the_record() {
    let w = World::new();
    let sleeper = |ms: u64| {
        if ms == 2000 {
            w.platform.set_boot_id("boot-2");
        }
    };
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &sleeper,
        &never,
    );
    assert!(matches!(out, WatchOutcome::LeftRecord(_)), "{out:?}");
    assert_eq!(w.state(), State::Running);
}

#[test]
fn newer_session_in_the_terminal_is_not_ended() {
    let w = World::new();
    let newer = ProcRef {
        pid: 600,
        start: 60,
    };
    w.store(newer, Some(SHELL), "s2", 20);
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &no_sleep,
        &never,
    );
    assert!(matches!(out, WatchOutcome::LeftRecord(_)), "{out:?}");
    let rec = record::load(&mut *w.core.0.borrow_mut(), &w.t)
        .unwrap()
        .unwrap();
    assert_eq!(rec.state, State::Running);
    assert_eq!(rec.session_id, "s2");
    assert_eq!(rec.agent_proc, Some(newer));
}

#[test]
fn record_whose_shell_is_cores_holder_is_never_ended() {
    let w = World::new();
    w.store(AGENT, None, "s1", 20);
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &no_sleep,
        &never,
    );
    assert!(matches!(out, WatchOutcome::LeftRecord(_)), "{out:?}");
    assert_eq!(w.state(), State::Running);
}

#[test]
fn core_not_running_leaves_the_record() {
    let w = World::new();
    let connect = || Err(CoreError::NotRunning);
    let out = run(&w.t, AGENT, SHELL, &w.platform, &connect, &no_sleep, &never);
    assert!(matches!(out, WatchOutcome::LeftRecord(_)), "{out:?}");
    assert_eq!(w.state(), State::Running);
}

#[test]
fn core_failing_after_connect_leaves_the_record() {
    let w = World::new();
    w.core.0.borrow_mut().fail_with(Some(CoreError::Timeout));
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &no_sleep,
        &never,
    );
    assert!(matches!(out, WatchOutcome::LeftRecord(_)), "{out:?}");
    w.core.0.borrow_mut().fail_with(None);
    assert_eq!(w.state(), State::Running);
}

#[test]
fn gives_up_after_thirty_days() {
    let w = World::new();
    w.platform
        .add_process(ProcInfo::new(500, 400, 50, vec!["claude".into()]));
    let slept = Cell::new(0u64);
    let sleeper = |ms: u64| slept.set(slept.get() + ms);
    let out = run(
        &w.t,
        AGENT,
        SHELL,
        &w.platform,
        &w.connect(),
        &sleeper,
        &never,
    );
    assert!(matches!(out, WatchOutcome::LeftRecord(_)), "{out:?}");
    assert_eq!(slept.get(), 30 * 24 * 3600 * 1000);
    assert_eq!(w.state(), State::Running);
}
