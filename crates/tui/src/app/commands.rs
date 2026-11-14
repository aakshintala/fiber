//! The input box's completion panels, the commands Enter and Esc send,
//! the built-in commands and the key map overlay (`docs/tui.md`, "Keys",
//! "Bindings", "Slash commands", "Quit").

use std::path::PathBuf;
use std::time::Instant;

use contract::ErrorCode;
use contract::commands::hub::HubCommand;
use contract::commands::{
    AddDirectoryArgs, ChangeWorkspaceArgs, CloseArgs, Command, Compact, ContentArgs, ImportArgs,
    Name, RunCommand, SandboxArgs,
};
use contract::events::CommandAccepted;
use contract::{CommandId, SessionId};

use super::{App, Effect, Kind, Link, Phase, Sent, Surface, hub_line, session_line};
use crate::editor::{OpenFile, Target};
use crate::ext::KeyOutcome;
use crate::import_report::parse_import;
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
/// image parts in order, each run one text part.
pub(super) fn content_arg(draft: &Draft) -> ContentArgs {
    ContentArgs {
        content: draft.content(),
    }
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
        "rules" => &["permissions"],
        "close" => &["stop"],
        "name" => &["rename"],
        "feedback" => &["bug"],
        "quit" => &["exit"],
        "delegates" => &["agents"],
        "jobs" => &["tasks", "ps"],
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
        name: "hooks",
        description: "Opens the hooks view.",
        hint: None,
        run: run_hooks,
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
        name: "doctor",
        description: "Opens the doctor report.",
        hint: None,
        run: run_doctor,
    },
    CommandEntry {
        name: "diff",
        description: "Opens the Changed files view.",
        hint: None,
        run: run_diff,
    },
    CommandEntry {
        name: "release-notes",
        description: "Opens the running version's release notes.",
        hint: None,
        run: run_release_notes,
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
        name: "add-dir",
        description: "Adds an existing folder to the session's boundary.",
        hint: Some("<path>"),
        run: run_add_dir,
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
        name: "cd",
        description: "Moves the session to another workspace.",
        hint: Some("<path>"),
        run: run_cd,
    },
    CommandEntry {
        name: "fork",
        description: "Rewinds to the end of the conversation.",
        hint: None,
        run: run_fork,
    },
    CommandEntry {
        name: "compact",
        description: "Starts a compaction.",
        hint: Some("[instructions]"),
        run: run_compaction,
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
        name: "sandbox",
        description: "Turns the session sandbox on or off.",
        hint: Some("[on|off]"),
        run: run_sandbox,
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
        name: "archive",
        description: "Stops the session and its cascade as /delete does, then archives that one session and goes home, asking nothing.",
        hint: None,
        run: run_archive,
    },
    CommandEntry {
        name: "delete",
        description: "Stops, then deletes the session on screen.",
        hint: None,
        run: run_delete,
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
    CommandEntry {
        name: "jobs",
        description: "Opens the running jobs view: every running job of the session on screen, in the order they started.",
        hint: None,
        run: run_jobs,
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

fn run_compaction(app: &mut App, rest: &str) -> Option<Effect> {
    let instructions = (!rest.is_empty()).then(|| rest.to_owned());
    Some(
        app.send_command(Command::Compact(Compact { instructions }))
            .0,
    )
}

fn run_name(app: &mut App, rest: &str) -> Option<Effect> {
    Some(
        app.send_command(Command::Name(Name {
            text: rest.to_owned(),
        }))
        .0,
    )
}

pub(super) fn run_reload(app: &mut App, _rest: &str) -> Option<Effect> {
    // A reloaded extension draws again: its next failure reports anew.
    app.renderer_reported.clear();
    if let Some(loader) = app.home.launch.extensions.clone() {
        let (renderer, notices) = loader.load();
        app.use_renderer(renderer);
        for (code, text) in notices {
            app.push_notice(ErrorCode::from_name(&code), text);
        }
    }
    Some(app.send_command(Command::Reload).0)
}

/// `/sandbox on|off`: sends the session sandbox switch, `on` or `off`
/// case-insensitively. Bare or anything else keeps the draft and shows the
/// usage, sending nothing (`docs/invocation.md`, "What each command does").
fn run_sandbox(app: &mut App, rest: &str) -> Option<Effect> {
    let enabled = if rest.trim().eq_ignore_ascii_case("on") {
        true
    } else if rest.trim().eq_ignore_ascii_case("off") {
        false
    } else {
        app.push_notice(ErrorCode::Usage, "/sandbox needs on or off.".to_owned());
        app.draft.clear();
        app.draft.paste("/sandbox ");
        return Some(Effect::None);
    };
    Some(
        app.send_command(Command::Sandbox(SandboxArgs { enabled }))
            .0,
    )
}

fn run_import(app: &mut App, rest: &str) -> Option<Effect> {
    // A bare `/import` imports both sources; a named source imports
    // that one; anything else is refused locally naming the two, with
    // the draft kept and nothing sent — the same outcome as the
    // session's refusal, without a round trip. The source rides beside
    // the pending entry under the command's id, so the accepted
    // answer's report names it; a bare import reports the generic
    // title.
    let rest = rest.trim();
    let Some(source) = parse_import(rest) else {
        app.push_notice(
            ErrorCode::InvalidArguments,
            "`source` must be `claude-code` or `codex`.".to_owned(),
        );
        return Some(Effect::None);
    };
    let (effect, _) = app.send_kind(Kind::Import(source), Command::Import(ImportArgs { source }));
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

fn run_delete(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.delete_on_screen())
}

fn run_archive(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.archive_on_screen())
}

/// `/feedback`: with no description, asks for one by leaving the draft
/// on `/feedback `; with a description, the loop writes the report off
/// its thread through the injected writer, and the answer lands as a
/// notice.
fn run_feedback(app: &mut App, rest: &str) -> Option<Effect> {
    let description = rest.trim();
    if description.is_empty() {
        app.push_notice(
            ErrorCode::Other("feedback".to_owned()),
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

fn run_hooks(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_session_view(super::SessionView::Hooks))
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

fn run_cd(app: &mut App, rest: &str) -> Option<Effect> {
    Some(app.send_change_workspace(rest))
}

impl App {
    /// Sends `change_workspace` with `rest` as the path: with no path
    /// the notice says so and nothing goes out, leaving the draft for
    /// the path. A nonempty path goes out unchanged as the JSON string,
    /// never evaluated as a shell command. Records the session on
    /// screen, so a late answer after a switch elsewhere never attaches.
    fn send_change_workspace(&mut self, rest: &str) -> Effect {
        if rest.is_empty() {
            self.push_notice(ErrorCode::Usage, "/cd needs a path.".to_owned());
            return Effect::None;
        }
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        let (effect, id) = self.send_command(Command::ChangeWorkspace(ChangeWorkspaceArgs {
            path: rest.to_owned(),
        }));
        if let Some(id) = id {
            self.outbox
                .wait(&id, super::outbox::Waiting::ChangeWorkspace(session));
        }
        effect
    }
}

fn run_fork(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    let Phase::Attached { .. } = &app.phase else {
        app.push_notice(ErrorCode::SessionNotFound, NO_SESSION.to_owned());
        return Some(Effect::None);
    };
    Some(app.send_fork())
}

fn run_delegates(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_session_view(super::SessionView::AllDelegates))
}

/// `/jobs` (`/tasks` and `/ps` run the same command): the running jobs
/// list, the same list the panel's Jobs card opens. An attached session
/// with no running job gets the `No jobs running` notice and no view;
/// with no session the shared no-session notice answers instead. Words
/// after the command are ignored, as `/delegates` ignores them.
fn run_jobs(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    // No session first: the shared no-session notice path answers, and
    // the empty-jobs check only judges an attached session.
    if app.session().is_none() {
        return Some(app.open_session_view(super::SessionView::Jobs));
    }
    if app.panel_state().running_jobs().is_empty() {
        app.push_notice(ErrorCode::NoJobsRunning, "No jobs running".to_owned());
        return Some(Effect::None);
    }
    Some(app.open_session_view(super::SessionView::Jobs))
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

fn run_doctor(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_config_view(super::ConfigView::Doctor))
}

fn run_diff(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_session_view(super::SessionView::ChangedFiles))
}

fn run_release_notes(app: &mut App, _rest: &str) -> Option<Effect> {
    app.draft.clear();
    Some(app.open_session_view(super::SessionView::ReleaseNotes))
}

fn run_copy(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.copy_last_reply())
}

/// `/add-dir`: with a path, sends `add_directory` with it; with none,
/// asks for one by leaving the draft on `/add-dir `.
fn run_add_dir(app: &mut App, rest: &str) -> Option<Effect> {
    let path = rest.trim();
    if path.is_empty() {
        app.push_notice(
            ErrorCode::Other("add-dir".to_owned()),
            "Add a folder after /add-dir, then press Enter.".to_owned(),
        );
        app.draft.clear();
        app.draft.paste("/add-dir ");
        return Some(Effect::None);
    }
    Some(
        app.send_command(Command::AddDirectory(AddDirectoryArgs {
            path: path.to_owned(),
        }))
        .0,
    )
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
        app.push_notice(ErrorCode::SessionNotFound, NO_SESSION.to_owned());
        return Some(Effect::None);
    };
    if let Some(workspace) = app.panel_state.workspace() {
        app.push_notice(ErrorCode::Workspace, workspace.to_owned());
    } else {
        app.pwd_pending = true;
    }
    Some(Effect::None)
}

