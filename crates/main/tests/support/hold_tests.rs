//! `Hold`'s drop: what it leaves behind for a holder that has not
//! opened the FIFO yet.

use super::*;

/// The drop unlinks the FIFO: a holder that has not opened it yet
/// fails its open with ENOENT and exits instead of parking on the
/// FIFO forever past the test. The reap of a parked holder stays
/// pinned by `hold_drop_releases_and_reaps_the_holder`.
#[test]
fn drop_unlinks_the_fifo_so_late_starters_fail_open() {
    let setup = Setup::new();
    let command = format!("head -n 1 {}", setup.workspace().join("hold").display());
    let hold = Hold::park(&setup, &command);
    let fifo = hold.path().to_path_buf();
    drop(hold);
    assert!(
        !fifo.exists(),
        "the drop unlinks the FIFO so a late holder fails open"
    );
}
