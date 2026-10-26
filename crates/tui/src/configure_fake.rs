//! A recording [`Configure`] for the views' tests: rows it was given, the
//! writes it was asked for, and an answer for each write.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use contract::{ErrorCode, Secret};

use std::sync::Arc;

use contract::clock::Clock;

use crate::ThemeSetting;
use crate::configure::{
    BrowserLogin, Configure, ConfigureError, HostGap, HostSaved, KeyEdit, Layer, LoginShow,
    LoginTarget, LogoutRow, NotesFile, NotesSaved, NotesWrite, ReviewerModels, ReviewerNotes,
    Revoked, RulesScope, RulesSection, Saved, SettingRow, Shown, SkillsDisabled, Stored,
    SwitchScope, ToolGroup, ToolLists, ToolSwitches, WriteScope,
};

/// One notes save the view asked for: the workspace and each sent layer.
pub(crate) type NotesSave = (PathBuf, Option<NotesWrite>, Option<NotesWrite>);

/// The hub's notes answer for a workspace with no test answer set: both
/// layers empty, each file named with an `absent` version.
pub(crate) fn empty_notes() -> ReviewerNotes {
    let layer = |layer: Layer| NotesFile {
        file: file(layer),
        text: String::new(),
        version: "absent".to_owned(),
    };
    ReviewerNotes {
        global: layer(Layer::Global),
        project: layer(Layer::Project),
    }
}

/// A browser login for the views' tests: `run` blocks until the answer
/// sender answers, and `cancel` records once per call. The sender lives
/// outside this object: `new` hands it to the test, so a test ending
/// drops it and disconnects a blocked `run` even while a worker thread
/// still holds this login.
pub(crate) struct FakeLogin {
    answer_rx: Mutex<Option<std::sync::mpsc::Receiver<Result<Stored, ConfigureError>>>>,
    cancels: Mutex<usize>,
}

impl FakeLogin {
    pub(crate) fn new() -> (
        Self,
        std::sync::mpsc::Sender<Result<Stored, ConfigureError>>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Self {
                answer_rx: Mutex::new(Some(rx)),
                cancels: Mutex::new(0),
            },
            tx,
        )
    }

    /// How many times `cancel` ran.
    pub(crate) fn cancels(&self) -> usize {
        self.cancels.lock().map(|cancels| *cancels).unwrap_or(0)
    }
}

impl BrowserLogin for FakeLogin {
    fn run(&self) -> Result<Stored, ConfigureError> {
        // The test answers from its own thread. The wait has no deadline
        // of its own: the test ending drops the sender and releases it.
        let rx = self
            .answer_rx
            .lock()
            .ok()
            .and_then(|mut rx| rx.take())
            .unwrap_or_else(|| panic!("the FakeLogin's run ran twice"));
        rx.recv()
            .unwrap_or_else(|_| panic!("the FakeLogin was never answered"))
    }

    fn cancel(&self) {
        if let Ok(mut cancels) = self.cancels.lock() {
            *cancels += 1;
        }
    }
}

/// One revoke the view asked for: the workspace, scope, line and text.
pub(crate) type Revoke = (PathBuf, RulesScope, usize, String);

/// One write the view asked for: the workspace, layer, key and text.
pub(crate) type Write = (PathBuf, Layer, String, String);

/// A fresh directory `delete_stored` removes files from, plain
/// `std::fs`: unique per `Fake`, gone on drop, so a screen test can
/// assert the file is gone from the fake home. Setup is best-effort:
/// the tests assert the files they need.
fn fresh_home() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("fiber-tui-fake-home-{}-{n}", std::process::id()));
    drop(std::fs::create_dir_all(&dir));
    dir
}

/// One switch the view asked for: the group, tool, scope and whether on.
pub(crate) type Switched = (ToolGroup, String, SwitchScope, bool);

/// One skill switch the view asked for: the workspace, name, scope and
/// whether on.
pub(crate) type SkillSwitched = (PathBuf, String, SwitchScope, bool);

/// One install held at a gate: it signals on `entered` once its call is
/// recorded and waits on `answer`.
pub(crate) struct InstallGate {
    answer: std::sync::mpsc::Receiver<Result<crate::ProviderInstall, ConfigureError>>,
    entered: std::sync::mpsc::Sender<()>,
}

