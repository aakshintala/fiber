//! A recording [`Configure`] for the views' tests: rows it was given, the
//! writes it was asked for, and an answer for each write.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use contract::{ErrorCode, Secret};

use std::sync::Arc;

use contract::clock::Clock;

use crate::ThemeSetting;
use crate::configure::{
    BrowserLogin, Configure, ConfigureError, KeyEdit, Layer, LoginShow, LoginTarget, Revoked,
    RulesScope, RulesSection, Saved, SettingRow, Shown, SkillsDisabled, Stored, SwitchScope,
    ToolGroup, ToolLists, ToolSwitches, WriteScope,
};

/// A browser login the recording seam never starts: its tests arrive with
/// the seam's own in the next task.
struct Unstarted;

impl BrowserLogin for Unstarted {
    fn run(&self) -> Result<Stored, ConfigureError> {
        unreachable!("no test starts a browser login through this seam yet");
    }

    fn cancel(&self) {}
}

/// One revoke the view asked for: the workspace, scope, line and text.
pub(crate) type Revoke = (PathBuf, RulesScope, usize, String);

/// One write the view asked for: the workspace, layer, key and text.
pub(crate) type Write = (PathBuf, Layer, String, String);

/// One switch the view asked for: the group, tool, scope and whether on.
pub(crate) type Switched = (ToolGroup, String, SwitchScope, bool);

/// One skill switch the view asked for: the workspace, name, scope and
/// whether on.
pub(crate) type SkillSwitched = (PathBuf, String, SwitchScope, bool);

/// A seam over rows in memory.
pub(crate) struct Fake {
    /// What `settings` answers.
    pub(crate) rows: Mutex<Result<Vec<SettingRow>, ConfigureError>>,
    /// Every workspace `settings` was asked about, in order.
    pub(crate) reads: Mutex<Vec<PathBuf>>,
    /// Every write asked for, in order.
    pub(crate) writes: Mutex<Vec<Write>>,
    /// The refusal the next writes get; `None` saves them.
    pub(crate) refuse: Mutex<Option<String>>,
    /// The warnings the next saves carry; empty saves quietly.
    pub(crate) warnings: Mutex<Vec<String>>,
    /// Every MCP server and extension's lists `tool_switches` answers.
    pub(crate) switches: Mutex<Vec<ToolSwitches>>,
    /// Every switch asked for, in order.
    pub(crate) switched: Mutex<Vec<Switched>>,
    /// Whether `switch_tool` fails after applying the edit to its lists.
    pub(crate) fail_after_write: Mutex<bool>,
    /// What `skills_disabled` answers.
    pub(crate) skills_off: Mutex<Result<SkillsDisabled, ConfigureError>>,
    /// Every skill switch asked for, in order.
    pub(crate) skill_switched: Mutex<Vec<SkillSwitched>>,
    /// What `skill_text` answers.
    pub(crate) texts: Mutex<Result<String, ConfigureError>>,
    /// Every `/keys` save asked for, in order.
    pub(crate) keys_saved: Mutex<Vec<Vec<KeyEdit>>>,
    /// The theme files `themes` lists.
    pub(crate) themes: Vec<String>,
    /// What `global_file` answers.
    pub(crate) global: PathBuf,
    /// What `rules` answers.
    pub(crate) rules: Mutex<Result<(RulesSection, RulesSection), ConfigureError>>,
    /// Every revoke asked for, in order.
    pub(crate) revokes: Mutex<Vec<Revoke>>,
    /// What `revoke` answers.
    pub(crate) revoked: Mutex<Result<Revoked, ConfigureError>>,
    /// What `login_targets` answers.
    pub(crate) targets: Mutex<Result<Vec<LoginTarget>, ConfigureError>>,
    /// Every key asked to store, in order: the name, the label and the key.
    pub(crate) stores: Mutex<Vec<(String, Option<String>, Secret)>>,
    /// What `store_key` answers.
    pub(crate) stored: Mutex<Result<Stored, ConfigureError>>,
}

impl Fake {
    /// A seam answering `rows`, saving every write.
    pub(crate) fn new(rows: Vec<SettingRow>) -> Self {
        Self {
            rows: Mutex::new(Ok(rows)),
            reads: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
            refuse: Mutex::new(None),
            warnings: Mutex::new(Vec::new()),
            switches: Mutex::new(Vec::new()),
            switched: Mutex::new(Vec::new()),
            fail_after_write: Mutex::new(false),
            skills_off: Mutex::new(Ok(SkillsDisabled::default())),
            skill_switched: Mutex::new(Vec::new()),
            texts: Mutex::new(Ok(String::new())),
            keys_saved: Mutex::new(Vec::new()),
            themes: Vec::new(),
            global: file(Layer::Global),
            rules: Mutex::new(Ok((
                RulesSection {
                    file: PathBuf::from("/home/rules"),
                    rows: Ok(Vec::new()),
                },
                RulesSection {
                    file: PathBuf::from("/home/projects/-w/rules"),
                    rows: Ok(Vec::new()),
                },
            ))),
            revokes: Mutex::new(Vec::new()),
            revoked: Mutex::new(Ok(Revoked::Removed)),
            targets: Mutex::new(Ok(Vec::new())),
            stores: Mutex::new(Vec::new()),
            stored: Mutex::new(Ok(Stored {
                path: "credentials/acme/default".to_owned(),
                replaced: false,
            })),
        }
    }

