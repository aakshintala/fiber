//! Home's feed, recent pages and project counts (`docs/tui.md`, "The
//! session list"). It owns the request ids and cursor used to fold them.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::super::{App, Effect, mint};
use super::{refusal, refusal_code};
use crate::home::{Row, Sessions, recent_rows};

/// The home feed and recent-page requests waiting for their answers.
#[derive(Default)]
pub(super) struct Recent {
    /// `feed` and the first `recent` page went out.
    fed: bool,
    /// The `feed` command waiting for its answer.
    feed_id: Option<String>,
    /// The latest `recent` command waiting for its answer, and whether it
    /// asked the first page.
    recent_ask: Option<(String, bool)>,
    /// The scoped `recent` commands waiting for their answers, command
    /// id to project: at most one entry per project.
    scoped_asks: BTreeMap<String, String>,
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
    pub(super) code: String,
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
    pub(super) fn first_asks(&mut self) -> Option<Vec<String>> {
        if self.fed {
            return None;
        }
        self.fed = true;
        let feed = mint();
        let recent = mint();
        self.feed_id = Some(feed.clone());
        self.recent_ask = Some((recent.clone(), true));
        Some(vec![
            json!({"id": feed, "command": "feed"}).to_string(),
            recent_line(&recent, None, None),
        ])
    }

    /// Correlates one feed or recent answer, clearing the matching request
    /// before returning its fold instructions. Stale ids return `None`.
    pub(super) fn answer(
        &mut self,
        id: &str,
        accepted: bool,
        payload: &serde_json::Map<String, Value>,
        sessions: &mut Sessions,
    ) -> Option<Outcome> {
        if self.feed_id.as_deref() == Some(id) {
            self.feed_id = None;
            let notice = (!accepted).then(|| Notice::from_payload(payload));
            return Some(Outcome::Feed(notice));
        }

        if let Some((_, first)) = self.recent_ask.as_ref().filter(|(asked, _)| asked == id) {
            let first = *first;
            self.recent_ask = None;
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
            return Some(Outcome::First { first, result });
        }

        // A scoped answer folds only while its id is still
        // pending: a stale answer after a reconnect drops
        // untouched.
        let project = self.scoped_asks.remove(id)?;
        let result = if accepted {
            scoped_result(payload).ok_or_else(|| Notice::from_payload(payload))
        } else {
            Err(Notice::from_payload(payload))
        };
        if result.is_err() {
            sessions.clear_dirty(&project);
        }
        Some(Outcome::Scoped { project, result })
    }

    /// Asks the first unscoped `recent` page again: an accepted delete
    /// fills the gap it left. It also asks the scoped `recent` for the
    /// deleted row's project, or marks it dirty while one is in flight.
    pub(super) fn ask_recent_first(
        &mut self,
        delete_project: Option<&str>,
        sessions: &mut Sessions,
        connected: bool,
    ) -> Vec<String> {
        if !connected {
            return Vec::new();
        }
        let id = mint();
        self.recent_ask = Some((id.clone(), true));
        let mut out = vec![recent_line(&id, None, None)];
        if let Some(project) = delete_project {
            out.extend(self.scoped_first(project, sessions, connected));
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
    ) -> Vec<String> {
        if project.is_empty() || !connected {
            return Vec::new();
        }
        if self.scoped_asks.values().any(|pending| pending == project) {
            sessions.mark_dirty(project);
            return Vec::new();
        }
        let id = mint();
        self.scoped_asks.insert(id.clone(), project.to_owned());
        vec![recent_line(&id, Some(project), None)]
    }

    /// One scoped `recent` first page per project lacking a stored total
    /// and an in-flight scoped ask, skipping projects in `before`: every
    /// unscoped page passes the projects already on home and fans out only
    /// for a project first appearing there, so a refill never retries an
    /// already-seen project without a new trigger.
    pub(super) fn fan_out_scoped(
        &mut self,
        before: &BTreeSet<String>,
        sessions: &mut Sessions,
        launch_project: Option<&str>,
        connected: bool,
    ) -> Vec<String> {
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
                    && !self
                        .scoped_asks
                        .values()
                        .any(|pending| pending == section.project)
            })
            .map(|section| section.project.to_owned())
            .collect();
        let mut out = Vec::new();
        for project in &projects {
            out.extend(self.scoped_first(project, sessions, connected));
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
    pub(super) fn follow_up_scoped(
        &mut self,
        project: &str,
        sessions: &mut Sessions,
        connected: bool,
    ) -> Vec<String> {
        if !sessions.is_dirty(project) {
            return Vec::new();
        }
        sessions.clear_dirty(project);
        self.scoped_first(project, sessions, connected)
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
    ) -> Option<String> {
        if project.is_empty() || !connected {
            return None;
        }
        if self.scoped_asks.values().any(|pending| pending == project) {
            return None;
        }
        let id = mint();
        self.scoped_asks.insert(id.clone(), project.to_owned());
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
    ) -> Option<String> {
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
            return self.scoped_page(&project, before.as_deref(), connected);
        }
        if !sessions.more() || self.recent_ask.is_some() {
            return None;
        }
        let before = sessions
            .last_recent()
            .filter(|last| last.key == focused)?
            .id
            .0
            .clone();
        let id = mint();
        self.recent_ask = Some((id.clone(), false));
        Some(recent_line(&id, None, Some(before.as_str())))
    }

