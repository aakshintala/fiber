//! The input box's completion panels, the commands Enter and Esc send,
//! the built-in commands and the key map overlay (`docs/tui.md`, "Keys",
//! "Bindings", "Slash commands", "Quit").

use std::path::PathBuf;
use std::time::Instant;

use contract::SessionId;
use contract::events::CommandAccepted;
use serde_json::json;

use super::{App, ClearPending, Effect, Kind, Link, Phase, Surface, mint, session_command};
use crate::editor::Target;
use crate::import_report::ImportSource;
use crate::input::Draft;
use crate::keymap::KeyMap;
use crate::keys::{Edit, Key};
use crate::shell;
use crate::slash;

/// What the notice says when a command needs a session and none is
/// attached.
const NO_SESSION: &str = "No session on screen.";

/// The `@` panel's state.
pub(crate) use crate::completion_rows::{Completions, Rows};

/// The draft's content as a prompt's `content` argument: text runs and
/// image parts in order, each run one text part. `SentPart` serializes as
/// the wire part, `type` with the part's fields, so the content converts
/// directly. Every part serializes, so the fallback never runs.
pub(super) fn content_arg(draft: &Draft) -> serde_json::Value {
    let content =
        serde_json::to_value(draft.content()).unwrap_or(serde_json::Value::Array(Vec::new()));
    json!({ "content": content })
}

/// One built-in command: adding one is one row plus its handler, which
/// lives beside the state it touches.
pub(crate) struct CommandEntry {
    /// The name typed after `/`.
    pub(crate) name: &'static str,
    /// A one-line description, drawn in the `/` list.
    pub(crate) description: &'static str,
    /// An argument hint, such as `[instructions]`.
    pub(crate) hint: Option<&'static str>,
    /// Runs the command on the rest of the draft.
    pub(crate) run: fn(&mut App, &str) -> Option<Effect>,
}

/// Other names typed after `/` that run the same command, drawn
/// on the same list row: `/exit` on the quit row, `/keybindings` and
/// `/keymap` on the keys row. Empty elsewhere, so an alias never
/// doubles the row. The `/` list and dispatch both read it beside
/// the table.
pub(crate) fn aliases_of(name: &str) -> &'static [&'static str] {
    match name {
        "thinking" => &["effort"],
        "usage" => &["cost"],
        "pwd" => &["cwd"],
        "settings" => &["config"],
        "keys" => &["keybindings", "keymap"],
        "handoff" => &["compact"],
        "rules" => &["permissions"],
        "close" => &["stop"],
        "name" => &["rename"],
        "feedback" => &["bug"],
        "quit" => &["exit"],
        "delegates" => &["agents"],
        _ => &[],
    }
}

