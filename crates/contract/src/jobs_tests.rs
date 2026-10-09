//! The seam's closure types print without their closures, and its
//! `write` default reports every job as not running.

use super::{End, Foreground, Input, Jobs, Lines, OpenError, Opened, Opening, Stop, WriteError};

#[test]
fn stop_and_end_debug_without_their_closures() {
    let stop = Stop(Box::new(|| {}));
    assert_eq!(format!("{stop:?}"), "Stop(..)");
    let input = Input(Box::new(|bytes, _, _| Ok(bytes.len())));
    assert_eq!(format!("{input:?}"), "Input(..)");
    let lines = Lines(Box::new(|_| {}));
    assert_eq!(format!("{lines:?}"), "Lines(..)");
    let end = End(Box::new(|_| {}));
    assert_eq!(format!("{end:?}"), "End(..)");
    let call: std::sync::Arc<dyn Fn() -> bool + Send + Sync> = std::sync::Arc::new(|| true);
    let foreground = Foreground(std::sync::Arc::downgrade(&call));
    assert_eq!(format!("{foreground:?}"), "Foreground(..)");
}

/// Jobs without a terminal: every stub, and no `write` override.
struct NoWrite;

impl Jobs for NoWrite {
    fn open(&self, _opening: Opening) -> Result<Opened, OpenError> {
        Err(OpenError::Io {
            path: "unused".into(),
            source: std::io::Error::other("unused"),
        })
    }

    fn stop(&self, _job_id: &crate::JobId) -> bool {
        false
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _call: Foreground) {}

    fn running(&self) -> Vec<crate::JobId> {
        Vec::new()
    }

    fn stop_delegates(&self) -> usize {
        0
    }

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<crate::inbox::Delivery>) {}
}

#[test]
fn write_without_an_override_reports_not_running() {
    assert!(matches!(
        NoWrite.write(&crate::JobId("j_1".into()), "hi"),
        Err(WriteError::NotRunning)
    ));
}
