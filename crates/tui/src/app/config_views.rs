//! The configuration views on the app (`docs/tui.md`, "Swapped views"):
//! which one is open, the seam they read and write through, the theme a
//! choice queued for the loop, and the last call's prompt size, which a
//! reload's cache rebuild costs. One view is open at a time; while it is,
//! it takes every key but Ctrl+C.

mod credential;

use std::path::PathBuf;
use std::sync::Arc;

use contract::events::UsageRecorded;
use contract::events::{CommandAccepted, CommandRejected, CommandResult};
use contract::{Envelope, SessionId};
use serde_json::{Value, json};

use super::{App, Effect, doctor_view};
use crate::ThemeSetting;
use crate::configure::{Configure, ConfigureError, Layer, Saved};
use crate::keys::{Edit, Key};
use crate::login_view::Login;
use crate::login_worker::{LoginJob, LoginStart, LoginStep, LoginTicket, LoginWorker};
use crate::logout_view::Logout;
use crate::mcp_view::Mcp;
use crate::rules_view::Rules;
use crate::settings_view::{Act, BUILT_IN_THEMES, Ctx, Settings, THEME};
use crate::skills_view::Skills;
use crate::swapped::{Frame, List, Spot};
use crate::tools_view::Tools;

/// What a view says when the terminal was given no seam.
const UNAVAILABLE: &str = "Not available in this terminal.";

/// A configuration view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigView {
    /// `/settings`.
    Settings,
    /// `/theme`: the theme picker, opened on its choices.
    Theme,
    /// `/tools`.
    Tools,
    /// `/skills`.
    Skills,
    /// `/rules`.
    Rules,
    /// `/login`.
    Login,
    /// `/logout`: built by the slash table below.
    Logout,
    /// `/mcp`.
    Mcp,
    /// `/credential`.
    Credential,
    /// `/doctor`: the report `fiber doctor` prints.
    Doctor,
}

/// The open view.
#[derive(Debug)]
enum Open {
    /// `/settings`.
    Settings(Settings),
    /// `/tools`.
    Tools(Tools),
    /// `/skills`.
    Skills(Skills),
    /// `/rules`.
    Rules(Rules),
    /// `/login`.
    Login(Login),
    /// `/logout`.
    Logout(Logout),
    /// `/mcp`.
    Mcp(Mcp),
    /// `/credential`.
    Credential(credential::Picker),
    /// `/doctor`: the scrolled list and the snapshot lines.
    Doctor(doctor_view::Doctor),
    /// A view with no seam to read through, by its title and the
    /// command that opened it.
    Unavailable(&'static str, &'static str),
}

impl Open {
    /// The asking view's rejection: only for the id of the command it
    /// sent. Every other view keeps what it shows.
    fn rejected(&mut self, id: &str, message: &str) {
        match self {
            Open::Tools(tools) => tools.rejected(id, message),
            Open::Skills(skills) => skills.rejected(id, message),
            Open::Mcp(mcp) => mcp.rejected(id, message),
            Open::Credential(picker) => picker.rejected(id, message),
            // `/logout` and `/doctor` send nothing: neither waits on
            // an answer.
            Open::Logout(_) | Open::Doctor(..) => {}
            Open::Settings(_) | Open::Rules(_) | Open::Login(_) | Open::Unavailable(..) => {}
        }
    }
}

/// `/tools` waiting for the `tools` command `id` sent.
fn build_tools(id: String, ctx: &Ctx<'_>) -> Open {
    Open::Tools(Tools::open(id, ctx))
}

/// `/mcp` waiting for the `mcp_servers` command `id` sent.
fn build_mcp(id: String, _ctx: &Ctx<'_>) -> Open {
    Open::Mcp(Mcp::open(id))
}

/// `/skills` waiting for the `skills` command `id` sent.
fn build_skills(id: String, ctx: &Ctx<'_>) -> Open {
    Open::Skills(Skills::open(id, ctx))
}

/// The configuration views' state.
#[derive(Default)]
pub(in crate::app) struct ConfigViews {
    /// The seam `main` passed, if any.
    seam: Option<Arc<dyn Configure>>,
    /// Hub-only operations; the configuration seam also points here remotely.
    remote: Option<Arc<crate::remote::Remote>>,
    /// The model-list reader the loop thread and the start pick read
    /// through: the same function take_reads hands the thread.
    models: Option<crate::ReadModels>,
    /// The feedback report writer `main` injected: the loop thread
    /// writes through it, and none writes nothing.
    feedback: Option<crate::WriteFeedback>,
    /// The doctor report reader `main` injected: the `/doctor` view
    /// shows its snapshot, and none opens the Unavailable shape.
    doctor: Option<contract::feedback::ReadDoctor>,
    /// The sole owner of an outstanding hub editor copy.
    copy: Option<crate::remote::RemoteCopy>,
    /// The open view.
    open: Option<Open>,
    /// A theme chosen and not yet applied by the loop.
    theme: Option<ThemeSetting>,
    /// The latest call's prompt size in tokens, and its session.
    usage: Option<(SessionId, u64)>,
    /// The browser login tickets handed out, in order: incremented before
    /// use, so the first ticket is 1. Tickets stay distinct until the
    /// counter saturates at `u64::MAX`, which is then handed out again
    /// for every later start.
    tickets: LoginTicket,
    /// The browser login start the loop has not picked up yet.
    pending_login: Option<LoginStart>,
    /// The newest credential query and saving switch awaiting answers.
    credential: credential::Waits,
}