/// The single command table: the `/` list and dispatch both read it.
/// `?` and `help` are two rows sharing one handler.
pub(crate) static TABLE: &[CommandEntry] = &[
    CommandEntry {
        name: "home",
        description: "Goes home, focused on the session on screen's row.",
        hint: None,
        run: run_home,
    },
    CommandEntry {
        name: "new",
        description: "Goes home with the cursor in the input box, to pick the workspace, worktree, model and thinking level for a new session. The session left keeps running.",
        hint: None,
        run: run_new,
    },
    CommandEntry {
        name: "resume",
        description: "Goes home, focused on the first exited row.",
        hint: None,
        run: run_resume,
    },
    CommandEntry {
        name: "model",
        description: "Opens the model picker.",
        hint: None,
        run: run_model,
    },
    CommandEntry {
        name: "thinking",
        description: "Sets the thinking level, or opens the model picker on it.",
        hint: Some("[<level>]"),
        run: run_thinking,
    },
    CommandEntry {
        name: "credential",
        description: "Switches the credential label, or opens the credential picker.",
        hint: Some("[<label>]"),
        run: run_credential,
    },
    CommandEntry {
        name: "scoped-models",
        description: "Chooses which models the model picker shows.",
        hint: None,
        run: run_scoped_models,
    },
    CommandEntry {
        name: "tools",
        description: "Opens the tools view.",
        hint: None,
        run: run_tools,
    },
    CommandEntry {
        name: "mcp",
        description: "Opens the MCP servers view.",
        hint: None,
        run: run_mcp,
    },
    CommandEntry {
        name: "context",
        description: "Opens the context breakdown.",
        hint: None,
        run: run_context,
    },
    CommandEntry {
        name: "usage",
        description: "Opens the usage view.",
        hint: None,
        run: run_usage,
    },
    CommandEntry {
        name: "quota",
        description: "Opens the quota view and fetches every label's quota.",
        hint: None,
        run: run_quota,
    },
    CommandEntry {
        name: "status",
        description: "Opens the status view.",
        hint: None,
        run: run_status,
    },
    CommandEntry {
        name: "diff",
        description: "Opens the Changed files view.",
        hint: None,
        run: run_diff,
    },
    CommandEntry {
        name: "copy",
        description: "Copies the newest reply.",
        hint: None,
        run: run_copy,
    },
    CommandEntry {
        name: "pwd",
        description: "Shows the session's workspace; on home, shows the notice 'No session on screen'.",
        hint: None,
        run: run_pwd,
    },
    CommandEntry {
        name: "panel",
        description: "Shows or hides the panel.",
        hint: None,
        run: run_panel,
    },
    CommandEntry {
        name: "rail",
        description: "Shows or hides the rail.",
        hint: None,
        run: run_rail,
    },
    CommandEntry {
        name: "rules",
        description: "Opens the standing rules.",
        hint: None,
        run: run_rules,
    },
    CommandEntry {
        name: "reviewer-setup",
        description: "Sets up the reviewer's notes with you.",
        hint: None,
        run: run_reviewer_setup,
    },
    CommandEntry {
        name: "settings",
        description: "Opens the configuration keys.",
        hint: None,
        run: run_settings,
    },
    CommandEntry {
        name: "theme",
        description: "Opens the theme picker, or sets the named theme.",
        hint: Some("[<name>]"),
        run: run_theme,
    },
    CommandEntry {
        name: "keys",
        description: "Opens the rebinding screen.",
        hint: None,
        run: run_keys,
    },
    CommandEntry {
        name: "skills",
        description: "Opens the skills.",
        hint: None,
        run: run_skills,
    },
    CommandEntry {
        name: "rewind",
        description: "Opens the rewind view.",
        hint: None,
        run: run_rewind,
    },
    CommandEntry {
        name: "handoff",
        description: "Starts a handoff.",
        hint: Some("[instructions]"),
        run: run_handoff,
    },
    CommandEntry {
        name: "name",
        description: "Names the session.",
        hint: Some("<text>"),
        run: run_name,
    },
    CommandEntry {
        name: "login",
        description: "Logs in.",
        hint: None,
        run: run_login,
    },
    CommandEntry {
        name: "logout",
        description: "Logs out.",
        hint: None,
        run: run_logout,
    },
    CommandEntry {
        name: "reload",
        description: "Reloads configuration, MCP servers and extensions.",
        hint: None,
        run: run_reload,
    },
    CommandEntry {
        name: "import",
        description: "Copies another harness's configuration into Fiber home, once.",
        hint: Some("[<source>]"),
        run: run_import,
    },
    CommandEntry {
        name: "clear",
        description: "Starts a fresh session in place, closing the session on screen.",
        hint: None,
        run: run_clear,
    },
    CommandEntry {
        name: "close",
        description: "Stops the session on screen.",
        hint: Some("[all]"),
        run: run_close,
    },
    CommandEntry {
        name: "feedback",
        description: "Writes a feedback report and prints its path and the issue link.",
        hint: Some("<description>"),
        run: run_feedback,
    },
    CommandEntry {
        name: "quit",
        description: "Quits.",
        hint: None,
        run: run_quit,
    },
    CommandEntry {
        name: "approvals",
        description: "Reopens the waiting approvals and questions.",
        hint: None,
        run: run_approvals,
    },
    CommandEntry {
        name: "?",
        description: "Opens the key map.",
        hint: None,
        run: run_keymap,
    },
    CommandEntry {
        name: "help",
        description: "Opens the key map.",
        hint: None,
        run: run_keymap,
    },
    CommandEntry {
        name: "delegates",
        description: "Opens the delegates view: every delegate of the session on screen, running and finished.",
        hint: None,
        run: run_delegates,
    },
];

fn run_home(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.leave_on_row())
}

fn run_new(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.leave())
}

fn run_resume(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.resume_list())
}

fn run_model(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_model_picker(crate::model_picker::Mode::Choose))
}

fn run_thinking(app: &mut App, rest: &str) -> Option<Effect> {
    Some(app.thinking_command(rest))
}

fn run_scoped_models(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.scoped_models_command())
}

fn run_panel(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.toggle_panel())
}

fn run_rail(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.toggle_rail())
}

fn run_handoff(app: &mut App, rest: &str) -> Option<Effect> {
    let args = (!rest.is_empty()).then(|| json!({ "instructions": rest }));
    Some(app.send_command("handoff", args).0)
}

