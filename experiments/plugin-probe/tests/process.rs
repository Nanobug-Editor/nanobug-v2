//! Public pipe protocol checks using real worker processes.

use nanobug_probe_wire::{Draft, Event, Failure, Owner, Request, Rule, receive, send};
use std::{
    io::BufReader,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

/// Controls one worker by its public command line and bounded pipe messages.
struct Worker {
    process: Child,
    stdin: ChildStdin,
    events: Receiver<Event>,
}

impl Worker {
    /// Start the test binary in `role` and read replies away from the test deadline.
    fn start(role: &str) -> Self {
        let mut process = Command::new(env!("CARGO_BIN_EXE_nanobug-probe-worker"))
            .arg(role)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = process.stdin.take().unwrap();
        let mut stdout = BufReader::new(process.stdout.take().unwrap());
        let (sender, events) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(event) = receive(&mut stdout).unwrap() {
                if sender.send(event).is_err() {
                    break;
                }
            }
        });
        Self {
            process,
            stdin,
            events,
        }
    }

    /// Send `request` without coupling the test to the worker's internals.
    fn send(&mut self, request: Request) {
        send(&mut self.stdin, &request).unwrap();
    }

    /// Receive one reply, failing promptly if the worker wedges.
    fn next(&self) -> Event {
        self.events
            .recv_timeout(Duration::from_secs(15))
            .expect("worker reply")
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[test]
fn guest_draws_the_host_confirmed_unicode_draft() {
    let mut worker = Worker::start("wasm");
    let owner = Owner {
        scope: 7,
        generation: 3,
    };
    let draft = Draft {
        revision: 5,
        text: "中文🙂".into(),
    };
    worker.send(Request::Open {
        owner,
        draft: draft.clone(),
    });
    assert!(matches!(worker.next(), Event::Frame {
        owner: actual, draft: restored, title, instances: 1, ..
    } if actual == owner && restored == draft && title == "Plugin P0"));
}

#[test]
fn one_project_can_close_while_another_keeps_its_wasm_surface() {
    let mut worker = Worker::start("wasm");
    let first = Owner {
        scope: 1,
        generation: 2,
    };
    let second = Owner {
        scope: 2,
        generation: 2,
    };
    for (owner, count) in [(first, 1), (second, 2)] {
        worker.send(Request::Open {
            owner,
            draft: Draft::default(),
        });
        assert!(
            matches!(worker.next(), Event::Frame { owner: actual, instances, .. }
            if actual == owner && instances == count)
        );
    }
    worker.send(Request::Close { owner: first });
    assert!(matches!(worker.next(), Event::Closed { owner, instances: 1 } if owner == first));
    worker.send(Request::Input {
        owner: Owner {
            generation: 1,
            ..second
        },
        draft: Draft::default(),
    });
    assert!(matches!(worker.next(), Event::ScopeError { .. }));

    worker.send(Request::Work {
        owner: second,
        spin: true,
    });
    assert!(matches!(worker.next(), Event::WorkStarted { owner } if owner == second));
    let draft = Draft {
        revision: 1,
        text: "Second project".into(),
    };
    worker.send(Request::Input {
        owner: second,
        draft: draft.clone(),
    });
    let mut drew = false;
    let mut interrupted = false;
    for _ in 0..2 {
        match worker.next() {
            Event::Frame {
                owner,
                draft: actual,
                instances: 1,
                ..
            } => {
                assert_eq!((owner, actual), (second, draft.clone()));
                drew = true;
            }
            Event::WorkDone {
                owner,
                result: Err(Failure::WasmBudget),
                ..
            } => {
                assert_eq!(owner, second);
                interrupted = true;
            }
            other => panic!("unexpected worker event: {other:?}"),
        }
    }
    assert!(drew && interrupted);
}

#[test]
fn closing_scope_drops_its_queued_work_before_reporting_release() {
    let mut worker = Worker::start("wasm");
    let first = Owner {
        scope: 1,
        generation: 1,
    };
    let second = Owner {
        scope: 2,
        generation: 1,
    };
    for owner in [first, second] {
        worker.send(Request::Open {
            owner,
            draft: Draft::default(),
        });
        assert!(matches!(worker.next(), Event::Frame { .. }));
    }
    worker.send(Request::Work {
        owner: first,
        spin: true,
    });
    assert!(matches!(worker.next(), Event::WorkStarted { owner } if owner == first));
    worker.send(Request::Work {
        owner: first,
        spin: true,
    });
    worker.send(Request::Close { owner: first });
    worker.send(Request::Work {
        owner: second,
        spin: false,
    });

    let mut closed = false;
    loop {
        match worker.next() {
            Event::Closed {
                owner,
                instances: 1,
            } if owner == first => closed = true,
            Event::WorkStarted { owner } if owner == first => {
                assert!(!closed, "queued work began after scope release");
            }
            Event::WorkDone { owner, .. } if owner == first => {}
            Event::WorkDone {
                owner,
                result: Ok(42),
                ..
            } if owner == second => {
                assert!(closed);
                break;
            }
            other => panic!("unexpected worker event: {other:?}"),
        }
    }
}

#[test]
fn waiting_rule_does_not_block_another_rule_and_late_data_cannot_publish() {
    let mut worker = Worker::start("rules");
    worker.send(Request::Evaluate {
        id: 1,
        rule: Rule::Flag,
    });
    assert!(matches!(worker.next(), Event::ReadFlag { id: 1 }));
    worker.send(Request::Evaluate {
        id: 2,
        rule: Rule::Busy,
    });
    assert!(matches!(
        worker.next(),
        Event::RuleDone {
            id: 2,
            result: Err(Failure::JsBudget),
            ..
        }
    ));
    assert!(
        matches!(worker.next(), Event::RuleDone { id: 1, result: Err(Failure::RuleDeadline), total_us, .. }
        if total_us >= 4_950_000)
    );

    worker.send(Request::FileResult {
        id: 1,
        result: Ok("yes".into()),
    });
    worker.send(Request::Evaluate {
        id: 3,
        rule: Rule::Flag,
    });
    assert!(matches!(worker.next(), Event::ReadFlag { id: 3 }));
    worker.send(Request::FileResult {
        id: 3,
        result: Ok("yes\n".into()),
    });
    assert!(matches!(
        worker.next(),
        Event::RuleDone {
            id: 3,
            result: Ok(true),
            ..
        }
    ));
}
