//! A second model for the `model` driver command
//! (`docs/model-routing.md`, "Naming a model" and "Thinking"): the
//! preparation `main` composes at startup and hands to `Loop::switcher`,
//! built from the same calls `parts_in` makes for the startup model. Any
//! installed model is reachable. A credential this process has not read
//! yet, and a Lua provider the session does not hold loaded, are read and
//! started on a thread of their own while the loop waits; shutdown ends
//! that wait (`docs/configuration.md`, "Secrets").

mod labels;
pub(crate) mod quota;
mod read;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use config::{Config, ProviderData, Secret};
use contract::clock::Clock;
use contract::commands::ModelArgs;
use contract::diag::Op;
use contract::files::PathLock;
use contract::inbox::Rejection;
use contract::provider::{Cost, Provider};
use contract::shapes::Failure;
use contract::{ErrorCode, ThinkingLevel};
use extensions::{LuaProvider, Providers, SessionExtensions};

pub(crate) use self::read::{CancelOnDrop, Reads};
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
#[derive(Clone)]
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

/// Serialises starts without owning a provider's lifetime.
pub(crate) struct StartLock {
    started: Mutex<BTreeMap<String, Weak<LuaProvider>>>,
    #[cfg(test)]
    pause: Mutex<Option<(String, std::sync::mpsc::Sender<Ack>)>>,
}

#[cfg(test)]
#[derive(Debug, PartialEq)]
enum Point {
    P,
    Q,
}

#[cfg(test)]
struct Ack {
    point: Point,
    thread: String,
    release: Option<std::sync::mpsc::Sender<()>>,
}

impl StartLock {
    fn seeded(loaded: &[(String, Arc<LuaProvider>)]) -> Self {
        Self {
            started: Mutex::new(
                loaded
                    .iter()
                    .map(|(name, lua)| (name.clone(), Arc::downgrade(lua)))
                    .collect(),
            ),
            #[cfg(test)]
            pause: Mutex::default(),
        }
    }

    fn start(
        &self,
        loader: &Loader,
        config: &Config,
        name: &str,
        extension: &str,
    ) -> Result<Arc<LuaProvider>, extensions::Error> {
        #[cfg(test)]
        self.checkpoint(name, Point::P);
        let mut started = self.started.lock().unwrap_or_else(PoisonError::into_inner);
        let found = started.get(name).and_then(Weak::upgrade);
        #[cfg(test)]
        self.checkpoint(name, Point::Q);
        if let Some(lua) = found {
            return Ok(lua);
        }
        let lua = loader.extensions.start_provider(
            config,
            Arc::clone(&loader.clock),
            Arc::clone(&loader.locks),
            extension,
            name,
        )?;
        started.insert(name.to_owned(), Arc::downgrade(&lua));
        Ok(lua)
    }

    #[cfg(test)]
    fn pause_at_lock(&self, name: &str, acked: std::sync::mpsc::Sender<Ack>) {
        *self.pause.lock().unwrap() = Some((name.to_owned(), acked));
    }

    #[cfg(test)]
    fn unpause(&self) {
        *self.pause.lock().unwrap() = None;
    }

    #[cfg(test)]
    fn checkpoint(&self, name: &str, point: Point) {
        let armed = self.pause.lock().unwrap().clone();
        if let Some((armed, acked)) = armed {
            if armed != name {
                return;
            }
            let thread = std::thread::current().name().unwrap_or("").to_owned();
            match point {
                Point::P => {
                    let _sent = acked.send(Ack {
                        point,
                        thread,
                        release: None,
                    });
                }
                Point::Q => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let _sent = acked.send(Ack {
                        point,
                        thread,
                        release: Some(tx),
                    });
                    let _released = rx.recv();
                }
            }
        }
    }
}

/// Non-route inputs replaced together when a reload installs.
#[derive(Clone)]
pub(crate) struct Reloadable {
    pub(crate) config: Arc<Config>,
    /// Re-cap rebuilt tools from their uncapped originals on reload or switch.
    pub(crate) caps: r#loop::ResultCaps,
    pub(crate) backend: Option<Arc<dyn contract::search::SearchBackend>>,
    pub(crate) hosted_stands: bool,
    pub(crate) loader: Loader,
}

