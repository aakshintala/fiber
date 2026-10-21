//! The conversation's click targets, a code block's `copy` cells, which
//! copy its code and show "Copied" until the next key or click, and
//! `/copy`, which copies the newest reply the same way (`docs/tui.md`,
//! "Look", "Selection and copy").

use std::ops::Range;

use super::{App, Effect, Target};
use crate::markdown::CopyTarget;
use crate::turn::{Entry, Turn};

/// The code block `target` names among `turns`, rendered at the card's
/// inner width with its copy cells shifted past the card's padding, so
/// the hit lands on the drawn label (`docs/tui.md`, "Turns").
pub(crate) fn copy_target(turns: &[Turn], target: Target, width: u16) -> Option<CopyTarget> {
    let Target::Copy { reply: id, block } = target else {
        return None;
    };
    let inner = crate::surface::inset(width);
    let pad = width.saturating_sub(inner) / 2;
    turns
        .iter()
        .flat_map(|turn| &turn.entries)
        .find_map(|entry| match entry {
            Entry::Reply { reply, .. } if reply.id() == id => {
                reply.rendered(inner).target(block).map(|mut target| {
                    target.cols.start = target.cols.start.saturating_add(pad);
                    target.cols.end = target.cols.end.saturating_add(pad);
                    target
                })
            }
            Entry::Reply { .. }
            | Entry::Steer(_)
            | Entry::Group(_)
            | Entry::Aside(_)
            | Entry::Band(_)
            | Entry::Answers(_) => None,
        })
}

/// What `/copy` finds walking the pages from the newest.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LastReply {
    /// The newest message's text parts, joined by a blank line.
    Text(String),
    /// A dropped page came before any reply: the newest may be on it.
    PagedOut,
    /// No page holds a reply.
    NoReply,
}

/// The newest reply in `pages`, each page's turns from the newest page
/// back, `None` for a dropped one: every text part of the latest message
/// that has text, as the model wrote it.
pub(crate) fn last_reply<'a>(pages: impl Iterator<Item = Option<&'a [Turn]>>) -> LastReply {
    for turns in pages {
        let Some(turns) = turns else {
            return LastReply::PagedOut;
        };
        for turn in turns.iter().rev() {
            let Some(newest) = turn.entries.iter().rev().find_map(reply_action) else {
                continue;
            };
            let parts: Vec<&str> = turn
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Reply { action, reply } if action == newest => Some(reply.text()),
                    Entry::Reply { .. }
                    | Entry::Steer(_)
                    | Entry::Group(_)
                    | Entry::Aside(_)
                    | Entry::Band(_)
                    | Entry::Answers(_) => None,
                })
                .collect();
            return LastReply::Text(parts.join("\n\n"));
        }
    }
    LastReply::NoReply
}

/// The message a reply entry belongs to.
fn reply_action(entry: &Entry) -> Option<&str> {
    match entry {
        Entry::Reply { action, .. } => Some(action),
        Entry::Steer(_)
        | Entry::Group(_)
        | Entry::Aside(_)
        | Entry::Band(_)
        | Entry::Answers(_) => None,
    }
}

impl App {
    /// `/copy`: the newest reply in the conversation on screen goes out
    /// through the copy path and "Copied" shows; otherwise a notice says
    /// why nothing was copied. While an opening pass runs, the screen
    /// stays as it was, so the copy waits for the frame after the pass
    /// ends ([`Self::take_owed_copy`]).
    pub(super) fn copy_last_reply(&mut self) -> Effect {
        self.draft.clear();
        // A job's view, or a delegate's with no transcript, leaves the
        // parent's conversation behind it, hidden.
        if self.item_open() && !self.item_transcript() {
            self.notices
                .push("copy", "No reply to copy in this view.".to_owned());
            return Effect::None;
        }
        if self.pass_open() {
            self.copy_owed = true;
            return Effect::None;
        }
        self.copy_now().map_or(Effect::None, Effect::Copy)
    }

    /// A `/copy` that waited for an opening pass, once no pass runs:
    /// the text the frame copies. `None` while nothing waits, while the
    /// pass goes on, or when the notice says why nothing was copied.
    pub(crate) fn take_owed_copy(&mut self) -> Option<String> {
        if !self.copy_owed || self.pass_open() {
            return None;
        }
        self.copy_owed = false;
        self.copy_now()
    }

    /// The newest reply on screen, with "Copied" showing; `None` after
    /// the notice that says why there is none.
    fn copy_now(&mut self) -> Option<String> {
        match last_reply(self.screen.pages().newest_first()) {
            LastReply::Text(text) => {
                self.copied = true;
                Some(text)
            }
            LastReply::PagedOut => {
                self.notices.push(
                    "copy",
                    "The last reply is paged out; jump to the end, then /copy again.".to_owned(),
                );
                None
            }
            LastReply::NoReply => {
                self.notices
                    .push("copy", "No reply to copy yet.".to_owned());
                None
            }
        }
    }

    /// For each line of [`Self::lines`] that opens something, its index and
    /// what it opens.
    #[cfg(test)]
    pub(crate) fn targets(&self) -> Vec<(usize, Target)> {
        self.screen
            .pages()
            .rows()
            .into_iter()
            .enumerate()
            .filter_map(|(at, (_, target))| target.map(|target| (at, target)))
            .collect()
    }

    /// The columns of the `copy` cells `target` covers on its line, when
    /// it is a code block's copy target.
    pub(crate) fn copy_cells(&self, target: Target) -> Option<Range<u16>> {
        self.screen
            .pages()
            .copy_target(target)
            .map(|copy| copy.cols)
    }

    /// A click on a code block's `copy`: copies its code and shows
    /// "Copied".
    pub(super) fn copy(&mut self, target: Target) -> Effect {
        let code = self.screen.pages().copy_target(target);
        self.copied = code.is_some();
        code.map_or(Effect::None, |copy| Effect::Copy(copy.code))
    }

    /// Hides "Copied": a left press, on a target or not, starts a click.
    pub(crate) fn clear_copied(&mut self) {
        self.copied = false;
    }

    /// Whether "Copied" shows.
    pub(crate) fn copied(&self) -> bool {
        self.copied
    }
}

#[cfg(test)]
#[path = "copy_tests.rs"]
mod tests;