/// A seam over rows in memory.
pub(crate) struct Fake {
    /// Canned filtered blocking names or query error.
    pub(crate) blocking_hooks_answer: Mutex<Result<Vec<String>, ConfigureError>>,
    /// Each supplied workspace and exact ordered override list.
    pub(crate) blocking_hooks_queries: Mutex<Vec<(PathBuf, Vec<String>)>>,
    /// Canned global hooks install result.
    pub(crate) hooks_install_answer: Mutex<Result<(), ConfigureError>>,
    /// Number of global hooks installs requested.
    pub(crate) hooks_installs: Mutex<usize>,
    /// The threads hooks queries and installs ran on, in order: neither
    /// ever runs on the loop thread.
    pub(crate) hooks_threads: Mutex<Vec<std::thread::ThreadId>>,
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
    /// What `import_pending` answers: false keeps existing tests'
    /// homes offer-free.
    pub(crate) pending: Mutex<bool>,
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
    /// What `logout_rows` answers.
    pub(crate) logout: Mutex<Result<Vec<LogoutRow>, ConfigureError>>,
    /// Every delete asked for, in order: the name and the label.
    pub(crate) deletes: Mutex<Vec<(String, Option<String>)>>,
    /// What `delete_stored` answers.
    pub(crate) deleted: Mutex<Result<String, ConfigureError>>,
    /// The fake home `delete_stored` removes files from, gone on drop.
    home: PathBuf,
    /// Every key asked to store, in order: the name, the label and the key.
    pub(crate) stores: Mutex<Vec<(String, Option<String>, Secret)>>,
    /// What `store_key` answers.
    pub(crate) stored: Mutex<Result<Stored, ConfigureError>>,
    /// Every provider asked to log in through the browser, in order.
    pub(crate) login_names: Mutex<Vec<String>>,
    /// The `LoginShow` each browser login was given, in order.
    pub(crate) login_shown: Mutex<Vec<Arc<dyn LoginShow>>>,
    /// The `FakeLogin` each browser login returned, in order.
    pub(crate) logins: Mutex<Vec<Arc<FakeLogin>>>,
    /// The answer sender each browser login waits on, in order: owned by
    /// the seam (which the test owns), never by the login the worker holds.
    answers: Mutex<Vec<std::sync::mpsc::Sender<Result<Stored, ConfigureError>>>>,
    /// Every `install_provider` asked for, in order: the workspace and the
    /// provider.
    pub(crate) installs: Mutex<Vec<(PathBuf, String)>>,
    /// The threads installs ran on, in order: an install never runs on the
    /// loop thread.
    pub(crate) install_threads: Mutex<Vec<std::thread::ThreadId>>,
    /// What an ungated install answers.
    pub(crate) install_answer: Mutex<Result<crate::ProviderInstall, ConfigureError>>,
    /// Gates queued for the next installs: each install takes one, signals
    /// it has reached it, and waits on it. A test ending drops its sender
    /// and releases the install, as `FakeLogin` does, so a failed test
    /// cannot retain a worker forever.
    pub(crate) install_gates: Mutex<Vec<InstallGate>>,
    /// Every reviewer read asked for, in order: the provider name.
    pub(crate) reviewer_reads: Mutex<Vec<String>>,
    /// The threads reviewer reads ran on, in order: discovery never runs
    /// on the loop thread.
    pub(crate) reviewer_threads: Mutex<Vec<std::thread::ThreadId>>,
    /// Gates queued for the next reviewer reads: each read takes one
    /// and waits on it. A test ending drops its sender and releases the
    /// read, as `FakeLogin` does, so a failed test cannot retain a worker
    /// forever.
    pub(crate) reviewer_gates:
        Mutex<Vec<std::sync::mpsc::Receiver<Result<ReviewerModels, ConfigureError>>>>,
    /// What an ungated reviewer read answers.
    pub(crate) reviewer_answer: Mutex<Result<ReviewerModels, ConfigureError>>,
    /// Every reviewer save asked for, in order: the provider and the id.
    pub(crate) reviewer_saves: Mutex<Vec<(String, String)>>,
    /// What reviewer saves answer.
    pub(crate) reviewer_saved: Mutex<Result<(), ConfigureError>>,
    /// What `gaps` answers: no gaps by default, so existing tests keep
    /// their reviewer-only flow.
    pub(crate) gaps_answer: Mutex<Result<Vec<HostGap>, ConfigureError>>,
    /// Every gaps read asked for, in order: the provider name.
    pub(crate) gap_reads: Mutex<Vec<String>>,
    /// Every host answer asked to save, in order: the provider, key and
    /// value. The value is the person's typed answer, kept only here.
    pub(crate) host_saves: Mutex<Vec<(String, String, String)>>,
    /// What `save_host` answers: saved unmasked by default.
    pub(crate) host_saved: Mutex<Result<HostSaved, ConfigureError>>,
    /// What `reviewer_notes` answers.
    pub(crate) notes: Mutex<Result<ReviewerNotes, ConfigureError>>,
    /// Every notes save asked for, in order.
    pub(crate) notes_saves: Mutex<Vec<NotesSave>>,
    /// What notes saves answer.
    pub(crate) notes_saved: Mutex<Result<NotesSaved, ConfigureError>>,
}

