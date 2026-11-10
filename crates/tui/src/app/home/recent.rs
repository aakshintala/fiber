//! Home's feed, recent pages and project counts (`docs/tui.md`, "The
//! session list"). It owns the request ids and cursor used to fold them.

use std::collections::BTreeSet;

use serde_json::Value;

use super::super::outbox::{Outbox, RecentAsk, Waiting};
use super::super::{App, Effect, Sent, hub_line};
use super::{refusal, refusal_code};
use crate::home::{Row, Sessions, recent_rows};
use contract::CommandId;
use contract::ErrorCode;
use contract::commands::hub::{HubCommand, NoArgs};

/// The home feed and recent-page requests waiting for their answers.
/// Each one lives only as its outbox `Feed` or `Recent` tag.
#[derive(Default)]
pub(super) struct Recent {
    /// `feed` and the first `recent` page went out.
    fed: bool,
}

/// Whether a scoped `recent` ask for `project` is out.
fn scoped_out(outbox: &Outbox, project: &str) -> bool {
    outbox.tags().any(|(_, waiting)| {
        matches!(waiting, Waiting::Recent(RecentAsk::Scoped(asked)) if asked == project)
    })
}

/// Whether an unscoped `recent` ask, first page or later, is out.
fn unscoped_out(outbox: &Outbox) -> bool {
    outbox
        .tags()
        .any(|(_, waiting)| matches!(waiting, Waiting::Recent(RecentAsk::First | RecentAsk::Page)))
}

/// Settles every unscoped `recent` tag.
fn settle_unscoped(outbox: &mut Outbox) {
    outbox.settle_where(|waiting| {
        matches!(waiting, Waiting::Recent(RecentAsk::First | RecentAsk::Page))
    });
}

/// A correlated answer the home screen consumes.
pub(super) enum Outcome {
    /// The feed answer, with a refusal notice when it was rejected.
    Feed(Option<Notice>),
    /// An unscoped recent answer and the projects on home before it folded.
    First {
        /// Whether the answer was for the first page.
        first: bool,
        /// Its rows and the projects previously on home, or a refusal.
        result: Result<(Vec<Row>, BTreeSet<String>), Notice>,
    },
    /// A scoped recent answer for one project.
    Scoped {
        /// The project whose rows and total this answer owns.
        project: String,
        /// Its rows and total, or a refusal or malformed answer.
        result: Result<(Vec<Row>, usize), Notice>,
    },
}

/// A refusal notice from a correlated recent answer.
pub(super) struct Notice {
    /// The stable refusal code.
    pub(super) code: ErrorCode,
    /// The refusal text shown to the person.
    pub(super) message: String,
}

impl Notice {
    fn from_payload(payload: &serde_json::Map<String, Value>) -> Self {
        Self {
            code: refusal_code(payload),
            message: refusal(payload),
        }
    }
}

impl Recent {
    /// Takes the one-time feed and first unscoped page, if not sent yet.
    /// The call after `fed` is cleared settles the old feed and unscoped
    /// recent tags.
    pub(super) fn first_asks(&mut self, outbox: &mut Outbox) -> Option<Vec<Sent>> {
        if self.fed {
            return None;
        }
        self.fed = true;
        outbox.settle_where(|waiting| matches!(waiting, Waiting::Feed));
        settle_unscoped(outbox);
        let feed = CommandId(support::mint("c_"));
        let recent = CommandId(support::mint("c_"));
        outbox.wait(&feed.0, Waiting::Feed);
        outbox.wait(&recent.0, Waiting::Recent(RecentAsk::First));
        Some(vec![
            Sent {
                line: hub_line(&feed, HubCommand::Feed(NoArgs {})),
                id: feed,
            },
            recent_line(&recent, None, None),
        ])
    }

    /// Folds one feed or recent answer by the tag its id carried, read
    /// before the tag settles. A tag that is neither the feed's nor a
    /// recent page's returns `None`.
    pub(super) fn answer(
        &mut self,
        tag: &Waiting,
        accepted: bool,
        payload: &serde_json::Map<String, Value>,
        sessions: &mut Sessions,
    ) -> Option<Outcome> {
        if matches!(tag, Waiting::Feed) {
            let notice = (!accepted).then(|| Notice::from_payload(payload));
            return Some(Outcome::Feed(notice));
        }
        let Waiting::Recent(ask) = tag else {
            return None;
        };
        let first = match ask {
            RecentAsk::Scoped(project) => {
                let result = if accepted {
                    scoped_result(payload).ok_or_else(|| Notice::from_payload(payload))
                } else {
                    Err(Notice::from_payload(payload))
                };
                if result.is_err() {
                    sessions.clear_dirty(project);
                }
                return Some(Outcome::Scoped {
                    project: project.clone(),
                    result,
                });
            }
            RecentAsk::First => true,
            RecentAsk::Page => false,
        };
        let result = if accepted {
            let (rows, _) = payload.get("result").map(recent_rows).unwrap_or_default();
            // Projects on home before this page folds:
            // only a project first appearing in the page
            // earns an ask. The delete path separately asks
            // its captured project directly, so a refill
            // cannot retry an already-seen refusal.
            let before = sessions
                .sections(None)
                .into_iter()
                .map(|section| section.project.to_owned())
                .collect();
            Ok((rows, before))
        } else {
            Err(Notice::from_payload(payload))
        };
        Some(Outcome::First { first, result })
    }

