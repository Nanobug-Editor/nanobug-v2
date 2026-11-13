//! Exercise deadlines and independent progress through the actual QuickJS worker.

use nanobug_plugin_sdk::{ErrorKind, Reply, Request, WIRE, receive, send};
use serde_json::json;
use std::{
    io::BufReader,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Worker {
    child: Child,
    input: ChildStdin,
    replies: mpsc::Receiver<Reply>,
}

impl Worker {
    fn start() -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nanobug-plugin-runtime"));
        command
            .arg("rules")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, replies) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(reply)) = receive::<Reply>(&mut reader) {
                if sender.send(reply).is_err() {
                    break;
                }
            }
        });
        let worker = Self {
            child,
            input,
            replies,
        };
        assert!(matches!(worker.next(), Reply::Hello { wire: WIRE }));
        worker
    }

    fn post(&mut self, request: Request) {
        send(&mut self.input, &request).unwrap();
    }

    fn next(&self) -> Reply {
        self.replies.recv_timeout(Duration::from_secs(3)).unwrap()
    }

    fn evaluate(&mut self, id: u64, source: &str, remaining_ms: u64) {
        self.post(Request::Evaluate {
            id,
            source: source.into(),
            context: json!({}),
            remaining_ms,
        });
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_slow_query_does_not_block_another_rule_and_can_be_cancelled() {
    let mut worker = Worker::start();
    worker.evaluate(1, "return await host.exists('fixture');", 5000);
    let Reply::Query { id: 1, query, .. } = worker.next() else {
        panic!("expected first query")
    };
    worker.evaluate(2, "return true;", 5000);
    assert!(matches!(
        worker.next(),
        Reply::Rule {
            id: 2,
            matches: true
        }
    ));
    worker.post(Request::CancelRule { id: 1 });
    assert!(matches!(
        worker.next(),
        Reply::Error {
            id: 1,
            kind: ErrorKind::Cancelled,
            ..
        }
    ));
    worker.post(Request::Data {
        id: 1,
        query,
        result: Ok(json!(true)),
    });
    worker.evaluate(3, "return false;", 5000);
    assert!(matches!(
        worker.next(),
        Reply::Rule {
            id: 3,
            matches: false
        }
    ));
}

#[test]
fn rule_worker_admits_only_four_independent_evaluations() {
    let mut worker = Worker::start();
    let mut first_query = None;
    for id in 1..=4 {
        worker.evaluate(id, "return await host.exists('fixture');", 5000);
        let Reply::Query {
            id: received,
            query,
            ..
        } = worker.next()
        else {
            panic!("expected pending query for {id}")
        };
        assert_eq!(received, id);
        if id == 1 {
            first_query = Some(query);
        }
    }
    worker.evaluate(5, "return true;", 5000);
    assert!(matches!(
        worker.next(),
        Reply::Error {
            id: 5,
            kind: ErrorKind::Busy,
            ..
        }
    ));
    worker.post(Request::Data {
        id: 1,
        query: first_query.unwrap(),
        result: Ok(json!(true)),
    });
    assert!(matches!(
        worker.next(),
        Reply::Rule {
            id: 1,
            matches: true
        }
    ));
    worker.evaluate(5, "return true;", 5000);
    assert!(matches!(
        worker.next(),
        Reply::Rule {
            id: 5,
            matches: true
        }
    ));
}

#[test]
fn computation_and_overall_deadlines_have_distinct_results() {
    let mut worker = Worker::start();
    let mut observed = Vec::new();
    for id in 11..=16 {
        let started = Instant::now();
        worker.evaluate(id, "while (true) {}", 5000);
        assert!(
            matches!(worker.next(), Reply::Error { id: actual, kind: ErrorKind::Computation, .. } if actual == id)
        );
        let latency = started.elapsed();
        assert!(latency >= Duration::from_millis(20) && latency < Duration::from_millis(500));
        observed.push(latency);
    }
    eprintln!("QuickJS interrupt round trips: {observed:?}");
    let started = Instant::now();
    worker.evaluate(2, "return await host.exists('fixture');", 500);
    assert!(matches!(worker.next(), Reply::Query { id: 2, .. }));
    assert!(matches!(
        worker.next(),
        Reply::Error {
            id: 2,
            kind: ErrorKind::Deadline,
            ..
        }
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_waiting_query_cannot_extend_the_five_second_deadline() {
    let mut worker = Worker::start();
    let started = Instant::now();
    worker.evaluate(1, "return await host.exists('fixture');", 10_000);
    assert!(matches!(worker.next(), Reply::Query { id: 1, .. }));
    assert!(matches!(
        worker.replies.recv_timeout(Duration::from_secs(7)).unwrap(),
        Reply::Error {
            id: 1,
            kind: ErrorKind::Deadline,
            ..
        }
    ));
    let elapsed = started.elapsed();
    assert!((Duration::from_millis(4900)..Duration::from_secs(7)).contains(&elapsed));
    eprintln!("Five-second deadline round trip: {elapsed:?}");
}

#[test]
fn an_io_wait_does_not_spend_the_js_budget_or_enable_network_access() {
    let mut worker = Worker::start();
    worker.evaluate(1, r#"
        const present = await host.exists('fixture');
        const capabilities = Object.keys(host).sort().join(',');
        return present && capabilities === 'context,documentPath,exists,list,readBytes,readDocument,readFile,scanText'
            && typeof fetch === 'undefined' && typeof require === 'undefined'
            && typeof XMLHttpRequest === 'undefined' && typeof WebSocket === 'undefined'
            && typeof process === 'undefined' && typeof Deno === 'undefined';
    "#, 5000);
    let Reply::Query { id: 1, query, .. } = worker.next() else {
        panic!("expected host data query")
    };
    std::thread::sleep(Duration::from_millis(80));
    worker.post(Request::Data {
        id: 1,
        query,
        result: Ok(json!(true)),
    });
    assert!(matches!(
        worker.next(),
        Reply::Rule {
            id: 1,
            matches: true
        }
    ));
}

#[cfg(windows)]
#[test]
fn rule_child_has_no_direct_tcp_or_udp_endpoint_during_an_approved_query() {
    use std::{net::TcpListener, os::windows::process::CommandExt, path::PathBuf};

    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut worker = Worker::start();
    let child_pid = worker.child.id();
    worker.evaluate(
        1,
        &format!(
            r#"
        const url = 'http://127.0.0.1:{port}/fixture';
        if (typeof fetch === 'function') await fetch(url);
        if (typeof XMLHttpRequest === 'function' || typeof WebSocket === 'function') return false;
        return await host.exists('fixture');
    "#
        ),
        5000,
    );
    let Reply::Query {
        id: 1,
        query,
        kind,
        args,
    } = worker.next()
    else {
        panic!("expected only the approved local path query")
    };
    assert_eq!(kind, nanobug_plugin_sdk::Query::Exists);
    assert_eq!(args, json!({"path":"fixture"}));

    let netstat =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/netstat.exe");
    let sample = Command::new(netstat)
        .creation_flags(0x0800_0000)
        .args(["-a", "-n", "-o"])
        .output()
        .unwrap();
    assert!(sample.status.success());
    let endpoints = String::from_utf8_lossy(&sample.stdout);
    assert!(
        endpoints
            .lines()
            .any(|line| line.contains(&format!("127.0.0.1:{port}"))),
        "OS endpoint sample did not observe the control listener"
    );
    assert!(
        !endpoints.lines().any(|line| {
            let mut columns = line.split_whitespace();
            matches!(columns.next(), Some("TCP" | "UDP"))
                && columns.last().and_then(|value| value.parse().ok()) == Some(child_pid)
        }),
        "the rule worker had a direct network endpoint"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    worker.post(Request::Data {
        id: 1,
        query,
        result: Ok(json!(true)),
    });
    assert!(matches!(
        worker.next(),
        Reply::Rule {
            id: 1,
            matches: true
        }
    ));
}
