//! The model picker on the app: the catalogue it lists, and the reads it
//! owes the loop (`docs/tui.md`, "Swapped views"). Opening, keys and
//! choosing are Part 2's; this is the read half, so no read is dead code.

use super::App;
use crate::catalogue::{Catalogue, Refresh};

impl App {
    /// Folds a model-list read's answer: each catalogue notice shows once,
    /// and a read error shows once with the old catalogue kept.
    pub(crate) fn on_models(&mut self, result: Result<Catalogue, String>) {
        match &result {
            Ok(catalogue) => {
                for notice in &catalogue.notices {
                    self.push_notice(notice.clone());
                }
            }
            Err(error) => {
                self.push_notice(error.clone());
            }
        }
        self.model_picker.store(result);
    }

    /// The model-list read the loop owes, if one is owed.
    pub(crate) fn take_reads(&mut self) -> Option<Refresh> {
        self.model_picker.take_read()
    }
}

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod tests;
