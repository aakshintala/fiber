//! `/mcp` (`docs/tui.md`, "Swapped views"; `docs/mcp.md`, "Servers, their
//! state and the per-server commands"): one row per configured server with
//! its layer, state and tool count. Enter reconnects a failed server and
//! logs in one that needs it; a repository's unapproved server only says
//! to run `fiber approve`, and Enter on a ready or not-started row does
//! nothing.

use std::collections::HashMap;

use contract::events::{McpServerLayer, McpServerRow, McpServerState};

use crate::format::{cut, width};
use crate::keys::Key;
use crate::settings_view::{Act, Ctx};
use crate::swapped::{Frame, Ink, List, Spot, rows_height};
use crate::theme::Role;

/// A server name's column.
const NAME: usize = 18;

/// The last row: per-tool switches stay in `/tools`.
const NOTE: &str = "Per-tool on/off switches stay in `/tools`.";

fn layer_word(layer: McpServerLayer) -> &'static str {
    match layer {
        McpServerLayer::Global => "global",
        McpServerLayer::Project => "project",
        McpServerLayer::Repository => "repository",
    }
}

/// The advice Enter shows on a repository's unapproved server: the same
/// sentence the hub refuses its login and reconnect with
/// (`cli::CliError::McpUnapproved`).
fn unapproved_advice(server: &str) -> String {
    format!(
        "`{server}` is declared by the repository but not approved; run `fiber approve` in the repository."
    )
}

/// A wait in flight for `server`: the driver command's id, and whether it
/// is the login (else the reconnect).
#[derive(Debug)]
struct Wait {
    /// The `mcp_login` or `mcp_reconnect` command's id.
    id: String,
    /// Whether the wait is the login.
    login: bool,
}

/// The `/mcp` view's state: the answer's rows, sorted by name, then
/// the note naming where the per-tool switches stay.
#[derive(Debug)]
pub(crate) struct Mcp {
    /// The `mcp_servers` command's id until its answer arrives.
    awaiting: Option<String>,
    /// The answer's rows, sorted by name.
    servers: Vec<McpServerRow>,
    /// The driver commands in flight, by server.
    waiting: HashMap<String, Wait>,
    /// The answering view's width: the login URL is cut to fit it, as
    /// the frame has no width to cut by.
    columns: usize,
    list: List,
    /// What the last action said, shown below the rows.
    said: Vec<String>,
}

impl Mcp {
    /// The view opening with the `mcp_servers` command `id` sent.
    pub(crate) fn open(id: String) -> Self {
        Self {
            awaiting: Some(id),
            servers: Vec::new(),
            waiting: HashMap::new(),
            columns: 0,
            list: List::default(),
            said: Vec::new(),
        }
    }

    /// Whether a fresh query would say anything new: none while the last
    /// answer is still in flight.
    pub(crate) fn live(&self) -> bool {
        self.awaiting.is_none()
    }

    /// The `mcp_servers` command `id` the next answer fills.
    pub(crate) fn expect(&mut self, id: String) {
        self.awaiting = Some(id);
    }

    /// Whether `id` is the `mcp_servers` command the view waits on.
    fn is_waiting(&self, id: &str) -> bool {
        self.awaiting.as_deref() == Some(id)
    }

    /// Server rows and the last note: the list's length.
    fn len(&self) -> usize {
        self.servers.len() + 1
    }

