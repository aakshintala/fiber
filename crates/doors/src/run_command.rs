//! The `command` driver command (`docs/extensions.md`, "Commands and screens";
//! `docs/invocation.md`, "What each command does"): runs an extension's command
//! by name, with the text after it as arguments, as a person typing
//! `/name args` does. It writes no durable event, so the reader thread
//! answers it: the call is queued held, `command_accepted` is queued, and
//! only then is the call released, so acceptance always precedes `run`
//! starting.

use contract::commands::RunCommand;
use contract::{CommandId, ErrorCode};

use crate::client::{accept, reject};

/// Runs the `command` driver command on the reader thread.
pub(crate) fn run(conn: &mut crate::client::Conn, id: CommandId, args: &RunCommand) {
    let text = args.text.clone().unwrap_or_default();
    if args.name.is_empty() {
        reject(
            conn,
            Some(id),
            ErrorCode::InvalidArguments,
            "The `command` command takes a `name`.",
        );
        return;
    }
    let Some(door) = conn.gate.door() else {
        reject(
            conn,
            Some(id.clone()),
            ErrorCode::UnknownCommand,
            &format!("`{}` names no extension command.", args.name),
        );
        return;
    };
    let release = match door.command(&args.name, &text) {
        Ok(release) => release,
        Err(rejection) => {
            reject(conn, Some(id), rejection.code, &rejection.message);
            return;
        }
    };
    // The call is on the extension's queue and held: `command_accepted`
    // always precedes `run` starting. Session order is push order. Released
    // even when the acceptance never reached the client, so the stream
    // does not hold a held job forever.
    accept(conn, id, None);
    release();
}