impl App {
    /// Sets the seam the views read and write through.
    pub(crate) fn set_configure(&mut self, seam: Option<Arc<dyn Configure>>) {
        self.config_views.seam = seam;
    }

    /// Sets the model-list reader the start pick reads through.
    pub(crate) fn set_models(&mut self, models: Option<crate::ReadModels>) {
        self.config_views.models = models;
    }

    /// Sets the feedback report writer the loop writes through.
    pub(crate) fn set_feedback(&mut self, writer: Option<crate::WriteFeedback>) {
        self.config_views.feedback = writer;
    }

    /// Sets the doctor report reader the `/doctor` view shows.
    pub(crate) fn set_doctor(&mut self, reader: Option<contract::feedback::ReadDoctor>) {
        self.config_views.doctor = reader;
    }

    /// The feedback report writer, absent with nothing to write through.
    pub(crate) fn feedback_writer(&self) -> Option<crate::WriteFeedback> {
        self.config_views.feedback.clone()
    }

    /// The model-list reader, absent with no list to read.
    pub(in crate::app) fn models_reader(&self) -> Option<crate::ReadModels> {
        self.config_views.models.clone()
    }

    /// Installs the hub seam and its remote editor and search operations.
    pub(crate) fn set_remote(&mut self, remote: Arc<crate::remote::Remote>) {
        self.config_views.seam = Some(Arc::clone(&remote) as Arc<dyn Configure>);
        self.config_views.remote = Some(remote);
    }

    /// The remote operations, absent on local terminals.
    pub(crate) fn remote(&self) -> Option<Arc<crate::remote::Remote>> {
        self.config_views.remote.clone()
    }

    /// The seam the views and the model picker write through, if any.
    pub(crate) fn configure_seam(&self) -> Option<Arc<dyn Configure>> {
        self.config_views.seam.clone()
    }

    /// Runs `/theme <name>`: saves the named theme to the global file
    /// and applies it at once, without opening the picker. The whole
    /// name is matched exactly against the built-ins and the seam's
    /// file themes, so `solar dawn` is one theme and `Light` is
    /// unknown; an unknown name is refused with a notice naming it,
    /// and nothing changes. With no seam, or nothing addressable, a
    /// notice says so and nothing changes.
    pub(in crate::app) fn theme_command(&mut self, name: &str) -> Effect {
        self.draft.clear();
        let Some(seam) = self.configure_seam() else {
            self.push_notice("io_failed", UNAVAILABLE.to_owned());
            return Effect::None;
        };
        let Some(workspace) = self.view_workspace() else {
            // A failed remote read already pushed its notice; a local
            // terminal with no workspace says the view is unavailable.
            if self.remote().is_none() {
                self.push_notice("io_failed", UNAVAILABLE.to_owned());
            }
            return Effect::None;
        };
        let known = BUILT_IN_THEMES
            .iter()
            .map(|name| (*name).to_owned())
            .chain(seam.themes(&workspace));
        if !known.into_iter().any(|known| known == name) {
            self.push_notice("invalid_arguments", format!("Unknown theme \"{name}\"."));
            return Effect::None;
        }
        let text = Value::String(name.to_owned()).to_string();
        match self.write_global(THEME, &text) {
            Some(Ok(_)) => self.config_act(Act::Theme(seam.theme(&workspace, name))),
            Some(Err(error)) => {
                self.push_notice(
                    "io_failed",
                    format!("Could not save {THEME}: {}", error.message),
                );
                Effect::None
            }
            None => {
                self.push_notice("io_failed", UNAVAILABLE.to_owned());
                Effect::None
            }
        }
    }

    /// Saves one setting's text to the global configuration through the
    /// seam, as `fiber config set` would (`docs/tui.md`, "Layout",
    /// "The rail"). With no seam nothing is saved; a failed save and
    /// each warning is a notice, so it shows on the frame drawn next,
    /// keeping the live width. With no local workspace the write goes to
    /// the hub-global file.
    pub(crate) fn save_setting(&mut self, key: &str, value: &str) {
        let Some(result) = self.write_global(key, value) else {
            return;
        };
        match result {
            Ok(saved) => {
                for warning in saved.warnings {
                    self.push_notice("config_invalid", warning);
                }
            }
            Err(error) => self.push_notice(
                "io_failed",
                format!("Could not save {key}: {}", error.message),
            ),
        }
    }

    /// The workspace a configuration view reads: the one in use, or the
    /// hub-global parent with no local workspace. `None` when the
    /// hub-global read fails (after its notice) or a local terminal names
    /// none.
    fn view_workspace(&mut self) -> Option<PathBuf> {
        if self.installing() {
            self.push_notice("io_failed", Self::INSTALLING_TEXT.to_owned());
            return None;
        }
        match self.workspace() {
            Some(workspace) => Some(workspace),
            None => {
                let remote = self.remote()?;
                match remote.global_parent() {
                    Ok(parent) => Some(parent),
                    Err(error) => {
                        self.push_notice(
                            "io_failed",
                            format!("Could not load configuration: {}", error.message),
                        );
                        None
                    }
                }
            }
        }
    }