/// What preparing a switch reads: the whole startup registry, holding no
/// Lua provider, every model every provider names before placeholders are
/// filled, the configuration, each key read so far with the label each
/// provider is on, and the Lua providers loaded now: the session's and its
/// reviewer's.
pub(crate) struct Switching {
    pub(crate) now: Arc<Mutex<Reloadable>>,
    registry: Providers,
    naming: Vec<(String, String)>,
    /// Route thinking and credentials stay fixed to the startup configuration.
    startup_config: Config,
    remembered: Mutex<Remembered>,
    /// The label each provider is on in the running session, moved only
    /// when a switch applies: `host.model` reads it at call time, so a
    /// credential or `/model` switch moves the next call but never one in
    /// flight (`docs/model-routing.md`, "Which credential a session uses").
    /// It is not `remembered.selected`, which `prepare` publishes before
    /// the loop applies the switch.
    current: Mutex<BTreeMap<String, String>>,
    loaded: Mutex<BTreeMap<String, Arc<LuaProvider>>>,
    started: Arc<StartLock>,
    reads: Arc<Reads>,
    /// The session's diagnostic writer: a switch prepares observed
    /// factories over it. `None` until the session shares its writer, and
    /// a switch then carries no factories.
    diag: Option<Arc<log::diag::Diag>>,
}

/// What a switch publishes to the session's door when it applies.
pub(crate) struct Door {
    /// Replaces or removes one entry of the `tools` answer.
    pub(crate) declare: doors::Declare,
    /// A `web_search` an extension or MCP server registered at start
    /// replaced the hosted one, and it stands across switches.
    pub(crate) hosted_stands: bool,
    /// The search backend Fiber's own `web_search` runs: the load's choice,
    /// fixed for the session.
    pub(crate) backend: Option<Arc<dyn contract::search::SearchBackend>>,
}

/// Whether a `web_search` registered by anything other than `builtin` is
/// among `tools`: that one replaced the hosted search at start
/// (`docs/architecture.md`, "Tool seam").
pub(crate) fn hosted_stands(tools: &[(String, Arc<dyn contract::tool::Tool>)]) -> bool {
    tools
        .iter()
        .any(|(by, tool)| by != "builtin" && tool.definition().name == "web_search")
}

/// How `host.model` reaches its model reference: `reacher` builds one per
/// `ModelHost`, and the host calls it once per call, right after the
/// budget gate and before the first budget check.
pub(crate) type Reach = Arc<dyn Fn(&str) -> Result<Reached, Failure> + Send + Sync>;

/// One `host.model` call's route: the connected provider, the model
/// declaration's prices and whether a subscription serves it. The same
/// two fields `prepare` puts in `r#loop::Model`.
pub(crate) struct Reached {
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) cost: Option<Cost>,
    pub(crate) subscription: bool,
}

/// `connect` as a value, so a test records what the reach connected: the
/// signature of `connect` (`connect.rs`), with production passing
/// `crate::connect` itself.
pub(crate) type Connector = Arc<
    dyn for<'a> Fn(
            extensions::Model<'a>,
            Option<Secret>,
            Option<Arc<dyn contract::signing::Signer>>,
            Option<(&Arc<LuaProvider>, &str)>,
            &crate::connect::Here,
        ) -> Result<Arc<dyn Provider>, Failure>
        + Send
        + Sync,