    /// The writes asked for so far.
    pub(crate) fn writes(&self) -> Vec<Write> {
        self.writes.lock().map(|w| w.clone()).unwrap_or_default()
    }

    /// The workspaces read so far.
    pub(crate) fn reads(&self) -> Vec<PathBuf> {
        self.reads.lock().map(|r| r.clone()).unwrap_or_default()
    }

    /// The revokes asked for so far.
    pub(crate) fn revokes(&self) -> Vec<Revoke> {
        self.revokes.lock().map(|r| r.clone()).unwrap_or_default()
    }

    /// The names and labels asked to store so far, in order.
    pub(crate) fn stores(&self) -> Vec<(String, Option<String>)> {
        self.stores
            .lock()
            .map(|stores| {
                stores
                    .iter()
                    .map(|(name, label, _)| (name.clone(), label.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `n`th stored key's value.
    pub(crate) fn secret_of(&self, n: usize) -> String {
        self.stores
            .lock()
            .ok()
            .and_then(|stores| stores.get(n).map(|(_, _, key)| key.expose().to_owned()))
            .unwrap_or_default()
    }

    /// The `/keys` saves asked for so far.
    pub(crate) fn keys_saved(&self) -> Vec<Vec<KeyEdit>> {
        self.keys_saved
            .lock()
            .map(|saved| saved.clone())
            .unwrap_or_default()
    }
}

/// The file a write to `layer` lands in, under `/home`.
pub(crate) fn file(layer: Layer) -> PathBuf {
    PathBuf::from(match layer {
        Layer::Global => "/home/config.json",
        Layer::Project => "/home/projects/-w/config.json",
        Layer::Repository => "/w/.fiber/config.json",
    })
}

/// A row for `key` holding `value` from `layer`, written within `scope`.
pub(crate) fn row(key: &str, value: Shown, layer: &str, scope: WriteScope) -> SettingRow {
    let file = match layer {
        "global" => Some(file(Layer::Global)),
        "project" => Some(file(Layer::Project)),
        "repository" => Some(file(Layer::Repository)),
        _ => None,
    };
    SettingRow {
        key: key.to_owned(),
        value,
        layer: layer.to_owned(),
        file,
        scope,
    }
}

impl Configure for Fake {
    fn settings(&self, workspace: &Path) -> Result<Vec<SettingRow>, ConfigureError> {
        if let Ok(mut reads) = self.reads.lock() {
            reads.push(workspace.to_path_buf());
        }
        self.rows.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |rows| rows.clone(),
        )
    }

    fn set(
        &self,
        workspace: &Path,
        layer: Layer,
        key: &str,
        text: &str,
    ) -> Result<Saved, ConfigureError> {
        if let Ok(mut writes) = self.writes.lock() {
            writes.push((
                workspace.to_path_buf(),
                layer,
                key.to_owned(),
                text.to_owned(),
            ));
        }
        match self.refuse.lock().ok().and_then(|refuse| refuse.clone()) {
            Some(message) => Err(ConfigureError {
                code: ErrorCode::Usage,
                message,
            }),
            None => Ok(Saved {
                file: file(layer),
                warnings: self.warnings.lock().map(|w| w.clone()).unwrap_or_default(),
            }),
        }
    }

    fn global_file(&self) -> PathBuf {
        self.global.clone()
    }

    fn rules(&self, workspace: &Path) -> Result<(RulesSection, RulesSection), ConfigureError> {
        if let Ok(mut reads) = self.reads.lock() {
            reads.push(workspace.to_path_buf());
        }
        self.rules.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |rules| rules.clone(),
        )
    }

    fn revoke(
        &self,
        workspace: &Path,
        scope: RulesScope,
        line: usize,
        text: &str,
    ) -> Result<Revoked, ConfigureError> {
        if let Ok(mut revokes) = self.revokes.lock() {
            revokes.push((workspace.to_path_buf(), scope, line, text.to_owned()));
        }
        self.revoked.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |revoked| revoked.clone(),
        )
    }

    fn login_targets(&self) -> Result<Vec<LoginTarget>, ConfigureError> {
        self.targets.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |targets| targets.clone(),
        )
    }

    fn browser_login(
        &self,
        name: &str,
        shown: Arc<dyn LoginShow>,
        clock: Arc<dyn Clock>,
    ) -> Arc<dyn BrowserLogin> {
        let _ = (name, shown, clock);
        Arc::new(Unstarted)
    }

    fn store_key(
        &self,
        name: &str,
        label: Option<&str>,
        key: Secret,
    ) -> Result<Stored, ConfigureError> {
        if let Ok(mut stores) = self.stores.lock() {
            stores.push((name.to_owned(), label.map(str::to_owned), key));
        }
        self.stored.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |stored| stored.clone(),
        )
    }

