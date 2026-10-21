//! The accepted-import report overlay (`docs/tui.md`, "Notices",
//! "Overlays"): one scrollable overlay per accepted import, no notices.
//!
//! The report names its source in the title, groups what the import
//! copied by kind in hub order, and lists what it left behind in hub
//! order with each reason dim. The frame shows a window of the body
//! rows at the scrolled-to offset, so the keys and the wheel clamp to
//! what the draw shows.

#[cfg(test)]
#[path = "import_report_tests.rs"]
mod tests;