    /// Starts a new connection's feed cycle and queues one scoped first
    /// page per project already on home.
    pub(super) fn reset_for_connection(
        &mut self,
        sessions: &Sessions,
        launch_project: Option<&str>,
    ) -> Vec<String> {
        self.fed = false;
        self.scoped_asks.clear();
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
            let id = mint();
            self.scoped_asks.insert(id.clone(), project.clone());
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
    pub(super) fn scoped_first(&mut self, project: &str) -> Vec<String> {
        let connected = self.outbox.is_up();
        let Some(home) = self.home.as_mut() else {
            return Vec::new();
        };
        home.recent
            .scoped_first(project, &mut home.sessions, connected)
    }

    /// Asks the next scoped page for a project, without marking an
    /// in-flight ask dirty.
    pub(super) fn scoped_page(&mut self, project: &str, before: Option<&str>) -> Vec<String> {
        let connected = self.outbox.is_up();
        let Some(home) = self.home.as_mut() else {
            return Vec::new();
        };
        home.recent
            .scoped_page(project, before, connected)
            .into_iter()
            .collect()
    }

    /// Asks the first unscoped `recent` page again: an accepted delete
    /// fills the gap it left. It also asks the scoped `recent` for the
    /// deleted row's project, or marks it dirty while one is in flight.
    pub(super) fn ask_recent_first(&mut self, delete_project: Option<&str>) -> Vec<String> {
        let connected = self.outbox.is_up();
        let Some(home) = self.home.as_mut() else {
            return Vec::new();
        };
        home.recent
            .ask_recent_first(delete_project, &mut home.sessions, connected)
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
        let answer = {
            let home = self.home.as_mut()?;
            home.recent
                .answer(id, accepted, payload, &mut home.sessions)?
        };
        match answer {
            Outcome::Feed(notice) => {
                if let Some(notice) = notice {
                    self.notices.push(&notice.code, notice.message);
                }
                Some(Vec::new())
            }
            Outcome::First { first, result } => match result {
                Ok((rows, before)) => {
                    // The unscoped answer's `total` is never
                    // stored: only a scoped answer sets a
                    // project's count.
                    self.session_labels(&rows);
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.recent(rows, first);
                        home.refill_picker();
                    }
                    self.list_answered(first, true);
                    // Every unscoped page fans out one scoped
                    // first page per newly seen project.
                    let lines = self
                        .home
                        .as_mut()
                        .map(|home| {
                            let launch_project = home.launch_project().map(str::to_owned);
                            home.recent.fan_out_scoped(
                                &before,
                                &mut home.sessions,
                                launch_project.as_deref(),
                                connected,
                            )
                        })
                        .unwrap_or_default();
                    Some(lines)
                }
                Err(notice) => {
                    self.notices.push(&notice.code, notice.message);
                    self.list_answered(first, false);
                    Some(Vec::new())
                }
            },
            Outcome::Scoped { project, result } => match result {
                Ok((rows, total)) => {
                    self.session_labels(&rows);
                    let dirty = if let Some(home) = self.home.as_mut() {
                        let dirty =
                            home.recent
                                .answer_scoped(&project, rows, total, &mut home.sessions);
                        home.refill_picker();
                        dirty
                    } else {
                        false
                    };
                    let lines = if dirty {
                        self.home
                            .as_mut()
                            .map(|home| {
                                home.recent.follow_up_scoped(
                                    &project,
                                    &mut home.sessions,
                                    connected,
                                )
                            })
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    Some(lines)
                }
                Err(notice) => {
                    self.notices.push(&notice.code, notice.message);
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
        let home = self.home.as_mut()?;
        let line = home
            .recent
            .page_recent(focused, &mut home.sessions, connected)?;
        Some(Effect::Send(vec![line]))
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

/// A `recent` line: the first page, or past `before`. `project` scopes
/// the ask to one project, `before` pages past a row; each rides only
/// when set, so the unscoped first page carries no `args`.
fn recent_line(id: &str, project: Option<&str>, before: Option<&str>) -> String {
    let mut args = serde_json::Map::new();
    if let Some(project) = project {
        args.insert("project".to_owned(), Value::String(project.to_owned()));
    }
    if let Some(before) = before {
        args.insert("before".to_owned(), Value::String(before.to_owned()));
    }
    if args.is_empty() {
        json!({"id": id, "command": "recent"}).to_string()
    } else {
        json!({"id": id, "command": "recent", "args": Value::Object(args)}).to_string()
    }
}