    /// Asks the first unscoped `recent` page again: an accepted delete
    /// fills the gap it left. It also asks the scoped `recent` for the
    /// deleted row's project, or marks it dirty while one is in flight.
    pub(super) fn ask_recent_first(
        &mut self,
        delete_project: Option<&str>,
        sessions: &mut Sessions,
        connected: bool,
        outbox: &mut Outbox,
    ) -> Vec<Sent> {
        if !connected {
            return Vec::new();
        }
        let id = CommandId(support::mint("c_"));
        settle_unscoped(outbox);
        outbox.wait(&id.0, Waiting::Recent(RecentAsk::First));
        let mut out = vec![recent_line(&id, None, None)];
        if let Some(project) = delete_project {
            out.extend(self.scoped_first(project, sessions, connected, outbox));
        }
        out
    }

    /// Sends one scoped `recent` first page for `project`, recording its
    /// id. While one is in flight for the project, marks it dirty instead
    /// of duplicating the ask: the answer sends exactly one follow-up.
    /// Nothing goes out with no home, with the link down, or for an
    /// empty project.
    pub(super) fn scoped_first(
        &mut self,
        project: &str,
        sessions: &mut Sessions,
        connected: bool,
        outbox: &mut Outbox,
    ) -> Vec<Sent> {
        if project.is_empty() || !connected {
            return Vec::new();
        }
        if scoped_out(outbox, project) {
            sessions.mark_dirty(project);
            return Vec::new();
        }
        let id = CommandId(support::mint("c_"));
        outbox.wait(
            &id.0,
            Waiting::Recent(RecentAsk::Scoped(project.to_owned())),
        );
        vec![recent_line(&id, Some(project), None)]
    }

    /// One scoped `recent` first page per project lacking a stored total
    /// and an in-flight scoped ask, skipping projects in `before`: every
    /// unscoped page passes the projects already on home and fans out only
    /// for a project first appearing there, so a refill never retries an
    /// already-seen project without a new trigger.
    fn fan_out_scoped(
        &mut self,
        before: &BTreeSet<String>,
        sessions: &mut Sessions,
        launch_project: Option<&str>,
        connected: bool,
        outbox: &mut Outbox,
    ) -> Vec<Sent> {
        if !connected {
            return Vec::new();
        }
        let projects: Vec<String> = sessions
            .sections(launch_project)
            .into_iter()
            .filter(|section| {
                !section.project.is_empty()
                    && !before.contains(section.project)
                    && sessions.total_for(section.project).is_none()
                    && !scoped_out(outbox, section.project)
            })
            .map(|section| section.project.to_owned())
            .collect();
        let mut out = Vec::new();
        for project in &projects {
            out.extend(self.scoped_first(project, sessions, connected, outbox));
        }
        out
    }

    /// Folds valid rows into the project's stored page and total, returning
    /// whether a retrigger needs a fresh page.
    pub(super) fn answer_scoped(
        &mut self,
        project: &str,
        rows: Vec<Row>,
        total: usize,
        sessions: &mut Sessions,
    ) -> bool {
        sessions.scoped(rows);
        sessions.note_total(project, total);
        sessions.is_dirty(project)
    }

    /// Clears a dirty marker and sends exactly one follow-up after a valid
    /// scoped answer.
    fn follow_up_scoped(
        &mut self,
        project: &str,
        sessions: &mut Sessions,
        connected: bool,
        outbox: &mut Outbox,
    ) -> Vec<Sent> {
        if !sessions.is_dirty(project) {
            return Vec::new();
        }
        sessions.clear_dirty(project);
        self.scoped_first(project, sessions, connected, outbox)
    }

