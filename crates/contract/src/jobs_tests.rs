//! The seam's closure types print without their closures.

use super::{End, Foreground, Input, Lines, Stop};

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