>;

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
        reads: Arc<Reads>,
    ) -> Self {
        registry.forget_lua();
        let mut remembered = Remembered::default();
        let mut current = BTreeMap::new();
        for (name, (label, (key, _))) in credentials {
            remembered.keys.insert((name.clone(), label.clone()), key);
            remembered.selected.insert(name.clone(), label.clone());
            current.insert(name, label);
        }
        Self {
            registry,
            naming,
            now: Arc::new(Mutex::new(Reloadable {
                config: Arc::new(config.clone()),
                caps: crate::settings::result_caps(&config),
                backend: loader.extensions.search_backend(),
                hosted_stands: false,
                loader,
            })),
            startup_config: config,
            remembered: Mutex::new(remembered),
            current: Mutex::new(current),
            started: Arc::new(StartLock::seeded(&loaded)),
            loaded: Mutex::new(loaded.into_iter().collect()),
            reads,
            diag: None,
        }
    }

    /// Shares the session's diagnostic writer: a switch prepares observed
    /// factories over it, applied with the route. Without it a switch
    /// carries no factories.
    pub(crate) fn diagnostics(mut self, diag: Arc<log::diag::Diag>) -> Self {
        self.diag = Some(diag);
        self
    }
    /// What shutdown cancels: every read a switch has running.
    pub(crate) fn reads(&self) -> Arc<Reads> {
        Arc::clone(&self.reads)
    }

    /// The `Loop::switcher` closure: `prepare` over the shared `Switching`,
    /// publishing to `door` when a switch applies.
    pub(crate) fn closure(self: Arc<Self>, door: Door) -> r#loop::Prepare {
        {
            let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
            now.backend = door.backend.clone();
            now.hosted_stands = door.hosted_stands;
        }
        let prepare = self;
        Arc::new(move |args, label, chosen| {
            let now = prepare.reloadable();
            let current_door = Door {
                declare: door.declare.clone(),
                backend: now.backend,
                hosted_stands: now.hosted_stands,
            };
            crate::switch::prepare(&prepare, &current_door, args, label, chosen)
        })
    }

    pub(crate) fn reloaded(&self, now: Reloadable) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = now;
    }

    pub(crate) fn loader(&self) -> Loader {
        self.reloadable().loader
    }

    pub(crate) fn reloadable(&self) -> Reloadable {
        self.now
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn resolved(&self, reference: &str) -> Result<extensions::Model<'_>, Failure> {
        self.registry
            .resolve(reference)
            .map_err(|error| crate::failed(error.code(), error))
    }

    pub(crate) fn reviewer_for(
        self: &Arc<Self>,
        route: &r#loop::ReloadRoute,
        config: &Config,
        here: &crate::Here,
    ) -> (
        Result<r#loop::Reviewer, Failure>,
        Option<contract::diag::Observe>,
        Vec<PathBuf>,
        Applied,
    ) {
        let mut staged: Vec<(String, Got)> = Vec::new();
        let mut files = Vec::new();
        let result = self.resolved(&route.model).and_then(|resolved| {
            let mut lookup = |data: &ProviderData| {
                let own = data.name == resolved.provider.name;
                let want = self.want(
                    &data.name,
                    if own {
                        route.credential.as_deref()
                    } else {
                        None
                    },
                );
                let got = self.read(want)?;
                let answer = (got.label.clone(), got.access.clone());
                files.extend(got.file.clone());
                if !own {
                    staged.push((data.name.clone(), got));
                }
                Ok(answer)
            };
            crate::choose_reviewer(
                &self.registry,
                config,
                &resolved,
                here,
                &mut lookup,
                self.diag.as_ref(),
            )
        });
        let shared = self.clone();
        let publish: Applied = Box::new(move || {
            let mut remembered = shared
                .remembered
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for (name, got) in staged {
                remembered
                    .keys
                    .entry((name.clone(), got.label.clone()))
                    .or_insert_with(|| got.access.key.clone());
                remembered.selected.insert(name, got.label);
            }
        });
        match result {
            Ok((reviewer, observe)) => (Ok(reviewer), observe, files, publish),
            Err(error) => (Err(error), None, files, publish),
        }
    }

    /// Moves `provider` to `label`: the label the last applied switch
    /// landed on, read by the next `host.model` reach. Runs on apply only,
    /// never in `prepare`'s publish, so a rejected, failed or
    /// never-applied preparation moves nothing.
    fn repoint(&self, provider: &str, label: &str) {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(provider.to_owned(), label.to_owned());
    }

    /// How one `host.model` call reaches its model reference: resolving it
    /// against the registry and connecting it with the session's current
    /// credential label, once per call. The returned `Reached` serves
    /// every attempt and retry of that call: an in-flight call keeps the
    /// provider and key it started with, and a re-point moves only the
    /// next call. A `connect` error is `model_unconfigured` carrying its
    /// message; a registry error and a credential read error keep their
    /// own codes.
    pub(crate) fn reacher(self: &Arc<Self>, connect: Connector) -> Reach {
        let shared = Arc::clone(self);
        Arc::new(move |reference: &str| {
            let resolved = shared
                .registry
                .resolve(reference)
                .map_err(|error| crate::failed(error.code(), error))?;
            let name = resolved.provider.name.clone();
            let label = shared
                .current
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&name)
                .cloned();
            let want = shared.want(&name, label.as_deref());
            let reader = Arc::clone(&shared);
            let read: Result<Got, Failure> = shared
                .reads
                .run("credential read", move || reader.read(want))?;
            let got = read?;
            let loader = shared.loader();
            let here = crate::Here {
                workspace: loader.workspace.clone(),
                clock: Arc::clone(&loader.clock),
                cache_diagnostics: shared
                    .startup_config
                    .flag("cache.diagnostics", None)
                    .unwrap_or_default(),
            };
            let provider = connect(
                resolved,
                got.access.key.clone(),
                got.access.signer.clone(),
                got.access.lua.as_ref().map(|lua| (lua, got.label.as_str())),
                &here,
            )
            .map_err(|failure| crate::failed(ErrorCode::ModelUnconfigured, failure.message))?;
            Ok(Reached {
                provider,
                cost: resolved.model.cost.clone(),
                subscription: resolved.model.subscription,
            })
        })
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
                Some(data) => self.startup_config.credentials().credential_label(data),
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
        let loader = self.loader();
        let access = crate::scripted::access(data, || {
            let lua = match (want.lua, loader.owners.get(&want.name)) {
                (Some(lua), _) => Some(lua),
                (None, Some(extension)) => Some(
                    self.started
                        .start(&loader, &self.startup_config, &want.name, extension)
                        .map_err(|e| crate::failed(e.code(), e))?,
                ),
                (None, None) => None,
            };
            let read =
                crate::lua_providers::session_credential(lua.as_ref(), data, &want.label, || {
                    if let Some(key) = want.known.clone().flatten() {
                        return Ok(key);
                    }
                    // A label that names no credential is `credential_missing`,
                    // naming the labels there are, before any configured
                    // source is read or any command runs; the label the
                    // provider is on now answers without a read
                    // (`docs/model-routing.md`, "Which credential a session uses").
                    // A Lua `credential()` provider never reaches this
                    // callback, so it accepts any label; a scripted provider
                    // was rejected before the read (`scripted::label`).
                    let current = self.selected_of(
                        &self
                            .remembered
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner),
                        &want.name,
                        Some(data),
                    );
                    let labels = self.startup_config.credentials().labels(data);
                    if want.label != current && !labels.contains(&want.label) {
                        return Err(crate::credential::no_label(
                            &want.name,
                            &want.label,
                            &labels,
                        ));
                    }
                    let run = |command: &mut std::process::Command,
                               timeout: std::time::Duration| {
                        self.reads.credential(command, timeout, &*loader.clock)
                    };
                    let read = crate::credential::switch_credential(
                        &self.startup_config,
                        data,
                        &want.label,
                        &run,
                    )?;
                    file = read.file;
                    Ok(read.secret)
                })?;
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
    // One line for every entered preparation, however it ends:
    // declared first so it drops last, after the remembered lock's block
    // has ended. The `Applied` closure runs later when the switch
    // applies, and is not timed. Without a shared writer this stays shut.
    let _timing = switching
        .diag
        .as_ref()
        .and_then(|diag| log::diag::timed(diag, Op::RoutingSwitch, None, None));
    // An unparseable thinking level rejects before any lookup.
    let asked = match &args.thinking {
        Some(level) => Some(
            level
                .parse::<ThinkingLevel>()
                .map_err(|_| invalid(format!("unknown thinking level `{level}`")))?,
        ),
        None => None,
    };
    let now = switching.reloadable();
    let resolved = switching
        .registry
        .resolve_named(&switching.naming, &args.model)
        .map_err(invalid)?;
    crate::scripted::label(resolved.provider, label).map_err(rejection)?;
    let reference = resolved.reference();
    let context_window = crate::settings::context_window(resolved.model, &reference)
        .map_err(|failure| invalid(failure.message))?;
    let mut notices = Vec::new();
    let thinking = crate::settings::thinking(
        resolved.thinking,
        asked.or(chosen),
        &switching.startup_config,
        resolved.model,
        &reference,
        &mut notices,
    )
    .map_err(rejection)?;
    // The reviewer's model, before the read: Fiber never reviews with the
    // session's own model.
    let judge = crate::reviewer_reference(&now.config, &resolved).and_then(|typed| {
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
        .run("credential read", move || {
            let got = shared.read(session_want)?;
            Ok((got, judge_want.map(|want| shared.read(want))))
        })
        .map_err(rejection)?;
    let (got, judge_got) = read.map_err(rejection)?;
    let here = crate::Here {
        workspace: now.loader.workspace.clone(),
        clock: Arc::clone(&now.loader.clock),
        cache_diagnostics: now
            .config
            .flag("cache.diagnostics", None)
            .unwrap_or_default(),
    };
    // The session's route with its observed factory when the session
    // shared its writer; without one the switch carries no factories.
    let (provider, observe) = match &switching.diag {
        Some(diag) => crate::connect_observed(
            resolved,
            got.access.key.clone(),
            got.access.signer.clone(),
            got.access.lua.as_ref().map(|lua| (lua, got.label.as_str())),
            &here,
            diag,
        )
        .map_err(|failure| invalid(failure.message))?,
        None => (
            crate::connect(
                resolved,
                got.access.key.clone(),
                got.access.signer.clone(),
                got.access.lua.as_ref().map(|lua| (lua, got.label.as_str())),
                &here,
            )
            .map_err(|failure| invalid(failure.message))?,
            None,
        ),
    };
    let mut lookup = |provider: &ProviderData| -> Result<(String, Access), Failure> {
        match &judge_got {
            _ if provider.name == session => Ok((got.label.clone(), got.access.clone())),
            Some(Ok(judged)) => Ok((judged.label.clone(), judged.access.clone())),
            Some(Err(failure)) => Err(failure.clone()),
            None => Err(doors::failure(
                ErrorCode::InvalidArguments,
                format!("The credential for `{}` was not read.", provider.name),
            )),
        }
    };
    // The reviewer's failure is not a rejection: the loop gets it with no
    // factory, and every reviewed call escalates it (`docs/permissions.md`,
    // "How it runs").
    let (reviewer, reviewer_observe) = match crate::choose_reviewer(
        &switching.registry,
        &now.config,
        &resolved,
        &here,
        &mut lookup,
        switching.diag.as_ref(),
    ) {
        Ok((reviewer, observe)) => (Ok(reviewer), observe),
        Err(failure) => (Err(failure), None),
    };
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
    let (switched_provider, switched_label) = (session.clone(), got.label.clone());
    let applied: Applied = Box::new(move || {
        shared.keep_loaded(keep);
        shared.repoint(&switched_provider, &switched_label);
        if let Some(publish) = publish {
            publish();
        }
    });
    Ok(r#loop::Prepared {
        provider,
        model: r#loop::Model {
            reference: reference.clone(),
            cost: resolved.model.cost.clone(),
            subscription: resolved.model.subscription,
        },
        thinking,
        // The suffix first, then what was asked for now, then the
        // session's choice.
        chosen: resolved.thinking.or(asked).or(chosen),
        credential: crate::scripted::credential(resolved.provider, got.label),
        cache_lifetime: crate::settings::cache_lifetime(&now.config, &reference),
        context_window,
        addendum: switching.registry.addendum(&resolved).map(str::to_owned),
        handoff: crate::handoff::handoff_settings(&now.config, &reference),
        reviewer,
        observe,
        reviewer_observe,
        web_search,
        // At most one notice: a configured level the model lacks.
        notice: notices.into_iter().next(),
        applied: Some(applied),
        credential_files,
    })
}

