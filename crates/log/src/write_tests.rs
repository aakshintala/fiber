use super::*;

#[test]
fn watchers_attached_and_dropped_while_idle_leave_nothing_behind() {
    let sessions = fakes::TempDir::new("log-unit-idle");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let kept = log.watch();
    for _ in 0..1000 {
        drop(log.watch());
    }
    // The watcher still held, and the one dropped last, whose queue is
    // already freed: the registry does not grow.
    assert_eq!(log.lock().watchers.len(), 2);
    assert_eq!(
        log.lock()
            .watchers
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count(),
        1
    );
    drop(kept);
    drop(log.watch());
    assert_eq!(log.lock().watchers.len(), 1);
    drop(log);
}

#[test]
fn an_artifact_lands_in_the_session_artifacts_and_a_path_is_refused() {
    let sessions = fakes::TempDir::new("log-unit-artifact");
    let log = Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let (relative, path) = log.write_artifact("a_1.txt", b"full").unwrap();
    assert_eq!(relative, "artifacts/a_1.txt");
    assert_eq!(path, sessions.path().join("s_1/artifacts/a_1.txt"));
    assert_eq!(fs::read(&path).unwrap(), b"full");
    for name in ["../escape.txt", "sub/a.txt", "..", ""] {
        assert!(
            matches!(log.write_artifact(name, b"x"), Err(Error::Io { .. })),
            "{name}"
        );
    }
    assert!(!sessions.path().join("s_1/escape.txt").exists());
    drop(log);
}
