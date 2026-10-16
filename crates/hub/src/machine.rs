//! Machine-side configuration, supplied by the composition root.
use std::path::Path;
use std::sync::Arc;

use contract::clock::Clock;
use contract::shapes::Failure;
use serde_json::{Map, Value, json};

use crate::connection::{Hub, Refused};
use crate::feed::invalid;

type ShowCode = Box<dyn Fn(&str, &str) + Send + Sync>;

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