impl Fake {
    /// A seam answering `rows`, saving every write.
    pub(crate) fn new(rows: Vec<SettingRow>) -> Self {
        Self {
            blocking_hooks_answer: Mutex::new(Ok(Vec::new())),
            blocking_hooks_queries: Mutex::new(Vec::new()),
            hooks_install_answer: Mutex::new(Ok(())),
            hooks_installs: Mutex::new(0),
            hooks_threads: Mutex::new(Vec::new()),
            pending: Mutex::new(false),
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
            logout: Mutex::new(Ok(Vec::new())),
            deletes: Mutex::new(Vec::new()),
            deleted: Mutex::new(Ok("credentials/acme/default".to_owned())),
            home: fresh_home(),
            stores: Mutex::new(Vec::new()),
            stored: Mutex::new(Ok(Stored {
                path: "credentials/acme/default".to_owned(),
                replaced: false,
            })),
            login_names: Mutex::new(Vec::new()),
            login_shown: Mutex::new(Vec::new()),
            logins: Mutex::new(Vec::new()),
            answers: Mutex::new(Vec::new()),
            installs: Mutex::new(Vec::new()),
            install_threads: Mutex::new(Vec::new()),
            install_answer: Mutex::new(Ok(crate::ProviderInstall::default())),
            install_gates: Mutex::new(Vec::new()),
            reviewer_reads: Mutex::new(Vec::new()),
            reviewer_threads: Mutex::new(Vec::new()),
            reviewer_gates: Mutex::new(Vec::new()),
            reviewer_answer: Mutex::new(Ok(ReviewerModels {
                models: Vec::new(),
                selected: None,
                notices: Vec::new(),
            })),
            reviewer_saves: Mutex::new(Vec::new()),
            reviewer_saved: Mutex::new(Ok(())),
            gaps_answer: Mutex::new(Ok(Vec::new())),
            gap_reads: Mutex::new(Vec::new()),
            host_saves: Mutex::new(Vec::new()),
            host_saved: Mutex::new(Ok(HostSaved { masked: None })),
            notes: Mutex::new(Ok(empty_notes())),
            notes_saves: Mutex::new(Vec::new()),
            notes_saved: Mutex::new(Ok(NotesSaved::Written(Vec::new()))),
        }
    }

    /// The providers asked to log in through the browser so far, in order.
    pub(crate) fn login_names(&self) -> Vec<String> {
        self.login_names
            .lock()
            .map(|names| names.clone())
            .unwrap_or_default()
    }

    /// The providers `gaps` was asked about so far, in order.
    pub(crate) fn gap_reads(&self) -> Vec<String> {
        self.gap_reads
            .lock()
            .map(|reads| reads.clone())
            .unwrap_or_default()
    }

    /// The host answers `save_host` was asked to save so far, in order:
    /// the provider, key and value.
    pub(crate) fn host_saves(&self) -> Vec<(String, String, String)> {
        self.host_saves
            .lock()
            .map(|saves| saves.clone())
            .unwrap_or_default()
    }

    /// The `n`th browser login's `LoginShow`.
    pub(crate) fn login_show(&self, n: usize) -> Option<Arc<dyn LoginShow>> {
        self.login_shown
            .lock()
            .ok()
            .and_then(|shown| shown.get(n).cloned())
    }

    /// Answers the `n`th browser login's run with `result`.
    pub(crate) fn answer(&self, n: usize, result: Result<Stored, ConfigureError>) {
        let answers = self.answers.lock().expect("answers");
        let tx = answers.get(n).expect("one login started");
        drop(tx.send(result));
    }

    /// The `n`th browser login returned.
    pub(crate) fn login(&self, n: usize) -> Option<Arc<FakeLogin>> {
        self.logins
            .lock()
            .ok()
            .and_then(|logins| logins.get(n).cloned())
    }

    /// The providers asked for reviewer choices so far, in order.
    pub(crate) fn reviewer_reads(&self) -> Vec<String> {
        self.reviewer_reads
            .lock()
            .map(|reads| reads.clone())
            .unwrap_or_default()
    }

