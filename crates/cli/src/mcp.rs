//! `fiber mcp add` (`docs/invocation.md`, "Commands and flags";
//! `docs/mcp.md`, "Configuration"): declares a remote or stdio server in one
//! layer's file, stores each `--secret` value beside the configuration, and
//! records the approval when the layer is the repository's. A secret value
//! never reaches configuration output, `Debug`, an error or a log: it is a
//! [`Secret`](config::Secret) from the moment it is read.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;

use config::{
    Config, ConfigError, CredentialFile, Layer, ProjectKey, Secret, Sources, delete_secret,
    insert_entry, read_secret, remove_entry, store_secret,
};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::OfferedKind;
use contract::mcp_remote::McpRemote;
use contract::shapes::Failure;
use doors::failure;
use extensions::{Index, Store};
use serde_json::{Map, Value};

use crate::approve::say;
use crate::login::{KeyReader, NoEcho, Plain};
use crate::{fail, failed, project_of, usage};

/// A server's declaration as the layer's file holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpDeclaration {
    /// A remote server: `{ "url": <url> }`.
    Remote {
        /// The server's URL.
        url: String,
    },
    /// A stdio server: its program, arguments, clear variables and secret
    /// variable names.
    Stdio {
        /// The program.
        command: String,
        /// Its arguments.
        args: Vec<String>,
        /// Clear variables, in flag order.
        env: Vec<(String, String)>,
        /// Secret variable names, in flag order.
        secrets: Vec<String>,
    },
}

/// Where `run_add` reads secrets: the key reader, its input, and whether a
/// person is there to ask.
struct SecretInput<'a> {
    keys: &'a mut dyn KeyReader,
    input: &'a mut dyn BufRead,
    terminal: bool,
}

/// Where `run_add` reports: standard output and error.
struct Streams<'a> {
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
}

fn config_failure(e: ConfigError) -> Failure {
    failed(e.code(), e)
}

/// The layer in words, as usage failures name it.
fn layer_word(layer: Layer) -> &'static str {
    match layer {
        Layer::Global => "global",
        Layer::Project => "project",
        Layer::Repository => "repository",
    }
}

/// The name is a config path segment, a tool-name part and part of a
/// credential file name: non-empty, with no `"`, `/`, NUL, whitespace or
/// control character.
fn check_name(name: &str) -> Result<(), Failure> {
    let bad = name.is_empty()
        || name
            .bytes()
            .any(|byte| byte == b'"' || byte == b'/' || byte.is_ascii_whitespace() || byte.is_ascii_control());
    if bad {
        return Err(usage(format!(
            "`{name}` is not a server name: it is non-empty and holds no `\"`, `/`, whitespace or control character. Run `fiber --help` for usage."
        )));
    }
    Ok(())
}

/// An `env` variable name: `[A-Za-z_][A-Za-z0-9_]*`.
fn check_env_key(key: &str) -> Result<(), Failure> {
    if !config::valid_env_key(key) {
        return Err(usage(format!(
            "`{key}` is not a variable name: it matches `[A-Za-z_][A-Za-z0-9_]*`. Run `fiber --help` for usage."
        )));
    }
    Ok(())
}

/// Splits each `-e KEY=value`; a value without `=` is a usage error. An
/// empty value is allowed.
fn split_env(env: &[String]) -> Result<Vec<(String, String)>, Failure> {
    let mut pairs = Vec::new();
    for raw in env {
        let Some((key, value)) = raw.split_once('=') else {
            return Err(usage(format!(
                "`{raw}` is not `KEY=value`. Run `fiber --help` for usage."
            )));
        };
        check_env_key(key)?;
        pairs.push((key.to_owned(), value.to_owned()));
    }
    Ok(pairs)
}