    /// Handles one key.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            return Act::Close;
        }
        if *key == Key::Enter {
            return self.enter();
        }
        let shown = self.shown(ctx);
        if self.list.key(key, self.len(), shown) {
            self.said.clear();
        }
        Act::Stay
    }

    /// Shifts the window one wheel step: three rows, keeping the
    /// selection while it shows.
    pub(crate) fn wheel(&mut self, up: bool, ctx: &Ctx<'_>) -> Act {
        let shown = self.shown(ctx);
        let before = (self.list.top(), self.list.selected());
        self.list.wheel(up, self.len(), shown, 3);
        if (self.list.top(), self.list.selected()) != before {
            self.said.clear();
        }
        Act::Stay
    }

    /// A click: the ✕ closes, a row is selected.
    pub(crate) fn click(&mut self, spot: Spot, ctx: &Ctx<'_>) -> Act {
        match spot {
            Spot::Close => Act::Close,
            Spot::Row(at) => {
                let shown = self.shown(ctx);
                self.list.select(at, self.len(), shown);
                Act::Stay
            }
            Spot::Revoke(_) | Spot::Cell(_, _) | Spot::Item(_) | Spot::Switch { .. } => Act::Stay,
        }
    }

    /// Enter on the selected server: reconnect a failed one, log in one
    /// that needs it, the approve advice on an unapproved one, and
    /// nothing on a ready or not-started row.
    fn enter(&mut self) -> Act {
        let Some(row) = self.servers.get(self.list.selected()) else {
            // The last note: Enter does nothing on it.
            return Act::Stay;
        };
        if !row.approved {
            self.said = vec![unapproved_advice(&row.name)];
            return Act::Stay;
        }
        match row.state {
            McpServerState::Failed => Act::McpReconnect(row.name.clone()),
            McpServerState::NeedsLogin => Act::McpLogin(row.name.clone()),
            McpServerState::NotStarted | McpServerState::Ready => Act::Stay,
        }
    }

    /// The rows the view shows at `ctx`'s height.
    fn shown(&self, ctx: &Ctx<'_>) -> usize {
        rows_height(&self.frame(ctx.usage), ctx.height)
    }

    /// The answer to the sent command: any other answer, and an answer to
    /// an older id, is ignored.
    pub(crate) fn answered(&mut self, id: &str, servers: &[McpServerRow], ctx: &Ctx<'_>) {
        if !self.is_waiting(id) {
            return;
        }
        self.servers = servers.to_vec();
        self.servers.sort_by(|a, b| a.name.cmp(&b.name));
        // A fresh snapshot answers every wait the rows show: a login the
        // hub no longer waits on is over, one way or the other.
        self.waiting
            .retain(|server, _| servers.iter().any(|row| row.name == *server));
        self.columns = ctx.width;
        self.awaiting = None;
        let shown = rows_height(&self.frame(None), ctx.height);
        self.list.select(self.list.selected(), self.len(), shown);
    }

    /// The sent command's rejection: only for its id.
    pub(crate) fn rejected(&mut self, id: &str, message: &str) {
        if self.is_waiting(id) {
            self.awaiting = None;
        } else if self.followup(id) {
            // The wait the message answers is over.
        } else {
            return;
        }
        self.said = vec![message.to_owned()];
    }

    /// Remembers the driver command `id` sent for `server` and says
    /// what it is doing: a login waits on the browser, a reconnect
    /// restarts the one slot. A login when `login`, else a reconnect.
    pub(crate) fn sent(&mut self, server: &str, id: String, login: bool) {
        self.waiting.insert(server.to_owned(), Wait { id, login });
        self.said = vec![if login {
            format!("Logging in {server}…")
        } else {
            format!("Reconnecting {server}…")
        }];
    }

    /// Whether `id` answers a driver command in flight: the wait is over,
    /// answered or rejected.
    pub(crate) fn followup(&mut self, id: &str) -> bool {
        let Some(server) = self
            .waiting
            .iter()
            .find_map(|(server, wait)| (wait.id == id).then(|| server.clone()))
        else {
            return false;
        };
        self.waiting.remove(&server);
        true
    }

    /// The frame to draw: server rows sorted by name, then the note
    /// naming where the per-tool switches stay.
    pub(crate) fn frame(&self, _usage: Option<u64>) -> Frame {
        let mut rows: Vec<Vec<(String, Option<Spot>, Ink)>> = self
            .servers
            .iter()
            .map(|row| self.server_cells(row))
            .collect();
        rows.push(vec![(format!("  {NOTE}"), None, Ink::Muted)]);
        let below = if self.awaiting.is_some() {
            let mut below = vec!["Reading the servers…".to_owned()];
            below.extend(self.said.clone());
            below
        } else {
            self.said.clone()
        };
        Frame {
            title: "MCP servers".to_owned(),
            command: "/mcp".to_owned(),
            rows,
            list: self.list,
            below,
            field: None,
            footer: "↑↓ move · Enter reconnect or log in · Esc close".to_owned(),
        }
    }

    /// One server's cells: its name, layer and state, with the login wait
    /// and its URL while one is in flight. A repository's server nobody
    /// approved never starts: its row says so, and Enter gives the
    /// `fiber approve` advice.
    fn server_cells(&self, row: &McpServerRow) -> Vec<(String, Option<Spot>, Ink)> {
        if !row.approved {
            return vec![(
                format!(
                    "  {} · {} · not approved",
                    fit_name(&row.name),
                    layer_word(row.layer)
                ),
                None,
                Ink::Role(Role::Attention),
            )];
        }
        let waiting_login = self.waiting.get(&row.name).is_some_and(|wait| wait.login);
        // The login URL as the rows show it, cut to the answering
        // view's width: the frame has no width to cut by.
        let mut login_url = None;
        let (state, ink) = match row.state {
            McpServerState::NotStarted => ("not started".to_owned(), Ink::Muted),
            McpServerState::Ready => {
                let count = row.tools.unwrap_or(0);
                let tools = if count == 1 {
                    "1 tool".to_owned()
                } else {
                    format!("{count} tools")
                };
                (format!("ready · {tools}"), Ink::Plain)
            }
            McpServerState::Failed => {
                let state = if row.reason.is_empty() {
                    "failed".to_owned()
                } else {
                    format!("failed · {}", row.reason)
                };
                (state, Ink::Role(Role::Error))
            }
            McpServerState::NeedsLogin => {
                let mut state = "needs-login".to_owned();
                if waiting_login || row.auth_url.is_some() {
                    state.push_str(" · waiting for login");
                    login_url = row.auth_url.as_deref();
                }
                (state, Ink::Role(Role::Attention))
            }
        };
        let mut text = format!(
            "  {} · {} · {state}",
            fit_name(&row.name),
            layer_word(row.layer)
        );
        if let Some(url) = login_url {
            text.push_str(" · ");
            text.push_str(&cut(url, self.columns.saturating_sub(width(&text))));
        }
        vec![(text, None, ink)]
    }
}

/// `name` in the name column: padded, or cut with `…`.
fn fit_name(name: &str) -> String {
    if width(name) <= NAME {
        let mut out = name.to_owned();
        out.push_str(&" ".repeat(NAME.saturating_sub(width(name))));
        return out;
    }
    let mut out = cut(name, NAME.saturating_sub(1));
    out.push('…');
    out
}

#[cfg(test)]
#[path = "mcp_view_tests.rs"]
mod tests;