    /// The threads reviewer reads ran on, in order.
    pub(crate) fn reviewer_threads(&self) -> Vec<std::thread::ThreadId> {
        self.reviewer_threads
            .lock()
            .map(|threads| threads.clone())
            .unwrap_or_default()
    }

    /// The installs asked for so far, in order: the workspace and provider.
    pub(crate) fn installs(&self) -> Vec<(PathBuf, String)> {
        self.installs
            .lock()
            .map(|installs| installs.clone())
            .unwrap_or_default()
    }

    /// The threads installs ran on, in order.
    pub(crate) fn install_threads(&self) -> Vec<std::thread::ThreadId> {
        self.install_threads
            .lock()
            .map(|threads| threads.clone())
            .unwrap_or_default()
    }

    /// The threads hooks queries and installs ran on, in order.
    pub(crate) fn hooks_threads(&self) -> Vec<std::thread::ThreadId> {
        self.hooks_threads
            .lock()
            .map(|threads| threads.clone())
            .unwrap_or_default()
    }

    /// Queues a gate for the next install, returning its answer sender and
    /// the signal that the install is waiting: the install waits until the
    /// test answers or drops the sender.
    pub(crate) fn gate_install(
        &self,
    ) -> (
        std::sync::mpsc::Sender<Result<crate::ProviderInstall, ConfigureError>>,
        std::sync::mpsc::Receiver<()>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (entered, waiting) = std::sync::mpsc::channel();
        if let Ok(mut gates) = self.install_gates.lock() {
            gates.push(InstallGate {
                answer: rx,
                entered,
            });
        }
        (tx, waiting)
    }

    /// Queues a gate for the next reviewer read, returning its answer
    /// sender: the read waits until the test answers or drops it.
    pub(crate) fn gate_reviewer(
        &self,
    ) -> std::sync::mpsc::Sender<Result<ReviewerModels, ConfigureError>> {
        let (tx, rx) = std::sync::mpsc::channel();
        if let Ok(mut gates) = self.reviewer_gates.lock() {
            gates.push(rx);
        }
        tx
    }

    /// The notes saves asked for so far, in order.
    pub(crate) fn notes_saves(&self) -> Vec<NotesSave> {
        self.notes_saves
            .lock()
            .map(|saves| saves.clone())
            .unwrap_or_default()
    }

    /// The reviewer saves asked for so far, in order: provider and id.
    pub(crate) fn reviewer_saves(&self) -> Vec<(String, String)> {
        self.reviewer_saves
            .lock()
            .map(|saves| saves.clone())
            .unwrap_or_default()
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

    /// The fake home `delete_stored` removes files from: a test stores
    /// the credential files under it with plain `std::fs`.
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    /// The `/keys` saves asked for so far.
    pub(crate) fn keys_saved(&self) -> Vec<Vec<KeyEdit>> {
        self.keys_saved
            .lock()
            .map(|saved| saved.clone())
            .unwrap_or_default()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        // Best-effort cleanup: litter on failure is harmless.
        drop(std::fs::remove_dir_all(&self.home));
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
    fn blocking_hooks(
        &self,
        workspace: &Path,
        overrides: &[String],
    ) -> Result<Vec<String>, ConfigureError> {
        support::lock(&self.blocking_hooks_queries)
            .push((workspace.to_path_buf(), overrides.to_vec()));
        support::lock(&self.hooks_threads).push(std::thread::current().id());
        support::lock(&self.blocking_hooks_answer).clone()
    }

    fn install_hooks(&self, _clock: Arc<dyn Clock>) -> Result<(), ConfigureError> {
        *support::lock(&self.hooks_installs) += 1;
        support::lock(&self.hooks_threads).push(std::thread::current().id());
        support::lock(&self.hooks_install_answer).clone()
    }

    fn install_provider(
        &self,
        workspace: &Path,
        provider: &str,
        _clock: Arc<dyn Clock>,
    ) -> Result<crate::ProviderInstall, ConfigureError> {
        if let Ok(mut installs) = self.installs.lock() {
            installs.push((workspace.to_path_buf(), provider.to_owned()));
        }
        if let Ok(mut threads) = self.install_threads.lock() {
            threads.push(std::thread::current().id());
        }
        let gate = self
            .install_gates
            .lock()
            .ok()
            .and_then(|mut gates| gates.pop());
        if let Some(gate) = gate {
            gate.entered.send(()).unwrap_or(());
            return gate
                .answer
                .recv()
                .unwrap_or_else(|_| panic!("the install was never answered"));
        }
        self.install_answer.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |answer| answer.clone(),
        )
    }

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

    fn logout_rows(&self) -> Result<Vec<LogoutRow>, ConfigureError> {
        self.logout.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |rows| rows.clone(),
        )
    }