fn run_name(app: &mut App, rest: &str) -> Option<Effect> {
    Some(app.send_command("name", Some(json!({ "text": rest }))).0)
}

pub(super) fn run_reload(app: &mut App, _rest: &str) -> Option<Effect> {
    // A reloaded extension draws again: its next failure reports anew.
    app.renderer_reported.clear();
    if let Some(loader) = app
        .home
        .as_ref()
        .and_then(|home| home.launch.extensions.clone())
    {
        let (renderer, notices) = loader.load();
        app.use_renderer(renderer);
        for (code, text) in notices {
            app.push_notice(&code, text);
        }
    }
    Some(app.send_command("reload", None).0)
}

fn run_import(app: &mut App, rest: &str) -> Option<Effect> {
    // A bare `/import` imports both sources; anything else goes out as
    // `source`, and the session refuses what it does not take. The
    // source rides beside the pending entry under the command's id, so
    // the accepted answer's report names it; a bare import reports the
    // generic title.
    let rest = rest.trim();
    let args = (!rest.is_empty()).then(|| json!({"source": rest}));
    // The entry carries the import's source, so the accepted answer's
    // report names it; an unknown source stays a plain command, and
    // its accept opens no report.
    let kind = ImportSource::parse(rest).map_or(Kind::Command, Kind::Import);
    let (effect, _) = app.send_kind(kind, "import", args);
    Some(effect)
}

fn run_close(app: &mut App, rest: &str) -> Option<Effect> {
    if rest.trim() == "all" {
        return Some(app.ask_close_all());
    }
    Some(app.close())
}

fn run_clear(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.clear())
}

/// `/feedback`: with no description, asks for one by leaving the draft
/// on `/feedback `; with a description, the loop writes the report off
/// its thread through the injected writer, and the answer lands as a
/// notice.
fn run_feedback(app: &mut App, rest: &str) -> Option<Effect> {
    let description = rest.trim();
    if description.is_empty() {
        app.push_notice(
            "feedback",
            "Describe the feedback after /feedback, then press Enter.".to_owned(),
        );
        app.draft.clear();
        app.draft.paste("/feedback ");
        return Some(Effect::None);
    }
    Some(Effect::Feedback {
        description: description.to_owned(),
    })
}

fn run_quit(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.quit())
}

fn run_approvals(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.next_request())
}

fn run_settings(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.open_config_view(super::ConfigView::Settings))
}

fn run_theme(app: &mut App, rest: &str) -> Option<Effect> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Some(app.open_config_view(super::ConfigView::Theme));
    }
    Some(app.theme_command(rest))
}

fn run_keys(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.open_keys())
}

fn run_tools(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.open_config_view(super::ConfigView::Tools))
}

fn run_mcp(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.open_config_view(super::ConfigView::Mcp))
}

fn run_skills(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.open_config_view(super::ConfigView::Skills))
}

fn run_rewind(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_session_view(super::SessionView::Rewind))
}

fn run_delegates(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_session_view(super::SessionView::AllDelegates))
}

fn run_context(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_session_view(super::SessionView::Context))
}

fn run_usage(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_session_view(super::SessionView::Usage))
}

fn run_credential(app: &mut App, rest: &str) -> Option<Effect> {
    Some(app.credential_command(rest))
}

fn run_quota(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.quota_view())
}

fn run_status(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_session_view(super::SessionView::Status))
}

fn run_diff(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_session_view(super::SessionView::ChangedFiles))
}

fn run_copy(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.copy_last_reply())
}

/// Shows the session's recorded workspace root as a notice: the
/// absolute root `session_started` recorded, which is the worktree when
/// the session runs in one. On home, with no session on screen, the
/// `session_not_found` notice. While the attached session's root has
/// not folded yet, the notice waits for its fold. A lost link still
/// shows the recorded root, and nothing is ever sent.
fn run_pwd(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    let Phase::Attached { .. } = &app.phase else {
        app.push_notice("session_not_found", NO_SESSION.to_owned());
        return Some(Effect::None);
    };
    if let Some(workspace) = app.panel_state.workspace() {
        app.push_notice("workspace", workspace.to_owned());
    } else {
        app.pwd_pending = true;
    }
    Some(Effect::None)
}

fn run_rules(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.open_config_view(super::ConfigView::Rules))
}

fn run_reviewer_setup(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.open_reviewer_setup())
}

fn run_login(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.open_config_view(super::ConfigView::Login))
}

fn run_logout(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.open_config_view(super::ConfigView::Logout))
}

fn run_keymap(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    app.open_keymap();
    Some(Effect::None)
}

