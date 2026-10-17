use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn spawn_failure_runs_no_preparation_and_names_the_reload_thread() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let build: BuildReload = Arc::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Err(contract::inbox::Rejection {
            code: contract::ErrorCode::ConfigInvalid,
            message: "must not run".into(),
        })
    });
    let prepare = preparer(
        build,
        Arc::new(|_| {
            Err(doors::failure(
                contract::ErrorCode::IoFailed,
                "a thread for the credential read: injected spawn failure",
            ))
        }),
    );
    let error = match prepare(&r#loop::ReloadRoute {
        model: "p/m".into(),
        credential: None,
    }) {
        Err(error) => error,
        Ok(_) => panic!("spawn failure accepted"),
    };
    assert_eq!(error.code, contract::ErrorCode::IoFailed);
    assert_eq!(
        error.message,
        "a thread for the reload: injected spawn failure"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn config_rejection_passes_through_without_installing() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let build: BuildReload = Arc::new(move |route| {
        assert_eq!(route.model, "p/current");
        assert_eq!(route.credential.as_deref(), Some("work"));
        count.fetch_add(1, Ordering::SeqCst);
        Err(contract::inbox::Rejection {
            code: contract::ErrorCode::ConfigInvalid,
            message: "config.json: invalid JSON".into(),
        })
    });
    let prepare = preparer(build, Arc::new(|work| Ok(work())));
    let error = match prepare(&r#loop::ReloadRoute {
        model: "p/current".into(),
        credential: Some("work".into()),
    }) {
        Err(error) => error,
        Ok(_) => panic!("invalid configuration accepted"),
    };
    assert_eq!(error.code, contract::ErrorCode::ConfigInvalid);
    assert_eq!(error.message, "config.json: invalid JSON");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
