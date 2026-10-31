//! `fiber mcp login` and `fiber mcp logout` (`docs/invocation.md`,
//! "Commands and flags"; `docs/mcp.md`, "What Fiber does with MCP"): the
//! login looks the server's URL up in the merged configuration, refuses a
//! repository's server nobody approved, acquires the token through the
//! seam and stores it by canonical URL; the logout deletes the stored
//! token. A token value never reaches an error or a log: only the file
//! name prints, on the login's success line.

use std::io;
use std::path::Path;
use std::sync::Arc;

use config::{Config, McpTokenFiles, Sources, delete_credential};
use contract::clock::Clock;
use contract::events::OfferedKind;
use contract::mcp_remote::McpRemote;
use contract::mcp_tokens::TokenFiles;
use extensions::{Browser, Decision, Index, Store};

use crate::approve::say;
use crate::mcp::Streams;
use crate::{CliError, project_of, with_dirs};

/// A config path segment holding a `.` is quoted, as the startup seam
/// reads it: `mcp.servers."a.b".url`.
fn quoted(segment: &str) -> String {
    if segment.contains('.') {
        format!("\"{segment}\"")
    } else {
        segment.to_owned()
    }
}

/// The server's effective `url` and whether it comes from the repository's
/// layer. A non-string `url` reads as no `url`: the configuration load
/// already refused a malformed file.
fn effective_url(config: &Config, server: &str) -> Option<(String, bool)> {
    let (value, source) = config.get(&format!("mcp.servers.{}.url", quoted(server)), None)?;
    let url = value.as_str()?.to_owned();
    Some((url, matches!(source, config::Source::Repository(_))))
}

/// Whether `mcp.servers` declares `server` at all: a declared stdio server
/// holds no `url`.
fn is_declared(config: &Config, server: &str) -> bool {
    config
        .get(&format!("mcp.servers.{}", quoted(server)), None)
        .is_some()
}

fn unknown(server: &str) -> CliError {
    CliError::McpServerUnknown {
        server: server.to_owned(),
    }
}

fn stdio(server: &str) -> CliError {
    CliError::McpServerStdio {
        server: server.to_owned(),
    }
}

fn userinfo(server: &str) -> CliError {
    CliError::UrlHasUserinfo {
        server: server.to_owned(),
    }
}

fn unapproved(server: &str) -> CliError {
    CliError::McpUnapproved {
        server: server.to_owned(),
    }
}

/// The repository declaration's URL, checked against its approval: the
/// entry is read once from the repository's file, and the URL the login
/// opens is read from that same entry, never from a second read, so the
/// approval and the URL cannot come from different files. `between` runs
/// once after the entry is read and before the hash and decision: the
/// forced-interleaving pause point, with no `#[cfg(test)]` hook in the
/// production path.
fn approved_url(
    home: &Path,
    workspace: &Path,
    project: &config::ProjectKey,
    server: &str,
    effective: &str,
    between: &dyn Fn(),
    clock: &Arc<dyn Clock>,
) -> Result<String, CliError> {
    let declared = config::declared(workspace)?;
    let Some(entry) = declared.mcp_servers.get(server) else {
        return Err(unapproved(server));
    };
    let Some(entry_url) = entry.get("url").and_then(|url| url.as_str()) else {
        return Err(unapproved(server));
    };
    if entry_url != effective {
        return Err(unapproved(server));
    }
    between();
    let item = extensions::mcp_server_item(workspace, server, entry)?;
    let mut index = Index::load(home);
    let hash = extensions::hash(&mut index, &item)?;
    // The index only saves a re-hash; failing to write it costs nothing else.
    index.save().unwrap_or(());
    let approved = Store::new(home, project, Arc::clone(clock))
        .decision(OfferedKind::McpServer, &hash)
        .is_some_and(|decision| decision == Decision::Approve);
    if !approved {
        return Err(unapproved(server));
    }
    Ok(entry_url.to_owned())
}