impl App {
    /// Handles one key at `now`, read from the injected clock. A recall
    /// waiting for a page waits on only through ↑ and the keys that move
    /// the view.
    pub(crate) fn on_key(&mut self, key: Key, now: Instant) -> Effect {
        if !matches!(
            key,
            Key::Up | Key::PageUp | Key::PageDown | Key::End | Key::CtrlO
        ) {
            self.history.cancel();
        }
        let effect = self.route_key(key, now);
        self.edited();
        self.settle();
        effect
    }

    /// Handles one key that edits the draft, the approval panel first.
    /// Nothing while the key map is open, while navigating with the panel
    /// closed, or in the Ctrl+R panel. A recall waiting for a page waits
    /// no more.
    pub(crate) fn on_edit(&mut self, edit: Edit) -> Effect {
        self.armed_at = None;
        self.copy_owed = false;
        self.history.cancel();
        // The quit question and the import report own every key through
        // `route_key`, so a paste or a home-owned arrow reaching here
        // changes neither the draft nor the surface behind them. They run
        // ahead of the picker, the chip row and the draft, as their keys
        // run ahead of every other key path.
        let top = self.top_surface();
        if matches!(top, Surface::Quit | Surface::Clear | Surface::Report) {
            return Effect::None;
        }
        // The reviewer setup takes every edit while it is on top: a paste
        // goes into its field, the rest do nothing behind it.
        if top == Surface::ReviewerSetup {
            return self.reviewer_setup_edit(&edit);
        }
        // The key map above the picker takes the edit: Left and Right
        // move between its tabs, every other edit does nothing behind it.
        if top == Surface::Keymap
            && let Some(effect) = self.keyboard.edit_key(&edit)
        {
            return effect;
        }
        // The open picker owns every edit but Ctrl+C, which never
        // arrives as one.
        if let Some(effect) = self.model_picker_edit(&edit) {
            return effect;
        }
        if let Some(effect) = self.config_view_edit(&edit) {
            return effect;
        }
        if let Some(effect) = self.session_view_edit(&edit) {
            return effect;
        }
        // Delete on a focused home row asks to delete it when it
        // exited, ahead of the focus early return below.
        if let Some(effect) = self.home_edit(&edit) {
            return effect;
        }
        // The `/keys` screen takes every stroke in `on_press`, so only
        // a paste reaches here, and it does nothing behind the screen.
        if self.keys_screen_open() {
            return Effect::None;
        }
        if let Some(effect) = self.offer_edit(&edit) {
            return effect;
        }
        if let Some(effect) = self.find_edit(&edit) {
            return effect;
        }
        // A request on the panel takes the edit ahead of the prompt search.
        if self.panel().is_none() && (self.search_edit(&edit) || self.focus.is_some()) {
            return Effect::None;
        }
        // A running `tty` job's view types into the job ahead of the
        // input box, behind every overlay above.
        if let Some(effect) = self.item_job_edit(&edit) {
            return effect;
        }
        crate::input::route(edit, &mut self.draft, &mut self.queue);
        self.completion.selected = 0;
        let effect = self.query_changed();
        self.settle();
        effect
    }

    /// A key the draft takes: a character or Backspace, which may open or
    /// search the `@` panel, or ↑ ↓ by wrapped row and then through earlier
    /// prompts. `None` for any other key.
    pub(super) fn draft_key(&mut self, key: &Key) -> Option<Effect> {
        match key {
            Key::Char(ch) => Some(self.type_char(*ch)),
            Key::Backspace => Some(self.backspace()),
            Key::Up | Key::Down => self.recall_key(key),
            Key::Enter
            | Key::Esc
            | Key::CtrlC
            | Key::CtrlO
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlL
            | Key::CtrlF => None,
        }
    }

    /// The workspace in use: the attached session's row workspace on
    /// home, else the launch directory, else none on a remote home.
    /// Never an empty path: every conversion filters empty values out.
    pub(crate) fn workspace(&self) -> Option<PathBuf> {
        if self.home.is_some() {
            self.home_workspace()
        } else {
            self.workspace.clone()
        }
    }

    /// The current search generation.
    pub(crate) fn generation(&self) -> u64 {
        self.completion.generation
    }

