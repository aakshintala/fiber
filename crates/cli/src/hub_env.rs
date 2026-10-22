//! The hub's captured environment for one placeholder lookup
//! (`docs/invocation.md`, "A session's environment"): the values of the
//! placeholder variable names the installed providers declare, read from
//! the running hub, so the list and the session agree. No hub answers and
//! the caller falls back to the process environment, as before.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::commands::hub::{HubCommand, HubCommandLine, PlaceholderEnvArgs};
use contract::{CommandId, HubLine};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fd::{AsFd, BorrowedFd};

/// The `id` of the one command the hub query sends.
const ENV_ID: &str = "c_env";

/// How long the hub query waits for the answer: the connect deadline plus
/// the login-shell capture's 10-second limit, since a hub in its first
/// seconds answers only after its first capture (`docs/invocation.md`,
/// "A session's environment"). `cli` declares it because `cli` may not
/// depend on `hub`.
pub(crate) const HUB_ENV_WITHIN: Duration =
    Duration::from_secs(doors::hub::CONNECT_DEADLINE.as_secs() + 10);

/// The hub's captured values for the requested placeholder variable
/// names, or `None` when no hub answers: absent, refusing or silent. The
/// caller falls back to the process environment on `None`, so today's
/// offline behaviour is unchanged. `pub` for the session startup in
/// `main`, which fills placeholders from the same hub values so a
/// session choosing a model agrees with `fiber models`.
pub fn hub_env(
    home: &Path,
    names: &[String],
    clock: &dyn Clock,
) -> Option<BTreeMap<String, String>> {
    hub_env_with(home, names, clock, &mut || {}, &mut || {})
}

/// [`hub_env`] with hooks for tests: `before_read` runs just before each
/// answer read, and `before_wait` after each wait's deadline check, just
/// before [`Clock::wait_until`].
pub(crate) fn hub_env_with(
    home: &Path,
    names: &[String],
    clock: &dyn Clock,
    before_read: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Option<BTreeMap<String, String>> {
    if names.is_empty() {
        return None;
    }
    let deadline = clock.now() + HUB_ENV_WITHIN;
    let connected = doors::hub::probe_until(home, clock, deadline);
    let stream = match connected {
        doors::hub::Probe::Absent => return None,
        doors::hub::Probe::Failed(_) => return None,
        doors::hub::Probe::Connected(Err(_)) => return None,
        doors::hub::Probe::Connected(Ok((stream, _))) => stream,
    };
    ask(&stream, deadline, names, clock, before_read, before_wait)
}

/// Every placeholder variable name the installed providers declare,
/// sorted, each once: the only names the hub is ever asked for, never the
/// whole environment.
pub fn declared_names(providers: &extensions::Providers) -> Vec<String> {
    let mut names: Vec<String> = providers
        .names()
        .flat_map(|name| providers.data(name).placeholders.into_values())
        .filter_map(|placeholder| placeholder.env)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// One placeholder lookup for `fill_placeholders` and `add_session`: the
/// hub's captured environment for the providers' declared names, falling
/// back to the process environment when no hub answers
/// (`docs/invocation.md`, "A session's environment"). The lookup owns
/// the hub answer, so callers pass it by reference wherever a
/// `&dyn Fn(&str) -> Option<String>` fits.
pub fn placeholder_lookup(
    home: &Path,
    providers: &extensions::Providers,
    clock: &dyn Clock,
) -> impl Fn(&str) -> Option<String> + use<> {
    let answered = hub_env(home, &declared_names(providers), clock);
    move |name| lookup(&answered, &|name| std::env::var(name).ok(), name)
}

/// One placeholder value for `fill_placeholders`: the hub's answer when
/// one arrived, else the process environment. A name the hub answered
/// without stays a gap even when the process environment holds it, so a
/// stale client export cannot revive a value the hub never saw.
pub fn lookup(
    answered: &Option<BTreeMap<String, String>>,
    process: &dyn Fn(&str) -> Option<String>,
    name: &str,
) -> Option<String> {
    match answered {
        Some(values) => values.get(name).cloned(),
        None => process(name),
    }
}

/// Sends `placeholder_env` and reads lines until its answer, failing once
/// `deadline` passes on `clock`: the write and every read are
/// non-blocking under the same deadline, so unrelated lines, trickled
/// bytes and silence all end there. `before_read` runs just before each
/// answer read; `before_wait` runs after each wait's deadline check, just
/// before [`Clock::wait_until`]. The stream comes back blocking with no
/// read timeout, as it arrived. A refusal or a missing answer is `None`,
/// so the caller falls back to the process environment.
#[allow(
    clippy::too_many_arguments,
    reason = "one call of the one-shot query: home, deadline, names, clock and hooks"
)]
fn ask(
    stream: &UnixStream,
    deadline: Instant,
    names: &[String],
    clock: &dyn Clock,
    before_read: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Option<BTreeMap<String, String>> {
    let Ok(mut request) = serde_json::to_vec(&HubCommandLine {
        id: CommandId(ENV_ID.to_owned()),
        command: HubCommand::PlaceholderEnv(PlaceholderEnvArgs {
            names: names.to_vec(),
        }),
    }) else {
        return None;
    };
    request.push(b'\n');
    let result = exchange(
        stream,
        &request,
        deadline,
        clock,
        false,
        before_read,
        before_wait,
        || (),
        || (),
        |_| (),
        answer,
    );
    let restored = restore_blocking(stream);
    match result {
        // The exchange's own answer wins over a restore failure.
        Err(()) => None,
        Ok(values) => restored.map(|()| Some(values)).unwrap_or(None),
    }
}

// Restores the blocking mode `exchange` lifted: macOS refuses the option
// once the peer has closed, while its buffered bytes stay readable, so
// that refusal still keeps a close-after-answer a reply. Exempt from
// mutation testing: the `InvalidInput` refusal happens only on macOS,
// and mutants run on Linux, where no test through this seam can produce
// it (docs/testing/mutants.md).
#[cfg_attr(false, mutants::skip)]
pub(crate) fn restore_blocking(stream: &UnixStream) -> std::io::Result<()> {
    match stream.set_nonblocking(false) {
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
        other => other,
    }
}

/// Sends `request` and reads lines until the answer, failing once
/// `deadline` passes on `clock`: the write and every read are
/// non-blocking under the same deadline, so unrelated lines, trickled
/// bytes and silence all end there. `before_read` runs just before each
/// read; `before_wait` runs after each wait's deadline check, just before
/// [`Clock::wait_until`]. When `wait_for_close` the answer does not end
/// the exchange: lines keep draining until the peer closes, and that
/// close is the result. Malformed lines never end it either: they are
/// skipped. `closed`, `late` and `lost` shape the three failures;
/// `on_line` maps each parsed line to its answer, if any. It leaves the
/// stream's mode alone; the caller restores blocking after.
#[allow(
    clippy::too_many_arguments,
    reason = "one call of the shared query: stream, request, deadline, clock, end, hooks, error shapes and line handler"
)]
pub(crate) fn exchange<T, E>(
    mut stream: &UnixStream,
    request: &[u8],
    deadline: Instant,
    clock: &dyn Clock,
    wait_for_close: bool,
    before_read: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
    closed: impl Fn() -> E,
    late: impl Fn() -> E,
    lost: impl Fn(&std::io::Error) -> E,
    mut on_line: impl FnMut(&HubLine) -> Result<Option<T>, E>,
) -> Result<T, E> {
    stream.set_nonblocking(true).map_err(|error| lost(&error))?;
    let mut sent = 0;
    while sent < request.len() {
        match stream.write(request.get(sent..).unwrap_or_default()) {
            Ok(0) => return Err(closed()),
            Ok(wrote) => sent += wrote,
            Err(error) => {
                // `!=`, not a `WouldBlock` match guard: the guard's
                // always-wait mutant no test can reach on a connected
                // socket, while this form's flipped comparison fails the
                // send-buffer and silent-deadline tests.
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(lost(&error));
                }
                if clock.now() >= deadline {
                    return Err(late());
                }
                before_wait();
                wait_clock(clock, deadline, stream.as_fd(), PollFlags::OUT);
            }
        }
    }
    let mut done = None;
    let mut pending = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if clock.now() >= deadline {
            return Err(late());
        }
        before_read();
        match stream.read(&mut chunk) {
            Ok(0) => match done {
                Some(value) => return Ok(value),
                None => return Err(closed()),
            },
            Ok(read) => {
                pending.extend_from_slice(chunk.get(..read).unwrap_or_default());
            }
            Err(error) => {
                // `!=`, not a `WouldBlock` match guard: the guard's
                // always-wait mutant no test can reach on a connected
                // socket, while this form's flipped comparison fails the
                // silent-deadline tests.
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(lost(&error));
                }
                if clock.now() >= deadline {
                    return Err(late());
                }
                before_wait();
                wait_clock(clock, deadline, stream.as_fd(), PollFlags::IN);
            }
        }
        while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let Ok(line) = serde_json::from_slice::<HubLine>(&line) else {
                continue;
            };
            match on_line(&line) {
                Err(error) => return Err(error),
                Ok(Some(value)) => {
                    if wait_for_close {
                        done = Some(value);
                    } else {
                        return Ok(value);
                    }
                }
                Ok(None) => {}
            }
        }
    }
}

