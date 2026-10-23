//! The hub's build against this terminal's: one notice per connection when
//! they differ, and the reason an unreadable row gives (`docs/tui.md`,
//! "Notices" and "The session list").

use serde_json::{Map, Value};

use super::{App, hub_string};

/// A build as `fiber --version` writes it after "fiber ": the version, then
/// the first 7 characters of a nonempty commit in parentheses. Control
/// characters a hub put in either are dropped, so they never reach a row.
pub(crate) fn build_text(version: &str, commit: Option<&str>) -> String {
    let plain = |text: &str| text.chars().filter(|c| !c.is_control()).collect::<String>();
    let commit = commit.map(plain).filter(|commit| !commit.is_empty());
    match commit {
        Some(commit) => format!(
            "{} ({})",
            plain(version),
            commit.chars().take(7).collect::<String>()
        ),
        None => plain(version),
    }
}

/// Whether the hub's build is another than ours: the versions differ, or
/// both sides carry a commit and the commits differ. An empty commit counts
/// as none, and a build with no commit compares the version only.
fn differs(ours: (&str, Option<&str>), hub: (&str, Option<&str>)) -> bool {
    fn present(commit: Option<&str>) -> Option<&str> {
        commit.filter(|commit| !commit.is_empty())
    }
    ours.0 != hub.0
        || matches!((present(ours.1), present(hub.1)), (Some(ours), Some(hub)) if ours != hub)
}

/// The notice naming both builds. The same text shows for a remote
/// (listed) hub: ruling 4 on #2964 gives no remote form, so none is
/// invented (`docs/tui.md`, "Notices").
fn notice_text(hub: &str, ours: &str) -> String {
    format!(
        "The hub runs Fiber {hub} (this terminal: {ours}). `fiber stop` restarts it on the next launch."
    )
}

impl App {
    /// Reads a `hub_hello`'s build: records the hub's when it differs from
    /// ours and pushes the notice. Every `hub_hello` resets the record, so a
    /// reconnect to a rebuilt hub drops the difference. A hello with no
    /// version, or a terminal with no home, compares nothing.
    pub(super) fn hub_hello_version(&mut self, payload: &Map<String, Value>) {
        let version = hub_string(payload, "fiber_version").filter(|version| !version.is_empty());
        let commit = hub_string(payload, "fiber_commit");
        let Some(home) = self.home.as_mut() else {
            return;
        };
        let launch = &home.launch;
        let found = version.as_deref().filter(|version| {
            differs(
                (&launch.version, launch.commit.as_deref()),
                (version, commit.as_deref()),
            )
        });
        let Some(version) = found else {
            home.hub_other = None;
            return;
        };
        let hub = build_text(version, commit.as_deref());
        let text = notice_text(&hub, &build_text(&launch.version, launch.commit.as_deref()));
        home.hub_other = Some(hub);
        self.push_notice("hub_version", text);
    }

    /// Why an unreadable row cannot attach, with no final period.
    pub(in crate::app) fn unreadable_reason(&self) -> String {
        match self
            .home
            .as_ref()
            .and_then(|home| home.hub_other.as_deref())
        {
            Some(hub) => format!("Cannot attach: this session runs Fiber {hub}"),
            None => "Cannot attach: its status did not parse".to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "hub_version_tests.rs"]
mod tests;