    /// Runs the draft when its first word is an extension command or a
    /// built-in command; `None` when it is neither, and the draft goes out
    /// as typed. A draft holding an image is never a command: Enter sends
    /// it as a prompt.
    pub(super) fn built_in(&mut self) -> Option<Effect> {
        if self.draft.has_image() {
            return None;
        }
        let draft = self.draft.expand();
        let draft = draft.trim();
        let (head, rest) = draft.split_once(char::is_whitespace).unwrap_or((draft, ""));
        let name = head.strip_prefix('/')?;
        if self.completion.extensions.iter().any(|known| known == name) {
            let rest = rest.trim();
            let args = if rest.is_empty() {
                Some(json!({"name": name}))
            } else {
                Some(json!({"name": name, "text": rest}))
            };
            return Some(self.send_command("command", args).0);
        }
        if !slash::is_built_in(name) {
            return None;
        }
        let rest = rest.trim().to_owned();
        // One table drives dispatch: a built-in the terminal does not
        // draw goes out as typed. An alias runs its row's command.
        let entry = TABLE
            .iter()
            .find(|entry| entry.name == name || aliases_of(entry.name).contains(&name))?;
        (entry.run)(self, &rest)
    }

    /// Returns to the screen before a session: the conversation and the
    /// draft cleared, the old session left running. While a `start` waits
    /// for its answer only the draft is cleared.
    pub(super) fn go_home(&mut self) {
        self.reviewer_setup_left();
        if self.reset_session() {
            self.attached_screen_mut().clear();
        }
    }

    /// Resets the screen before a session without clearing its rows,
    /// so a reconnect keeps them for the landing: the conversation and
    /// the draft cleared everywhere else, the old session left running.
    /// While a `start` waits for its answer only the draft is cleared.
    /// False without the reset.
    pub(super) fn reset_session(&mut self) -> bool {
        self.draft.clear();
        // A `/pwd` waiting on the old session's root ends with the
        // detach: going home forgets the wait with the screen.
        self.pwd_pending = false;
        self.close_find();
        self.leave_session(self.session().cloned());
        // Leaving the parent closes the item view first, swapping the
        // screen back before anything clears it; outstanding `summary`
        // wishes are marked detached, so their lowering waits behind a
        // `full` still in flight without a running check.
        self.leave_item();
        self.end_overlay();
        if matches!(self.phase, Phase::Pending { .. }) {
            return false;
        }
        // Opening a session from home forgets where home came from,
        // so the back key and its hint go quiet everywhere the next
        // route home starts: a row open, a card switch, `/close`.
        if let Some(home) = self.home.as_mut() {
            home.came_from = None;
        }
        self.phase = Phase::Starting;
        self.clear_selection();
        // Home keeps no session's images: a late answer or viewer
        // completion from it is dropped.
        self.forget_images();
        self.panel_state.reset();
        self.session_views_reset();
        self.offer = crate::offer::Offer::default();
        self.completion.slash_rows = slash::rows(&[]);
        self.completion.extensions.clear();
        self.completion.commands_id = None;
        self.copy_owed = false;
        true
    }

    /// The attached session, when the command can go out: with none
    /// attached, a notice and the draft cleared; with the link not up, the
    /// draft stays.
    pub(super) fn command_session(&mut self) -> Option<(contract::SessionId, bool)> {
        let Phase::Attached { session, busy } = &self.phase else {
            self.notices
                .push("session_not_found", NO_SESSION.to_owned());
            self.draft.clear();
            return None;
        };
        (self.outbox.is_up()).then(|| (session.clone(), *busy))
    }

    /// Sends `command` to the attached session, returning the minted
    /// command id beside the effect for callers recording per-command
    /// state. A rejection returns the draft.
    fn send_command(
        &mut self,
        command: &str,
        args: Option<serde_json::Value>,
    ) -> (Effect, Option<String>) {
        self.send_kind(Kind::Command, command, args)
    }

    /// Sends `command` of `kind` to the attached session, returning the
    /// minted command id beside the effect. A rejection returns the draft.
    fn send_kind(
        &mut self,
        kind: Kind,
        command: &str,
        args: Option<serde_json::Value>,
    ) -> (Effect, Option<String>) {
        let Some((session, _)) = self.command_session() else {
            return (Effect::None, None);
        };
        let id = mint();
        let line = session_command(&id, command, &session, args).to_string();
        let draft = std::mem::take(&mut self.draft);
        self.outbox.insert(id.clone(), kind, draft);
        (Effect::Send(vec![line]), Some(id))
    }