    fn themes(&self, workspace: &Path) -> Vec<String> {
        let _ = workspace;
        self.themes.clone()
    }

    fn theme(&self, workspace: &Path, name: &str) -> ThemeSetting {
        let _ = workspace;
        match name {
            "auto" => ThemeSetting::Follow,
            "dark" => ThemeSetting::Dark,
            "light" => ThemeSetting::Light,
            _ => ThemeSetting::File {
                name: name.to_owned(),
                text: Err("gone".to_owned()),
            },
        }
    }

    fn tool_switches(&self, workspace: &Path) -> Result<Vec<ToolSwitches>, ConfigureError> {
        if let Ok(mut reads) = self.reads.lock() {
            reads.push(workspace.to_path_buf());
        }
        match self.refuse.lock().ok().and_then(|refuse| refuse.clone()) {
            Some(message) => Err(ConfigureError {
                code: ErrorCode::Usage,
                message,
            }),
            None => Ok(self
                .switches
                .lock()
                .map(|switches| switches.clone())
                .unwrap_or_default()),
        }
    }

    fn skills_disabled(&self, workspace: &Path) -> Result<SkillsDisabled, ConfigureError> {
        if let Ok(mut reads) = self.reads.lock() {
            reads.push(workspace.to_path_buf());
        }
        self.skills_off.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |lists| lists.clone(),
        )
    }

    fn switch_skill(
        &self,
        workspace: &Path,
        name: &str,
        scope: SwitchScope,
        on: bool,
    ) -> Result<(), ConfigureError> {
        if let Ok(mut switched) = self.skill_switched.lock() {
            switched.push((workspace.to_path_buf(), name.to_owned(), scope, on));
        }
        if let Ok(mut lists) = self.skills_off.lock()
            && let Ok(off) = lists.as_mut()
        {
            // The seam's `skills.disabled` handling, so a view
            // re-read sees the change (`docs/tui.md`, "Swapped views").
            let listed = match scope {
                SwitchScope::Project => &mut off.project,
                SwitchScope::Everywhere => &mut off.everywhere,
            };
            if on {
                listed.retain(|listed| listed != name);
            } else if !listed.iter().any(|listed| listed == name) {
                listed.push(name.to_owned());
            }
        }
        if self.fail_after_write.lock().is_ok_and(|fail| *fail) {
            return Err(ConfigureError {
                code: ErrorCode::IoFailed,
                message: "the write landed, then syncing failed".to_owned(),
            });
        }
        Ok(())
    }

    fn skill_text(&self, path: &Path) -> Result<String, ConfigureError> {
        let _ = path;
        self.texts.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |texts| texts.clone(),
        )
    }

    fn save_keys(&self, edits: &[KeyEdit]) -> Result<(), ConfigureError> {
        if let Ok(mut saved) = self.keys_saved.lock() {
            saved.push(edits.to_vec());
        }
        match self.refuse.lock().ok().and_then(|refuse| refuse.clone()) {
            Some(message) => Err(ConfigureError {
                code: ErrorCode::Usage,
                message,
            }),
            None => Ok(()),
        }
    }

    fn switch_tool(
        &self,
        workspace: &Path,
        group: &ToolGroup,
        tool: &str,
        scope: SwitchScope,
        on: bool,
    ) -> Result<(), ConfigureError> {
        if let Ok(mut switched) = self.switched.lock() {
            switched.push((group.clone(), tool.to_owned(), scope, on));
        }
        let _ = workspace;
        match self.refuse.lock().ok().and_then(|refuse| refuse.clone()) {
            Some(message) => Err(ConfigureError {
                code: ErrorCode::Usage,
                message,
            }),
            None => {
                if let Ok(mut switches) = self.switches.lock() {
                    let at = switches.iter().position(|known| known.group == *group);
                    let at = at.unwrap_or_else(|| {
                        switches.push(ToolSwitches {
                            group: group.clone(),
                            project: ToolLists::default(),
                            everywhere: ToolLists::default(),
                        });
                        switches.len().saturating_sub(1)
                    });
                    if let Some(known) = switches.get_mut(at) {
                        // The seam's `disabled` handling, so a view re-read
                        // sees the change (`docs/tui.md`, "Swapped views").
                        let lists = match scope {
                            SwitchScope::Project => &mut known.project.disabled,
                            SwitchScope::Everywhere => &mut known.everywhere.disabled,
                        };
                        if on {
                            lists.retain(|name| name != tool);
                        } else if !lists.iter().any(|name| name == tool) {
                            lists.push(tool.to_owned());
                        }
                    }
                }
                if self.fail_after_write.lock().is_ok_and(|fail| *fail) {
                    return Err(ConfigureError {
                        code: ErrorCode::IoFailed,
                        message: "the write landed, then syncing failed".to_owned(),
                    });
                }
                Ok(())
            }
        }
    }
}
