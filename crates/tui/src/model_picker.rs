//! The model picker's state (`docs/tui.md`, "Swapped views"): the
//! installed models, the scope they show under, and the read the loop
//! owes. Each open starts fresh; choosing and saving is Part 2.

use crate::catalogue::{Catalogue, Refresh};

/// The model picker: what it lists, and what it waits on.
#[derive(Default)]
pub(crate) struct ModelPicker {
    /// The installed models, from the latest read that answered.
    pub(crate) catalogue: Catalogue,
    /// Why the lists could not be read, when no catalogue is held yet.
    pub(crate) error: Option<String>,
    /// `scoped_models`: the references the picker shows; empty means
    /// every installed model (`docs/configuration.md`, "Keys").
    pub(crate) scoped: Vec<String>,
    /// The read the loop owes: `Stale` each time the picker opens,
    /// `Every` from its refresh button.
    pub(crate) want: Option<Refresh>,
}

impl ModelPicker {
    /// Stores a read's answer: the catalogue, or the error with the old
    /// catalogue kept. What the loop owes is taken, never cleared here:
    /// an answer to the startup read must not drop a meanwhile opened
    /// picker's `Stale`.
    pub(crate) fn store(&mut self, result: Result<Catalogue, String>) {
        match result {
            Ok(catalogue) => {
                self.catalogue = catalogue;
                self.error = None;
            }
            Err(error) => {
                self.error = Some(error);
            }
        }
    }

    /// The read the loop owes, if one is owed.
    pub(crate) fn take_read(&mut self) -> Option<Refresh> {
        self.want.take()
    }
}
