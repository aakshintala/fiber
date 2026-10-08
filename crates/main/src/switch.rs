//! A second model for the `model` driver command
//! (`docs/model-routing.md`, "Naming a model" and "Thinking"): the
//! preparation `main` composes at startup and hands to `Loop::switcher`,
//! built from the same calls `parts_in` makes for the startup model. Any
//! installed model is reachable. A credential this process has not read
//! yet, and a Lua provider the session does not hold loaded, are read and
//! started on a thread of their own while the loop waits; shutdown ends
//! that wait (`docs/configuration.md`, "Secrets").

mod read;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use config::{Config, ProviderData, Secret};
use contract::clock::Clock;
use contract::commands::ModelArgs;
use contract::files::PathLock;
use contract::inbox::Rejection;
use contract::shapes::Failure;
use contract::{ErrorCode, ThinkingLevel};
use extensions::{LuaProvider, Providers, SessionExtensions};

pub(crate) use self::read::Reads;
use crate::lua_providers::{Access, KeyAndSigner};

/// The credentials read at startup, by provider name: the label, and the
/// key and signer.
pub(crate) type Credentials = BTreeMap<String, (String, KeyAndSigner)>;

/// Each key this process has read, by provider and label; `None` when a Lua
/// `credential()` supplies the token.
type Keys = BTreeMap<(String, String), Option<Secret>>;

/// What this process remembers of each provider's credential: the keys it
/// has read and the label each provider is on, under one lock so the two
/// change together, only once the whole preparation has succeeded. A key
/// here is never read again, so a `command` source runs once per process.
/// No signer is kept: it holds its Lua provider, which would stay loaded.
#[derive(Default)]
struct Remembered {
    /// Each key read, by provider and label.
    keys: Keys,
    /// The label each provider is on.
    selected: BTreeMap<String, String>,
}

/// What starts a Lua provider a switch needs and the session does not hold
/// loaded.
pub(crate) struct Loader {
    pub(crate) extensions: Arc<SessionExtensions>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) locks: Arc<dyn PathLock>,
    /// The extension that registered each Lua provider, by provider name.
    pub(crate) owners: BTreeMap<String, String>,
    /// The session's workspace, which a `scripted` model's path resolves
    /// against.
    pub(crate) workspace: PathBuf,
}

/// What preparing a switch reads: the whole startup registry, holding no
/// Lua provider, every model every provider names before placeholders are
/// filled, the configuration, each key read so far with the label each
/// provider is on, and the Lua providers loaded now: the session's and its
/// reviewer's.
pub(crate) struct Switching {
    registry: Providers,
    naming: Vec<(String, String)>,
    config: Config,
    /// Each configured `tools."<name>".max_result_bytes`, for a hosted
    /// search the switch declares.
    caps: r#loop::ResultCaps,
    remembered: Mutex<Remembered>,
    loaded: Mutex<BTreeMap<String, Arc<LuaProvider>>>,
    loader: Loader,
    reads: Arc<Reads>,
}

/// What a switch publishes to the session's door when it applies.
pub(crate) struct Door {
    /// Replaces or removes one entry of the `tools` answer.
    pub(crate) declare: doors::Declare,
    /// A `web_search` an extension or MCP server registered at start
    /// replaced the hosted one, and it stands across switches.
    pub(crate) hosted_stands: bool,
}

/// Whether a `web_search` registered by anything other than `builtin` is
/// among `tools`: that one replaced the hosted search at start
/// (`docs/architecture.md`, "Tool seam").
pub(crate) fn hosted_stands(tools: &[(String, Arc<dyn contract::tool::Tool>)]) -> bool {
    tools
        .iter()
        .any(|(by, tool)| by != "builtin" && tool.definition().name == "web_search")
}

impl Switching {
    /// `registry` is the startup registry after its Lua providers were
    /// added and placeholders filled; the switch keeps it holding no Lua
    /// handle. `credentials` are the keys read at startup, and `loaded` the
    /// Lua providers the session and its reviewer use.
    pub(crate) fn new(
        mut registry: Providers,
        naming: Vec<(String, String)>,
        config: Config,
        credentials: Credentials,
        loaded: Vec<(String, Arc<LuaProvider>)>,
        loader: Loader,
    ) -> Self {
        registry.forget_lua();
        let mut remembered = Remembered::default();
        for (name, (label, (key, _))) in credentials {
            remembered.keys.insert((name.clone(), label.clone()), key);
            remembered.selected.insert(name, label);
        }
        Self {
            registry,
            naming,
            caps: crate::settings::result_caps(&config),
            config,
            remembered: Mutex::new(remembered),
            loaded: Mutex::new(loaded.into_iter().collect()),
            loader,
            reads: Arc::default(),
        }
    }