/// The declaration's value as the layer's file holds it: `{ "url": ... }`,
/// or `command` with `args` and `env` when they are non-empty. A secret
/// variable holds `{ "secret": "mcp.<name>.<KEY>" }`, never its value.
fn declaration_value(
    url: Option<&str>,
    command: &[String],
    pairs: &[(String, String)],
    name: &str,
    secrets: &[String],
) -> Value {
    if let Some(url) = url {
        return Value::Object(Map::from_iter([(
            "url".to_owned(),
            Value::from(url.to_owned()),
        )]));
    }
    let mut entry = Map::new();
    let Some((program, args)) = command.split_first() else {
        return Value::Object(entry);
    };
    entry.insert("command".to_owned(), Value::from(program.clone()));
    if !args.is_empty() {
        entry.insert(
            "args".to_owned(),
            Value::Array(args.iter().map(|arg| Value::from(arg.clone())).collect()),
        );
    }
    if !pairs.is_empty() || !secrets.is_empty() {
        let mut env = Map::new();
        for (key, value) in pairs {
            env.insert(key.clone(), Value::from(value.clone()));
        }
        for key in secrets {
            env.insert(
                key.clone(),
                Value::Object(Map::from_iter([(
                    "secret".to_owned(),
                    Value::from(format!("mcp.{name}.{key}")),
                )])),
            );
        }
        entry.insert("env".to_owned(), Value::Object(env));
    }
    Value::Object(entry)
}

/// Puts each secret this call stored back: the previous bytes restored, or
/// the file removed when there was none.
fn rollback(home: &Path, stored: &[(String, Option<Secret>)]) {
    for (name, previous) in stored {
        match previous {
            Some(secret) => {
                store_secret(home, name, secret).unwrap_or(());
            }
            None => {
                delete_secret(home, name).unwrap_or(false);
            }
        }
    }
}

/// Undoes the secrets a failed declaration write stored, unless the write
/// failed after its rename: then the new file is in place and the secrets
/// are retained, so no visible declaration references a deleted credential.
fn settle_secrets(home: &Path, stored: &[(String, Option<Secret>)], error: &ConfigError) {
    if matches!(error, ConfigError::Unsynced { .. }) {
        return;
    }
    rollback(home, stored);
}

/// Records the approval of the declaration just written to the repository,
/// hashed exactly as startup hashes it. A failure names the repository's
/// file and `fiber approve`; the declaration stays.
fn record_approval(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    name: &str,
    clock: &Arc<dyn Clock>,
) -> Result<(), Failure> {
    let repo_file = workspace.join(".fiber/config.json");
    let missing = || {
        failed(
            ErrorCode::IoFailed,
            format!(
                "could not record the approval of `{name}` in {}: the declaration is missing. Run `fiber approve` to finish it.",
                repo_file.display()
            )
        )
    };
    let items =
        extensions::declared_items(workspace, clock.as_ref()).map_err(|e| failed(e.code(), e))?;
    let item = items
        .iter()
        .find(|item| item.kind == OfferedKind::McpServer && item.name == name)
        .ok_or_else(missing)?;
    let mut index = Index::load(home);
    let hash = extensions::hash(&mut index, item).map_err(|e| failed(e.code(), e))?;
    // The index only saves a re-hash; failing to write it costs nothing else.
    index.save().unwrap_or(());
    Store::new(home, project, Arc::clone(clock))
        .approve(item, &hash)
        .map_err(|e| {
            failed(
                e.code(),
                format!(
                    "could not record the approval of `{name}` in {}: {e}. Run `fiber approve` to finish it.",
                    repo_file.display()
                ),
            )
        })
}

/// The per-name lock's label: the name with a suffix that is never `.lock`
/// or `.tmp`, so a server named `x.lock` still locks under its own file.
fn lock_label(name: &str) -> String {
    format!("{name}.add")
}

