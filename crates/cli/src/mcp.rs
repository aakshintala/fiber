//! `fiber mcp add` (`docs/invocation.md`, "Commands and flags";
//! `docs/mcp.md`, "Configuration"): declares a remote or stdio server in one
//! layer's file, stores each `--secret` value beside the configuration, and
//! records the approval when the layer is the repository's. A secret value
//! never reaches configuration output, `Debug`, an error or a log: it is a
//! [`Secret`](config::Secret) from the moment it is read.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::{
    Config, ConfigError, CredentialFile, Layer, ProjectKey, Secret, Sources, delete_secret,
    insert_entry, read_secret, remove_entry, store_secret,
};
use contract::clock::Clock;
use contract::events::OfferedKind;
use contract::mcp_remote::McpRemote;
use extensions::{Decision, Index, Store};
use serde_json::{Map, Value};

use crate::approve::say;
use crate::error::layer_word;
use crate::login::{KeyReader, NoEcho, Plain};
use crate::{CliError, project_of, with_dirs, write_line};

/// A server's declaration as the layer's file holds it. Its `Debug`
/// prints names only, never values: a url prints redacted (never its
/// query or userinfo), and a stdio server prints its env and secret key
/// names with its command and arguments redacted
/// (`docs/code-quality.md`, "Errors").
#[derive(Clone, PartialEq, Eq)]
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

impl std::fmt::Debug for McpDeclaration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Remote { .. } => f.debug_struct("Remote").field("url", &"redacted").finish(),
            Self::Stdio { env, secrets, .. } => {
                let names: Vec<&String> = env.iter().map(|(key, _)| key).collect();
                f.debug_struct("Stdio")
                    .field("command", &"redacted")
                    .field("args", &"redacted")
                    .field("env", &names)
                    .field("secrets", secrets)
                    .finish()
            }
        }
    }
}

/// Where `run_add` reads secrets: the key reader, its input, and whether a
/// person is there to ask.
pub(crate) struct SecretInput<'a> {
    keys: &'a mut dyn KeyReader,
    input: &'a mut dyn BufRead,
    terminal: bool,
}

/// Where `run_add` reports: standard output and error.
pub(crate) struct Streams<'a> {
    pub(crate) out: &'a mut dyn Write,
    pub(crate) err: &'a mut dyn Write,
}