    /// `/clear`: starts a fresh session in the attached session's
    /// workspace, in place. The `start` carries the old session's model,
    /// its thinking level when it has one, and its credential label; the
    /// screen switches only once the hub answers with a `session_id`,
    /// and the old session is sent `close` only then. The phase stays
    /// attached to the old session until that answer.
    pub(super) fn clear(&mut self) -> Effect {
        if self.clear_pending.is_some() {
            self.draft.clear();
            return Effect::None;
        }
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        // With no status folded yet the terminal names nothing: keep
        // the draft for the retry once the first status lands.
        let Some(status) = self.panel_state.status().cloned() else {
            return Effect::None;
        };
        // A wait holds the polite close: ask first, in the overlay, and
        // send nothing yet.
        if let Some((name, waiting)) = self.clear_wait(&session) {
            return self.open_clear(session, name, waiting);
        }
        self.clear_send(status, session, false)
    }

    /// The wait holding `session`, if it waits on one: its title and
    /// kind.
    fn clear_wait(&self, session: &SessionId) -> Option<(String, String)> {
        let row = self.home.as_ref()?.sessions.row(session)?;
        let waiting = row.waiting.as_ref()?;
        let kind = waiting.split(':').next()?.to_owned();
        Some((crate::home::title(row), kind))
    }

    /// Sends the clear `start` for `old` from `status`: `now` stages
    /// whether its `close` drops the wait. The pending draft is empty,
    /// so no first prompt goes out.
    pub(super) fn clear_send(
        &mut self,
        status: contract::events::SessionStatus,
        old: SessionId,
        now: bool,
    ) -> Effect {
        if self.clear_pending.is_some() {
            return Effect::None;
        }
        let mut overrides = Vec::new();
        if let Some(thinking) = status.thinking.as_deref() {
            overrides.push(format!(
                "{}={thinking}",
                super::model_picker::canonical_thinking_key(&status.model)
            ));
        }
        let credential = self
            .panel_state
            .clear_credential()
            .map(|credential| credential.map(str::to_owned));
        if let Some(Some(label)) = credential.as_ref() {
            let provider = status.model.split('/').next().unwrap_or(&status.model);
            overrides.push(format!("providers.\"{provider}\".credential={label}"));
        }
        let mut args = json!({
            "workspace": status.workspace,
            "model": status.model,
            "worktree": false,
        });
        if let Some(object) = args.as_object_mut() {
            if !overrides.is_empty() {
                object.insert("overrides".to_owned(), json!(overrides));
            }
            if self.panel_state.worktree() {
                object.insert("continues".to_owned(), json!(old.0.clone()));
            }
        }
        let id = mint();
        self.panel_state.note_start(&args);
        let line = json!({"id": id, "command": "start", "args": args}).to_string();
        self.draft.clear();
        let effect = match self
            .outbox
            .send(id.clone(), Kind::Start, Draft::default(), line)
        {
            Some(line) => Effect::Send(vec![line]),
            None => Effect::None,
        };
        self.clear_pending = Some(ClearPending {
            old,
            command_id: id,
            credential,
            now,
        });
        effect
    }

    /// `/close`: one `close` with `now`, whether the session is busy or
    /// idle, then home.
    fn close(&mut self) -> Effect {
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        let id = mint();
        let lines =
            vec![session_command(&id, "close", &session, Some(json!({"now": true}))).to_string()];
        // Home has a fresh draft: a rejected `close` gives only its notice.
        self.outbox.insert(id, Kind::Command, Draft::default());
        self.go_home();
        Effect::Send(lines)
    }

    /// The key map overlay's state, while it is open.
    pub(crate) fn keymap(&self) -> Option<&KeyMap> {
        self.keyboard.keymap()
    }

    /// The key map overlay's top row, while it is open.
    pub(crate) fn keymap_top(&self) -> Option<usize> {
        self.keyboard.keymap_top()
    }

    /// Closes the key map before a swapped view takes the conversation.
    pub(super) fn close_keymap(&mut self) {
        self.keyboard.close_keymap();
    }

    /// Opens the key map overlay at its top.
    pub(super) fn open_keymap(&mut self) -> Effect {
        self.keyboard.open_keymap()
    }

    /// A key while the key map is open: it takes every key but Ctrl+C,
    /// ⌥R and ⌥1–9, which arm quit and switch sessions past it. `None`
    /// while it is closed.
    pub(super) fn keymap_key(&mut self, key: &Key) -> Option<Effect> {
        let width = self.column_width();
        let height = self.conversation_height();
        let keyset = self.keys().clone();
        self.keyboard.keymap_key(key, &keyset, width, height)
    }

    /// A `commands` line for `session`, sent on attach and after each
    /// `reloaded`, `extensions_loaded`, and `turn_started`; its answer,
    /// and no older one, fills the `/` list.
    /// Until it answers, the list holds the built-ins only.
    pub(super) fn ask_commands(&mut self, session: &SessionId) -> String {
        self.completion.ask_commands(session)
    }