/// Parks `clock` until `until`, waiting on `fd` for real while parked:
/// the full bound on a real clock, a short slice on a fake one, which the
/// caller re-checks against the fake deadline after.
pub(crate) fn wait_clock(clock: &dyn Clock, until: Instant, fd: BorrowedFd, events: PollFlags) {
    clock.wait_until(Some(until), &mut |bound| {
        let wait = bound.or(Some(Duration::from_millis(10)));
        let spec = wait.map(|wait| Timespec {
            tv_sec: wait.as_secs().min(i64::MAX as u64).cast_signed(),
            tv_nsec: wait.subsec_nanos() as _,
        });
        let mut fds = [PollFd::new(&fd, events)];
        // The park itself is the wait: a signal delivery or a woken fd
        // both end it, so the count is discarded.
        #[allow(
            clippy::let_underscore_must_use,
            reason = "a woken fd and an expired wait both end the park: the count is unused"
        )]
        let _ = poll(&mut fds, spec.as_ref());
    });
}

/// The hub's captured values for the requested names, from its parsed
/// answer to `ENV_ID`; `None` for any other line. A refusal or a missing
/// answer is `None`, so the caller falls back to the process environment.
/// Malformed lines never reach it: the exchange skips them.
fn answer(line: &HubLine) -> Result<Option<BTreeMap<String, String>>, ()> {
    let Some(result) = crate::sessions::answer_of(line, ENV_ID, "placeholder_env") else {
        return Ok(None);
    };
    let Ok(result) = result else {
        return Err(());
    };
    match result.get("values") {
        None => Err(()),
        Some(values) => serde_json::from_value(values.clone())
            .map(Some)
            .map_err(|_| ()),
    }
}

#[cfg(test)]
#[path = "hub_env_tests.rs"]
mod tests;
