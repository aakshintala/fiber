//! The providers installed extensions register as data, and the session's
//! model chosen from them (`docs/model-routing.md`, "Naming a model" and
//! "Choosing the model").

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use config::{Config, ModelData, ProviderData};
use contract::ErrorCode;
use contract::events::Notice;
use serde_json::Value;

use crate::{API, Error};

/// The thinking levels a typed model may end in, after a `:`.
const THINKING: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Every provider the installed extensions register, by name.
#[derive(Debug, Clone, Default)]
pub struct Providers {
    by_name: BTreeMap<String, ProviderData>,
}

/// One model of one installed provider, as a session names it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model<'a> {
    /// Its provider's data.
    pub provider: &'a ProviderData,
    /// The model's data.
    pub model: &'a ModelData,
    /// The thinking level a `:<level>` suffix asked for.
    pub thinking: Option<&'static str>,
}

impl Model<'_> {
    /// The stored model reference, `provider/model`.
    pub fn reference(&self) -> String {
        format!("{}/{}", self.provider.name, self.model.id)
    }
}

impl Providers {
    /// Reads every extension in `extensions/` in Fiber home. One written for
    /// another extension API is left out, with a notice naming it and both
    /// numbers.
    pub fn load(home: &Path) -> Result<(Self, Vec<Notice>), Error> {
        let root = home.join("extensions");
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok((Self::default(), Vec::new())),
            Err(source) => return Err(Error::Io { path: root, source }),
        };
        let mut dirs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| Error::Io {
                path: root.clone(),
                source,
            })?;
            // A name starting with `.` is an install in progress.
            if !entry.file_name().to_string_lossy().starts_with('.') {
                dirs.push(entry.path());
            }
        }
        dirs.sort();
        let mut providers = Self::default();
        let mut notices = Vec::new();
        for dir in dirs {
            let manifest = config::read_manifest(&dir)?;
            if manifest.api != API {
                notices.push(Notice {
                    code: ErrorCode::ExtensionIncompatible,
                    message: Error::ApiVersion {
                        name: manifest.name.clone(),
                        api: manifest.api,
                    }
                    .to_string(),
                    extension: Some(manifest.name),
                });
                continue;
            }
            for mut data in config::read_providers(&dir)? {
                data.models
                    .retain(|model| match model.web_search.as_deref() {
                        Some(kind) if !model.protocol.reads_web_search(kind) => {
                            notices.push(Notice {
                                code: ErrorCode::ModelInvalid,
                                message: format!(
                                    "The model `{}/{}` names `{kind}` as its `web_search` type, \
                                 which its protocol does not read.",
                                    data.name, model.id
                                ),
                                extension: Some(manifest.name.clone()),
                            });
                            false
                        }
                        _ => true,
                    });
                providers.by_name.insert(data.name.clone(), data);
            }
        }
        Ok((providers, notices))
    }

    /// The installed providers' names, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    /// The provider installed under `name`.
    pub fn get(&self, name: &str) -> Option<&ProviderData> {
        self.by_name.get(name)
    }

    /// The session's model, in the order "Choosing the model" gives: the
    /// model a resumed session was using, then configuration's `model`, which
    /// `--model` and `-c model=` set for one run.
    pub fn choose<'a>(
        &'a self,
        resumed: Option<&str>,
        config: &Config,
    ) -> Result<Model<'a>, Error> {
        let configured = config.get("model", None).map(|(value, _)| value);
        let typed = resumed
            .or_else(|| configured.as_ref().and_then(Value::as_str))
            .ok_or(Error::NoModel)?;
        self.resolve(typed)
    }

    /// A model as a person types it: the exact `provider/model`, then the
    /// same with a `:<thinking level>` suffix taken off, then a bare id that
    /// exactly one installed provider has.
    pub fn resolve(&self, typed: &str) -> Result<Model<'_>, Error> {
        let stripped = typed.rsplit_once(':').and_then(|(rest, level)| {
            THINKING
                .iter()
                .find(|known| **known == level)
                .map(|known| (rest, *known))
        });
        let mut tries = vec![(typed, None)];
        tries.extend(stripped.map(|(rest, level)| (rest, Some(level))));
        for (text, thinking) in &tries {
            if let Some(found) = self.exact(text, *thinking) {
                return Ok(found);
            }
        }
        for (text, thinking) in &tries {
            let matches: Vec<Model<'_>> = self
                .by_name
                .values()
                .flat_map(|provider| {
                    provider
                        .models
                        .iter()
                        .filter(|model| model.id == *text)
                        .map(move |model| Model {
                            provider,
                            model,
                            thinking: *thinking,
                        })
                })
                .collect();
            match matches.as_slice() {
                [] => {}
                [one] => return Ok(*one),
                [..] => {
                    return Err(Error::Ambiguous {
                        id: (*text).into(),
                        matches: matches.iter().map(Model::reference).collect(),
                    });
                }
            }
        }
        let (text, _) = stripped.unwrap_or((typed, ""));
        Err(match text.split_once('/') {
            Some((provider, model)) if self.by_name.contains_key(provider) => Error::UnknownModel {
                provider: provider.into(),
                model: model.into(),
            },
            Some((provider, _)) => Error::ProviderMissing {
                provider: provider.into(),
            },
            None => Error::ModelMissing { id: text.into() },
        })
    }

    fn exact(&self, text: &str, thinking: Option<&'static str>) -> Option<Model<'_>> {
        let (name, id) = text.split_once('/')?;
        let provider = self.by_name.get(name)?;
        let model = provider.models.iter().find(|m| m.id == id)?;
        Some(Model {
            provider,
            model,
            thinking,
        })
    }
}