/// The name is a config path segment, a tool-name part and part of a
/// credential file name: non-empty, with no `"`, `/`, NUL, whitespace or
/// control character.
fn check_name(name: &str) -> Result<(), CliError> {
    if !config::valid_server_name(name) {
        return Err(CliError::McpName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// An `env` variable name: `[A-Za-z_][A-Za-z0-9_]*`.
fn check_env_key(key: &str) -> Result<(), CliError> {
    if !config::valid_env_key(key) {
        return Err(CliError::McpEnvKey {
            key: key.to_owned(),
        });
    }
    Ok(())
}

/// Splits each `-e KEY=value`; a value without `=` is a usage error. An
/// empty value is allowed.
fn split_env(env: &[String]) -> Result<Vec<(String, String)>, CliError> {
    let mut pairs = Vec::new();
    for raw in env {
        let Some((key, value)) = raw.split_once('=') else {
            return Err(CliError::McpEnvPair {
                raw: raw.to_owned(),
            });
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
/// the file removed when there was none. Each cleanup failure is reported
/// on stderr, naming the affected file; the exit code stays the caller's.
fn rollback(home: &Path, stored: &[(String, Option<Secret>)], err: &mut dyn Write) {
    for (name, previous) in stored {
        let outcome: Result<(), ConfigError> = match previous {
            Some(secret) => store_secret(home, name, secret),
            None => delete_secret(home, name).map(|_| ()),
        };
        if let Err(error) = outcome {
            say(
                err,
                &format!(
                    "fiber: could not restore {}: {error}\n",
                    home.join("credentials").join(name).display()
                ),
            );
        }
    }
}

/// Whether a `store_secret` result leaves the new bytes in place: success,
/// or `Unsynced` after its rename. Such a credential joins the rollback
/// list, so rollback restores or removes it.
fn track_after_store(result: &Result<(), ConfigError>) -> bool {
    result.is_ok() || matches!(result, Err(ConfigError::Unsynced { .. }))
}

/// Undoes the secrets a failed declaration write stored, unless the write
/// failed after its rename: then the new file is in place and the secrets
/// are retained, so no visible declaration references a deleted credential.
fn settle_secrets(
    home: &Path,
    stored: &[(String, Option<Secret>)],
    error: &ConfigError,
    err: &mut dyn Write,
) {
    if matches!(error, ConfigError::Unsynced { .. }) {
        return;
    }
    rollback(home, stored, err);
}

/// Records the approval of the declaration just written to the repository,
/// hashed exactly as startup hashes it. One snapshot of the repository's
/// declaration, taken under the writers' lock, is both checked against the
/// one just written and hashed: a concurrent replacement is refused as a
/// config failure naming the server, approving nothing, and a replacement
/// past the snapshot can only leave a stale approval behind, never approve
/// itself. Any other failure names the repository's file and
/// `fiber approve`; the declaration stays.
fn record_approval(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    name: &str,
    expected: &Value,
    clock: &Arc<dyn Clock>,
) -> Result<(), CliError> {
    let repo_file = workspace.join(".fiber/config.json");
    let approve_hint = |why: &str| CliError::ApprovalNotRecorded {
        name: name.to_owned(),
        file: repo_file.clone(),
        why: why.to_owned(),
    };
    // The same `config.json.lock` writers hold, so the snapshot below sees
    // one whole file: the declaration is read once, and never re-read for
    // the hash.
    let current = {
        let mut lock_name = repo_file.as_os_str().to_owned();
        lock_name.push(".lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(PathBuf::from(lock_name))
            .map_err(|e| approve_hint(&e.to_string()))?;
        lock_file.lock().map_err(|e| approve_hint(&e.to_string()))?;
        let declared = config::declared(workspace)?;
        match declared.mcp_servers.get(name) {
            Some(current) if current == expected => current.clone(),
            _ => {
                return Err(CliError::ApprovalChanged {
                    name: name.to_owned(),
                });
            }
        }
    };
    let item = extensions::mcp_server_item(workspace, name, &current)
        .map_err(|e| approve_hint(&e.to_string()))?;
    let mut index = Index::load(home);
    let hash = extensions::hash(&mut index, &item)?;
    // The index only saves a re-hash; failing to write it costs nothing else.
    index.save().unwrap_or(());
    Store::new(home, project, Arc::clone(clock))
        .approve(&item, &hash)
        .map_err(|source| CliError::ApprovalNotStored {
            name: name.to_owned(),
            file: repo_file.clone(),
            source,
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
) -> Result<(), CliError> {
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
            return Err(CliError::McpUrlAndCommand);
        }
        (None, true) => {
            return Err(CliError::McpTransportMissing);
        }
        _ => {}
    }
    if url.is_some() && (!env.is_empty() || !secrets.is_empty()) {
        return Err(CliError::McpUrlWithEnv);
    }
    if url.is_none() && command.first().is_some_and(String::is_empty) {
        return Err(CliError::McpProgramMissing);
    }
    let pairs = split_env(env)?;
    for key in secrets {
        check_env_key(key)?;
    }
    // A repeated KEY across `-e` and `--secret` is refused before
    // anything is read or written, so a declaration failure cannot
    // restore the first submitted value over the original credential.
    {
        let mut seen = std::collections::HashSet::new();
        for key in pairs.iter().map(|(key, _)| key).chain(secrets.iter()) {
            if !seen.insert(key) {
                return Err(CliError::McpKeyRepeated {
                    key: key.to_owned(),
                });
            }
        }
    }
    // A url that carries a user name or password is refused before
    // anything is written, through the seam, which owns the check.
    if let Some(url) = url
        && remote.has_userinfo(url)
    {
        return Err(CliError::UrlHasUserinfo {
            server: name.to_owned(),
        });
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
            .map_err(CliError::StandardInput)?;
        if value.expose().is_empty() {
            return Err(CliError::McpSecretEmpty {
                key: key.to_owned(),
            });
        }
        values.push(value);
    }
    let (_, project) = project_of(home, workspace)?;
    // The whole transaction runs under a per-name lock taken before the
    // layer file's lock, so a concurrent add of the same name cannot
    // restore or delete the winner's credential.
    let guard = CredentialFile::new(home, "mcp-add", &lock_label(name))?;
    let Some(_held) = guard.try_lock()? else {
        return Err(CliError::McpAddRunning {
            name: name.to_owned(),
        });
    };
    // Secrets are stored before the declaration is written, remembering
    // what was there; any failure before the rename puts each one back.
    // A store that returns `Unsynced` already renamed the new bytes into
    // place, so that credential joins the rollback list too.
    let mut stored: Vec<(String, Option<Secret>)> = Vec::new();
    for (key, value) in secrets.iter().zip(&values) {
        let credential = format!("mcp.{name}.{key}");
        let previous = read_secret(home, &credential).map_err(|e| {
            rollback(home, &stored, err);
            CliError::from(e)
        })?;
        let stored_result = store_secret(home, &credential, value);
        if track_after_store(&stored_result) {
            stored.push((credential.clone(), previous));
        }
        if let Err(e) = stored_result {
            rollback(home, &stored, err);
            return Err(CliError::from(e));
        }
    }
    let value = declaration_value(url, command, &pairs, name, secrets);
    let expected = value.clone();
    match insert_entry(home, workspace, &project, layer, "mcp.servers", name, value) {
        Ok(true) => {}
        Ok(false) => {
            rollback(home, &stored, err);
            return Err(CliError::McpAlreadyDeclared {
                name: name.to_owned(),
                layer,
            });
        }
        Err(e) => {
            settle_secrets(home, &stored, &e, err);
            if matches!(e, ConfigError::Unsynced { .. }) {
                return Err(CliError::SecretsKept(e));
            }
            return Err(CliError::from(e));
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
        record_approval(home, workspace, &project, name, &expected, clock)?;
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
        McpDeclaration::Remote { url } => (Some(url.as_str()), Vec::new(), Vec::new(), Vec::new()),
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
) -> Result<Vec<Layer>, CliError> {
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: Vec::new(),
    })?;
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
) -> Result<(), CliError> {
    let (_, project) = project_of(home, workspace)?;
    if remove_entry(home, workspace, &project, layer, "mcp.servers", name)? {
        return Ok(());
    }
    let declared_in = declaring_layers(home, workspace, &project, name)?;
    Err(CliError::McpNotDeclared {
        name: name.to_owned(),
        layer,
        declared_in,
    })
}

/// `fiber mcp remove [--project | --repo] <name>` in the current directory.
pub fn mcp_remove(layer: Layer, name: &str) -> i32 {
    with_dirs(|home, workspace| {
        let mut out = io::stdout();
        let mut err = io::stderr();
        run_remove(
            home,
            workspace,
            layer,
            name,
            Streams {
                out: &mut out,
                err: &mut err,
            },
        )
    })
}

/// `fiber mcp add [--project | --repo] <name> <url>` or `... -- <command>
/// Whether a declared entry holds at least one value: anything that is
/// not an object, at any depth. An entry of only emptied objects is not a
/// declaration and is not listed.
fn has_value(entry: &Value) -> bool {
    match entry {
        Value::Object(map) => map.values().any(has_value),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            true
        }
    }
}

/// A layer's place in the listing: global, then project, then repository.
fn layer_order(layer: Layer) -> u8 {
    match layer {
        Layer::Global => 0,
        Layer::Project => 1,
        Layer::Repository => 2,
    }
}

/// Whether the repository declaration `name` is approved: the recorded
/// decision for the hash of the declaration as startup hashes it. A
/// `never` and no record both read as unapproved, as does a declaration
/// there is nothing to hash.
fn is_approved(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    name: &str,
    clock: &Arc<dyn Clock>,
) -> Result<bool, CliError> {
    let items = extensions::declared_items(workspace, clock.as_ref())?;
    let Some(item) = items
        .iter()
        .find(|item| item.kind == OfferedKind::McpServer && item.name == name)
    else {
        return Ok(false);
    };
    let mut index = Index::load(home);
    let hash = extensions::hash(&mut index, item)?;
    // The index only saves a re-hash; failing to write it costs nothing else.
    index.save().unwrap_or(());
    Ok(Store::new(home, project, Arc::clone(clock))
        .decision(OfferedKind::McpServer, &hash)
        .is_some_and(|decision| decision == Decision::Approve))
}

/// One configured MCP server: what `fiber mcp list` prints and what the
/// hub's `mcp_servers` answer builds its rows from (`docs/mcp.md`,
/// "Servers, their state and the per-server commands"): its name, the
/// layer that declares it, and for a repository's server whether it is
/// approved. Nothing else about a declaration travels: a declaration can
/// hold tokens.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeclaredServer {
    /// The server's name.
    pub name: String,
    /// The layer declaring it.
    pub layer: Layer,
    /// Whether a repository's server is approved; always true for the
    /// global and project layers, which need no approval.
    pub approved: bool,
}

/// Every declared MCP server, sorted by name then layer (global, project,
/// repository): the rows `fiber mcp list` prints, and the hub's
/// `mcp_servers` snapshot base. An unreadable configuration fails as
/// `run_list` always failed it; a server is listed exactly when `run_list`
/// listed it.
pub fn declared_servers(
    home: &Path,
    workspace: &Path,
    clock: &Arc<dyn Clock>,
    overrides: &[String],
) -> Result<Vec<DeclaredServer>, CliError> {
    let (_, project) = project_of(home, workspace)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: overrides.to_vec(),
    })?;
    let mut rows: Vec<(String, Layer)> = Vec::new();
    for layer in [Layer::Global, Layer::Project, Layer::Repository] {
        if let Some(Value::Object(servers)) = config.in_layer("mcp.servers", layer) {
            for (name, entry) in &servers {
                if has_value(entry) {
                    rows.push((name.clone(), layer));
                }
            }
        }
    }
    rows.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(layer_order(left.1).cmp(&layer_order(right.1)))
    });
    // Approval resolves in listing order, as `run_list` always resolved
    // it, so a failing row reports the same error.
    let mut declared = Vec::with_capacity(rows.len());
    for (name, layer) in rows {
        let approved = if layer == Layer::Repository {
            is_approved(home, workspace, &project, &name, clock)?
        } else {
            true
        };
        declared.push(DeclaredServer {
            name,
            layer,
            approved,
        });
    }
    Ok(declared)
}

/// `fiber mcp list`: one row per declaration, sorted by name then layer:
/// the name, the layer, and for a repository's server whether it is
/// approved. Nothing else about a declaration prints: a declaration can
/// hold tokens.
pub(crate) fn run_list(
    home: &Path,
    workspace: &Path,
    streams: Streams<'_>,
    clock: &Arc<dyn Clock>,
) -> Result<(), CliError> {
    let Streams { out, err: _ } = streams;
    let rows = declared_servers(home, workspace, clock, &[])?;
    let mut table: Vec<Vec<String>> = Vec::new();
    for row in &rows {
        let approval = if row.layer == Layer::Repository {
            if row.approved {
                "approved"
            } else {
                "not approved"
            }
        } else {
            ""
        };
        table.push(vec![
            row.name.clone(),
            layer_word(row.layer).to_owned(),
            approval.to_owned(),
        ]);
    }
    for line in crate::table::pad(&table) {
        write_line(out, &line).unwrap_or(());
    }
    Ok(())
}

/// `fiber mcp list` in the current directory.
pub fn mcp_list(clock: Arc<dyn Clock>) -> i32 {
    with_dirs(|home, workspace| {
        let mut out = io::stdout();
        let mut err = io::stderr();
        run_list(
            home,
            workspace,
            Streams {
                out: &mut out,
                err: &mut err,
            },
            &clock,
        )
    })
}

/// Whether the secret prompt runs with echo off: both stdin and stderr must
/// be a terminal.
fn prompt_on(stdin_tty: bool, stderr_tty: bool) -> bool {
    stdin_tty && stderr_tty
}

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
    with_dirs(|home, workspace| {
        let stdin = io::stdin();
        let on_terminal = stdin.is_terminal();
        let mut out = io::stdout();
        let mut err = io::stderr();
        let terminal = prompt_on(on_terminal, err.is_terminal());
        let mut keys: Box<dyn KeyReader> = if on_terminal {
            Box::new(NoEcho)
        } else {
            Box::new(Plain)
        };
        run_add(
            home,
            workspace,
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
    })
}

#[cfg(test)]
#[path = "mcp_add_tests.rs"]
mod add_tests;

#[cfg(test)]
#[path = "mcp_remove_tests.rs"]
mod remove_tests;

#[cfg(test)]
#[path = "mcp_list_tests.rs"]
mod list_tests;