    /// What shutdown cancels: every read a switch has running.
    pub(crate) fn reads(&self) -> Arc<Reads> {
        Arc::clone(&self.reads)
    }

    /// The `Loop::switcher` closure: `prepare` over the shared `Switching`,
    /// publishing to `door` when a switch applies.
    pub(crate) fn closure(self, door: Door) -> r#loop::Prepare {
        let shared = Arc::new(self);
        Arc::new(move |args, label, chosen| prepare(&shared, &door, args, label, chosen))
    }

    /// Holds exactly `keep` loaded: the Lua providers of the session and the
    /// reviewer a switch just applied. Any other is unloaded once nothing
    /// else holds it, such as a cost lookup it still owes
    /// (`docs/model-routing.md`, "Model discovery").
    fn keep_loaded(&self, keep: Vec<(String, Arc<LuaProvider>)>) {
        let dropped = std::mem::replace(
            &mut *self.loaded.lock().unwrap_or_else(PoisonError::into_inner),
            keep.into_iter().collect(),
        );
        drop(dropped);
    }

    /// What the read for provider `name` starts from: its label, its key
    /// when this process already read one, and its Lua provider when it is
    /// loaded. `label` is the `credential` command's label, else the
    /// provider's selected label, else the configured one.
    fn want(&self, name: &str, label: Option<&str>) -> Want {
        let remembered = self
            .remembered
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let selected = match label {
            Some(label) => label.to_owned(),
            None => self.selected_of(&remembered, name, self.registry.get(name)),
        };
        let known = remembered
            .keys
            .get(&(name.to_owned(), selected.clone()))
            .cloned();
        let lua = self
            .loaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned();
        Want {
            name: name.to_owned(),
            label: selected,
            known,
            lua,
        }
    }

    /// The label provider `name` is on: its selected label, else the
    /// configured one for `data`, else nothing when the registry holds no
    /// such provider. The read calls this under the lock at read time, so
    /// a `prepare` for the same provider that publishes `selected` between
    /// `want()` and the read is seen.
    fn selected_of(
        &self,
        remembered: &Remembered,
        name: &str,
        data: Option<&ProviderData>,
    ) -> String {
        remembered
            .selected
            .get(name)
            .cloned()
            .unwrap_or_else(|| match data {
                Some(data) => self.config.credential_label(data),
                None => String::new(),
            })
    }

    /// Reads provider `want.name`'s access on the read thread: starts its Lua
    /// provider when none is loaded and an extension registered it, then
    /// reads its key unless one was read before, and builds its signer. A
    /// scripted provider reads nothing. Writes neither the keys nor the
    /// loaded providers.
    fn read(&self, want: Want) -> Result<Got, Failure> {
        let data = self.registry.get(&want.name).ok_or_else(|| {
            doors::failure(
                ErrorCode::InvalidArguments,
                format!("No provider `{}` is installed.", want.name),
            )
        })?;
        let mut file = None;
        let access = crate::scripted::access(data, || {
            let lua = match (want.lua, self.loader.owners.get(&want.name)) {
                (Some(lua), _) => Some(lua),
                (None, Some(extension)) => Some(
                    self.loader
                        .extensions
                        .start_provider(
                            &self.config,
                            Arc::clone(&self.loader.clock),
                            Arc::clone(&self.loader.locks),
                            extension,
                            &want.name,
                        )
                        .map_err(|e| crate::failed(e.code(), e))?,
                ),
                (None, None) => None,
            };
            let read = crate::lua_providers::session_credential(
                lua.as_ref(),
                data,
                &want.label,
                || {
                    if let Some(key) = want.known.clone().flatten() {
                        return Ok(key);
                    }
                    // A label that names no credential is `credential_missing`,
                    // naming the labels there are, before any configured
                    // source is read or any command runs; the label the
                    // provider is on now answers without a read
                    // (`docs/model-routing.md`, "Which credential a session uses").
                    // A Lua `credential()` provider never reaches this
                    // callback, and a scripted provider never reads, so both
                    // accept any label.
                    let current = self.selected_of(
                        &self
                            .remembered
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner),
                        &want.name,
                        Some(data),
                    );
                    let labels = self.config.labels(data);
                    if want.label != current && !labels.contains(&want.label) {
                        let listed = config::Config::listed(&labels);
                        return Err(doors::failure(
                            ErrorCode::CredentialMissing,
                            format!(
                                "`{}` has no credential label `{}`. The labels for `{}` are: {listed}",
                                want.name, want.label, want.name
                            ),
                        ));
                    }
                    let run = |command: &mut std::process::Command| self.reads.command(command);
                    let read = crate::credential::switch_credential(
                        &self.config,
                        data,
                        &want.label,
                        &run,
                    )?;
                    file = read.file;
                    Ok(read.secret)
                },
            )?;
            Ok(Access::new(lua.as_ref(), read))
        })?;
        Ok(Got {
            label: want.label,
            access,
            file,
        })
    }
}

