//! A recording [`Configure`] for the views' tests: rows it was given, the
//! writes it was asked for, and an answer for each write.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use contract::ErrorCode;

use crate::ThemeSetting;
use crate::configure::{Configure, ConfigureError, Layer, Saved, SettingRow, Shown, WriteScope};

/// One write the view asked for: the workspace, layer, key and text.
pub(crate) type Write = (PathBuf, Layer, String, String);

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
    /// The theme files `themes` lists.
    pub(crate) themes: Vec<String>,
}

impl Fake {
    /// A seam answering `rows`, saving every write.
    pub(crate) fn new(rows: Vec<SettingRow>) -> Self {
        Self {
            rows: Mutex::new(Ok(rows)),
            reads: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
            refuse: Mutex::new(None),
            themes: Vec::new(),
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
                warnings: Vec::new(),
            }),
        }
    }

    fn global_file(&self) -> PathBuf {
        file(Layer::Global)
    }

    fn themes(&self) -> Vec<String> {
        self.themes.clone()
    }

    fn theme(&self, name: &str) -> ThemeSetting {
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
}
