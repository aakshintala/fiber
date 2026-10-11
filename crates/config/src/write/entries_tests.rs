//! Tests for [`update_global_entries`]: one write per call.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::super::atomic::before_rename;
use super::update_global_entries;

#[test]
fn two_entries_in_one_call_write_once() {
    let root = fakes::TempDir::new("fiber-key-entries");
    let home = root.path().to_path_buf();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    before_rename(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    update_global_entries(
        &home,
        "keys",
        &[
            ("new_session".to_owned(), Some(json!(["ctrl+t"]))),
            ("copy_focused".to_owned(), Some(json!(["c"]))),
        ],
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