/// Where one provider's read for a switch starts.
struct Want {
    name: String,
    label: String,
    /// The key read before, if any.
    known: Option<Option<Secret>>,
    /// Its Lua provider, when loaded.
    lua: Option<Arc<LuaProvider>>,
}

/// One provider's access, read for a switch, and the `file` source it read.
struct Got {
    label: String,
    access: Access,
    file: Option<PathBuf>,
}

/// What the read job answers: the session provider's access, and the
/// reviewer's when it is on another provider.
type Read = Result<(Got, Option<Result<Got, Failure>>), Failure>;

/// Prepares a switch from the same calls `parts_in` makes for the startup
/// model: resolving, thinking, the reviewer's model, the credential and
/// Lua provider, connecting, the cache lifetime, the handoff settings, the
/// addendum and the hosted search. Every check that can reject runs before
/// the read, so a read's key is cached only for an admitted switch; a
/// failed read rejects with its own code and caches nothing. The keys and
/// the selected labels publish together, under one lock, only after the
/// whole preparation (read, `connect`, the reviewer choice) has succeeded.
/// The reviewer's failure is not a rejection: the loop gets it, and every
/// reviewed call escalates it (`docs/permissions.md`, "How it runs").
/// `label` is the `credential` command's label; `None` keeps the
/// provider's selected label.
pub(crate) fn prepare(
    switching: &Arc<Switching>,
    door: &Door,
    args: &ModelArgs,
    label: Option<&str>,
    chosen: Option<ThinkingLevel>,
) -> Result<r#loop::Prepared, Rejection> {
    // An unparseable thinking level rejects before any lookup.
    let asked = match &args.thinking {
        Some(level) => Some(
            level
                .parse::<ThinkingLevel>()
                .map_err(|_| invalid(format!("unknown thinking level `{level}`")))?,
        ),
        None => None,
    };
    let resolved = resolve(&switching.registry, &switching.naming, &args.model)?;
    let reference = resolved.reference();
    let context_window = crate::settings::context_window(resolved.model, &reference)
        .map_err(|failure| invalid(failure.message))?;
    let mut notices = Vec::new();
    let thinking = crate::settings::thinking(
        resolved.thinking,
        asked.or(chosen),
        &switching.config,
        resolved.model,
        &reference,
        &mut notices,
    )
    .map_err(rejection)?;
    crate::connect::speaks(resolved.model.protocol, &reference)
        .map_err(|failure| invalid(failure.message))?;
    // The reviewer's model, before the read: Fiber never reviews with the
    // session's own model.
    let judge = crate::reviewer_reference(&switching.config, &resolved).and_then(|typed| {
        let model = switching
            .registry
            .resolve(&typed)
            .map_err(|e| crate::failed(e.code(), e))?;
        Ok((model.reference(), model.provider.name.clone()))
    });
    if let Ok((judged, _)) = &judge
        && *judged == reference
    {
        return Err(invalid(format!(
            "`{reference}` is this session's reviewer model; set `reviewer.model` to another model first."
        )));
    }
    let (web_search, publish) = hosted(switching, door, resolved.model.web_search.as_deref())?;
    let session = resolved.provider.name.clone();
    let session_want = switching.want(&session, label);
    let judge_want = judge
        .as_ref()
        .ok()
        .filter(|(_, name)| *name != session)
        .map(|(_, name)| switching.want(name, None));
    let shared = Arc::clone(switching);
    let read: Read = switching
        .reads
        .run(move || {
            let got = shared.read(session_want)?;
            Ok((got, judge_want.map(|want| shared.read(want))))
        })
        .map_err(rejection)?;
    let (got, judge_got) = read.map_err(rejection)?;
    let here = crate::Here {
        workspace: switching.loader.workspace.clone(),
        clock: Arc::clone(&switching.loader.clock),
    };
    let provider = crate::connect(
        resolved,
        got.access.key.clone(),
        got.access.signer.clone(),
        got.access.lua.as_ref(),
        &here,
    )
    .map_err(|failure| invalid(failure.message))?;
    let mut lookup = |provider: &ProviderData| -> Result<Access, Failure> {
        match &judge_got {
            _ if provider.name == session => Ok(got.access.clone()),
            Some(Ok(judged)) => Ok(judged.access.clone()),
            Some(Err(failure)) => Err(failure.clone()),
            None => Err(doors::failure(
                ErrorCode::InvalidArguments,
                format!("The credential for `{}` was not read.", provider.name),
            )),
        }
    };
    let reviewer = crate::choose_reviewer(
        &switching.registry,
        &switching.config,
        &resolved,
        &here,
        &mut lookup,
    );
    // The keys and the selected labels publish together, under one lock,
    // only once the whole preparation has succeeded: a `connect` failure
    // or a cancelled read publishes nothing (`docs/model-routing.md`,
    // "When a credential is missing or fails").
    {
        let mut remembered = switching
            .remembered
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        remembered
            .keys
            .entry((session.clone(), got.label.clone()))
            .or_insert_with(|| got.access.key.clone());
        remembered
            .selected
            .insert(session.clone(), got.label.clone());
        if let Some(Ok(judged)) = &judge_got
            && let Ok((_, name)) = &judge
        {
            remembered
                .keys
                .entry((name.clone(), judged.label.clone()))
                .or_insert_with(|| judged.access.key.clone());
            remembered
                .selected
                .insert(name.clone(), judged.label.clone());
        }
    }
    // Loaded after the switch applies: the session's Lua provider, and the
    // reviewer's when the reviewer stands.
    let judged = judge_got.and_then(Result::ok);
    let mut keep: Vec<(String, Arc<LuaProvider>)> = Vec::new();
    for lua in [Some(&got), judged.as_ref().filter(|_| reviewer.is_ok())]
        .into_iter()
        .flatten()
        .filter_map(|read| read.access.lua.as_ref())
    {
        keep.push((lua.name().to_owned(), Arc::clone(lua)));
    }
    let credential_files = [Some(&got), judged.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|read| read.file.clone())
        .collect();
    let shared = Arc::clone(switching);
    let applied: Applied = Box::new(move || {
        shared.keep_loaded(keep);
        if let Some(publish) = publish {
            publish();
        }
    });
    Ok(r#loop::Prepared {
        provider,
        model: r#loop::Model {
            reference: reference.clone(),
            cost: resolved.model.cost.clone().map(crate::cost::declared),
            subscription: resolved.model.subscription,
        },
        thinking,
        // The suffix first, then what was asked for now, then the
        // session's choice.
        chosen: resolved.thinking.or(asked).or(chosen),
        credential: Some(got.label),
        cache_lifetime: crate::settings::cache_lifetime(&switching.config, &reference),
        context_window,
        addendum: switching.registry.addendum(&resolved).map(str::to_owned),
        handoff: crate::handoff::handoff_settings(&switching.config, &reference),
        reviewer,
        web_search,
        // At most one notice: a configured level the model lacks.
        notice: notices.into_iter().next(),
        applied: Some(applied),
        credential_files,
    })
}