#[allow(
    clippy::too_many_arguments,
    reason = "one straight-line transaction: shape, secrets, declaration, approval"
)]
pub(crate) fn run_add(
    home: &Path,
    workspace: &Path,
    layer: Layer,
    name: &str,
    url: Option<&str>,
    env: &[String],
    secrets: &[String],
    command: &[String],
    remote: &dyn McpRemote,
    reads: SecretInput<'_>,
    streams: Streams<'_>,
    clock: &Arc<dyn Clock>,
) -> Result<(), Failure> {
    let SecretInput {
        keys,
        input,
        terminal,
    } = reads;
    let Streams { out: _, err } = streams;
    check_name(name)?;
    // Exactly one transport: both a url and a command, or neither, is a
    // usage error, as are `-e` and `--secret` with a url.
    match (url, command.is_empty()) {
        (Some(_), false) => {
            return Err(usage(
                "`fiber mcp add` takes either a url or `-- <command>`, not both. Run `fiber --help` for usage.",
            ));
        }
        (None, true) => {
            return Err(usage(
                "`fiber mcp add` takes a url or `-- <command>`. Run `fiber --help` for usage.",
            ));
        }
        _ => {}
    }
    if url.is_some() && (!env.is_empty() || !secrets.is_empty()) {
        return Err(usage(
            "`-e` and `--secret` apply only to `-- <command>`, not to a url. Run `fiber --help` for usage.",
        ));
    }
    if url.is_none() && command.first().is_some_and(String::is_empty) {
        return Err(usage(
            "`fiber mcp add` takes a program after `--`. Run `fiber --help` for usage.",
        ));
    }
    let pairs = split_env(env)?;
    for key in secrets {
        check_env_key(key)?;
    }
    for (key, _) in &pairs {
        if secrets.contains(key) {
            return Err(usage(format!(
                "`{key}` is both `-e` and `--secret`; a variable is one or the other. Run `fiber --help` for usage.",
            )));
        }
    }
    // A url that carries a user name or password is refused before
    // anything is written, through the seam, which owns the check.
    if let Some(url) = url
        && remote.has_userinfo(url)
    {
        return Err(failed(
            ErrorCode::ConfigInvalid,
            format!("`{name}`'s url carries a user name or password; remove it."),
        ));
    }
    // Every secret value is read before anything is stored, so an empty
    // one writes nothing.
    let mut values = Vec::new();
    for key in secrets {
        let prompt = if terminal {
            format!("Secret {key} for {name}: ")
        } else {
            String::new()
        };
        let value = keys
            .read_key_exact(&prompt, input, err)
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard input: {e}")))?;
        if value.expose().is_empty() {
            return Err(usage(format!(
                "No value was given for `--secret {key}`; nothing was stored. Run `fiber --help` for usage."
            )));
        }
        values.push(value);
    }
    let (_, project) = project_of(home, workspace)?;
    // The whole transaction runs under a per-name lock taken before the
    // layer file's lock, so a concurrent add of the same name cannot
    // restore or delete the winner's credential.
    let guard =
        CredentialFile::new(home, "mcp-add", &lock_label(name)).map_err(config_failure)?;
    let Some(_held) = guard.try_lock().map_err(config_failure)? else {
        return Err(failed(
            ErrorCode::IoFailed,
            format!("another `fiber mcp add` of `{name}` is running"),
        ));
    };
    // Secrets are stored before the declaration is written, remembering
    // what was there; any failure before the rename puts each one back.
    let mut stored: Vec<(String, Option<Secret>)> = Vec::new();
    for (key, value) in secrets.iter().zip(&values) {
        let credential = format!("mcp.{name}.{key}");
        let previous = read_secret(home, &credential).map_err(|e| {
            rollback(home, &stored);
            config_failure(e)
        })?;
        store_secret(home, &credential, value).map_err(|e| {
            rollback(home, &stored);
            config_failure(e)
        })?;
        stored.push((credential, previous));
    }
    let value = declaration_value(url, command, &pairs, name, secrets);
    match insert_entry(home, workspace, &project, layer, "mcp.servers", name, value) {
        Ok(true) => {}
        Ok(false) => {
            rollback(home, &stored);
            return Err(usage(format!(
                "`{name}` is already declared in the {} layer. Run `fiber mcp remove {name}` to remove it first. Run `fiber --help` for usage.",
                layer_word(layer)
            )));
        }
        Err(e) => {
            settle_secrets(home, &stored, &e);
            if matches!(e, ConfigError::Unsynced { .. }) {
                return Err(failed(
                    e.code(),
                    format!("{e} The secrets the declaration refers to were kept."),
                ));
            }
            return Err(config_failure(e));
        }
    }
    for (credential, previous) in &stored {
        // A secret that is already stored is replaced, and the command
        // says so, because logging in again is how a key is rotated.
        if previous.is_some() {
            say(err, &format!("fiber: replaced credentials/{credential}\n"));
        }
    }
    if layer == Layer::Repository {
        record_approval(home, workspace, &project, name, clock)?;
    }
    Ok(())
}

