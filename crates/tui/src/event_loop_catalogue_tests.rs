//! Forced interleavings between startup catalogue reads and picker refreshes.

use super::Input;
use crate::catalogue::{Catalogue, ModelEntry, ReadModels, Reader, Refresh};
use crate::event_loop::tests::{DEADLINE, new_loop};
use fakes::Deadline;
use ratatui::backend::TestBackend;
use std::sync::{Arc, Mutex, mpsc};

fn catalogue(reference: &str) -> Catalogue {
    let (provider, id) = reference.split_once('/').expect("model reference");
    Catalogue {
        models: vec![ModelEntry {
            reference: reference.to_owned(),
            provider: provider.to_owned(),
            id: id.to_owned(),
            name: None,
            levels: Vec::new(),
            default_level: None,
            configured: None,
            roles: Vec::new(),
            price: None,
        }],
        ..Default::default()
    }
}

fn screen_text(backend: &TestBackend) -> String {
    backend
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("")
}

/// Force Cached to run before Stale is released and apply each answer via
/// `Input::Models`, matching the startup/picker overlap in the binary.
#[test]
fn startup_catalogue_cached_then_queued_stale_are_applied() {
    let (mut lp, _) = new_loop(TestBackend::new(80, 24), None);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (returned_tx, returned_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_in = Arc::clone(&calls);
    let entered = entered_tx.clone();
    let returned = returned_tx.clone();
    let held = Arc::clone(&release_rx);
    let read: ReadModels = Arc::new(move |refresh| {
        calls_in.lock().expect("calls lock").push(refresh);
        entered.send(refresh).expect("test observes read entry");
        held.lock()
            .expect("release lock")
            .recv()
            .expect("test releases read");
        returned.send(refresh).expect("test observes read return");
        Ok(match refresh {
            Refresh::Cached => catalogue("alpha/one"),
            Refresh::Stale => catalogue("beta/two"),
            Refresh::Every => unreachable!("this test asks only Cached and Stale"),
        })
    });
    lp.model_reader = Reader::new(Some(read));
    let (files_tx, files_rx) = mpsc::channel();
    lp.files_out = files_tx.clone();
    let (_, idle) = mpsc::channel();

    // The initial frame precedes startup's cached read in run_with.
    lp.frame(&idle);
    lp.app.note_catalogue_read();
    lp.model_reader.ask(Refresh::Cached, &files_tx);
    assert_eq!(
        Deadline::after(DEADLINE).recv(&entered_rx),
        Ok(Refresh::Cached),
        "read-return checkpoint: startup Cached entered and is held"
    );

    assert_eq!(lp.step(Input::Bytes(vec![0x0c]), &idle), None);
    assert!(lp.app.model_picker_open(), "Ctrl+L opens the picker");
    assert_eq!(
        lp.app.take_reads(),
        None,
        "the loop took its owed Stale ask"
    );
    assert_eq!(
        lp.model_reader.queued_for_test(),
        Some(Refresh::Stale),
        "queued-restart checkpoint: Stale was taken into the reader's queue"
    );
    assert_eq!(
        *calls.lock().expect("calls lock"),
        [Refresh::Cached],
        "no second read entered while Cached is held"
    );

    release_tx.send(()).expect("release Cached");
    assert_eq!(
        Deadline::after(DEADLINE).recv(&returned_rx),
        Ok(Refresh::Cached),
        "read-return checkpoint: Cached returned"
    );
    let cached = Deadline::after(DEADLINE)
        .recv(&files_rx)
        .expect("worker-send checkpoint: Cached Input::Models sent");
    assert!(
        matches!(&cached, Input::Models(Ok(result)) if result.models[0].reference == "alpha/one")
    );
    assert_eq!(lp.step(cached, &idle), None);
    assert_eq!(
        lp.app.model_picker_view().map(|view| {
            view.sections
                .iter()
                .map(|section| section.models.len())
                .sum::<usize>()
        }),
        Some(1),
        "delivery/application checkpoint: Cached catalogue was stored"
    );
    let cached_screen = screen_text(lp.screen.backend());
    assert!(
        cached_screen.contains("alpha") && cached_screen.contains("one"),
        "the screen shows the cached provider and model: {cached_screen:?}"
    );

    assert_eq!(
        Deadline::after(DEADLINE).recv(&entered_rx),
        Ok(Refresh::Stale),
        "queued-restart checkpoint: Stale entered after Cached was applied"
    );
    assert_eq!(
        *calls.lock().expect("calls lock"),
        [Refresh::Cached, Refresh::Stale]
    );
    release_tx.send(()).expect("release Stale");
    assert_eq!(
        Deadline::after(DEADLINE).recv(&returned_rx),
        Ok(Refresh::Stale),
        "read-return checkpoint: Stale returned"
    );
    let stale = Deadline::after(DEADLINE)
        .recv(&files_rx)
        .expect("worker-send checkpoint: Stale Input::Models sent");
    assert!(
        matches!(&stale, Input::Models(Ok(result)) if result.models[0].reference == "beta/two")
    );
    assert_eq!(lp.step(stale, &idle), None);
    assert_eq!(
        lp.app.model_picker_view().map(|view| {
            view.sections
                .iter()
                .flat_map(|section| section.models.iter())
                .map(|model| model.id.clone())
                .collect::<Vec<_>>()
        }),
        Some(vec!["two".to_owned()]),
        "delivery/application checkpoint: Stale catalogue replaced Cached"
    );
    let stale_view = lp.app.model_picker_view().expect("picker remains open");
    assert!(
        !stale_view
            .status
            .iter()
            .any(|line| line == "Reading the model lists…")
    );
    let stale_screen = screen_text(lp.screen.backend());
    assert!(
        stale_screen.contains("beta") && stale_screen.contains("two"),
        "the screen shows the refreshed provider and model: {stale_screen:?}"
    );
}

/// The non-overlap order remains a control: Cached can land before the picker
/// asks for Stale, and the subsequent refresh still runs through the loop.
#[test]
fn startup_catalogue_cached_applied_before_picker_starts_stale() {
    let (mut lp, _) = new_loop(TestBackend::new(80, 24), None);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_in = Arc::clone(&seen);
    lp.model_reader = Reader::new(Some(Arc::new(move |refresh| {
        seen_in.lock().expect("seen lock").push(refresh);
        Ok(match refresh {
            Refresh::Cached => catalogue("alpha/one"),
            Refresh::Stale => catalogue("beta/two"),
            Refresh::Every => unreachable!("this test asks Cached and Stale"),
        })
    })));
    let (files_tx, files_rx) = mpsc::channel();
    lp.files_out = files_tx.clone();
    let (_, idle) = mpsc::channel();
    lp.frame(&idle);
    lp.app.note_catalogue_read();
    lp.model_reader.ask(Refresh::Cached, &files_tx);
    let cached = Deadline::after(DEADLINE)
        .recv(&files_rx)
        .expect("Cached Input::Models arrives");
    assert_eq!(lp.step(cached, &idle), None);
    assert!(lp.app.model_picker_view().is_none());
    assert_eq!(lp.step(Input::Bytes(vec![0x0c]), &idle), None);
    let stale = Deadline::after(DEADLINE)
        .recv(&files_rx)
        .expect("Stale Input::Models arrives");
    assert_eq!(lp.step(stale, &idle), None);
    assert_eq!(
        *seen.lock().expect("seen lock"),
        [Refresh::Cached, Refresh::Stale]
    );
    assert_eq!(
        lp.app.model_picker_view().map(|view| {
            view.sections
                .iter()
                .map(|section| section.models.len())
                .sum::<usize>()
        }),
        Some(1)
    );
    drop(lp);
}