    /// Asks the next scoped `recent` page for `project` past `before`,
    /// recording its id. While one is in flight for the project, sends
    /// nothing: paging is not a re-trigger, so it marks nothing dirty.
    /// Nothing goes out with no home, with the link down, or for an
    /// empty project.
    fn scoped_page(
        &mut self,
        project: &str,
        before: Option<&str>,
        connected: bool,
        outbox: &mut Outbox,
    ) -> Option<Sent> {
        if project.is_empty() || !connected {
            return None;
        }
        if scoped_out(outbox, project) {
            return None;
        }
        let id = CommandId(support::mint("c_"));
        outbox.wait(
            &id.0,
            Waiting::Recent(RecentAsk::Scoped(project.to_owned())),
        );
        Some(recent_line(&id, Some(project), before))
    }

    /// Asks the next `recent` page past the focused last row. When the
    /// focused row is its project's last loaded row and the held total
    /// says more, the page is scoped to that project and needs the link;
    /// else the unscoped path asks past the unscoped cursor with no link
    /// check. `None` unless the focused row ends the list and a page may
    /// exist, with nothing in flight.
    pub(super) fn page_recent(
        &mut self,
        focused: u64,
        sessions: &mut Sessions,
        connected: bool,
        outbox: &mut Outbox,
    ) -> Option<Sent> {
        // The scoped page wins while the project holds more: the held
        // total is that count's source of truth.
        let scoped = sessions.by_key(focused).and_then(|row| {
            let project = row.project.clone();
            let total = sessions.total_for(&project)?;
            let section = sessions
                .sections(None)
                .into_iter()
                .find(|section| section.project == project)?;
            let last = section.exited.last()?;
            (last.key == focused && total > section.exited.len())
                .then(|| (project, Some(last.id.0.clone())))
        });
        if let Some((project, before)) = scoped {
            return self.scoped_page(&project, before.as_deref(), connected, outbox);
        }
        if !sessions.more() || unscoped_out(outbox) {
            return None;
        }
        let before = sessions
            .last_recent()
            .filter(|last| last.key == focused)?
            .id
            .0
            .clone();
        let id = CommandId(support::mint("c_"));
        outbox.wait(&id.0, Waiting::Recent(RecentAsk::Page));
        Some(recent_line(&id, None, Some(before.as_str())))
    }

    /// Starts a new connection's feed cycle and queues one scoped first
    /// page per project already on home. The dropped scoped tags settle
    /// here, so a late answer matches nothing; the feed and unscoped
    /// tags stay.
    pub(super) fn reset_for_connection(
        &mut self,
        sessions: &Sessions,
        launch_project: Option<&str>,
        outbox: &mut Outbox,
    ) -> Vec<Sent> {
        self.fed = false;
        outbox.settle_where(|waiting| matches!(waiting, Waiting::Recent(RecentAsk::Scoped(_))));
        let projects: Vec<String> = sessions
            .sections(launch_project)
            .into_iter()
            .map(|section| section.project.to_owned())
            .collect();
        let mut out = Vec::new();
        for project in projects {
            if project.is_empty() {
                continue;
            }
            let id = CommandId(support::mint("c_"));
            outbox.wait(&id.0, Waiting::Recent(RecentAsk::Scoped(project.clone())));
            out.push(recent_line(&id, Some(&project), None));
        }
        out
    }
}

impl App {
    /// Sends one scoped `recent` first page for `project`, recording its
    /// id. While one is in flight for the project, marks it dirty instead
    /// of duplicating the ask: the answer sends exactly one follow-up.
    /// Nothing goes out with no home, with the link down, or for an
    /// empty project.
    pub(super) fn scoped_first(&mut self, project: &str) -> Vec<Sent> {
        let connected = self.outbox.is_up();
        let outbox = &mut self.outbox;
        let home = &mut self.home;
        home.recent
            .scoped_first(project, &mut home.sessions, connected, outbox)
    }

    /// Asks the next scoped page for a project, without marking an
    /// in-flight ask dirty.
    pub(super) fn scoped_page(&mut self, project: &str, before: Option<&str>) -> Vec<Sent> {
        let connected = self.outbox.is_up();
        let outbox = &mut self.outbox;
        let home = &mut self.home;
        home.recent
            .scoped_page(project, before, connected, outbox)
            .into_iter()
            .collect()
    }

    /// Asks the first unscoped `recent` page again: an accepted delete
    /// fills the gap it left. It also asks the scoped `recent` for the
    /// deleted row's project, or marks it dirty while one is in flight.
    pub(super) fn ask_recent_first(&mut self, delete_project: Option<&str>) -> Vec<Sent> {
        let connected = self.outbox.is_up();
        let outbox = &mut self.outbox;
        let home = &mut self.home;
        home.recent
            .ask_recent_first(delete_project, &mut home.sessions, connected, outbox)
    }

