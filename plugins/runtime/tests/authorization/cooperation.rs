//! Exercise optional peer calls through two installed guest workers.

use super::*;
use nanobug::plugins::session::Rule;

const PEER_NAME: &str = "Optional Peer";
const PEER_ID: &str = "optional-peer";

/// Send one peer call on a command and report its callback on the next Diagnose request.
fn caller_wasm() -> Vec<u8> {
    let request = json!({"services":[{"token":7,"operation":{
        "service":"call_plugin", "name":PEER_NAME, "id":PEER_ID, "method":"echo",
        "argument":{"text":"peer 中文👋"}
    }}]})
    .to_string();
    let prefix = r#"{"service_value":"#;
    let escape = |value: &str| {
        value
            .bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    let event = r#"{"event":{"event":""#.len();
    let command = i32::from_le_bytes(*b"comm");
    let service = i32::from_le_bytes(*b"serv");
    let response = 1_048_576_u64;
    let data = response as usize + prefix.len();
    let overhead = prefix.len() + 1;
    wat::parse_str(format!(
        r#"(module
        (memory (export "memory") 32)
        (data (i32.const 16) "{{}}")
        (data (i32.const 128) "{}")
        (data (i32.const 4096) "{}")
        (global $length (mut i32) (i32.const 0))
        (func (export "nb_alloc") (param i32) (result i32) i32.const 8192)
        (func (export "nb_run") (param $input i32) (param $size i32) (result i64)
            (local $event i32)
            (local.set $event (i32.load offset={event} (local.get $input)))
            (if (i32.eq (local.get $event) (i32.const {command})) (then
                (global.set $length (i32.const 0))
                (return (i64.const {request_result}))))
            (if (i32.eq (local.get $event) (i32.const {service})) (then
                (memory.copy (i32.const {response}) (i32.const 4096) (i32.const {prefix_length}))
                (memory.copy (i32.const {data}) (local.get $input) (local.get $size))
                (i32.store8 (i32.add (i32.const {data}) (local.get $size)) (i32.const 125))
                (global.set $length (i32.add (local.get $size) (i32.const {overhead})))
                (return (i64.const {empty_result}))))
            (if (i32.eqz (global.get $length)) (then (return (i64.const {empty_result}))))
            (i64.or (i64.const {response_result}) (i64.extend_i32_u (global.get $length))))
        (func (export "nb_free") (param i32 i32)))"#,
        escape(&request),
        escape(prefix),
        request_result = (128_u64 << 32) | request.len() as u64,
        empty_result = (16_u64 << 32) | 2,
        response_result = response << 32,
        prefix_length = prefix.len(),
    ))
    .unwrap()
}

fn install_caller(session: &Session, path: &Path, name: &str, granted: bool) -> Identity {
    let permissions = if granted {
        "'read_document', 'collaborate'"
    } else {
        "'read_document'"
    };
    let manifest = format!(
        "name={name:?}\nid='peer-caller'\nversion='1.0.0'\npermissions=[{permissions}]\n\
         [engines]\nnanobug='^0.1.0'\n[[modules]]\nid='main'\nwasm='main.wasm'\n"
    );
    write_bundle(path, &manifest, &caller_wasm());
    install(session, path)
}

fn ask(session: &Session, caller: &Key) -> Value {
    call(session, caller).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (reply, response) = async_channel::bounded(1);
        session
            .send(Action::Invoke {
                key: caller.clone(),
                event: PluginEvent::Diagnose,
                reply: Some(reply),
                timeout: Duration::from_secs(2),
            })
            .unwrap();
        if let Some(callback) = answer(&response).unwrap().output.service_value {
            assert_eq!(callback["event"]["event"], "service_result");
            assert_eq!(callback["event"]["result"]["token"], 7);
            return callback["event"]["result"]["value"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "optional service callback was lost"
        );
    }
}

fn stop_owned_peer(pid: u32) {
    let pid = pid.to_string();
    #[cfg(windows)]
    let result = std::process::Command::new("taskkill")
        .args(["/PID", &pid, "/F"])
        .output();
    #[cfg(unix)]
    let result = std::process::Command::new("kill")
        .args(["-KILL", &pid])
        .output();
    assert!(result.unwrap().status.success());
}