    /// Writes one global setting: at the workspace in use through the
    /// seam, or at the hub-global parent with no local workspace. `None`
    /// with no seam or nothing to address; the caller keeps its own
    /// no-seam notice.
    pub(in crate::app) fn write_global(
        &self,
        key: &str,
        text: &str,
    ) -> Option<Result<Saved, ConfigureError>> {
        let seam = self.config_views.seam.clone()?;
        if self.installing() {
            return Some(Err(ConfigureError {
                code: contract::ErrorCode::IoFailed,
                message: Self::INSTALLING_TEXT.to_owned(),
            }));
        }
        Some(match self.workspace() {
            Some(workspace) => seam.set(&workspace, Layer::Global, key, text),
            // With no local workspace the hub-global parent stands in
            // for the workspace: on a remote terminal the seam is the
            // remote, so this writes the hub global file, client-owned
            // keys to the local client dir, as `set` does. A local
            // terminal names none and has no remote: nothing to address.
            None => {
                let remote = self.remote()?;
                let parent = match remote.global_parent() {
                    Ok(parent) => parent,
                    Err(error) => return Some(Err(error)),
                };
                seam.set(&parent, Layer::Global, key, text)
            }
        })
    }

    /// Opens `view`, closing any open, its unsaved edit dropped. With no
    /// seam the view says it is not available.
    pub(crate) fn open_config_view(&mut self, view: ConfigView) -> Effect {
        if self.installing() {
            self.push_notice("io_failed", Self::INSTALLING_TEXT.to_owned());
            return Effect::None;
        }
        self.draft.clear();
        self.model_picker.close();
        self.close_session_view();
        self.config_views.open = None;
        let open = match (self.config_views.seam.clone(), view) {
            // `/doctor` reads a local snapshot through no seam: with no
            // reader the view says it is not available, and a capture
            // failure keeps a notice and opens nothing.
            (_, ConfigView::Doctor) => match self.config_views.doctor.clone() {
                None => Open::Unavailable("Doctor", "/doctor"),
                Some(reader) => match reader() {
                    Ok(report) => Open::Doctor(doctor_view::Doctor::open(&report)),
                    Err(text) => {
                        self.push_notice("io_failed", text);
                        return Effect::None;
                    }
                },
            },
            (_, ConfigView::Credential) => {
                let Some((id, line)) = self.send_config_command("credentials", None) else {
                    return Effect::None;
                };
                self.outbox.pending.insert(
                    id.clone(),
                    (super::Kind::Command, crate::input::Draft::default()),
                );
                self.config_views.open = Some(Open::Credential(credential::Picker::open(id)));
                return Effect::Send(vec![line]);
            }
            (Some(seam), ConfigView::Settings) => {
                let Some(workspace) = self.view_workspace() else {
                    return Effect::None;
                };
                Open::Settings(Settings::open(&self.config_ctx(seam.as_ref(), &workspace)))
            }
            (Some(seam), ConfigView::Theme) => {
                let Some(workspace) = self.view_workspace() else {
                    return Effect::None;
                };
                Open::Settings(Settings::open_theme(
                    &self.config_ctx(seam.as_ref(), &workspace),
                ))
            }
            (Some(seam), ConfigView::Tools) => {
                return self.open_asking(seam, "tools", build_tools);
            }
            (Some(seam), ConfigView::Skills) => {
                return self.open_asking(seam, "skills", build_skills);
            }
            (Some(seam), ConfigView::Rules) => {
                let Some(workspace) = self.view_workspace() else {
                    return Effect::None;
                };
                Open::Rules(Rules::open(&self.config_ctx(seam.as_ref(), &workspace)))
            }
            (Some(seam), ConfigView::Mcp) => {
                return self.open_asking(seam, "mcp_servers", build_mcp);
            }
            (Some(seam), ConfigView::Login) => {
                let Some(workspace) = self.view_workspace() else {
                    return Effect::None;
                };
                Open::Login(Login::open(&self.config_ctx(seam.as_ref(), &workspace)))
            }
            (Some(seam), ConfigView::Logout) => {
                let Some(workspace) = self.view_workspace() else {
                    return Effect::None;
                };
                Open::Logout(Logout::open(&self.config_ctx(seam.as_ref(), &workspace)))
            }
            (None, ConfigView::Settings) => Open::Unavailable("Settings", "/settings"),
            (None, ConfigView::Theme) => Open::Unavailable("Themes", "/theme"),
            (None, ConfigView::Tools) => Open::Unavailable("Tools", "/tools"),
            (None, ConfigView::Skills) => Open::Unavailable("Skills", "/skills"),
            (None, ConfigView::Rules) => Open::Unavailable("Rules", "/rules"),
            (None, ConfigView::Login) => Open::Unavailable("Log in", "/login"),
            (None, ConfigView::Logout) => Open::Unavailable("Log out", "/logout"),
            (None, ConfigView::Mcp) => Open::Unavailable("MCP servers", "/mcp"),
        };
        self.config_views.open = Some(open);
        Effect::None
    }