/// What `Prepared.applied` runs when the switch applies.
pub(crate) type Applied = Box<dyn FnOnce() + Send>;

/// The search after the switch, and what applying it publishes to
/// the door's `tools` answer: the new model's hosted search when it has
/// one, else Fiber's own over the backend, else none, unless another
/// registrant's `web_search` stands (`docs/tools.md`, "web_search": the
/// model sees one tool, whichever way it runs).
fn hosted(
    switching: &Switching,
    door: &Door,
    kind: Option<&str>,
) -> Result<(r#loop::Hosted, Option<Applied>), Rejection> {
    let now = switching.reloadable();
    if door.hosted_stands {
        return Ok((r#loop::Hosted::Keep, None));
    }
    let declare = Arc::clone(&door.declare);
    let Some((tool, info)) =
        crate::builtin::web_search(kind, door.backend.as_ref()).map_err(rejection)?
    else {
        let applied = Box::new(move || declare("web_search", None));
        return Ok((
            r#loop::Hosted::Withdraw("web_search".to_owned()),
            Some(applied),
        ));
    };
    let tool = r#loop::capped(vec![("builtin".to_owned(), Arc::clone(&tool))], &now.caps)
        .pop()
        .map_or(tool, |(_, capped)| capped);
    let applied = Box::new(move || {
        let name = info.name.clone();
        declare(&name, Some(info));
    });
    Ok((r#loop::Hosted::Declare(tool), Some(applied)))
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

#[cfg(test)]
#[path = "switch_tests.rs"]
mod tests;