/// What `Prepared.applied` runs when the switch applies.
type Applied = Box<dyn FnOnce() + Send>;

/// The hosted search after the switch, and what applying it publishes to
/// the door's `tools` answer: the new model's hosted search when it has
/// one, else none, unless another registrant's `web_search` stands
/// (`docs/tools.md`, "Hosted by the provider").
fn hosted(
    switching: &Switching,
    door: &Door,
    kind: Option<&str>,
) -> Result<(r#loop::Hosted, Option<Applied>), Rejection> {
    if door.hosted_stands {
        return Ok((r#loop::Hosted::Keep, None));
    }
    let declare = Arc::clone(&door.declare);
    let Some(kind) = kind else {
        let applied = Box::new(move || declare("web_search", None));
        return Ok((
            r#loop::Hosted::Withdraw("web_search".to_owned()),
            Some(applied),
        ));
    };
    let (tool, info) = crate::builtin::hosted(kind).map_err(rejection)?;
    let tool = r#loop::capped(
        vec![("builtin".to_owned(), Arc::clone(&tool))],
        &switching.caps,
    )
    .pop()
    .map_or(tool, |(_, capped)| capped);
    let applied = Box::new(move || {
        let name = info.name.clone();
        declare(&name, Some(info));
    });
    Ok((r#loop::Hosted::Declare(tool), Some(applied)))
}

/// The typed model against the switch registry and the naming list: an
/// exact `provider/model` resolves as at startup, while a bare id counts
/// every provider that names it, configured or not. A bare id tries the
/// full typed id first, so a literal id ending in a thinking level matches
/// before the suffix strips (`docs/model-routing.md`, "Naming a model": the
/// exact match comes first, since OpenRouter ids contain colons); only when
/// nothing names the full id does the stripped id count. Every failure is
/// the resolution's own message as `invalid_arguments`.
fn resolve<'a>(
    registry: &'a Providers,
    naming: &[(String, String)],
    typed: &str,
) -> Result<extensions::Model<'a>, Rejection> {
    let (rest, _) = Providers::split_thinking(typed);
    if rest.contains('/') {
        // A scripted model is added to the registry only at start.
        return registry.resolve(typed).map_err(|error| {
            invalid(if rest.starts_with("scripted/") {
                crate::scripted::START_ONLY.to_owned()
            } else {
                error.to_string()
            })
        });
    }
    if typed != rest
        && let Some(prepared) = resolve_literal(registry, naming, typed)
    {
        return prepared;
    }
    let mut matches = named(naming, rest);
    match registry.resolve(typed) {
        Ok(model) => {
            let reference = model.reference();
            if !matches.contains(&reference) {
                matches.push(reference);
            }
            if matches.len() > 1 {
                return Err(ambiguous(rest, &mut matches));
            }
            Ok(model)
        }
        Err(error) => {
            if let extensions::Error::Ambiguous {
                matches: theirs, ..
            } = &error
            {
                for reference in theirs {
                    if !matches.contains(reference) {
                        matches.push(reference.clone());
                    }
                }
            }
            if matches.len() > 1 {
                return Err(ambiguous(rest, &mut matches));
            }
            Err(invalid(error.to_string()))
        }
    }
}

/// The `provider/model` references the naming list holds for the bare id
/// `text`.
fn named(naming: &[(String, String)], text: &str) -> Vec<String> {
    naming
        .iter()
        .filter(|(_, id)| id == text)
        .map(|(provider, id)| format!("{provider}/{id}"))
        .collect()
}

/// A bare id that is also a literal model id ending in a thinking level:
/// the full typed id against the registry and the naming list, before the
/// suffix strips. `Some` when the full id names anything: the match or its
/// ambiguity. `None` when nothing in the registry names it, and the
/// stripped id counts instead.
fn resolve_literal<'a>(
    registry: &'a Providers,
    naming: &[(String, String)],
    typed: &str,
) -> Option<Result<extensions::Model<'a>, Rejection>> {
    let mut matches = named(naming, typed);
    // Check the exact provider/model first: bare resolution can discard an
    // unconfigured literal for suffix matches. Qualified resolution can
    // strip a suffix too, so require the error to name this exact reference.
    if let [reference] = matches.as_slice()
        && let Err(extensions::Error::Unconfigured { message }) = registry.resolve(reference)
        && message.starts_with(&format!("The model `{reference}` "))
    {
        return Some(Err(invalid(message)));
    }
    match registry.resolve(typed) {
        // The registry's own exact-first order tries the full id before
        // the suffix strips, so a model whose id is the full text is a
        // full-id match; anything else resolved through the suffix.
        Ok(model) if model.model.id == typed => {
            let reference = model.reference();
            if !matches.contains(&reference) {
                matches.push(reference);
            }
            if matches.len() > 1 {
                return Some(Err(ambiguous(typed, &mut matches)));
            }
            return Some(Ok(model));
        }
        Err(extensions::Error::Ambiguous {
            id,
            matches: theirs,
        }) if id == typed => {
            for reference in theirs {
                if !matches.contains(&reference) {
                    matches.push(reference.clone());
                }
            }
            return Some(Err(ambiguous(typed, &mut matches)));
        }
        Ok(_) | Err(_) => {}
    }
    if matches.len() > 1 {
        return Some(Err(ambiguous(typed, &mut matches)));
    }
    None
}

/// A rejected switch: the message with code `invalid_arguments`.
fn invalid(message: String) -> Rejection {
    Rejection {
        code: ErrorCode::InvalidArguments,
        message,
    }
}

/// A failure as a rejection with its own code.
fn rejection(failure: Failure) -> Rejection {
    Rejection {
        code: failure.code,
        message: failure.message,
    }
}

/// A bare id more than one provider names, in the shape `resolve` names
/// it.
fn ambiguous(id: &str, matches: &mut Vec<String>) -> Rejection {
    matches.sort();
    matches.dedup();
    invalid(format!(
        "The model `{id}` is offered by more than one provider: {}. \
         Name one as `provider/model`.",
        matches.join(", ")
    ))
}

#[cfg(test)]
#[path = "switch_tests.rs"]
mod tests;