    fn delete_stored(&self, name: &str, label: Option<String>) -> Result<String, ConfigureError> {
        if let Ok(mut deletes) = self.deletes.lock() {
            deletes.push((name.to_owned(), label.clone()));
        }
        let answer = self.deleted.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |deleted| deleted.clone(),
        );
        if answer.is_ok() {
            // The hub lists what is on disk: the file goes through
            // plain `std::fs`, and the scripted rows lose the deleted
            // A refused delete removes nothing.
            let file = match &label {
                Some(label) => self.home.join(format!("credentials/{name}/{label}")),
                None => self.home.join(format!("credentials/{name}")),
            };
            // Best-effort: the tests assert the file they need.
            drop(std::fs::remove_file(file));
            if let Ok(mut rows) = self.logout.lock()
                && let Ok(rows) = rows.as_mut()
                && let Some(at) = rows.iter().position(|row| match row {
                    LogoutRow::Provider {
                        name: n, label: l, ..
                    } => n == name && Some(l.as_str()) == label.as_deref(),
                    LogoutRow::Secret { name: n, .. } => n == name && label.is_none(),
                })
            {
                rows.remove(at);
            }
        }
        answer
    }

    fn browser_login(
        &self,
        name: &str,
        shown: Arc<dyn LoginShow>,
        clock: Arc<dyn Clock>,
    ) -> Arc<dyn BrowserLogin> {
        let _ = clock;
        if let Ok(mut names) = self.login_names.lock() {
            names.push(name.to_owned());
        }
        if let Ok(mut all) = self.login_shown.lock() {
            all.push(Arc::clone(&shown));
        }
        let (login, answer) = FakeLogin::new();
        let login = Arc::new(login);
        if let Ok(mut logins) = self.logins.lock() {
            logins.push(Arc::clone(&login));
        }
        if let Ok(mut answers) = self.answers.lock() {
            answers.push(answer);
        }
        login
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

    fn reviewer_models(
        &self,
        name: &str,
        clock: Arc<dyn Clock>,
    ) -> Result<ReviewerModels, ConfigureError> {
        let _ = clock;
        if let Ok(mut reads) = self.reviewer_reads.lock() {
            reads.push(name.to_owned());
        }
        if let Ok(mut threads) = self.reviewer_threads.lock() {
            threads.push(std::thread::current().id());
        }
        let gate = self
            .reviewer_gates
            .lock()
            .ok()
            .and_then(|mut gates| gates.pop());
        if let Some(rx) = gate {
            return rx
                .recv()
                .unwrap_or_else(|_| panic!("the reviewer read was never answered"));
        }
        self.reviewer_answer.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |answer| answer.clone(),
        )
    }

    fn reviewer_notes(&self, workspace: &Path) -> Result<ReviewerNotes, ConfigureError> {
        let _ = workspace;
        self.notes.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |notes| notes.clone(),
        )
    }

    fn save_reviewer_notes(
        &self,
        workspace: &Path,
        global: Option<NotesWrite>,
        project: Option<NotesWrite>,
    ) -> Result<NotesSaved, ConfigureError> {
        if let Ok(mut saves) = self.notes_saves.lock() {
            saves.push((workspace.to_path_buf(), global.clone(), project.clone()));
        }
        self.notes_saved.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |saved| saved.clone(),
        )
    }

    fn set_reviewer(&self, name: &str, id: &str) -> Result<(), ConfigureError> {
        if let Ok(mut saves) = self.reviewer_saves.lock() {
            saves.push((name.to_owned(), id.to_owned()));
        }
        self.reviewer_saved.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |saved| saved.clone(),
        )
    }

    fn gaps(&self, name: &str) -> Result<Vec<HostGap>, ConfigureError> {
        if let Ok(mut reads) = self.gap_reads.lock() {
            reads.push(name.to_owned());
        }
        self.gaps_answer.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |answer| answer.clone(),
        )
    }

    fn save_host(&self, name: &str, key: &str, value: &str) -> Result<HostSaved, ConfigureError> {
        if let Ok(mut saves) = self.host_saves.lock() {
            saves.push((name.to_owned(), key.to_owned(), value.to_owned()));
        }
        self.host_saved.lock().map_or_else(
            |_| {
                Err(ConfigureError {
                    code: ErrorCode::IoFailed,
                    message: "poisoned".to_owned(),
                })
            },
            |saved| saved.clone(),
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

    fn import_pending(&self) -> bool {
        self.pending.lock().is_ok_and(|pending| *pending)
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