    /// A session's `command_accepted`: the answer to the latest `commands`
    /// sent replaces the session's rows of the `/` list.
    pub(super) fn commands_answered(&mut self, accepted: &CommandAccepted) {
        self.completion.commands_answered(accepted);
    }

    /// Whether the draft starts with an unknown slash command: its first
    /// word starts with `/`, names neither a built-in command nor an entry
    /// in the session's current `commands` answer, and holds no second
    /// `/`, so it is no path. Refusing posts the notice and keeps the
    /// whole draft, image included; `true` means Enter sends nothing.
    fn refuse_unknown_slash(&mut self) -> bool {
        // The typed text, without image labels: `expand` draws each
        // image as `[Image #N]`, which would split the first word.
        let mut text = String::new();
        for part in self.draft.content() {
            if let contract::commands::SentPart::Text { text: run } = part {
                text.push_str(&run);
            }
        }
        let trimmed = text.trim();
        let (head, _) = trimmed
            .split_once(char::is_whitespace)
            .unwrap_or((trimmed, ""));
        let Some(name) = head.strip_prefix('/') else {
            return false;
        };
        if name.is_empty() || name.contains('/') {
            return false;
        }
        if slash::is_built_in(name) {
            return false;
        }
        if self
            .completion
            .slash_rows
            .iter()
            .any(|row| row.name == name || row.aliases.iter().any(|alias| alias == name))
        {
            return false;
        }
        self.push_notice(
            "unknown_command",
            format!("Unknown command /{name}. Type / for the list."),
        );
        true
    }

    /// Enter sends the draft: `start` with no session, `prompt` when
    /// idle, `steer` during a turn, and `shell` for a `!` command whenever
    /// attached. A draft holding an image sends its content as a prompt
    /// or `steer`, never as a built-in or a `!` command. An unknown slash
    /// command is refused before any of that, keeping the draft.
    pub(super) fn on_enter(&mut self) -> Effect {
        if self.refuse_unknown_slash() {
            return Effect::None;
        }
        if let Some(effect) = self.built_in() {
            return effect;
        }
        // In an item view Enter steers the open delegate; a `/`
        // built-in already ran above, and anything it leaves (such as
        // `/close`) falls through to the attached session.
        if self.item_open()
            && let Some(effect) = self.item_enter()
        {
            return effect;
        }
        let has_image = self.draft.has_image();
        let text = self.draft.expand();
        if matches!(self.phase, Phase::Starting)
            && self.home.as_ref().is_some_and(|home| home.no_workspace())
        {
            return self.home_click(crate::home::Spot::Workspace);
        }
        // A command sent after the connection is lost goes nowhere, so the
        // draft stays. An empty draft stays too, unless it holds an image.
        if matches!(self.outbox.link, Link::Down | Link::Refused) {
            return Effect::None;
        }
        if !has_image && text.trim().is_empty() {
            return Effect::None;
        }
        if self.steering.is_selected() {
            return self.amend();
        }
        let id = mint();
        let args = content_arg(&self.draft);
        let shell = (!has_image).then(|| shell::parse(&text)).flatten();
        let (kind, line) = match (&self.phase, shell) {
            (Phase::Starting | Phase::Pending { .. }, Some(_)) => {
                self.notices
                    .push("session_not_found", "Start a session first.".to_owned());
                return Effect::None;
            }
            (Phase::Pending { .. }, None) => return Effect::None,
            (Phase::Starting, None) => {
                // With no model on the chip and none chosen, Enter opens
                // the picker and keeps the draft, sending nothing.
                if self
                    .home
                    .as_ref()
                    .is_some_and(|home| home.launch.model.is_none())
                    && self.model_picker.start_model.is_none()
                {
                    return self.open_model_picker(crate::model_picker::Mode::Choose);
                }
                let mut args = self.start_args();
                // A session-only choice on home rides this `start`.
                self.with_start_model(&mut args);
                self.panel_state.note_start(&args);
                let line = json!({"id": id, "command": "start", "args": args});
                (Kind::Start, line)
            }
            (Phase::Attached { session, .. }, Some((command, send))) => {
                (Kind::Shell, shell::command(&id, session, command, send))
            }
            (Phase::Attached { session, busy }, None) => {
                let (kind, command) = if *busy {
                    (Kind::Steer, "steer")
                } else {
                    (Kind::Prompt, "prompt")
                };
                (kind, session_command(&id, command, session, Some(args)))
            }
        };
        if kind == Kind::Start {
            self.phase = Phase::Pending {
                command_id: id.clone(),
            };
        }
        let draft = std::mem::take(&mut self.draft);
        let line = line.to_string();
        match self.outbox.send(id, kind, draft, line) {
            Some(line) => Effect::Send(vec![line]),
            None => Effect::None,
        }
    }