#[test]
fn optional_peer_calls_respect_grants_scope_readiness_and_deadlines() {
    let fixture = tempfile::tempdir().unwrap();
    let session = session(&fixture.path().join("profile"), false);
    let first = project(fixture.path(), 1);
    let second = project(fixture.path(), 2);
    session.projects(Some(first.id), vec![first, second]);
    let caller = install_caller(&session, &fixture.path().join("caller.zip"), "Caller", true);
    let caller_first = key(&caller, 1);
    let caller_second = key(&caller, 2);
    let view = wait(&session, |view| {
        view.modules.get(&caller_first) == Some(&State::Ready)
            && view.modules.get(&caller_second) == Some(&State::Ready)
    });
    let caller_pid = view.workers[&caller.namespace()];
    assert_eq!(
        ask(&session, &caller_first),
        json!({"Err":"Optional plugin is not installed"})
    );
    assert_eq!(session.view().installed.len(), 1);

    let peer = Identity::new(PEER_NAME.into(), PEER_ID.into()).unwrap();
    let scope = ConfigScope::Project(1);
    let mut config = Config::default();
    config
        .plugin_mut(peer.clone())
        .rules
        .insert("main".into(), Rule::Never);
    session
        .send(Action::StageConfig(scope, config.clone()))
        .unwrap();
    session.send(Action::ApplyConfig(scope)).unwrap();
    wait(&session, |view| {
        view.configurations[&scope].applied == config
    });

    let archive = fixture.path().join("peer.zip");
    let manifest = format!(
        "name={PEER_NAME:?}\nid={PEER_ID:?}\nversion='1.0.0'\n\
         [engines]\nnanobug='^0.1.0'\n[public_services]\necho='main'\n\
         [[modules]]\nid='main'\nwasm='main.wasm'\n"
    );
    write_bundle(&archive, &manifest, &guest(json!({})));
    assert_eq!(install(&session, &archive), peer);
    let peer_first = key(&peer, 1);
    let peer_second = key(&peer, 2);
    let view = wait(&session, |view| {
        view.modules.get(&peer_first) == Some(&State::Unmatched)
            && view.modules.get(&peer_second) == Some(&State::Ready)
    });
    let peer_pid = view.workers[&peer.namespace()];
    assert_eq!(
        ask(&session, &caller_first),
        json!({"Err":"Optional plugin service is inactive in this project"})
    );
    assert_eq!(
        session.view().runtime_status[&peer.namespace()].ready_modules,
        1
    );

    let accepted = ask(&session, &caller_second);
    assert_eq!(
        accepted["Ok"]["event"],
        json!({"event":"service_call", "name":"echo", "argument":{"text":"peer 中文👋"}})
    );
    assert_eq!(accepted["Ok"]["document"], Value::Null);
    let denied = install_caller(
        &session,
        &fixture.path().join("denied.zip"),
        "Denied Caller",
        false,
    );
    let denied = key(&denied, 2);
    wait(&session, |view| {
        view.modules.get(&denied) == Some(&State::Ready)
    });
    assert_eq!(
        ask(&session, &denied),
        json!({"Err":"Plugin collaboration is not granted"})
    );

    // A zero-deadline service call uses the same real peer invocation and reply path.
    let (reply, response) = async_channel::bounded(1);
    session
        .send(Action::Invoke {
            key: peer_second.clone(),
            event: PluginEvent::ServiceCall {
                name: "echo".into(),
                argument: Value::Null,
            },
            reply: Some(reply),
            timeout: Duration::ZERO,
        })
        .unwrap();
    assert_eq!(
        answer(&response).unwrap_err(),
        "Optional plugin service timed out"
    );
    assert!(ask(&session, &caller_second).get("Ok").is_some());
    assert_eq!(session.view().workers[&peer.namespace()], peer_pid);

    stop_owned_peer(peer_pid);
    wait(&session, |view| {
        view.modules.get(&peer_second) == Some(&State::Recovering)
    });
    assert_eq!(
        ask(&session, &caller_second),
        json!({"Err":"Optional plugin service is not ready"})
    );
    wait(&session, |view| {
        view.modules.get(&peer_second) == Some(&State::Ready)
    });
    assert!(ask(&session, &caller_second).get("Ok").is_some());
    assert_eq!(session.view().workers[&caller.namespace()], caller_pid);

    session
        .send(Action::SetEnabled(peer.namespace(), false))
        .unwrap();
    wait(&session, |view| {
        view.modules.get(&peer_second) == Some(&State::Disabled)
            && !view.workers.contains_key(&peer.namespace())
    });
    assert_eq!(
        ask(&session, &caller_second),
        json!({"Err":"Optional plugin is disabled"})
    );
    assert!(!session.view().workers.contains_key(&peer.namespace()));
    assert_eq!(
        session.view().modules.get(&caller_second),
        Some(&State::Ready)
    );
    session.send(Action::Uninstall(peer.namespace())).unwrap();
    wait(&session, |view| {
        view.installed
            .iter()
            .all(|record| record.release.manifest.identity() != peer)
    });
    assert_eq!(
        ask(&session, &caller_second),
        json!({"Err":"Optional plugin is not installed"})
    );
    assert_eq!(session.view().workers[&caller.namespace()], caller_pid);
}
