//! Machine-side configuration, supplied by the composition root.
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use contract::SessionId;
use contract::clock::Clock;
use contract::commands::hub::PruneExpect;
use contract::shapes::Failure;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::connection::{Hub, Refused};
use crate::feed::invalid;

/// Callback that shows a device login's URL and code.
pub type ShowCode = Box<dyn Fn(&str, &str) + Send + Sync>;

/// Configuration on the machine running the hub, not the terminal.
pub trait Machine: Send + Sync {
    /// Starts a device-code login on the hub's machine.
    fn device_login(
        &self,
        name: &str,
        shown: ShowCode,
        clock: Arc<dyn Clock>,
    ) -> Arc<dyn DeviceLogin>;
    /// Creates a retained workspace search, resolving `~/` against `home`.
    fn file_search(&self, workspace: &Path, home: Option<&Path>) -> Box<dyn FileSearch>;
    /// Runs one configuration operation with `operation` removed from `args`.
    fn configure(
        &self,
        operation: &str,
        args: &Map<String, Value>,
        clock: Arc<dyn Clock>,
    ) -> Result<Option<Value>, Failure>;
    /// Runs the one-time import of `source` (`None` runs both) from the
    /// person's home into the hub machine's Fiber home, answering the
    /// imported and left-behind JSON the driver answers with.
    fn import(
        &self,
        home: &Path,
        source: Option<&str>,
        clock: Arc<dyn Clock>,
    ) -> Result<Option<Value>, Failure>;
    /// Reads a configuration view's file and its version.
    fn read_config(&self, workspace: &Path, path: &Path) -> Result<(String, String), Failure>;
    /// Writes text only while the configuration file still has `version`.
    fn write_config(
        &self,
        workspace: &Path,
        path: &Path,
        text: &str,
        version: &str,
    ) -> Result<(), Failure>;
    /// Prunes the machine's Fiber home across every project. With
    /// `dry_run`, selects and answers the preview without removing
    /// anything; otherwise removes the previewed `expect` items and answers
    /// what ran. `delete` removes one session through the hub's own delete,
    /// with the previewed cascade set as `expect`, so a set that changed
    /// mid-prompt fails that one session without failing the prune.
    #[allow(
        clippy::too_many_arguments,
        clippy::type_complexity,
        reason = "one seam call carries the whole prune: its flags, the previewed items and the session-delete callback"
    )]
    fn prune(
        &self,
        home: &Path,
        older_than: Option<String>,
        cascade: bool,
        force: bool,
        dry_run: bool,
        expect: Option<PruneExpect>,
        clock: Arc<dyn Clock>,
        delete: &mut (
                 dyn FnMut(&SessionId, bool, Option<&BTreeSet<SessionId>>) -> Result<(), Failure>
                     + Send
             ),
    ) -> Result<PruneOutcome, Failure>;
}

/// What a `prune` run answers: the preview lines and items for a dry
/// run, which removes nothing, or the space freed, how many items the
/// confirmed run tried to remove, and each item that failed or was
/// skipped. The caller knows which mode it asked for, so no flag
/// distinguishes them: a dry run leaves the confirmed fields empty, and
/// a confirmed run leaves the preview fields empty.
#[derive(Debug, Clone, PartialEq)]
pub struct PruneOutcome {
    /// The printable preview lines, in print order: set on a dry run.
    pub lines: Vec<String>,
    /// The previewed items, each named as the dry run names it: set on
    /// a dry run.
    pub items: PruneExpect,
    /// The bytes removed, or the bytes the preview would free.
    pub freed: u64,
    /// How many items the confirmed run tried to remove.
    pub total: usize,
    /// Each item that failed or was skipped, in removal order: set on
    /// a confirmed run.
    pub failures: Vec<PruneFailure>,
}

/// One prune item that failed or was skipped: what stayed, its code and
/// why, as the local prune prints them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PruneFailure {
    /// The item that stayed, named as the dry run names it.
    pub item: String,
    /// Why it stayed.
    pub code: contract::ErrorCode,
    /// What the client is told.
    pub message: String,
}

