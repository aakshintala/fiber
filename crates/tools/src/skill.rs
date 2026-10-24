//! The `skill` tool (`docs/tools.md`, "Skills"): the model loads one
//! skill from the skills listing by name and gets back its `SKILL.md`
//! body, read from disk at the call.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use contract::emit::Emit;
use contract::events::{Control, SkillLoad};
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Effect};
use contract::skills::{SkillRead, Skills};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::error::SkillError;
use crate::files::path_text;
use crate::tool_util::{cancelled_before, effects, errored, no_effects};

/// The call's arguments, checked against the schema before the call runs.
#[derive(Debug, Deserialize)]
struct Args {
    name: String,
}

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
        support::lock(&self.judged)
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
        let Ok(args) = crate::tool_util::arguments::<Args>(arguments) else {
            return Ok(no_effects());
        };
        let name = args.name;
        let Ok((target, _)) = self.resolve(&name) else {
            self.judged().remove(&name);
            return Ok(no_effects());
        };
        self.judged().insert(name, target.clone());
        let path = path_text(&target);
        Ok(effects(
            vec![Effect::Reads],
            true,
            Some(vec![path]),
            Some(String::new()),
            None,
        ))
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        if cancel.is_cancelled() {
            return cancelled_before();
        }
        let args: Args = match crate::tool_util::parsed(arguments) {
            Ok(args) => args,
            Err(error) => return errored(SkillError::from(error)),
        };
        let name = args.name;
        let (target, listing) = match self.resolve(&name) {
            Ok(resolved) => resolved,
            Err(Resolve::NotListed) => {
                return errored(SkillError::NotListed { name: name.clone() });
            }
            Err(Resolve::Unreadable { file, error }) => {
                return errored(SkillError::Unreadable { file, error });
            }
            Err(Resolve::NotUtf8) => {
                return errored(SkillError::NotUtf8 { name: name.clone() });
            }
        };
        if self.judged().get(&name) != Some(&target) {
            return errored(SkillError::PathChanged { name: name.clone() });
        }
        let body = match self.skills.body(&name, &target) {
            Ok(body) => body,
            Err(SkillRead::Io(message)) => return errored(SkillError::Io { message }),
            Err(SkillRead::Invalid) => {
                return errored(SkillError::Invalid {
                    name: name.clone(),
                    listing: listing.clone(),
                });
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
                name: None,
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