/// `fiber mcp login <server>`'s target: the URL the flow opens, read
/// once from the merged configuration and checked exactly as the login
/// checks it: a name nothing declares fails, a stdio server fails (only
/// a remote server holds a token), and a repository's server fails
/// unless its current content is approved, with the approval and the URL
/// read from the same entry (`approved_url`). The hub's `mcp_login`
/// entry shares this check, so the two cannot diverge (`docs/mcp.md`,
/// "Servers, their state and the per-server commands"). Returns the
/// loaded configuration with the target, so the caller builds its
/// one-shot writer without a second `Config::load`.
pub fn login_target(
    home: &Path,
    workspace: &Path,
    server: &str,
    between: &dyn Fn(),
    clock: &Arc<dyn Clock>,
    overrides: &[String],
) -> Result<(String, Config), CliError> {
    let (_, project) = project_of(home, workspace)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: overrides.to_vec(),
    })
    .map_err(CliError::from)?;
    let Some((url, from_repository)) = effective_url(&config, server) else {
        if is_declared(&config, server) {
            return Err(stdio(server));
        }
        return Err(unknown(server));
    };
    if from_repository {
        approved_url(home, workspace, &project, server, &url, between, clock)
            .map(|url| (url, config))
    } else {
        Ok((url, config))
    }
}

/// `fiber mcp login <server>`: looks the URL up, refuses an unapproved
/// repository server, and stores the acquired token by canonical URL,
/// replacing any existing one.
#[allow(
    clippy::too_many_arguments,
    reason = "one straight-line command: home, workspace, server, seams, streams and clock"
)]
pub(crate) fn run_login(
    home: &Path,
    workspace: &Path,
    server: &str,
    remote: &dyn McpRemote,
    browser: &dyn Browser,
    between: &dyn Fn(),
    streams: Streams<'_>,
    clock: &Arc<dyn Clock>,
) -> Result<(), CliError> {
    let Streams { out: _, err } = streams;
    let (url, config) = login_target(home, workspace, server, between, clock, &[])?;
    if remote.has_userinfo(&url) {
        return Err(userinfo(server));
    }
    let key = remote.token_key(&url)?;
    let files = McpTokenFiles::new(home.to_path_buf());
    let files_ref: &dyn TokenFiles = &files;
    // The one-shot writer: `Some` at debug, `None` at info — never
    // `None`-at-debug on this HTTP path.
    let debug = crate::debug_writer::debug_writer_for(home, &config, clock);
    if let Err(error) = remote.login(files_ref, &url, &|url| browser.open(url), &**clock, debug) {
        if error.code == contract::ErrorCode::ConfigInvalid {
            // Only the PKCE refusal reaches here with this code: the
            // `url` and userinfo checks above already ran, so prefixing
            // the server names the refusal without printing any code.
            return Err(CliError::McpLoginRefused {
                server: server.to_owned(),
                message: error.message,
            });
        }
        return Err(CliError::from(error));
    }
    say(err, &format!("fiber: stored credentials/mcp/{key}\n"));
    Ok(())
}

/// `fiber mcp logout <server>`: deletes the stored token by key, leaving
/// the lock file. With no stored token it succeeds and says so.
pub(crate) fn run_logout(
    home: &Path,
    workspace: &Path,
    server: &str,
    remote: &dyn McpRemote,
    streams: Streams<'_>,
) -> Result<(), CliError> {
    let Streams { out: _, err } = streams;
    let (_, project) = project_of(home, workspace)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .map_err(CliError::from)?;
    let Some((url, _)) = effective_url(&config, server) else {
        if is_declared(&config, server) {
            return Err(stdio(server));
        }
        return Err(unknown(server));
    };
    if remote.has_userinfo(&url) {
        return Err(userinfo(server));
    }
    let key = remote.token_key(&url)?;
    if delete_credential(home, "mcp", &key)? {
        Ok(())
    } else {
        say(err, &format!("fiber: no token was stored for `{server}`\n"));
        Ok(())
    }
}

/// `fiber mcp login <server>` in the current directory.
pub fn mcp_login(server: &str, remote: &dyn McpRemote, clock: Arc<dyn Clock>) -> i32 {
    with_dirs(|home, workspace| {
        let mut out = io::stdout();
        let mut err = io::stderr();
        let browser = extensions::SystemBrowser::default();
        run_login(
            home,
            workspace,
            server,
            remote,
            &browser,
            &|| {},
            Streams {
                out: &mut out,
                err: &mut err,
            },
            &clock,
        )
    })
}

/// `fiber mcp logout <server>` in the current directory.
pub fn mcp_logout(server: &str, remote: &dyn McpRemote) -> i32 {
    with_dirs(|home, workspace| {
        let mut out = io::stdout();
        let mut err = io::stderr();
        run_logout(
            home,
            workspace,
            server,
            remote,
            Streams {
                out: &mut out,
                err: &mut err,
            },
        )
    })
}

#[cfg(test)]
#[path = "mcp_login_tests.rs"]
mod tests;