fn run_rules(app: &mut App, _rest: &str) -> Option<Effect> {
    Some(app.open_config_view(super::ConfigView::Rules))
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
        let result = self.on_key_inner(key, now);
        self.settle();
        result
    }

    /// Handles the route without settling; the caller owns the layout pass.
    pub(crate) fn on_key_inner(&mut self, key: Key, now: Instant) -> Effect {
        if !matches!(
            key,
            Key::Up | Key::PageUp | Key::PageDown | Key::End | Key::CtrlO
        ) {
            self.history.cancel();
        }
        let effect = self.route_key(key, now);
        self.edited();
        effect
    }

    /// Handles one key that edits the draft, the approval panel first.
    /// Nothing while the key map is open, while navigating with the panel
    /// closed, or in the Ctrl+R panel. A recall waiting for a page waits
    /// no more.
    #[cfg(test)]
    pub(crate) fn on_edit(&mut self, edit: Edit) -> Effect {
        let result = self.on_edit_inner(edit);
        self.settle();
        result
    }

    /// Handles the route without settling; the caller owns the layout pass.
    pub(crate) fn on_edit_inner(&mut self, edit: Edit) -> Effect {
        self.armed_at = None;
        self.copy_owed = false;
        self.history.cancel();
        // The quit question and the import report own every key through
        // `route_key`, so a paste or a home-owned arrow reaching here
        // changes neither the draft nor the surface behind them. They run
        // ahead of the picker, the chip row and the draft, as their keys
        // run ahead of every other key path. The delete question swallows
        // edits too, over the attached session as on home.
        let top = self.top_surface();
        if matches!(
            top,
            Surface::Quit | Surface::Clear | Surface::Report | Surface::DeleteQuestion
        ) {
            return Effect::None;
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
        self.query_changed()
    }

    /// A key the draft takes: a character or Backspace, which may open or
    /// search the `@` panel, or ↑ ↓ by wrapped row and then through earlier
    /// prompts. `None` for any other key. While a read-only session is
    /// attached the draft takes nothing, so typing, Backspace, recall
    /// and `@` search change nothing and no completion can open.
    pub(super) fn draft_key(&mut self, key: &Key) -> Option<Effect> {
        if self.unwritable().is_some() {
            return None;
        }
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
            | Key::ShiftUp
            | Key::ShiftDown
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
        self.home_workspace()
    }

    /// The current search generation.
    pub(crate) fn generation(&self) -> u64 {
        self.completion.generation
    }

    /// Runs the TUI winner for `name` with `rest`: a handled command
    /// clears the draft and sends nothing; anything else is consumed
    /// silently with the draft preserved — the failure already has its
    /// notice, and a conflict's merge notice stands. A TUI name never
    /// reaches the driver command path.
    fn run_tui_command(&mut self, name: &str, rest: &str) -> Effect {
        let ctx = self.command_ctx();
        let outcome = self.handle.get().map(|renderer| {
            let outcome = renderer.run_tui_command(&ctx, name, rest.trim());
            for (code, text) in renderer.take_pending() {
                self.push_notice(ErrorCode::from_name(&code), text);
            }
            outcome
        });
        if outcome == Some(KeyOutcome::Handled) {
            self.draft.clear();
        }
        Effect::None
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
        if self.completion.tui_conflicts.contains(name) {
            // A conflicted name runs nowhere: consumed, nothing sent,
            // no second notice, draft preserved.
            return Some(Effect::None);
        }
        if self
            .completion
            .tui_commands
            .iter()
            .any(|command| command.name == name)
        {
            return Some(self.run_tui_command(name, rest));
        }
        let extension = self
            .completion
            .extensions
            .iter()
            .find(|known| known.as_str() == name)
            .or_else(|| {
                let entry = TABLE
                    .iter()
                    .find(|entry| aliases_of(entry.name).contains(&name))?;
                self.completion
                    .extensions
                    .iter()
                    .find(|known| known.as_str() == entry.name)
            });
        if let Some(extension) = extension {
            let rest = rest.trim();
            let text = (!rest.is_empty()).then(|| rest.to_owned());
            return Some(
                self.send_command(Command::Command(RunCommand {
                    name: (*extension).clone(),
                    text,
                }))
                .0,
            );
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
        if self.reset_session() {
            self.attached_screen_mut().clear();
        }
    }

    /// The missing workspace path when the attached session is read
    /// only: the hub replays its log instead of resuming it
    /// (`docs/invocation.md`, "Lifecycle"). `None` anywhere else, so
    /// the read-only bar draws only for a read-only attach and no
    /// prompt ever leaves one.
    pub(crate) fn unwritable(&self) -> Option<&str> {
        match &self.phase {
            Phase::Attached { session, .. } => self.unwritable_session(session),
            Phase::Starting | Phase::Pending { .. } => None,
        }
    }

    /// Resets the screen before a session without clearing its rows,
    /// so a reconnect keeps them for the landing: the conversation and
    /// the draft cleared everywhere else, the old session left running.
    /// While a `start` waits for its answer only the draft is cleared.
    /// False without the reset.
    pub(super) fn reset_session(&mut self) -> bool {
        self.draft.clear();
        // Leaving and going home end a read-only attach with the open
        // gate: the next open sets its own mark.
        self.clear_unwritable();
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
        self.home.came_from = None;
        self.phase = Phase::Starting;
        self.clear_selection();
        // Home keeps no session's images: a late answer or viewer
        // completion from it is dropped.
        self.forget_images();
        self.panel_state.reset();
        self.session_views_reset();
        // Replacing the whole offer retires its source view's ask, and
        // dropping the commands id forgets its wait.
        if let Some(asked) = self.offer.source_asked() {
            self.outbox.settled(&asked);
        }
        self.offer = crate::offer::Offer::default();
        self.completion.session_answer.clear();
        self.completion.extensions.clear();
        self.completion.commands_id = None;
        // The session rows go with the session; the TUI rows refresh for
        // home, which holds every loaded winner with no half gate.
        self.refresh_tui_commands();
        if self.handle.get().is_none() {
            self.merge_slash_rows();
        }
        self.outbox.forget(&super::outbox::Waiting::Commands);
        self.copy_owed = false;
        true
    }

    /// The attached session, when the command can go out: with none
    /// attached, a notice and the draft cleared; with the link not up, the
    /// draft stays.
    pub(super) fn command_session(&mut self) -> Option<(contract::SessionId, bool)> {
        let Phase::Attached { session, busy } = &self.phase else {
            self.notices
                .push(ErrorCode::SessionNotFound, NO_SESSION.to_owned());
            self.draft.clear();
            return None;
        };
        (self.outbox.is_up()).then(|| (session.clone(), *busy))
    }

    /// Sends `command` to the attached session, returning the minted
    /// command id beside the effect for callers recording per-command
    /// state. A rejection returns the draft.
    fn send_command(&mut self, command: Command) -> (Effect, Option<String>) {
        self.send_kind(Kind::Command, command)
    }

    /// Sends `command` of `kind` to the attached session, returning the
    /// minted command id beside the effect. A rejection returns the draft.
    fn send_kind(&mut self, kind: Kind, command: Command) -> (Effect, Option<String>) {
        let Some((session, _)) = self.command_session() else {
            return (Effect::None, None);
        };
        let id = support::mint("c_");
        let line = session_line(&contract::CommandId(id.clone()), &session, command);
        let draft = std::mem::take(&mut self.draft);
        self.outbox.insert(id.clone(), kind, draft);
        (Effect::Send(vec![line]), Some(id))
    }

    /// `/close`: one `close` with `now`, whether the session is busy or
    /// idle, then home.
    fn close(&mut self) -> Effect {
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        let id = support::mint("c_");
        let lines = vec![session_line(
            &contract::CommandId(id.clone()),
            &session,
            Command::Close(CloseArgs { now: true }),
        )];
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
    /// Until it answers, the list holds the built-ins and the TUI
    /// commands only.
    pub(super) fn ask_commands(&mut self, session: &SessionId) -> String {
        let line = self.completion.ask_commands(session);
        self.merge_slash_rows();
        if let Some(id) = &self.completion.commands_id {
            self.outbox.wait(id, super::outbox::Waiting::Commands);
        }
        line
    }

    /// A session's `command_accepted`: the answer to the latest `commands`
    /// sent replaces the session's rows of the `/` list, re-resolved
    /// against the TUI commands.
    pub(super) fn commands_answered(&mut self, accepted: &CommandAccepted) {
        if self.completion.commands_answered(accepted) {
            self.merge_slash_rows();
        }
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
        // A conflicted name has no row: without this `on_enter` refuses
        // it as unknown before `built_in` consumes it.
        if self.completion.tui_conflicts.contains(name) {
            return false;
        }
        self.push_notice(
            ErrorCode::UnknownCommand,
            format!("Unknown command /{name}. Type / for the list."),
        );
        true
    }

    /// Starts the home session requested by a settings screen, using the
    /// same gates and start builder as Enter without requiring prompt text.
    pub(super) fn start_session_for_settings(&mut self) -> Effect {
        if self.first_run_no_credential && self.on_home() {
            return self.open_first_run_login();
        }
        if self.home.no_workspace() {
            return self.home_click(crate::home::Spot::Workspace);
        }
        if matches!(self.outbox.link, Link::Down | Link::Refused) {
            return Effect::None;
        }
        self.start_session()
    }

    /// Starts the session with the current home selection, keeping the
    /// normal session-only model and failure handling.
    fn start_session(&mut self) -> Effect {
        if self.home.launch.model.is_none() && self.model_picker.start_model.is_none() {
            return self.open_model_picker(crate::model_picker::Mode::Choose);
        }
        let Some(mut start) = self.start_args() else {
            return Effect::None;
        };
        self.with_start_model(&mut start);
        self.panel_state
            .note_start(&serde_json::to_value(&start).unwrap_or(serde_json::Value::Null));
        let id = CommandId(support::mint("c_"));
        let sent = Sent {
            line: hub_line(&id, HubCommand::Start(start)),
            id: id.clone(),
        };
        self.phase = Phase::Pending {
            command_id: sent.id.0.clone(),
        };
        let draft = std::mem::take(&mut self.draft);
        match self
            .outbox
            .send(sent.id.0.clone(), Kind::Start, draft, sent.line.clone())
        {
            Some(_) => Effect::SendTracked(vec![sent]),
            None => Effect::None,
        }
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
        if self.first_run_no_credential && self.on_home() {
            return self.open_first_run_login();
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
        if matches!(self.phase, Phase::Starting) && self.home.no_workspace() {
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
        let id = support::mint("c_");
        let args = content_arg(&self.draft);
        let shell = (!has_image).then(|| shell::parse(&text)).flatten();
        // A read-only session emits nothing new: no prompt, no steer,
        // no shell run. The bar below is the only affordance.
        if matches!(&self.phase, Phase::Attached { session, .. } if self.unwritable_session(session).is_some())
        {
            return Effect::None;
        }
        let (kind, line) = match (&self.phase, shell) {
            (Phase::Starting | Phase::Pending { .. }, Some(_)) => {
                self.notices.push(
                    ErrorCode::SessionNotFound,
                    "Start a session first.".to_owned(),
                );
                return Effect::None;
            }
            (Phase::Pending { .. }, None) => return Effect::None,
            (Phase::Starting, None) => return self.start_session(),
            (Phase::Attached { session, .. }, Some((command, send))) => {
                (Kind::Shell, shell::command(&id, session, command, send))
            }
            (Phase::Attached { session, busy }, None) => {
                let (kind, command) = if *busy {
                    (Kind::Steer, Command::Steer(args))
                } else {
                    (Kind::Prompt, Command::Prompt(args))
                };
                (
                    kind,
                    session_line(&contract::CommandId(id.clone()), session, command),
                )
            }
        };
        if kind == Kind::Start {
            self.phase = Phase::Pending {
                command_id: id.clone(),
            };
        }
        let draft = std::mem::take(&mut self.draft);
        // A `!!` run has no line on the session stream: its row starts
        // here, where the terminal sends it, and its answer ends it.
        let show_only = match (&self.phase, shell) {
            (Phase::Attached { session, .. }, Some((command, false))) => {
                Some((session.clone(), shell::live_head(command, false)))
            }
            (Phase::Starting | Phase::Pending { .. } | Phase::Attached { .. }, _) => None,
        };
        let run = id.clone();
        if kind == Kind::Shell {
            self.outbox.wait(&id, super::outbox::Waiting::Shell);
        }
        match self.outbox.send(id, kind, draft, line) {
            Some(line) => {
                if let Some((session, head)) = show_only {
                    self.fold_pages(&session).start_shell(&run, head);
                }
                Effect::Send(vec![line])
            }
            None => Effect::None,
        }
    }

    /// The first prompt of a session `start` made, carrying `draft`, sent
    /// after its `subscribe` so the session counts this terminal before
    /// the prompt. A rejection returns the draft to an empty box.
    pub(super) fn first_prompt(&mut self, session: &SessionId, draft: Draft) -> String {
        let id = support::mint("c_");
        let line = session_line(
            &contract::CommandId(id.clone()),
            session,
            Command::Prompt(content_arg(&draft)),
        );
        self.outbox.insert(id, Kind::Prompt, draft);
        line
    }

    /// Esc with nothing open goes home from a read-only session and
    /// interrupts the turn otherwise: `cancel`, only when busy.
    pub(super) fn on_esc(&mut self) -> Effect {
        // A read-only session takes no prompt, so Esc leaves it the way
        // the go-home action does.
        if let Phase::Attached { session, .. } = &self.phase
            && self.unwritable_session(session).is_some()
        {
            return self.on_action("go_home");
        }
        let session = match &self.phase {
            Phase::Attached {
                session,
                busy: true,
            } if self.connected() => session.clone(),
            Phase::Starting | Phase::Pending { .. } | Phase::Attached { .. } => {
                return Effect::None;
            }
        };
        self.send_cancel(&session)
    }

    /// Interrupts the turn of the conversation on screen, whatever is open
    /// over it: a delegate's own in its view, else the attached session's.
    /// It closes nothing and leaves the draft and the steering queue.
    pub(super) fn interrupt_turn(&mut self) -> Effect {
        match self.interrupt_session() {
            Some(session) => self.send_cancel(&session),
            None => Effect::None,
        }
    }

    /// One `cancel` of `session`'s turn, settled by its answer.
    fn send_cancel(&mut self, session: &SessionId) -> Effect {
        let id = support::mint("c_");
        let line = session_line(&contract::CommandId(id.clone()), session, Command::Cancel);
        self.outbox.insert(id, Kind::Cancel, Draft::default());
        Effect::Send(vec![line])
    }

    /// Ctrl+G: the Changed files row or diff at its first change, or the
    /// paste token beside the cursor, or else the whole draft, every token
    /// expanded. A recall waiting for a page waits no more.
    pub(super) fn open_in_editor(&mut self) -> Effect {
        if self.changed_files_open() {
            if self.remote().is_some() {
                // A remote terminal's files live on the hub: fetch the
                // hub file at the session's workspace and edit a
                // temporary copy, written back on return.
                return self.open_remote_changed_file();
            }
            return match self.changed_file_for_editor() {
                Some((path, line)) => Effect::OpenFile(OpenFile {
                    path,
                    line: Some(line),
                }),
                None => Effect::None,
            };
        }
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
        self.editor_returned_inner(target, result);
        self.settle();
    }

    /// Handles the route without settling; the caller owns the layout pass.
    pub(crate) fn editor_returned_inner(&mut self, target: Target, result: Result<String, String>) {
        match (result, target) {
            (Err(notice), _) => self.notices.push(ErrorCode::IoFailed, notice),
            (Ok(text), Target::Token(number)) => self.draft.set_token(number, &text),
            (Ok(text), Target::Draft) => self.draft.edited(&text),
            (Ok(_), Target::Item) => {}
        }
        self.completion.selected = 0;
        self.edited();
    }

    /// A clipboard image read finished: its image lands at the cursor
    /// when its ticket is running and the box holds its draft, its notice
    /// shows, and anything else is dropped silently.
    #[cfg(test)]
    pub(crate) fn on_image(&mut self, ticket: u64, result: Result<String, String>) {
        self.on_image_inner(ticket, result);
        self.settle();
    }

    /// Handles the route without settling; the caller owns the layout pass.
    pub(crate) fn on_image_inner(&mut self, ticket: u64, result: Result<String, String>) {
        match self.paste.land(ticket, self.draft.serial(), result) {
            super::paste::Landed::Image(data) => {
                self.draft.insert_image(data);
                self.edited();
            }
            super::paste::Landed::Notice(notice) => {
                self.notices.push(ErrorCode::IoFailed, notice);
            }
            super::paste::Landed::Dropped => {}
        }
    }

    /// A feedback report finished writing: the path with the issue link
    /// shows, and the draft clears. A failure keeps the draft with its
    /// notice, as a rejected command does.
    #[cfg(test)]
    pub(crate) fn on_feedback(&mut self, result: Result<crate::FeedbackDone, String>) {
        self.on_feedback_inner(result);
        self.settle();
    }

    /// Handles the route without settling; the caller owns the layout pass.
    pub(crate) fn on_feedback_inner(&mut self, result: Result<crate::FeedbackDone, String>) {
        match result {
            Ok(done) => {
                self.draft.clear();
                self.notices.push(
                    ErrorCode::Other("feedback".to_owned()),
                    format!("Wrote {}\n{}", done.path.display(), done.link),
                );
            }
            Err(notice) => {
                self.notices
                    .push(ErrorCode::Other("feedback".to_owned()), notice);
            }
        }
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