/// A device-code login owned by one hub connection.
pub trait DeviceLogin: Send + Sync {
    /// Waits for the login to end, returning its stored credentials path.
    fn run(&self) -> Result<String, Failure>;
    /// Returns once nothing more can be stored.
    fn cancel(&self);
}

/// A connection's search core; only its search worker calls it.
pub trait FileSearch: Send {
    /// Returns rows or an error row, or no result once cancelled.
    fn search(
        &mut self,
        text: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Result<Vec<String>, String>>;
}

pub(crate) fn on_configure(hub: &Hub, args: &Map<String, Value>) -> Result<Option<Value>, Refused> {
    let operation = args
        .get("operation")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let mut rest = args.clone();
    rest.remove("operation");
    hub.machine
        .as_ref()
        .ok_or_else(invalid)?
        .configure(operation, &rest, Arc::clone(&hub.clock))
        .map_err(refused)
}

/// Runs the one-time import through the machine: `source` names one
/// source, absent runs both, and anything else is refused naming the
/// two, mirroring the driver. The person's home comes from the hub's
/// environments, the `directories` precedent; without one there is no
/// source home to read.
pub(crate) fn on_import(hub: &Hub, args: &Map<String, Value>) -> Result<Option<Value>, Refused> {
    if args.keys().any(|key| key != "source") {
        return Err(invalid());
    }
    let source = match args.get("source") {
        None => None,
        Some(Value::String(source)) if source == "claude-code" || source == "codex" => {
            Some(source.as_str())
        }
        Some(_) => {
            return Err(Refused {
                code: contract::ErrorCode::InvalidArguments,
                message: "`source` must be `claude-code` or `codex`.".to_owned(),
            });
        }
    };
    let Some(home) = hub.environments.user_home() else {
        return Err(Refused {
            code: contract::ErrorCode::IoFailed,
            message: "HOME is not set, so the source home cannot be found.".to_owned(),
        });
    };
    hub.machine
        .as_ref()
        .ok_or_else(invalid)?
        .import(&home, source, Arc::clone(&hub.clock))
        .map_err(refused)
}

pub(crate) fn on_read_config(
    hub: &Hub,
    args: &Map<String, Value>,
) -> Result<Option<Value>, Refused> {
    shape(args, &["workspace", "path"])?;
    let (workspace, path) = paths(args)?;
    let (text, version) = hub
        .machine
        .as_ref()
        .ok_or_else(invalid)?
        .read_config(workspace, path)
        .map_err(refused)?;
    Ok(Some(json!({"text":text,"version":version})))
}

pub(crate) fn on_write_config(
    hub: &Hub,
    args: &Map<String, Value>,
) -> Result<Option<Value>, Refused> {
    shape(args, &["workspace", "path", "text", "version"])?;
    let (workspace, path) = paths(args)?;
    hub.machine
        .as_ref()
        .ok_or_else(invalid)?
        .write_config(
            workspace,
            path,
            string(args, "text")?,
            string(args, "version")?,
        )
        .map_err(refused)?;
    Ok(None)
}

fn paths(args: &Map<String, Value>) -> Result<(&Path, &Path), Refused> {
    let workspace = Path::new(string(args, "workspace")?);
    if !workspace.is_absolute() {
        return Err(invalid());
    }
    Ok((workspace, Path::new(string(args, "path")?)))
}

fn string<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, Refused> {
    args.get(key).and_then(Value::as_str).ok_or_else(invalid)
}

fn shape(args: &Map<String, Value>, keys: &[&str]) -> Result<(), Refused> {
    if args.len() != keys.len() {
        return Err(invalid());
    }
    for key in keys {
        string(args, key)?;
    }
    Ok(())
}

fn refused(failure: Failure) -> Refused {
    Refused {
        code: failure.code,
        message: failure.message,
    }
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