    /// The first prompt of a session `start` made, carrying `draft`, sent
    /// after its `subscribe` so the session counts this terminal before
    /// the prompt. A rejection returns the draft to an empty box.
    pub(super) fn first_prompt(&mut self, session: &SessionId, draft: Draft) -> String {
        let id = mint();
        let args = content_arg(&draft);
        let line = session_command(&id, "prompt", session, Some(args)).to_string();
        self.outbox.insert(id, Kind::Prompt, draft);
        line
    }

    /// Esc with nothing open interrupts the turn: `cancel`, only when busy.
    pub(super) fn on_esc(&mut self) -> Effect {
        let session = match &self.phase {
            Phase::Attached {
                session,
                busy: true,
            } if self.connected() => session.clone(),
            Phase::Starting | Phase::Pending { .. } | Phase::Attached { .. } => {
                return Effect::None;
            }
        };
        let id = mint();
        let line = session_command(&id, "cancel", &session, None).to_string();
        self.outbox.insert(id, Kind::Cancel, Draft::default());
        Effect::Send(vec![line])
    }

    /// Ctrl+G: the paste token beside the cursor, or else the whole draft,
    /// every token expanded. A recall waiting for a page waits no more.
    pub(super) fn open_in_editor(&mut self) -> Effect {
        if let Some(number) = self.draft.token_at_cursor() {
            return self.open_token(number);
        }
        self.history.cancel();
        Effect::Editor {
            target: Target::Draft,
            text: self.draft.expand(),
        }
    }

    /// Paste token `number`'s text in the editor; nothing when the draft
    /// holds no such token. A recall waiting for a page waits no more.
    pub(super) fn open_token(&mut self, number: usize) -> Effect {
        self.history.cancel();
        match self.draft.token_text(number) {
            Some(text) => Effect::Editor {
                target: Target::Token(number),
                text: text.to_owned(),
            },
            None => Effect::None,
        }
    }

    /// The editor returned: its text replaces `target`'s, the whole draft's
    /// as typed with image labels relinked and the cursor at its end; an
    /// error is the notice, and the draft stays.
    pub(crate) fn editor_returned(&mut self, target: Target, result: Result<String, String>) {
        match (result, target) {
            (Err(notice), _) => self.notices.push("io_failed", notice),
            (Ok(text), Target::Token(number)) => self.draft.set_token(number, &text),
            (Ok(text), Target::Draft) => self.draft.edited(&text),
            (Ok(_), Target::Item) => {}
        }
        self.completion.selected = 0;
        self.edited();
        self.settle();
    }

    /// A clipboard image read finished: its image lands at the cursor
    /// when its ticket is running and the box holds its draft, its notice
    /// shows, and anything else is dropped silently.
    pub(crate) fn on_image(&mut self, ticket: u64, result: Result<String, String>) {
        match self.paste.land(ticket, self.draft.serial(), result) {
            super::paste::Landed::Image(data) => {
                self.draft.insert_image(data);
                self.edited();
                self.settle();
            }
            super::paste::Landed::Notice(notice) => {
                self.notices.push("io_failed", notice);
                self.settle();
            }
            super::paste::Landed::Dropped => {}
        }
    }

    /// A feedback report finished writing: the path with the issue link
    /// shows, and the draft clears. A failure keeps the draft with its
    /// notice, as a rejected command does.
    pub(crate) fn on_feedback(&mut self, result: Result<crate::FeedbackDone, String>) {
        match result {
            Ok(done) => {
                self.draft.clear();
                self.notices.push(
                    "feedback",
                    format!("Wrote {}\n{}", done.path.display(), done.link),
                );
            }
            Err(notice) => {
                self.notices.push("feedback", notice);
            }
        }
        self.settle();
    }
}

#[cfg(test)]
#[path = "commands_testkit.rs"]
pub(crate) mod commands_testkit;

#[cfg(test)]
#[path = "exit_alias_tests.rs"]
mod exit_alias;

#[cfg(test)]
#[path = "aliases_tests.rs"]
mod aliases;

#[cfg(test)]
#[path = "diff_command_tests.rs"]
mod diff_command;

#[cfg(test)]
#[path = "status_command_tests.rs"]
mod status_command;

#[cfg(test)]
#[path = "copy_command_tests.rs"]
mod copy_command;

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
