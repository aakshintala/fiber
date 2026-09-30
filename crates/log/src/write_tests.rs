use super::*;

#[test]
fn watchers_attached_and_dropped_while_idle_leave_nothing_behind() {
    let sessions = std::env::temp_dir().join(format!("log-unit-idle-{}", std::process::id()));
    fs::remove_dir_all(&sessions).unwrap_or(());
    let log = Log::create(&sessions, SessionId("s_1".into())).unwrap();
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
    fs::remove_dir_all(&sessions).unwrap_or(());
}
