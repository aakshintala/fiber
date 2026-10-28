//! The plain-data session argv (`docs/invocation.md`, "Commands and
//! flags"): the one writer of every session command line. The hub and the
//! delegates build it; `main`'s clap `SessionArgs` parses and converts
//! into it.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::{JobId, SessionId};

/// One session command line, as data: the order and the `--name=value`
/// rules live in [`SessionArgv::to_argv`], the only code that writes a
/// session argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionArgv {
    /// The session's id, minted by whoever starts it.
    pub id: SessionId,
    /// The workspace the session runs in.
    pub workspace: PathBuf,
    /// The model for this run, as a person types it.
    pub model: Option<String>,
    /// One configuration key for this run, `<key>=<value>`, in order.
    pub overrides: Vec<String>,
    /// The first prompt. With none the session waits for a client.
    pub prompt: Option<String>,
    /// Resume the session `--id` names instead of starting it.
    pub resume: bool,
    /// The credential label the resumed session starts on.
    pub credential: Option<String>,
    /// Run the session in a new worktree.
    pub worktree: bool,
    /// Continue the session this rewind names as the session its
    /// `rewound` line points at.
    pub rewound_from: Option<SessionId>,
    /// The session whose worktree this session continues: its id. Set from `--continues`, beside `--rewound-from`.
    pub continues: Option<SessionId>,
    /// The parent session the fork continues: the session whose log
    /// holds the point.
    pub forked_from: Option<SessionId>,
    /// The point in that session, as a `seq`.
    pub forked_seq: Option<u64>,
    /// The parent session: its id.
    pub parent: Option<SessionId>,
    /// The delegate's job id in its parent session.
    pub delegate_id: Option<JobId>,
    /// This session's depth in the delegate tree, passed by its parent.
    pub depth: Option<u32>,
    /// The parent's added folders the child also runs with: the child's
    /// own log records them as its `directory_added` events, so the
    /// delegate tools take no new argument (`docs/delegates.md`, "The
    /// tools").
    pub added: Vec<PathBuf>,
    /// The session's environment came from the login-shell capture.
    pub login_shell: bool,
    /// A program drives the session (`docs/permissions.md`, "Supervised sessions").
    pub supervised: bool,
}

impl Default for SessionArgv {
    fn default() -> Self {
        Self {
            id: SessionId(String::new()),
            workspace: PathBuf::new(),
            model: None,
            overrides: Vec::new(),
            prompt: None,
            resume: false,
            credential: None,
            worktree: false,
            rewound_from: None,
            continues: None,
            forked_from: None,
            forked_seq: None,
            parent: None,
            delegate_id: None,
            depth: None,
            added: Vec::new(),
            login_shell: false,
            supervised: false,
        }
    }
}

impl SessionArgv {
    /// The argv of this session command, starting with `session`: each
    /// `--name=<value>` is one argument so a value starting with `-`
    /// never reads as another flag, and a `-c` value starting with `-`
    /// travels attached for the same reason. The order is fixed, so a
    /// caller and its test read the same list: `session`, `--id`,
    /// `--workspace`, then `--model`, each `-c`, `--resume`, `--prompt`,
    /// `--credential`, `--worktree`, `--rewound-from`, `--continues`, `--forked-from`,
    /// `--forked-seq`, `--parent`,
    /// `--delegate-id`, `--depth`, each `--added`, `--login-shell`,
    /// `--supervised`, each only when set.
    #[must_use]
    pub fn to_argv(&self) -> Vec<OsString> {
        let mut argv = vec![OsString::from("session")];
        argv.push(OsString::from(format!("--id={}", self.id.0)));
        let mut workspace = OsString::from("--workspace=");
        workspace.push(&self.workspace);
        argv.push(workspace);
        if let Some(model) = &self.model {
            argv.push(OsString::from(format!("--model={model}")));
        }
        for override_text in &self.overrides {
            if override_text.starts_with('-') {
                argv.push(OsString::from(format!("-c{override_text}")));
            } else {
                argv.push(OsString::from("-c"));
                argv.push(OsString::from(override_text));
            }
        }
        if self.resume {
            argv.push(OsString::from("--resume"));
        }
        if let Some(prompt) = &self.prompt {
            argv.push(OsString::from(format!("--prompt={prompt}")));
        }
        if let Some(credential) = &self.credential {
            argv.push(OsString::from(format!("--credential={credential}")));
        }
        if self.worktree {
            argv.push(OsString::from("--worktree"));
        }
        if let Some(rewound_from) = &self.rewound_from {
            argv.push(OsString::from(format!("--rewound-from={}", rewound_from.0)));
        }
        if let Some(continues) = &self.continues {
            argv.push(OsString::from(format!("--continues={}", continues.0)));
        }
        if let Some(forked_from) = &self.forked_from {
            argv.push(OsString::from(format!("--forked-from={}", forked_from.0)));
        }
        if let Some(forked_seq) = self.forked_seq {
            argv.push(OsString::from(format!("--forked-seq={forked_seq}")));
        }
        if let Some(parent) = &self.parent {
            argv.push(OsString::from(format!("--parent={}", parent.0)));
        }
        if let Some(delegate_id) = &self.delegate_id {
            argv.push(OsString::from(format!("--delegate-id={}", delegate_id.0)));
        }
        if let Some(depth) = self.depth {
            argv.push(OsString::from(format!("--depth={depth}")));
        }
        // One `--added=<path>` per inherited root, in fold order: the
        // child records each as its own `directory_added`, so the order
        // the parent folded is the order the child replays.
        for added in &self.added {
            let mut arg = OsString::from("--added=");
            arg.push(added);
            argv.push(arg);
        }
        if self.login_shell {
            argv.push(OsString::from("--login-shell"));
        }
        if self.supervised {
            argv.push(OsString::from("--supervised"));
        }
        argv
    }
}

#[cfg(test)]
#[path = "session_argv_tests.rs"]
mod tests;
