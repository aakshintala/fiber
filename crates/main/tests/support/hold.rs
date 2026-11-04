//! The test workspace's `Hold`: a thin wrapper over [`fakes::Hold`]
//! that only fixes the FIFO path, so the callers need no edit.

use std::fs;
use std::path::Path;

use super::{Deadline, Setup};

/// A parked holder on the test workspace's `hold` FIFO. `fakes::Hold`
/// does the work; this wrapper only fixes the path to
/// `<workspace>/hold`. See `fakes::Hold` for the contract.
pub(crate) struct Hold(fakes::Hold);

impl Hold {
    /// Parks a holder for `command` on `<workspace>/hold`. See
    /// `fakes::Hold::park`.
    pub(crate) fn park(setup: &Setup, command: &str) -> Self {
        Self(fakes::Hold::park(&setup.workspace().join("hold"), command))
    }

    /// The FIFO's path. See `fakes::Hold::path`.
    pub(crate) fn path(&self) -> &Path {
        self.0.path()
    }

    /// The release handle. See `fakes::Hold::release`.
    pub(crate) fn release(&mut self) -> &mut fs::File {
        self.0.release()
    }

    /// The holder already exited on the success path. See
    /// `fakes::Hold::stand_down`.
    pub(crate) fn stand_down(self, deadline: Deadline) {
        self.0.stand_down(deadline);
    }
}
