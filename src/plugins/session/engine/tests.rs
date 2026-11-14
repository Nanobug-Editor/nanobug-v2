use super::*;

#[test]
fn simultaneous_refreshes_keep_each_unsent_change_pending() {
    let prior = (
        Some((1, 1)),
        "previous".into(),
        BTreeMap::new(),
        None,
        [0; 3],
    );
    let current = (
        Some((1, 2)),
        "new draft".into(),
        BTreeMap::from([("flag".into(), true.into())]),
        Some(2),
        [1, 0, 0],
    );
    let observed = refresh_observed(Some(&prior), current.clone(), &PluginEvent::SettingsChanged);
    assert_eq!(
        (&observed.0, &observed.1, &observed.3),
        (&prior.0, &prior.1, &prior.3)
    );
    assert_eq!(observed.2, current.2);
    let observed = refresh_observed(Some(&observed), current.clone(), &PluginEvent::DraftChanged);
    assert_eq!(observed.1, current.1);
    assert_eq!((observed.0, observed.3), (prior.0, prior.3));
}

#[test]
fn runtime_uses_all_three_restarts_and_pause_is_sticky() {
    let mut backoff = Backoff::default();
    let mut now = Instant::now();
    for delay in [1, 3, 10] {
        backoff.failed(now);
        assert!(!backoff.paused());
        assert!(!backoff.ready(now + Duration::from_millis(delay * 1000 - 1)));
        now += Duration::from_secs(delay);
        assert!(backoff.ready(now));
    }
    backoff.failed(now);
    assert!(backoff.paused());
    assert!(!backoff.ready(now + Duration::from_secs(301)));
    assert!(Backoff::default().ready(now + Duration::from_secs(301)));
}

#[test]
#[ignore = "requires the real parser executable next to the test build"]
fn editor_document_remains_writable_during_parser_recovery() -> anyhow::Result<()> {
    use crate::document::Document;
    use nanobug_plugin_sdk::DocumentSnapshot;

    let project = tempfile::tempdir()?;
    let path = project.path().join("data.json");
    std::fs::write(&path, "{\"value\": 1}\r\n")?;
    let mut document = Document::open(&path)?;
    let snapshot = |doc: &Document| DocumentSnapshot {
        opening: doc.opening,
        revision: doc.revision,
        path: doc.path.clone(),
        text: doc.text.clone(),
    };
    let mut parser = Syntax::default();
    let first = parser.parse(1, "json".into(), snapshot(&document), None)?;
    assert_eq!(first["has_error"], false);
    let first_pid = parser.worker.as_ref().unwrap().pid();
    parser.worker.as_mut().unwrap().kill_for_test()?;
    let error = parser
        .parse(2, "json".into(), snapshot(&document), None)
        .unwrap_err();
    assert_eq!(error.to_string(), "Syntax worker stopped");

    document.replace(10..11, "2", true);
    document.save(None)?;
    assert_eq!(std::fs::read_to_string(&path)?, "{\"value\": 2}\r\n");
    assert!(parser.worker.is_none());
    std::thread::sleep(Duration::from_millis(1_050));
    let recovered = parser.parse(3, "json".into(), snapshot(&document), None)?;
    assert_eq!(recovered["revision"], document.revision);
    assert_eq!(recovered["cached"], false);
    assert_ne!(parser.worker.as_ref().unwrap().pid(), first_pid);
    Ok(())
}