    /// Sends `command` with `args` to the session on screen: the sent
    /// line and its id, for the caller to record and send. Nothing when
    /// no session is attached.
    fn send_config_command(
        &mut self,
        command: &str,
        args: Option<Value>,
    ) -> Option<(String, String)> {
        let (session, _) = self.command_session()?;
        let id = super::mint();
        let line = super::session_command(&id, command, &session, args).to_string();
        Some((id, line))
    }

    /// Opens a view on the session on screen that waits for its command's
    /// answer: sends `command` and opens what `build` makes of its id.
    /// With no session attached the notice is pushed and nothing opens.
    fn open_asking(
        &mut self,
        seam: Arc<dyn Configure>,
        command: &str,
        build: fn(String, &Ctx<'_>) -> Open,
    ) -> Effect {
        let Some((id, line)) = self.send_config_command(command, None) else {
            return Effect::None;
        };
        let Some(workspace) = self.view_workspace() else {
            return Effect::None;
        };
        let ctx = self.config_ctx(seam.as_ref(), &workspace);
        self.config_views.open = Some(build(id, &ctx));
        Effect::Send(vec![line])
    }

    /// Closes any open configuration view, so another swapped view can take
    /// its place.
    pub(in crate::app) fn close_config_view(&mut self) {
        self.config_views.open = None;
    }

    /// Whether a configuration view is open.
    pub(crate) fn config_view_open(&self) -> bool {
        self.config_views.open.is_some()
    }

    /// The open view's height: the attached conversation's, or the
    /// screen's on home. The frame drawn and the list's paging use one
    /// number.
    fn config_view_height(&self) -> usize {
        if self.on_home() {
            usize::from(self.screen.height())
        } else {
            self.conversation_height()
        }
    }

    /// The width the views draw in: the conversation column attached, the
    /// screen on home. Every draw after a resize wraps at the new width.
    fn config_view_width(&self) -> usize {
        // The text panes wrap at the width they draw in: the
        // conversation column attached, the screen on home.
        if self.on_home() {
            usize::from(self.screen.width())
        } else {
            usize::from(self.column_width())
        }
    }

    /// What a call needs: the seam, the workspace, the view's rows and
    /// the last call's prompt size on the session on screen.
    fn config_ctx<'a>(&self, seam: &'a dyn Configure, workspace: &'a std::path::Path) -> Ctx<'a> {
        let height = self.config_view_height();
        let width = self.config_view_width();
        let usage = self
            .config_views
            .usage
            .as_ref()
            .filter(|(session, _)| self.session() == Some(session))
            .map(|(_, tokens)| *tokens);
        Ctx {
            seam,
            workspace,
            height,
            width,
            usage,
        }
    }

    /// Hands `key` to the open view. `None` with no view open, and for
    /// Ctrl+C, which goes on to the quit flow.
    pub(in crate::app) fn config_view_key(&mut self, key: &Key) -> Option<Effect> {
        if self.config_views.open.is_none() || *key == Key::CtrlC {
            return None;
        }
        let height = self.config_view_height();
        if let Some(Open::Credential(picker)) = &mut self.config_views.open {
            let pick = picker.key(key, height);
            return Some(self.credential_pick(pick));
        }
        // `/doctor` shows a local snapshot through no seam: Esc and
        // scrolling never reach the configure seam or a workspace.
        if let Some(Open::Doctor(doctor)) = &mut self.config_views.open {
            let act = doctor.key(key, height);
            return Some(self.config_act(act));
        }
        let act = match self.config_views.seam.clone() {
            Some(seam) => {
                let Some(workspace) = self.view_workspace() else {
                    return Some(Effect::None);
                };
                let ctx = self.config_ctx(seam.as_ref(), &workspace);
                match &mut self.config_views.open {
                    Some(Open::Settings(settings)) => settings.key(key, &ctx),
                    Some(Open::Tools(tools)) => tools.key(key, &ctx),
                    Some(Open::Skills(skills)) => skills.key(key, &ctx),
                    Some(Open::Mcp(mcp)) => mcp.key(key, &ctx),
                    Some(Open::Rules(rules)) => rules.key(key, &ctx),
                    Some(Open::Login(login)) => login.key(key, &ctx),
                    Some(Open::Logout(logout)) => logout.key(key, &ctx),
                    // The early route above keeps `/doctor` out of the
                    // seam match: named here for exhaustiveness.
                    Some(Open::Doctor(..)) => esc(key),
                    Some(Open::Credential(_) | Open::Unavailable(..)) | None => esc(key),
                }
            }
            None => esc(key),
        };
        Some(self.config_act(act))
    }

    /// Shifts the open configuration view's window one wheel step through
    /// its own entry point: an open edit field takes Up/Down without
    /// scrolling, so the wheel is swallowed there and never retargets
    /// the row the field edits. `None` with no view open. Never an
    /// Up/Down step: the keys keep their own path.
    pub(in crate::app) fn config_view_wheel(&mut self, up: bool) -> Option<Effect> {
        self.config_views.open.as_ref()?;
        let height = self.config_view_height();
        if let Some(Open::Credential(picker)) = &mut self.config_views.open {
            let pick = picker.wheel(up, height);
            return Some(self.credential_pick(pick));
        }
        // `/doctor` shows a local snapshot through no seam, as its keys do.
        if let Some(Open::Doctor(doctor)) = &mut self.config_views.open {
            let act = doctor.wheel(up, height);
            return Some(self.config_act(act));
        }
        let act = match self.config_views.seam.clone() {
            Some(seam) => {
                let Some(workspace) = self.view_workspace() else {
                    return Some(Effect::None);
                };
                let ctx = self.config_ctx(seam.as_ref(), &workspace);
                match &mut self.config_views.open {
                    Some(Open::Settings(settings)) => settings.wheel(up, &ctx),
                    Some(Open::Tools(tools)) => tools.wheel(up, &ctx),
                    Some(Open::Skills(skills)) => skills.wheel(up, &ctx),
                    Some(Open::Mcp(mcp)) => mcp.wheel(up, &ctx),
                    Some(Open::Rules(rules)) => rules.wheel(up, &ctx),
                    Some(Open::Login(login)) => login.wheel(up, &ctx),
                    Some(Open::Logout(logout)) => logout.wheel(up, &ctx),
                    // The early routes above keep `/doctor`, the picker
                    // and the unavailable view out of the seam match:
                    // named here for exhaustiveness. None of them
                    // scrolls, so the wheel is swallowed.
                    Some(Open::Doctor(..) | Open::Credential(_) | Open::Unavailable(..)) | None => {
                        Act::Stay
                    }
                }
            }
            None => Act::Stay,
        };
        Some(self.config_act(act))
    }

    /// Hands an editing key to the open view's field. `None` with no view
    /// open.
    pub(in crate::app) fn config_view_edit(&mut self, edit: &Edit) -> Option<Effect> {
        self.config_views.open.as_ref()?;
        // `/doctor` has no field: edits change nothing, through no seam.
        if matches!(self.config_views.open, Some(Open::Doctor(..))) {
            return Some(Effect::None);
        }
        let act = match self.config_views.seam.clone() {
            Some(seam) => {
                let Some(workspace) = self.view_workspace() else {
                    return Some(Effect::None);
                };
                let ctx = self.config_ctx(seam.as_ref(), &workspace);
                match &mut self.config_views.open {
                    Some(Open::Settings(settings)) => {
                        settings.edit_key(edit.clone());
                        Act::Stay
                    }
                    Some(Open::Tools(tools)) => {
                        tools.edit_key(edit);
                        Act::Stay
                    }
                    Some(Open::Mcp(_)) => Act::Stay,
                    Some(Open::Skills(skills)) => {
                        skills.edit_key(edit);
                        Act::Stay
                    }
                    Some(Open::Rules(rules)) => rules.edit_key(edit, &ctx),
                    Some(Open::Login(login)) => {
                        login.edit_key(edit);
                        Act::Stay
                    }
                    // `/logout` has no field: nothing takes an edit.
                    Some(Open::Logout(_)) => Act::Stay,
                    // The early route above keeps `/doctor` out of the
                    // seam match: named here for exhaustiveness.
                    Some(Open::Doctor(..)) => Act::Stay,
                    Some(Open::Credential(_) | Open::Unavailable(..)) | None => Act::Stay,
                }
            }
            None => Act::Stay,
        };
        Some(self.config_act(act))
    }

    /// A click on the open view's `spot`.
    pub(in crate::app) fn config_view_click(&mut self, spot: Spot) -> Effect {
        let height = self.config_view_height();
        if let Some(Open::Credential(picker)) = &mut self.config_views.open {
            let pick = picker.click(spot, height);
            return self.credential_pick(pick);
        }
        // `/doctor` shows a local snapshot through no seam: a report
        // row takes no action without reaching the configure seam or a
        // workspace. The top-row close below stays shared.
        if matches!(self.config_views.open, Some(Open::Doctor(..)))
            && matches!(
                spot,
                Spot::Row(_) | Spot::Switch { .. } | Spot::Revoke(_) | Spot::Item(_)
            )
        {
            return self.config_act(Act::Stay);
        }
        let act = match (self.config_views.seam.clone(), spot) {
            // The picker's `x` cancels like Esc: with the theme choices
            // open the capture is queued first, then the view closes.
            (_, Spot::Close) => {
                let restore = match &self.config_views.open {
                    Some(Open::Settings(settings)) => settings.choices_entry(),
                    _ => None,
                };
                self.config_views.open = None;
                if let Some(entry) = restore {
                    return self.config_act(Act::Theme(entry));
                }
                return Effect::None;
            }
            (Some(seam), Spot::Row(_) | Spot::Switch { .. } | Spot::Revoke(_) | Spot::Item(_)) => {
                let Some(workspace) = self.view_workspace() else {
                    return Effect::None;
                };
                let ctx = self.config_ctx(seam.as_ref(), &workspace);
                match &mut self.config_views.open {
                    Some(Open::Settings(settings)) => settings.click(spot, &ctx),
                    Some(Open::Tools(tools)) => tools.click(spot, &ctx),
                    Some(Open::Skills(skills)) => skills.click(spot, &ctx),
                    Some(Open::Mcp(mcp)) => mcp.click(spot, &ctx),
                    Some(Open::Rules(rules)) => rules.click(spot, &ctx),
                    Some(Open::Login(login)) => login.click(spot, &ctx),
                    Some(Open::Logout(logout)) => logout.click(spot, &ctx),
                    // The early route above keeps `/doctor` out of the
                    // seam match: named here for exhaustiveness.
                    Some(Open::Doctor(..)) => Act::Stay,
                    Some(Open::Credential(_) | Open::Unavailable(..)) | None => Act::Stay,
                }
            }
            (None, Spot::Row(_) | Spot::Switch { .. } | Spot::Revoke(_) | Spot::Item(_)) => {
                Act::Stay
            }
            // Only the model picker draws cells with targets of their
            // own; here a cell is never pushed.
            (_, Spot::Cell(_, _)) => Act::Stay,
        };
        self.config_act(act)
    }

    /// Carries out what the view asked.
    fn config_act(&mut self, act: Act) -> Effect {
        match act {
            Act::Stay => Effect::None,
            Act::Close => {
                self.config_views.open = None;
                Effect::None
            }
            Act::Theme(setting) => {
                self.config_views.theme = Some(setting);
                Effect::None
            }
            Act::OpenLocal(file) => {
                let file = file
                    .or_else(|| self.remote().map(|remote| remote.local_global_file()))
                    .or_else(|| self.configure_seam().map(|seam| seam.global_file()));
                file.map_or(Effect::None, Effect::OpenFile)
            }
            Act::Open(file) => {
                let Some(remote) = self.remote() else {
                    return Effect::OpenFile(file);
                };
                // With no local workspace only a hub-global file has an
                // address; a project-scoped file fails on the notice path.
                let fetched = match self.workspace() {
                    Some(workspace) => remote.fetch_file(&workspace, &file),
                    None => remote.fetch_file_global(&file),
                };
                let copy = fetched
                    .map_err(|error| error.message)
                    .and_then(|(text, version)| {
                        crate::remote::RemoteCopy::create(
                            &std::env::temp_dir(),
                            &file,
                            &text,
                            version,
                        )
                        .map_err(|error| error.to_string())
                    });
                match copy {
                    Ok(copy) => {
                        let temp = copy.temp.clone();
                        self.config_views.copy = Some(copy);
                        Effect::OpenFile(temp)
                    }
                    Err(message) => {
                        self.notices.push("io_failed", message);
                        Effect::None
                    }
                }
            }
            Act::Login(name) => {
                // Tickets stay distinct until the counter saturates at
                // `u64::MAX`, which is then handed out again for every
                // later start: below saturation every start advances.
                self.config_views.tickets = self.config_views.tickets.next();
                let ticket = self.config_views.tickets;
                if let Some(seam) = self.configure_seam() {
                    self.config_views.pending_login = Some(LoginStart {
                        ticket,
                        name,
                        seam,
                        job: LoginJob::Browser,
                    });
                    if let Some(Open::Login(login)) = &mut self.config_views.open {
                        login.wait(ticket);
                    }
                }
                Effect::None
            }
            Act::Reviewer(name) => {
                // The key is stored; only the reviewer read runs, on a
                // fresh ticket with the same saturation rule as a login.
                self.config_views.tickets = self.config_views.tickets.next();
                let ticket = self.config_views.tickets;
                if let Some(seam) = self.configure_seam() {
                    self.config_views.pending_login = Some(LoginStart {
                        ticket,
                        name: name.clone(),
                        seam,
                        job: LoginJob::Reviewer,
                    });
                    if let Some(Open::Login(login)) = &mut self.config_views.open {
                        login.wait_reviewer(ticket, name);
                    }
                }
                Effect::None
            }
            Act::McpReconnect(server) => self.send_mcp_driver("mcp_reconnect", &server, false),
            Act::McpLogin(server) => self.send_mcp_driver("mcp_login", &server, true),
            Act::Copy(url) => {
                // As a focused link's `y`: the copy goes through OSC 52
                // and, on a local session, the system clipboard as well.
                self.copied = true;
                Effect::Copy(url)
            }
        }
    }

    /// Sends a per-server driver command for the open MCP view and
    /// remembers its id as the server's wait: the answer clears the wait
    /// and queries the rows again. Nothing when no session is attached.
    fn send_mcp_driver(&mut self, command: &str, server: &str, login: bool) -> Effect {
        let Some((id, line)) = self.send_config_command(command, Some(json!({"server": server})))
        else {
            return Effect::None;
        };
        if let Some(Open::Mcp(mcp)) = &mut self.config_views.open {
            mcp.sent(server, id, login);
        }
        Effect::Send(vec![line])
    }

    /// Re-sends `mcp_servers` for the open MCP view: its answer fills
    /// what `expect` names. Nothing when no session is attached.
    fn remcp_query(&mut self) -> Vec<String> {
        let Some((id, line)) = self.send_config_command("mcp_servers", None) else {
            return Vec::new();
        };
        if let Some(Open::Mcp(mcp)) = &mut self.config_views.open {
            mcp.expect(id);
        }
        vec![line]
    }

    /// The browser login start the loop has not picked up yet, once.
    pub(crate) fn take_login_start(&mut self) -> Option<LoginStart> {
        self.config_views.pending_login.take()
    }

    /// Keeps the started login worker in the waiting view; with no login
    /// view open it is dropped, cancelling it at once.
    pub(crate) fn login_started(&mut self, worker: LoginWorker) {
        match &mut self.config_views.open {
            Some(Open::Login(login)) => login.started(worker),
            _ => drop(worker),
        }
    }

    /// Folds the waiting ticket's login progress into the view: an event
    /// for another ticket changes nothing. Returns the URL to open, only
    /// for `Open` on the waiting ticket.
    pub(crate) fn on_login(&mut self, ticket: LoginTicket, step: LoginStep) -> Option<String> {
        match &mut self.config_views.open {
            Some(Open::Login(login)) => login.step(ticket, step),
            _ => None,
        }
    }

    /// The open logout view, in any mode: what the swapped view draws in
    /// the shared overlay frame.
    pub(crate) fn logout_view(&self) -> Option<&Logout> {
        match &self.config_views.open {
            Some(Open::Logout(logout)) => Some(logout),
            _ => None,
        }
    }

    /// The open login view, in any mode: what the swapped view draws in
    /// the shared overlay frame.
    pub(crate) fn login_view(&self) -> Option<&Login> {
        match &self.config_views.open {
            Some(Open::Login(login)) => Some(login),
            _ => None,
        }
    }

    /// The open view's frame.
    pub(crate) fn config_view_screen(&self) -> Option<Frame> {
        let usage = self
            .config_views
            .usage
            .as_ref()
            .filter(|(session, _)| self.session() == Some(session))
            .map(|(_, tokens)| *tokens);
        Some(match self.config_views.open.as_ref()? {
            Open::Settings(settings) => settings.frame(self.config_view_width(), usage),
            Open::Tools(tools) => {
                tools.frame(self.config_view_width(), self.reload_cost().as_deref())
            }
            Open::Skills(skills) => {
                skills.frame(self.config_view_width(), self.config_view_height())
            }
            Open::Mcp(mcp) => mcp.frame(usage),
            Open::Credential(picker) => picker.frame(),
            Open::Doctor(doctor) => doctor.frame(),
            Open::Rules(rules) => rules.frame(self.config_view_width(), self.motion().wall_ms()),
            Open::Login(_) => return None,
            // `/logout` draws its own overlay, as `/login` does.
            Open::Logout(_) => return None,
            Open::Unavailable(title, command) => Frame {
                title: (*title).to_owned(),
                command: (*command).to_owned(),
                rows: Vec::new(),
                list: List::default(),
                below: vec![UNAVAILABLE.to_owned()],
                field: None,
                footer: "Esc close".to_owned(),
            },
        })
    }

    /// The last call's prompt size on the session on screen, if its
    /// call was the session's own: what a reload's cache rebuild costs.
    /// A copied or extension call, or another session's, counts nothing.
    pub(in crate::app) fn usage_on_screen(&self) -> Option<u64> {
        self.config_views
            .usage
            .as_ref()
            .filter(|(session, _)| self.session() == Some(session))
            .map(|(_, tokens)| *tokens)
    }

    /// What a reload's cache rebuild costs on the session on screen, as
    /// the tools view's last row says it: its size, and its price when
    /// the on-screen model's catalogue entry names one. `None` before
    /// the session's first call.
    fn reload_cost(&self) -> Option<String> {
        let tokens = self.usage_on_screen()?;
        let model = self.panel_state.model();
        let price = self
            .model_picker
            .catalogue
            .models
            .iter()
            .find(|entry| Some(entry.reference.as_str()) == model)
            .and_then(|entry| entry.price.as_ref());
        Some(crate::model_picker::rebuild_cost(tokens, price))
    }

    /// The theme a choice queued, once.
    pub(crate) fn take_theme_choice(&mut self) -> Option<ThemeSetting> {
        self.config_views.theme.take()
    }

    /// The editor Ctrl+G opened returned: the open view reads its rows
    /// again, and an error is a notice.
    pub(crate) fn config_file_closed(&mut self, result: Result<(), String>) {
        let result =
            if let (Some(remote), Some(copy)) = (self.remote(), self.config_views.copy.take()) {
                // With no local workspace only a hub-global file has an
                // address; a project-scoped file fails on the notice path.
                let workspace = self.workspace();
                let result = result
                    .and_then(|()| {
                        std::fs::read_to_string(&copy.temp).map_err(|error| error.to_string())
                    })
                    .and_then(|text| {
                        let stored = match workspace.as_ref() {
                            Some(workspace) => {
                                remote.store_file(workspace, &copy.hub, &text, &copy.version)
                            }
                            None => remote.store_file_global(&copy.hub, &text, &copy.version),
                        };
                        stored.map_err(|error| error.message)
                    });
                drop(copy);
                result
            } else {
                result
            };
        if let Err(message) = result {
            self.notices.push("io_failed", message);
        }
        if let Some(seam) = self.config_views.seam.clone() {
            let Some(workspace) = self.view_workspace() else {
                return;
            };
            let ctx = self.config_ctx(seam.as_ref(), &workspace);
            match &mut self.config_views.open {
                Some(Open::Settings(settings)) => settings.reread(&ctx),
                Some(Open::Rules(rules)) => rules.reread(&ctx),
                Some(Open::Skills(skills)) => skills.reread(&ctx),
                // `/logout` and `/doctor` open no file an editor could
                // return from: the snapshot never rereads.
                Some(Open::Logout(_) | Open::Doctor(..)) => {}
                Some(
                    Open::Credential(_)
                    | Open::Tools(_)
                    | Open::Mcp(_)
                    | Open::Login(_)
                    | Open::Unavailable(..),
                )
                | None => {}
            }
        }
    }

    /// Folds a `command_accepted` or `command_rejected` on the session on
    /// screen into the open asking view: only the answer to the command
    /// it sent, of its result kind, fills it. A follow-up driver answer
    /// and a server event re-query the MCP rows. Every other line, and
    /// any line with another view open, does nothing. Returns the lines
    /// the fold sends.
    pub(in crate::app) fn config_views_line(&mut self, envelope: &Envelope) -> Vec<String> {
        let credential_send = self.credential_line(envelope);
        if !matches!(
            self.config_views.open,
            Some(Open::Tools(_) | Open::Skills(_) | Open::Mcp(_))
        ) {
            return credential_send;
        }
        let Some(seam) = self.config_views.seam.clone() else {
            return credential_send;
        };
        let Some(workspace) = self.view_workspace() else {
            return credential_send;
        };
        let ctx = self.config_ctx(seam.as_ref(), &workspace);
        let mut send = credential_send;
        match envelope.kind.as_str() {
            "command_accepted" => {
                if let Some(accepted) = super::read!(envelope, CommandAccepted) {
                    match &mut self.config_views.open {
                        Some(Open::Tools(tools)) => {
                            if let Some(CommandResult::Tools { tools: answered }) = accepted.result
                            {
                                tools.answered(&accepted.command_id.0, &answered, &ctx);
                            }
                        }
                        Some(Open::Skills(skills)) => {
                            if let Some(CommandResult::Skills { skills: answered }) =
                                accepted.result
                            {
                                skills.answered(&accepted.command_id.0, &answered, &ctx);
                            }
                        }
                        Some(Open::Mcp(mcp)) => match accepted.result {
                            Some(CommandResult::McpServers { servers }) => {
                                mcp.answered(&accepted.command_id.0, &servers, &ctx);
                            }
                            // A follow-up driver answer carries no
                            // result: its id clears the wait, and the
                            // rows query again.
                            _ => {
                                if mcp.followup(&accepted.command_id.0) {
                                    send.extend(self.remcp_query());
                                }
                            }
                        },
                        // `/logout` and `/doctor` wait on no command: no
                        // line folds into them.
                        Some(Open::Logout(_) | Open::Doctor(..)) => {}
                        Some(
                            Open::Credential(_)
                            | Open::Settings(_)
                            | Open::Rules(_)
                            | Open::Login(_)
                            | Open::Unavailable(..),
                        )
                        | None => {}
                    }
                }
            }
            "command_rejected" => {
                if let Some(rejected) = super::read!(envelope, CommandRejected)
                    && let Some(id) = rejected.command_id
                    && let Some(open) = &mut self.config_views.open
                {
                    open.rejected(&id.0, &rejected.message);
                }
            }
            // A server's state changed, or its set did: the open view
            // queries again, unless its last answer is still in flight.
            "mcp_server_failed" | "mcp_server_ready" | "reloaded" => {
                let live = matches!(&self.config_views.open, Some(Open::Mcp(mcp)) if mcp.live());
                if live {
                    send.extend(self.remcp_query());
                }
            }
            _ => {}
        }
        send
    }

    /// A hub `command_rejected` for the command the asking view sent
    /// shows its message: only for its id.
    pub(in crate::app) fn config_views_refused(&mut self, id: &str, message: &str) {
        if let Some(open) = &mut self.config_views.open {
            open.rejected(id, message);
        }
    }

    /// A `usage_recorded` on the session on screen: a call of its own,
    /// not a copy or an extension's, sets the prompt size a reload's cache
    /// rebuild costs: its input, cache read and cache write tokens.
    pub(in crate::app) fn config_views_usage(&mut self, envelope: &Envelope) {
        let Some(usage) = super::read!(envelope, UsageRecorded) else {
            return;
        };
        if usage.origin_session_id.is_some() || usage.extension.is_some() {
            return;
        }
        let tokens = usage.tokens.cache_write.values().fold(
            usage.tokens.input.saturating_add(usage.tokens.cache_read),
            |sum, n| sum.saturating_add(*n),
        );
        self.config_views.usage = Some((envelope.session_id.clone(), tokens));
    }
}

/// A view with nothing to show closes on Esc and does nothing else.
fn esc(key: &Key) -> Act {
    if *key == Key::Esc {
        Act::Close
    } else {
        Act::Stay
    }
}

#[cfg(test)]
#[path = "config_views_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "config_login_tests.rs"]
mod login_tests;

#[cfg(test)]
#[path = "config_logout_tests.rs"]
mod logout_tests;

#[cfg(test)]
#[path = "config_skills_tests.rs"]
mod skills_tests;

#[cfg(test)]
#[path = "config_mcp_tests.rs"]
mod mcp_tests;

#[cfg(test)]
#[path = "doctor_view_tests.rs"]
mod doctor_tests;