    /// Folds a scoped `recent` answer for `project`: rows append by
    /// project, the total keys by project, and a dirty project sends
    /// exactly one follow-up. A refusal or an unparseable accepted
    /// payload keeps the last held total with one notice, clears pending
    /// and dirty, and retries nothing.
    ///
    /// Also consumes feed and unscoped recent answers; unrelated command
    /// answers stay in `home_line`.
    pub(super) fn recent_answer(
        &mut self,
        id: &str,
        accepted: bool,
        payload: &serde_json::Map<String, Value>,
    ) -> Option<Vec<String>> {
        let connected = self.outbox.is_up();
        let tag = self.outbox.waiting(id)?;
        let answer = {
            let home = &mut self.home;
            home.recent
                .answer(&tag, accepted, payload, &mut home.sessions)?
        };
        self.outbox.settled(id);
        match answer {
            Outcome::Feed(notice) => {
                if let Some(notice) = notice {
                    self.notices.push(notice.code, notice.message);
                }
                Some(Vec::new())
            }
            Outcome::First { first, result } => match result {
                Ok((rows, before)) => {
                    // The unscoped answer's `total` is never
                    // stored: only a scoped answer sets a
                    // project's count.
                    self.session_labels(&rows);
                    self.home.sessions.recent(rows, first);
                    self.home.refill_picker();
                    self.list_answered(first, true);
                    // Every unscoped page fans out one scoped
                    // first page per newly seen project.
                    let home = &mut self.home;
                    let launch_project = home.launch_project().map(str::to_owned);
                    let outbox = &mut self.outbox;
                    let lines = home
                        .recent
                        .fan_out_scoped(
                            &before,
                            &mut home.sessions,
                            launch_project.as_deref(),
                            connected,
                            outbox,
                        )
                        .into_iter()
                        .map(|sent| outbox.fold_line(sent))
                        .collect();
                    Some(lines)
                }
                Err(notice) => {
                    self.notices.push(notice.code, notice.message);
                    self.list_answered(first, false);
                    Some(Vec::new())
                }
            },
            Outcome::Scoped { project, result } => match result {
                Ok((rows, total)) => {
                    self.session_labels(&rows);
                    let home = &mut self.home;
                    let dirty =
                        home.recent
                            .answer_scoped(&project, rows, total, &mut home.sessions);
                    home.refill_picker();
                    let outbox = &mut self.outbox;
                    let lines = if dirty {
                        home.recent
                            .follow_up_scoped(&project, &mut home.sessions, connected, outbox)
                            .into_iter()
                            .map(|sent| outbox.fold_line(sent))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Some(lines)
                }
                Err(notice) => {
                    self.notices.push(notice.code, notice.message);
                    Some(Vec::new())
                }
            },
        }
    }

    /// Asks the next `recent` page past the focused last row. When the
    /// focused row is its project's last loaded row and the held total
    /// says more, the page is scoped to that project and needs the link;
    /// else the unscoped path asks past the unscoped cursor with no link
    /// check. `None` unless the focused row ends the list and a page may
    /// exist, with nothing in flight.
    pub(super) fn page_recent(&mut self, focused: u64) -> Option<Effect> {
        let connected = self.outbox.is_up();
        let outbox = &mut self.outbox;
        let home = &mut self.home;
        let sent = home
            .recent
            .page_recent(focused, &mut home.sessions, connected, outbox)?;
        Some(Effect::SendTracked(vec![sent]))
    }
}

/// A scoped `recent` answer's rows and total: `None` when the payload
/// carries no readable command answer (a missing, null or foreign
/// `result`, a missing or foreign `sessions`, or a missing or foreign
/// `total`), following the `malformed` notice path. The hub contract
/// carries both (`docs/invocation.md`, "The hub" and `docs/events.md`,
/// "`command_accepted`").
fn scoped_result(payload: &serde_json::Map<String, Value>) -> Option<(Vec<Row>, usize)> {
    let result = payload.get("result")?;
    if !result.is_object() {
        return None;
    }
    result.get("sessions").and_then(Value::as_array)?;
    let (rows, total) = recent_rows(result);
    Some((rows, total?))
}

/// A `recent` line with its command id carried alongside: the first
/// page, or past `before`. `project` scopes the ask to one project,
/// `before` pages past a row; each rides only when set. An arg-less
/// ask writes an empty `args`, as the hub reads a missing `args` and
/// `{}` the same.
fn recent_line(id: &CommandId, project: Option<&str>, before: Option<&str>) -> Sent {
    Sent {
        line: hub_line(
            id,
            HubCommand::Recent(contract::commands::hub::Recent {
                before: before.map(str::to_owned),
                project: project.map(str::to_owned),
                archived: None,
            }),
        ),
        id: id.clone(),
    }
}
