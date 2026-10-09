//! The `skill` tool (`docs/tools.md`, "Skills"): the model loads one
//! skill from the skills listing by name and gets back its `SKILL.md`
//! body, read from disk at the call.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use contract::ErrorCode;
use contract::emit::Emit;
use contract::events::{Control, SkillLoad};
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect};
use contract::skills::{SkillRead, Skills};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use crate::files::{failed, path_text, string_argument, text_output};

/// Loads one skill from the skills listing. The definition never lists
/// skill names, so adding or removing a skill leaves the tool set's bytes
/// unchanged.
pub struct Skill {
    skills: Arc<dyn Skills>,
    judged: Mutex<BTreeMap<String, PathBuf>>,
}

impl Skill {
    /// A `skill` tool reading through `skills`.
    pub fn new(skills: Arc<dyn Skills>) -> Self {
        Self {
            skills,
            judged: Mutex::new(BTreeMap::new()),
        }
    }

    /// The skill's file, its canonical target and its listing path: the
    /// file the listing holds under `name` now, resolved to where the
    /// bytes really are.
    fn resolve(&self, name: &str) -> Result<(PathBuf, String), Resolve> {
        let file = self.skills.file(name).ok_or(Resolve::NotListed)?;
        let listing = path_text(&file);
        let target = std::fs::canonicalize(&file).map_err(|error| Resolve::Unreadable {
            file,
            error: error.to_string(),
        })?;
        if target.to_str().is_none() {
            return Err(Resolve::NotUtf8);
        }
        Ok((target, listing))
    }

    fn judged(&self) -> MutexGuard<'_, BTreeMap<String, PathBuf>> {
        self.judged.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Why the resolver could not name a real, UTF-8 target.
enum Resolve {
    /// The listing holds no such name.
    NotListed,
    /// The file does not resolve.
    Unreadable {
        /// The file discovery opened.
        file: PathBuf,
        /// The resolution failure.
        error: String,
    },
    /// The resolved target is not valid UTF-8.
    NotUtf8,
}

/// No effect: the call takes the fast path and `run` fails it.
fn no_effect() -> Effects {
    Effects {
        declared: DeclaredEffects {
            effects: Vec::new(),
            reversible: true,
            paths: None,
        },
        subject: Some(String::new()),
        prefix: None,
        always_reviewed: false,
    }
}

impl Tool for Skill {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "skill".to_owned(),
            description: "Loads a skill from the skills listing: give its name, and get back \
                 its instructions under the path of its SKILL.md, so you can read the files it \
                 refers to with `read`."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The skill's name, as the skills listing gives it."
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let Ok(name) = string_argument(arguments, "name", "Give the skill's name as `name`.")
        else {
            return Ok(no_effect());
        };
        let Ok((target, _)) = self.resolve(&name) else {
            return Ok(no_effect());
        };
        self.judged().insert(name, target.clone());
        let path = path_text(&target);
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: Some(vec![path]),
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        if cancel.is_cancelled() {
            return text_output("Cancelled before it started.\n".to_owned());
        }
        let Ok(name) = string_argument(arguments, "name", "Give the skill's name as `name`.")
        else {
            return failed(
                ErrorCode::InvalidArguments,
                "Give the skill's name as `name`.".to_owned(),
            );
        };
        let (target, listing) = match self.resolve(&name) {
            Ok(resolved) => resolved,
            Err(Resolve::NotListed) => {
                return failed(
                    ErrorCode::InvalidArguments,
                    format!(
                        "No skill named `{name}` is in the skills listing, or it is switched off. \
                         Load a name from the listing."
                    ),
                );
            }
            Err(Resolve::Unreadable { file, error }) => {
                return failed(
                    ErrorCode::IoFailed,
                    format!("Could not read skill {}: {error}.", file.display()),
                );
            }
            Err(Resolve::NotUtf8) => {
                return failed(
                    ErrorCode::InvalidArguments,
                    format!("Skill `{name}` cannot be loaded: its path is not valid UTF-8."),
                );
            }
        };
        if self.judged().get(&name) != Some(&target) {
            return failed(
                ErrorCode::PathChanged,
                format!(
                    "`{name}`'s SKILL.md changed between the permission check and the read. \
                     Nothing was read."
                ),
            );
        }
        let body = match self.skills.body(&name, &target) {
            Ok(body) => body,
            Err(SkillRead::Io(message)) => return failed(ErrorCode::IoFailed, message),
            Err(SkillRead::Invalid) => {
                return failed(
                    ErrorCode::InvalidArguments,
                    format!(
                        "Skill `{name}` at {listing} cannot be loaded: its header no longer \
                         parses, names another skill, or turns off model invocation."
                    ),
                );
            }
        };
        Output {
            content: vec![ContentPart::Text {
                text: format!("{listing}\n\n{body}"),
            }],
            control: Some(Control {
                handoff: None,
                questions: None,
                skill: Some(SkillLoad {
                    name,
                    path: listing,
                }),
            }),
            ..Output::default()
        }
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("skill")
    }
}

#[cfg(test)]
#[path = "skill_tests.rs"]
mod tests;