/// The declaration back as the raw parts the inner call validates: one
/// `KEY=value` line per clear variable. A value round-trips through the
/// first `=` whatever it holds.
pub(crate) fn split_declaration(
    declaration: &McpDeclaration,
) -> (Option<&str>, Vec<String>, Vec<String>, Vec<String>) {
    match declaration {
        McpDeclaration::Remote { url } => {
            (Some(url.as_str()), Vec::new(), Vec::new(), Vec::new())
        }
        McpDeclaration::Stdio {
            command,
            args,
            env,
            secrets,
        } => {
            let mut full = vec![command.clone()];
            full.extend(args.iter().cloned());
            let raw = env
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            (None, raw, secrets.clone(), full)
        }
    }
}

/// The layers whose `mcp.servers` declares `name`, lowest first: what a
/// refused `remove` names, so a mistyped name is caught rather than
/// stored where nothing reads it.
fn declaring_layers(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    name: &str,
) -> Result<Vec<Layer>, Failure> {
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: Vec::new(),
    })
    .map_err(config_failure)?;
    let mut layers = Vec::new();
    for layer in [Layer::Global, Layer::Project, Layer::Repository] {
        let declares = config
            .in_layer("mcp.servers", layer)
            .and_then(|servers| servers.as_object()?.contains_key(name).then_some(()))
            .is_some();
        if declares {
            layers.push(layer);
        }
    }
    Ok(layers)
}

/// `fiber mcp remove [--project | --repo] <name>`: removes the declaration
/// alone; stored secrets and approvals stay.
pub(crate) fn run_remove(
    home: &Path,
    workspace: &Path,
    layer: Layer,
    name: &str,
    _streams: Streams<'_>,
) -> Result<(), Failure> {
    let (_, project) = project_of(home, workspace)?;
    if remove_entry(home, workspace, &project, layer, "mcp.servers", name).map_err(config_failure)? {
        return Ok(());
    }
    let layers = declaring_layers(home, workspace, &project, name)?;
    let hint = if layers.is_empty() {
        "; no layer declares it".to_owned()
    } else {
        let names = layers
            .iter()
            .map(|layer| layer_word(*layer))
            .collect::<Vec<_>>()
            .join(", ");
        format!("; it is declared in the {names} layer(s)")
    };
    Err(usage(format!(
        "`{name}` is not declared in the {} layer{hint}. Run `fiber --help` for usage.",
        layer_word(layer)
    )))
}

/// `fiber mcp remove [--project | --repo] <name>` in the current directory.
pub fn mcp_remove(layer: Layer, name: &str) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir().map_err(|e| {
                failed(ErrorCode::IoFailed, format!("the current directory: {e}"))
            })?;
            let mut out = io::stdout();
            let mut err = io::stderr();
            run_remove(
                &home,
                &workspace,
                layer,
                name,
                Streams {
                    out: &mut out,
                    err: &mut err,
                },
            )
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// `fiber mcp add [--project | --repo] <name> <url>` or `... -- <command>
/// [args]...` in the current directory.
pub fn mcp_add(
    layer: Layer,
    name: &str,
    declaration: McpDeclaration,
    remote: &dyn McpRemote,
    clock: Arc<dyn Clock>,
) -> i32 {
    // The declaration is already shaped; the inner call reads it back as
    // the raw parts.
    let (url, env, secret_keys, command) = split_declaration(&declaration);
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            let stdin = io::stdin();
            let on_terminal = stdin.is_terminal();
            let mut out = io::stdout();
            let mut err = io::stderr();
            let terminal = on_terminal && err.is_terminal();
            let mut keys: Box<dyn KeyReader> = if on_terminal {
                Box::new(NoEcho)
            } else {
                Box::new(Plain)
            };
            run_add(
                &home,
                &workspace,
                layer,
                name,
                url,
                &env,
                &secret_keys,
                &command,
                remote,
                SecretInput {
                    keys: keys.as_mut(),
                    input: &mut stdin.lock(),
                    terminal,
                },
                Streams {
                    out: &mut out,
                    err: &mut err,
                },
                &clock,
            )
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

#[cfg(test)]
#[path = "mcp_add_tests.rs"]
mod add_tests;

#[cfg(test)]
#[path = "mcp_remove_tests.rs"]
mod remove_tests;
