//! The session sandbox toggle (`docs/sandbox.md`, "The rule"): the loop's
//! side of the `sandbox` driver command. The session owns the switch through
//! the [`ApplySandbox`] seam main wires in; the loop applies nothing itself,
//! so the toggle flips one handle and reports on the notice path, and
//! invalidates nothing.

use std::sync::Arc;

use contract::ErrorCode;
use contract::commands::SandboxArgs;
use contract::events::{Event, Notice};
use contract::inbox::Ack;

use crate::inbox::{CLOSING, accept, reject};
use crate::{Error, Loop};

/// Why the sandbox cannot be applied for this session: the closed set of
/// unavailability reports, mirroring `tools`' own cases without its off
/// state, which cannot occur on the enabling path. Unit variants, so the
/// set serialises to its own name rather than to `Ok`/`Err`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// This platform never applies either policy.
    UnsupportedPlatform,
    /// The kernel's Landlock ABI is below the floor of 2.
    AbiBelow2,
    /// `sandbox-exec` is missing or unusable on macOS.
    SandboxExecUnavailable,
    /// A deny path sits inside a writable grant.
    DenyInsideWrite,
    /// A deny path could not be resolved.
    UnresolvableDeny,
    /// A writable root could not be resolved.
    UnresolvedWrite,
}

/// Whether the toggle left the session able to confine: either it can, or
/// it cannot, and the two never mix (`docs/code-quality.md`, "Types"). An
/// already-reported unavailability carries no notice, so the `Option` is
/// required: the shared latch decides `Some` only the first time per
/// session.
#[derive(Debug)]
pub enum Availability {
    /// The toggle flipped the setting; confinement follows it.
    Available,
    /// The toggle flipped the setting on, but the sandbox cannot be
    /// applied: no call is confined, every call is reviewed, and the
    /// session says so once.
    Unavailable {
        /// Why the sandbox cannot be applied.
        cause: Cause,
        /// The one notice, or none once already reported.
        notice: Option<Notice>,
    },
}

/// The session's sandbox switch, wired by the session's assembly: flips the
/// shared setting first, then reports whether the sandbox applies.
pub type ApplySandbox = Arc<dyn Fn(bool) -> Availability + Send + Sync>;

/// The toggle's on reply: the setting holds until `/reload` drops it.
pub(crate) const SANDBOX_ON: &str = "Sandbox on for this session until /reload.";

/// The toggle's off reply.
pub(crate) const SANDBOX_OFF: &str = "Sandbox off for this session.";

impl Loop {
    /// Sets the session sandbox switch; without it the toggle is refused.
    pub fn sandbox_apply(mut self, apply: ApplySandbox) -> Self {
        self.sandbox_apply = Some(apply);
        self
    }

    /// Takes a `sandbox` command: flips the session setting, then reports
    /// on the notice path. Enabling while unavailable sets the setting on
    /// and accepts with no result; the notice goes out only when the shared
    /// latch is unset. Disabling never needs availability. A toggle on an
    /// already-on setting is a no-op with its reply. After `close` the
    /// command is rejected `closing`, as a turn can no longer start.
    pub(crate) fn take_sandbox(&mut self, args: SandboxArgs, ack: Ack) -> Result<(), Error> {
        if self.closing {
            reject(ack, contract::ErrorCode::Closing, CLOSING);
            return Ok(());
        }
        let Some(apply) = self.sandbox_apply.clone() else {
            reject(
                ack,
                contract::ErrorCode::InvalidArguments,
                "This session cannot switch the sandbox.",
            );
            return Ok(());
        };
        match apply(args.enabled) {
            Availability::Available => {
                let message = if args.enabled {
                    SANDBOX_ON
                } else {
                    SANDBOX_OFF
                };
                self.log.append(
                    &Event::Notice(Notice {
                        code: ErrorCode::ToolError,
                        message: message.to_owned(),
                        extension: None,
                    }),
                    None,
                    None,
                )?;
                accept(ack);
            }
            Availability::Unavailable { notice, .. } => {
                if let Some(notice) = notice {
                    self.log.append(&Event::Notice(notice), None, None)?;
                }
                accept(ack);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod tests;
